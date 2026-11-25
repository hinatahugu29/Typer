//! 補助パネル：アウトライン、文書設定バー、ステータスバー、テンプレートの保存・名前変更・削除ダイアログ

use super::*;

impl TyperApp {
    /// テンプレートの保存・名前の変更・削除の確認ダイアログを描画する
    pub(super) fn draw_template_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.template_dialog.take() else {
            return;
        };
        let title = match &dialog {
            TemplateDialog::Save { .. } => "💾 テンプレートとして保存",
            TemplateDialog::Rename { .. } => "✏ テンプレートの名前を変更",
            TemplateDialog::Delete { .. } => "🗑 テンプレートの削除",
        };
        let mut keep_open = true;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| match &mut dialog {
                TemplateDialog::Save {
                    name,
                    overwrite,
                    error,
                } => {
                    ui.label(
                        "現在の文書（フロントマターを含む全体）をテンプレートとして保存します。",
                    );
                    ui.horizontal(|ui| {
                        ui.label("名前:");
                        if ui.text_edit_singleline(name).changed() {
                            *overwrite = false;
                            *error = None;
                        }
                    });
                    if let Some(err) = error {
                        ui.colored_label(Color32::from_rgb(197, 34, 31), err.as_str());
                    }
                    ui.horizontal(|ui| {
                        let label = if *overwrite {
                            "上書き保存"
                        } else {
                            "保存"
                        };
                        if ui.button(label).clicked() {
                            match user_templates_dir() {
                                Some(dir) => {
                                    match self.save_current_as_template(&dir, name, *overwrite) {
                                        Ok(()) => keep_open = false,
                                        Err(e) => {
                                            // 同じ名前があるときは、もう一度押すと上書きする
                                            *overwrite = e.contains("すでに");
                                            *error = Some(if *overwrite {
                                                format!("{e}。もう一度押すと上書きします")
                                            } else {
                                                e
                                            });
                                        }
                                    }
                                }
                                None => *error = Some("保存先のフォルダが分かりません".into()),
                            }
                        }
                        if ui.button("キャンセル").clicked() {
                            keep_open = false;
                        }
                    });
                }
                TemplateDialog::Rename {
                    template,
                    name,
                    error,
                } => {
                    ui.horizontal(|ui| {
                        ui.label("新しい名前:");
                        if ui.text_edit_singleline(name).changed() {
                            *error = None;
                        }
                    });
                    if let Some(err) = error {
                        ui.colored_label(Color32::from_rgb(197, 34, 31), err.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("変更").clicked() {
                            match rename_user_template(template, name) {
                                Ok(_) => {
                                    self.status_message = format!(
                                        "テンプレートの名前を「{}」に変更しました",
                                        name.trim()
                                    );
                                    keep_open = false;
                                }
                                Err(e) => *error = Some(e),
                            }
                        }
                        if ui.button("キャンセル").clicked() {
                            keep_open = false;
                        }
                    });
                }
                TemplateDialog::Delete { template } => {
                    ui.label(format!("テンプレート「{}」を削除しますか？", template.name));
                    ui.label(
                        RichText::new("ファイルは完全に削除され、元に戻せません。")
                            .small()
                            .color(Color32::from_rgb(197, 34, 31)),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("削除する").clicked() {
                            self.status_message = match std::fs::remove_file(&template.path) {
                                Ok(()) => {
                                    format!("テンプレート「{}」を削除しました", template.name)
                                }
                                Err(e) => format!("削除できませんでした: {e}"),
                            };
                            keep_open = false;
                        }
                        if ui.button("キャンセル").clicked() {
                            keep_open = false;
                        }
                    });
                }
            });
        if keep_open {
            self.template_dialog = Some(dialog);
        }
    }

    /// 見出しアウトラインパネルを描画する（クリックで該当見出しへジャンプ）
    pub(super) fn draw_outline_panel(&mut self, ui: &mut egui::Ui) {
        // egui標準のリサイズは中身の描画サイズから幅を記憶し直すため、ドラッグした幅が保持されなかった。
        // 幅はアプリ側で持ち、パネルは固定幅で描いて、右端のつまみを自前で処理する
        let width = self.outline_width.clamp(140.0, 600.0);
        let panel = egui::Panel::left("outline_panel_fixed")
            .resizable(false)
            .exact_size(width)
            .show(ui, |ui| {
                // 中身を常にパネル幅いっぱいに広げ、描画幅と確保幅のずれ（黒い隙間）を防ぐ
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new("🗂 アウトライン").strong().small());
                ui.separator();
                let headings = extract_headings_outline(&self.markdown_text);
                if headings.is_empty() {
                    ui.label(RichText::new("見出しがありません").small().weak());
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("outline_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut clicked: Option<(usize, String, usize)> = None;
                            for (i, (level, title, char_idx)) in headings.iter().enumerate() {
                                let indent = (level.saturating_sub(1)) as f32 * 12.0;
                                ui.horizontal(|ui| {
                                    ui.add_space(indent);
                                    if ui.small_button(title).clicked() {
                                        clicked = Some((i, title.clone(), *char_idx));
                                    }
                                });
                            }
                            if let Some((i, title, char_idx)) = clicked {
                                self.cursor_char_range = Some((char_idx, char_idx));
                                self.pending_cursor_selection = Some((char_idx, char_idx));
                                self.scroll_editor_cursor_to_top = true;
                                let same_title_before =
                                    headings[..i].iter().filter(|h| h.1 == title).count();
                                self.pending_preview_jump = find_preview_heading(
                                    &self.preview_headings,
                                    i,
                                    &title,
                                    same_title_before,
                                );
                            }
                        });
                }
            });

        // 右端（パネル内側）にドラッグ用のつまみ。エディタと重ならないよう内側に置く
        let edge = panel.response.rect.right();
        let handle_rect = egui::Rect::from_min_max(
            egui::pos2(edge - 6.0, panel.response.rect.top()),
            egui::pos2(edge, panel.response.rect.bottom()),
        );
        let handle = ui.interact(
            handle_rect,
            ui.scope_id().with("outline_resize_handle"),
            Sense::drag(),
        );
        if handle.hovered() || handle.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            ui.painter().vline(
                edge - 1.0,
                handle_rect.y_range(),
                Stroke::new(2.0, ui.visuals().selection.bg_fill),
            );
        }
        if handle.dragged() {
            self.outline_width = (self.outline_width + handle.drag_delta().x).clamp(140.0, 600.0);
        }
    }

    /// ページ・基本書式設定GUIバーを描画する（フロントマターと双方向同期）
    pub(super) fn draw_page_settings_bar(&mut self, ui: &mut egui::Ui) {
        // 窓幅が足りないときは右端の項目が見切れないよう折り返し、バーの高さを内容に合わせる
        egui::Panel::top("page_settings_bar")
            .resizable(false)
            .min_size(34.0)
            .show(ui, |ui| {
                let mut doc_settings = parse_settings(&self.markdown_text);
                let mut changed = false;

                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new("⚙ 文書設定:").small().strong());

                    // 用紙サイズと向き
                    let current_paper = crate::model::paper_size(&doc_settings.paper);
                    egui::ComboBox::from_id_salt("paper_size")
                        .width(58.0)
                        .selected_text(current_paper.map(|p| p.label).unwrap_or("A4"))
                        .show_ui(ui, |ui| {
                            for paper in crate::model::PAPER_SIZES {
                                let selected = current_paper == Some(paper);
                                let label = format!(
                                    "{} ({}×{}mm)",
                                    paper.label, paper.width_mm, paper.height_mm
                                );
                                if ui.selectable_label(selected, label).clicked() && !selected {
                                    doc_settings.paper = paper.label.to_string();
                                    changed = true;
                                }
                            }
                        })
                        .response
                        .on_hover_text("用紙サイズ（B4・B5はJIS規格）");
                    let is_portrait = doc_settings.orientation == Orientation::Portrait;
                    if ui.selectable_label(is_portrait, "縦").clicked() && !is_portrait {
                        doc_settings.orientation = Orientation::Portrait;
                        changed = true;
                    }
                    if ui.selectable_label(!is_portrait, "横").clicked() && is_portrait {
                        doc_settings.orientation = Orientation::Landscape;
                        changed = true;
                    }

                    ui.separator();

                    // フォント選択
                    let is_gothic = doc_settings.font_choice == FontChoice::Gothic;
                    let is_mincho = doc_settings.font_choice == FontChoice::Mincho;
                    if ui.selectable_label(is_gothic, "ゴシック体").clicked() && !is_gothic {
                        doc_settings.font_choice = FontChoice::Gothic;
                        changed = true;
                    }
                    if ui.selectable_label(is_mincho, "明朝体").clicked() && !is_mincho {
                        doc_settings.font_choice = FontChoice::Mincho;
                        changed = true;
                    }
                    let is_custom = matches!(doc_settings.font_choice, FontChoice::Custom(_));
                    if ui
                        .selectable_label(is_custom, "カスタム")
                        .on_hover_text(
                            "Windowsにインストール済みの任意のフォント名を指定します（見つからない場合はゴシック体にフォールバックします）",
                        )
                        .clicked()
                        && !is_custom
                    {
                        doc_settings.font_choice = FontChoice::Custom("Meiryo".to_string());
                        changed = true;
                    }
                    if let FontChoice::Custom(name) = &mut doc_settings.font_choice {
                        changed |= font_name_input_with_suggestions(
                            ui,
                            name,
                            &mut self.show_font_suggestions,
                            available_font_families,
                            "例: Meiryo UI",
                        );
                    }

                    ui.label(RichText::new("欧文:").small())
                        .on_hover_text("英数字だけに使うフォント（例: Times New Roman）。空欄なら和文フォントの英数字をそのまま使います");
                    let mut latin = doc_settings.latin_font.clone().unwrap_or_default();
                    if font_name_input_with_suggestions(
                        ui,
                        &mut latin,
                        &mut self.show_latin_font_suggestions,
                        available_latin_font_families,
                        "和文と同じ",
                    ) {
                        let trimmed = latin.trim();
                        doc_settings.latin_font =
                            (!trimmed.is_empty()).then(|| trimmed.to_string());
                        changed = true;
                    }

                    ui.separator();

                    // 余白選択
                    ui.label(RichText::new("余白:").small());
                    for (label, val) in [
                        ("狭 15mm", "15mm"),
                        ("標準 20mm", "20mm"),
                        ("広 25mm", "25mm"),
                    ] {
                        if ui
                            .selectable_label(doc_settings.margin == val, label)
                            .clicked()
                            && doc_settings.margin != val
                        {
                            doc_settings.margin = val.to_string();
                            changed = true;
                        }
                    }

                    ui.separator();

                    // 文字サイズ選択
                    ui.label(RichText::new("文字:").small());
                    for val in ["9.5pt", "10pt", "10.5pt", "11pt", "12pt"] {
                        if ui
                            .selectable_label(doc_settings.font_size == val, val)
                            .clicked()
                            && doc_settings.font_size != val
                        {
                            doc_settings.font_size = val.to_string();
                            changed = true;
                        }
                    }

                    ui.separator();

                    // 基本行間スライダー
                    ui.label(RichText::new("行間:").small());
                    let mut lh = doc_settings.line_height;
                    if ui
                        .add(
                            egui::Slider::new(&mut lh, 1.1..=2.2)
                                .step_by(0.1)
                                .fixed_decimals(1),
                        )
                        .changed()
                    {
                        doc_settings.line_height = lh;
                        changed = true;
                    }

                    ui.separator();

                    // アクセントカラー選択（見出し罫線・表ヘッダ・リンク色等に反映）
                    ui.label(RichText::new("配色:").small());
                    for (label, hex) in [
                        ("青", "#1a73e8"),
                        ("緑", "#188038"),
                        ("赤", "#c5221f"),
                        ("紫", "#8430ce"),
                        ("橙", "#e37400"),
                    ] {
                        let is_selected = doc_settings.accent_color.eq_ignore_ascii_case(hex);
                        if ui.selectable_label(is_selected, label).clicked() && !is_selected {
                            doc_settings.accent_color = hex.to_string();
                            changed = true;
                        }
                    }

                    ui.separator();

                    // ページ番号表示のON/OFF
                    if ui
                        .checkbox(&mut doc_settings.show_page_number, "ページ番号")
                        .on_hover_text(
                            "PDF出力時の各ページ下部のページ番号表示を切り替えます（フロントマターの page-number と連動）",
                        )
                        .changed()
                    {
                        changed = true;
                    }

                    ui.separator();

                    // 段組み選択（1〜3段）
                    ui.label(RichText::new("段組み:").small());
                    for (label, n) in [("1段", 1u8), ("2段", 2u8), ("3段", 3u8)] {
                        if ui
                            .selectable_label(doc_settings.columns == n, label)
                            .clicked()
                            && doc_settings.columns != n
                        {
                            doc_settings.columns = n;
                            changed = true;
                        }
                    }

                    ui.separator();

                    // 透かし文字のON/OFF（フロントマターの watermark と連動）
                    let mut watermark_on = doc_settings.watermark.is_some();
                    if ui
                        .checkbox(&mut watermark_on, "透かし")
                        .on_hover_text(
                            "本文背面に薄く表示する透かし文字（例: 社外秘・DRAFT）を切り替えます",
                        )
                        .changed()
                    {
                        doc_settings.watermark = if watermark_on {
                            Some("社外秘".to_string())
                        } else {
                            None
                        };
                        changed = true;
                    }
                    if let Some(watermark) = doc_settings.watermark.as_mut() {
                        if ui
                            .add(
                                egui::TextEdit::singleline(watermark)
                                    .desired_width(70.0)
                                    .hint_text("社外秘"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });
                ui.add_space(2.0);

                if changed {
                    self.push_undo_snapshot();
                    self.markdown_text =
                        update_document_settings_in_markdown(&self.markdown_text, &doc_settings);
                    self.is_dirty = true;
                    self.revision += 1;
                    self.pending_render = true;
                    self.last_edit_time = Instant::now();
                }
            });
    }

    /// 下部ステータスバーを描画する（ファイル名・カーソル位置・組版状態・直近出力の「開く」ボタン）
    pub(super) fn draw_status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("bottom_status")
            .exact_size(26.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let file_label = self
                        .current_file
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "未保存の文書".to_string());
                    let dirty_mark = if self.is_dirty { " *" } else { "" };
                    ui.label(RichText::new(format!("📄 {file_label}{dirty_mark}")).small());
                    ui.separator();

                    let line_count = self.markdown_text.lines().count();
                    let char_count = self.markdown_text.chars().count();
                    let (cur_line, cur_col) = self
                        .cursor_char_range
                        .map(|(c, _)| char_idx_to_line_col(&self.markdown_text, c))
                        .unwrap_or((1, 1));
                    ui.label(
                        RichText::new(format!(
                            "行 {cur_line}, 列 {cur_col}  ({line_count} 行 / {char_count} 文字)"
                        ))
                        .small(),
                    );
                    ui.separator();

                    let page_count = self.page_textures.len();
                    if self.is_compiling {
                        ui.label(
                            RichText::new("⏳ 組版更新中...")
                                .small()
                                .color(Color32::from_rgb(227, 116, 0)),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "✔ 全 {page_count} ページ (組版 {:.1} ms)",
                                self.last_compile_ms
                            ))
                            .small()
                            .color(Color32::from_rgb(24, 128, 56)),
                        );
                    }

                    ui.separator();
                    ui.label(RichText::new(&self.status_message).small().weak());

                    if self.last_export_path.is_some() {
                        ui.separator();
                        if ui
                            .small_button("📂 開く")
                            .on_hover_text("直近に出力したファイルを既定のアプリで開きます")
                            .clicked()
                        {
                            self.action_open_last_export();
                        }
                    }
                });
            });
    }
}
