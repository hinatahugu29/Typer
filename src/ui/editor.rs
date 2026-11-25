//! エディタ：本文の編集欄、カーソル位置への挿入・選択範囲のラップ、表の貼り付け、画像挿入、検索・置換

use super::*;

impl TyperApp {
    pub(super) fn action_insert_image_dialog(&mut self) {
        if let Some(img_path) = rfd::FileDialog::new()
            .add_filter(
                "画像ファイル (*.png, *.jpg, *.jpeg, *.gif, *.svg, *.webp)",
                &["png", "jpg", "jpeg", "gif", "svg", "webp"],
            )
            .pick_file()
        {
            self.insert_image_path(&img_path);
        }
    }

    pub(super) fn insert_image_path(&mut self, img_path: &Path) {
        let stem = img_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("画像");
        let display_path = if let Some(base) = self.base_dir() {
            if let Ok(rel) = img_path.strip_prefix(&base) {
                rel.to_string_lossy().replace('\\', "/")
            } else {
                img_path.to_string_lossy().replace('\\', "/")
            }
        } else if let Ok(cwd) = std::env::current_dir() {
            if let Ok(rel) = img_path.strip_prefix(&cwd) {
                rel.to_string_lossy().replace('\\', "/")
            } else {
                img_path.to_string_lossy().replace('\\', "/")
            }
        } else {
            img_path.to_string_lossy().replace('\\', "/")
        };

        let snippet = format!("![{stem}]({display_path})");
        self.insert_at_cursor(&snippet);
        self.status_message = format!("画像を挿入しました: {display_path}");
    }

    /// 現在のカーソル位置にテキストを挿入する（選択範囲がある場合は置き換える）
    pub(super) fn insert_at_cursor(&mut self, snippet: &str) {
        self.push_undo_snapshot();

        let total_chars = self.markdown_text.chars().count();
        let (start_char, end_char) = self.cursor_char_range.unwrap_or((total_chars, total_chars));
        let start_byte = char_to_byte_idx(&self.markdown_text, start_char);
        let end_byte = char_to_byte_idx(&self.markdown_text, end_char);

        self.markdown_text
            .replace_range(start_byte..end_byte, snippet);

        let snippet_chars = snippet.chars().count();
        let new_pos = start_char + snippet_chars;
        self.cursor_char_range = Some((new_pos, new_pos));
        self.pending_cursor_selection = Some((new_pos, new_pos));

        self.is_dirty = true;
        self.revision += 1;
        self.pending_render = true;
        self.last_edit_time = Instant::now();
    }

    /// クリップボードの表データ（Excelなどでコピーしたセル）をMarkdownの表にして、カーソル位置に貼り付ける
    pub(super) fn paste_table_from_clipboard(&mut self) {
        let text = arboard::Clipboard::new().and_then(|mut c| c.get_text());
        match text {
            Ok(text) => self.insert_table_from_tsv(&text),
            Err(_) => {
                self.status_message = "クリップボードの文字を読み取れませんでした".to_string();
            }
        }
    }

    /// タブ区切りのテキストを表に変換して挿入する（変換できなければ何もせず知らせる）
    pub(super) fn insert_table_from_tsv(&mut self, text: &str) {
        match tsv_to_markdown_table(text) {
            Some(table) => {
                let rows = table.lines().count().saturating_sub(2);
                self.insert_at_cursor(&format!("\n{table}\n"));
                self.status_message = format!("表を貼り付けました（見出し行＋{rows}行）");
            }
            None => {
                self.status_message =
                    "クリップボードに表のデータがありません（Excelなどでセルを範囲選択してコピーしてください）"
                        .to_string();
            }
        }
    }

    /// 選択中のテキストがあれば `prefix` と `suffix` で囲み、なければ `prefix + default_inner + suffix` を挿入する
    pub(super) fn wrap_selection_or_insert(
        &mut self,
        prefix: &str,
        default_inner: &str,
        suffix: &str,
    ) {
        self.push_undo_snapshot();

        let total_chars = self.markdown_text.chars().count();
        let (start_char, end_char) = self.cursor_char_range.unwrap_or((total_chars, total_chars));
        let start_byte = char_to_byte_idx(&self.markdown_text, start_char);
        let end_byte = char_to_byte_idx(&self.markdown_text, end_char);

        let prefix_chars = prefix.chars().count();
        let suffix_chars = suffix.chars().count();

        if start_char < end_char {
            let selected = self.markdown_text[start_byte..end_byte].to_string();
            let replacement = format!("{prefix}{selected}{suffix}");
            let selected_chars = selected.chars().count();
            self.markdown_text
                .replace_range(start_byte..end_byte, &replacement);
            let new_start = start_char + prefix_chars;
            let new_end = new_start + selected_chars;
            self.cursor_char_range = Some((new_start, new_end));
            self.pending_cursor_selection = Some((new_start, new_end));
        } else {
            let replacement = format!("{prefix}{default_inner}{suffix}");
            let inner_chars = default_inner.chars().count();
            self.markdown_text
                .replace_range(start_byte..end_byte, &replacement);
            let new_start = start_char + prefix_chars;
            let new_end = new_start + inner_chars;
            let _ = suffix_chars;
            self.cursor_char_range = Some((new_start, new_end));
            self.pending_cursor_selection = Some((new_start, new_end));
        }

        self.is_dirty = true;
        self.revision += 1;
        self.pending_render = true;
        self.last_edit_time = Instant::now();
    }

    /// 検索クエリに一致するすべての文字インデックス範囲 `(start_char, end_char)` を返す
    pub(super) fn find_matches(&self) -> Vec<(usize, usize)> {
        if self.find_query.is_empty() {
            return Vec::new();
        }
        let q_chars = self.find_query.chars().count();

        if self.find_ignore_case {
            let haystack: Vec<char> = self.markdown_text.chars().collect();
            let needle: Vec<char> = self.find_query.chars().collect();
            let mut results = Vec::new();
            if haystack.len() >= needle.len() {
                for start in 0..=(haystack.len() - needle.len()) {
                    let matched = needle
                        .iter()
                        .enumerate()
                        .all(|(j, &nc)| haystack[start + j].to_lowercase().eq(nc.to_lowercase()));
                    if matched {
                        results.push((start, start + q_chars));
                    }
                }
            }
            results
        } else {
            let mut results = Vec::new();
            for (byte_idx, _) in self.markdown_text.match_indices(&self.find_query) {
                let start_char = byte_to_char_idx(&self.markdown_text, byte_idx);
                results.push((start_char, start_char + q_chars));
            }
            results
        }
    }

    pub(super) fn select_find_match(&mut self, match_idx: usize) {
        let matches = self.find_matches();
        if matches.is_empty() {
            return;
        }
        let idx = match_idx % matches.len();
        self.current_match_index = idx;
        let (s, e) = matches[idx];
        self.cursor_char_range = Some((s, e));
        self.pending_cursor_selection = Some((s, e));
    }

    pub(super) fn replace_current_match(&mut self) {
        let matches = self.find_matches();
        if matches.is_empty() {
            return;
        }
        self.push_undo_snapshot();
        let idx = self.current_match_index % matches.len();
        let (s_char, e_char) = matches[idx];
        let s_byte = char_to_byte_idx(&self.markdown_text, s_char);
        let e_byte = char_to_byte_idx(&self.markdown_text, e_char);

        let rep = self.replace_text.clone();
        let rep_chars = rep.chars().count();
        self.markdown_text.replace_range(s_byte..e_byte, &rep);

        self.cursor_char_range = Some((s_char, s_char + rep_chars));
        self.pending_cursor_selection = Some((s_char, s_char + rep_chars));
        self.is_dirty = true;
        self.revision += 1;
        self.pending_render = true;
        self.last_edit_time = Instant::now();
    }

    pub(super) fn replace_all_matches(&mut self) {
        let matches = self.find_matches();
        if matches.is_empty() {
            return;
        }
        self.push_undo_snapshot();
        // 後ろから置換することで、前方のマッチのバイトオフセットが崩れないようにする
        for &(s_char, e_char) in matches.iter().rev() {
            let s_byte = char_to_byte_idx(&self.markdown_text, s_char);
            let e_byte = char_to_byte_idx(&self.markdown_text, e_char);
            self.markdown_text
                .replace_range(s_byte..e_byte, &self.replace_text);
        }
        self.is_dirty = true;
        self.revision += 1;
        self.pending_render = true;
        self.last_edit_time = Instant::now();
        self.status_message = format!("{} 件をすべて置換しました", matches.len());
    }

    /// エディタのカーソルを原稿の指定行（1始まり）へ移し、その行を選択して表示する
    pub(super) fn jump_to_source_line(&mut self, line: usize) {
        let Some((start, end)) = line_char_range(&self.markdown_text, line) else {
            return;
        };
        self.cursor_char_range = Some((start, end));
        self.pending_cursor_selection = Some((start, end));
        self.status_message = format!("原稿の{line}行目へ移動しました");
    }

    /// 検索・置換バーを描画する（一致件数表示・前後ジャンプ・個別/一括置換）
    pub(super) fn draw_find_replace_bar(&mut self, ui: &mut egui::Ui) {
        egui::Frame::NONE
            .fill(Color32::from_rgb(241, 243, 244))
            .inner_margin(6.0)
            .corner_radius(4.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("🔍 検索:").small());
                    let find_resp = ui.add(
                        TextEdit::singleline(&mut self.find_query)
                            .desired_width(160.0)
                            .hint_text("検索語句..."),
                    );
                    let matches = self.find_matches();
                    let match_count = matches.len();
                    if find_resp.changed() {
                        self.current_match_index = 0;
                        if match_count > 0 {
                            self.select_find_match(0);
                        }
                    }

                    ui.label(RichText::new(format!("{match_count} 件")).small().weak());

                    if ui
                        .checkbox(&mut self.find_ignore_case, "Aa")
                        .on_hover_text("大文字・小文字を区別しない")
                        .changed()
                    {
                        self.current_match_index = 0;
                    }

                    if ui
                        .add_enabled(match_count > 0, egui::Button::new("◀ 前へ").small())
                        .clicked()
                    {
                        let prev = if self.current_match_index == 0 {
                            match_count.saturating_sub(1)
                        } else {
                            self.current_match_index - 1
                        };
                        self.select_find_match(prev);
                    }
                    if ui
                        .add_enabled(match_count > 0, egui::Button::new("次へ ▶").small())
                        .clicked()
                    {
                        self.select_find_match(self.current_match_index + 1);
                    }

                    ui.separator();
                    ui.label(RichText::new("置換:").small());
                    ui.add(
                        TextEdit::singleline(&mut self.replace_text)
                            .desired_width(150.0)
                            .hint_text("置換後の語句..."),
                    );

                    if ui
                        .add_enabled(match_count > 0, egui::Button::new("置換").small())
                        .clicked()
                    {
                        self.replace_current_match();
                    }
                    if ui
                        .add_enabled(match_count > 0, egui::Button::new("すべて置換").small())
                        .clicked()
                    {
                        self.replace_all_matches();
                    }

                    if ui
                        .small_button("✖")
                        .on_hover_text("検索バーを閉じる")
                        .clicked()
                    {
                        self.show_find_replace = false;
                    }
                });
            });
    }

    /// シンタックスハイライト付きのMarkdownエディタ（カーソル位置の反映・変更時のUndo記録）
    pub(super) fn draw_editor(&mut self, ui: &mut egui::Ui) {
        // カスタムシンタックスハイライト付きエディタ
        let dark_mode = self.dark_mode;
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut layout_job = highlight_custom_markdown(text.as_str(), dark_mode);
            layout_job.wrap.max_width = wrap_width;
            ui.fonts_mut(|f| f.layout_job(layout_job))
        };

        let prev_snapshot = self.markdown_text.clone();

        egui::ScrollArea::vertical()
            .id_salt("editor_scroll")
            .show(ui, |ui| {
                let mut output = TextEdit::multiline(&mut self.markdown_text)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .min_size(ui.available_size())
                    .lock_focus(true)
                    .layouter(&mut layouter)
                    .show(ui);

                // 挿入・検索ジャンプ後のカーソル位置反映
                if let Some((start_c, end_c)) = self.pending_cursor_selection.take() {
                    let range = CCursorRange::two(CCursor::new(start_c), CCursor::new(end_c));
                    output.state.cursor.set_char_range(Some(range));
                    output.state.store(ui.ctx(), output.response.id);
                    output.response.request_focus();

                    // カーソルを動かしただけではエディタはスクロールしないため、明示的に表示位置を合わせる
                    let cursor_rect = output
                        .galley
                        .pos_from_cursor(CCursor::new(start_c))
                        .translate(output.galley_pos.to_vec2());
                    let align = if std::mem::take(&mut self.scroll_editor_cursor_to_top) {
                        Some(egui::Align::Min)
                    } else {
                        None
                    };
                    ui.scroll_to_rect(cursor_rect, align);
                }

                // 現在のカーソル・選択位置を記録
                if let Some(range) = output.cursor_range {
                    let sorted = range.as_sorted_char_range();
                    self.cursor_char_range = Some((sorted.start.into(), sorted.end.into()));
                }

                if output.response.changed() {
                    if self.undo_stack.last().map(|s| s.as_str()) != Some(prev_snapshot.as_str()) {
                        if self.undo_stack.len() >= 100 {
                            self.undo_stack.remove(0);
                        }
                        self.undo_stack.push(prev_snapshot);
                        self.redo_stack.clear();
                    }
                    self.is_dirty = true;
                    self.revision += 1;
                    self.pending_render = true;
                    self.last_edit_time = Instant::now();
                }
            });
    }
}
