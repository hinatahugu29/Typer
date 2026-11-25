//! ファイル操作：開く・保存・自動保存・最近使ったファイル・PDF/HTML/Word出力・ドロップ・自分のテンプレート・未保存確認

use super::*;

impl TyperApp {
    pub(super) fn request_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        if self.is_dirty {
            self.pending_action = Some(action);
        } else {
            self.execute_pending_action(action, ctx);
        }
    }

    /// アクティブなタブに限らず、バックグラウンドの他タブも含めて未保存の変更があるか
    pub(super) fn any_unsaved_changes(&self) -> bool {
        self.is_dirty || self.tabs.iter().flatten().any(|snapshot| snapshot.is_dirty)
    }

    pub(super) fn execute_pending_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        match action {
            PendingAction::NewFile => {
                self.reset_to_blank_document();
                self.status_message = "新規文書を作成しました".to_string();
            }
            PendingAction::OpenFileDialog => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Markdown文書 (*.md, *.txt)", &["md", "txt"])
                    .pick_file()
                {
                    self.load_file_from_path(path);
                }
            }
            PendingAction::LoadFilePath(path) => {
                self.load_file_from_path(path);
            }
            PendingAction::CloseWindow => {
                self.allow_immediate_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            PendingAction::CloseTab => {
                self.close_active_tab();
            }
        }
    }

    /// 現在のアクティブ文書を、フロントマターの既定値を持つ空の新規文書にリセットする
    /// （`PendingAction::NewFile` と「最後の1タブを閉じる」の両方から共有される）
    pub(super) fn reset_to_blank_document(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.markdown_text = BLANK_DOCUMENT_TEMPLATE.to_string();
        self.current_file = None;
        self.is_dirty = false;
        self.cursor_char_range = None;
        self.pending_cursor_selection = None;
        self.last_export_path = None;
        self.revision += 1;
        self.trigger_preview_update();
    }

    pub(super) fn load_file_from_path(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                self.undo_stack.clear();
                self.redo_stack.clear();
                self.markdown_text = content;
                self.current_file = Some(path.clone());
                self.is_dirty = false;
                self.revision += 1;
                self.trigger_preview_update();
                self.status_message = format!("読み込み完了: {}", path.display());
                self.record_recent_file(&path);
            }
            Err(e) => {
                self.status_message = format!("ファイル読み込みエラー: {e}");
            }
        }
    }

    /// 最近使ったファイル一覧の先頭に追加し、重複排除・件数上限・永続化を行う
    pub(super) fn record_recent_file(&mut self, path: &Path) {
        let path_buf = path.to_path_buf();
        self.recent_files.retain(|p| p != &path_buf);
        self.recent_files.insert(0, path_buf);
        self.recent_files.truncate(8);
        if self.persist_settings {
            save_recent_files(&self.recent_files);
        }
    }

    /// 編集停止から一定時間が経過していれば自動保存バックアップを書き出す（失敗は無視）
    pub(super) fn maybe_autosave(&mut self) {
        const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);
        // 表示中のタブだけでなく、裏のタブの未保存の変更も対象にする。
        // 前回の自動保存から何も変わっていなければ書き直さない
        if !self.any_unsaved_changes() || self.revision == self.last_autosaved_revision {
            return;
        }
        if self.last_autosave_time.elapsed() < AUTOSAVE_INTERVAL {
            return;
        }
        self.last_autosave_time = Instant::now();
        self.last_autosaved_revision = self.revision;
        for (path, text) in self.autosave_targets() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, text);
        }
        // 異常終了時にも直近のタブ構成が残るよう、自動保存のタイミングでセッションも書き出す
        self.save_session_now();
    }

    /// 自動保存で書き出す (保存先, 本文) の一覧。未保存の変更があるタブすべてが対象
    pub(super) fn autosave_targets(&self) -> Vec<(PathBuf, String)> {
        let mut untitled = 0usize;
        let mut targets = Vec::new();
        for (i, slot) in self.tabs.iter().enumerate() {
            let (file, dirty, text) = match slot {
                Some(s) => (&s.current_file, s.is_dirty, &s.markdown_text),
                None if i == self.active_tab => {
                    (&self.current_file, self.is_dirty, &self.markdown_text)
                }
                None => continue,
            };
            let index = if file.is_none() {
                untitled += 1;
                untitled - 1
            } else {
                0
            };
            if !dirty {
                continue;
            }
            if let Some(path) = autosave_path_for(file, index) {
                targets.push((path, text.clone()));
            }
        }
        targets
    }

    /// 未保存の新規文書のファイル名候補を推測する（優先順位: フロントマターの `title:` → 最初のH1見出し → 既定値）
    pub(super) fn suggest_file_name(&self) -> String {
        let settings = parse_settings(&self.markdown_text);
        let candidate = settings.title.filter(|t| !t.trim().is_empty()).or_else(|| {
            extract_headings_outline(&self.markdown_text)
                .into_iter()
                .find(|(level, _, _)| *level == 1)
                .map(|(_, text, _)| text)
                .filter(|t| !t.trim().is_empty())
        });

        match candidate {
            Some(name) => format!("{}.md", sanitize_file_name(&name)),
            None => "report.md".to_string(),
        }
    }

    pub(super) fn action_save_file(&mut self, save_as: bool) -> bool {
        let target_path = if save_as || self.current_file.is_none() {
            let default_name = self
                .current_file
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| self.suggest_file_name());
            rfd::FileDialog::new()
                .add_filter("Markdown文書 (*.md)", &["md"])
                .set_file_name(&default_name)
                .save_file()
        } else {
            self.current_file.clone()
        };

        if let Some(path) = target_path {
            match std::fs::write(&path, &self.markdown_text) {
                Ok(()) => {
                    self.current_file = Some(path.clone());
                    self.is_dirty = false;
                    self.revision += 1;
                    self.trigger_preview_update();
                    self.status_message = format!("保存しました: {}", path.display());
                    self.record_recent_file(&path);
                    return true;
                }
                Err(e) => {
                    self.status_message = format!("保存エラー: {e}");
                }
            }
        }
        false
    }

    pub(super) fn action_export_pdf(&mut self) {
        let default_name = self
            .current_file
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(|s| format!("{s}.pdf"))
            .unwrap_or_else(|| "report.pdf".to_string());

        if let Some(path) = rfd::FileDialog::new()
            .add_filter("PDF文書 (*.pdf)", &["pdf"])
            .set_file_name(&default_name)
            .save_file()
        {
            let start = Instant::now();
            let doc = self.parse_for_export();
            let base = self.base_dir();
            let (typst_code, line_map) = generate_typst_source_with_map(&doc, base.as_deref());
            let mut engine = TypstEngine::new();
            match engine.export_pdf(&typst_code) {
                Ok(pdf_bytes) => match std::fs::write(&path, pdf_bytes) {
                    Ok(()) => {
                        let ms = start.elapsed().as_secs_f64() * 1000.0;
                        self.status_message =
                            format!("PDFを出力しました ({ms:.1} ms): {}", path.display());
                        self.last_export_path = Some(path);
                    }
                    Err(e) => {
                        self.status_message = format!("PDF書き込みエラー: {e}");
                    }
                },
                Err(e) => {
                    let described: Vec<String> = e
                        .describe(Some(&line_map))
                        .into_iter()
                        .map(|d| d.text)
                        .collect();
                    self.status_message = format!("PDF組版エラー: {}", described.join(" / "));
                }
            }
        }
    }

    /// 出力用に原稿を解析する。`title` も見出しも無い文書は、ファイル名をタイトルにする
    pub(super) fn parse_for_export(&self) -> crate::model::Document {
        let mut doc = parse_document(&self.markdown_text);
        if crate::parser::document_title(&doc).is_none() {
            doc.settings.title = self
                .current_file
                .as_ref()
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .map(str::to_string);
        }
        doc
    }

    pub(super) fn action_export_html(&mut self) {
        let default_name = self
            .current_file
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(|s| format!("{s}.html"))
            .unwrap_or_else(|| "report.html".to_string());

        if let Some(path) = rfd::FileDialog::new()
            .add_filter("HTML文書 (*.html)", &["html"])
            .set_file_name(&default_name)
            .save_file()
        {
            let doc = self.parse_for_export();
            let base = self.base_dir();
            let html = generate_html_with_base(&doc, base.as_deref());
            match std::fs::write(&path, html) {
                Ok(()) => {
                    self.status_message = format!("HTMLを出力しました: {}", path.display());
                    self.last_export_path = Some(path);
                }
                Err(e) => {
                    self.status_message = format!("HTML書き込みエラー: {e}");
                }
            }
        }
    }

    pub(super) fn action_export_docx(&mut self) {
        let default_name = self
            .current_file
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(|s| format!("{s}.docx"))
            .unwrap_or_else(|| "report.docx".to_string());

        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Word文書 (*.docx)", &["docx"])
            .set_file_name(&default_name)
            .save_file()
        {
            let doc = self.parse_for_export();
            let base = self.base_dir();
            match generate_docx_bytes_with_base(&doc, base.as_deref()) {
                Ok(bytes) => match std::fs::write(&path, bytes) {
                    Ok(()) => {
                        self.status_message =
                            format!("Wordファイルを出力しました: {}", path.display());
                        self.last_export_path = Some(path);
                    }
                    Err(e) => {
                        self.status_message = format!("Word書き込みエラー: {e}");
                    }
                },
                Err(e) => {
                    self.status_message = format!("Word変換エラー: {e}");
                }
            }
        }
    }

    /// 直近にエクスポートしたファイルをOS既定のアプリケーションで開く
    pub(super) fn action_open_last_export(&mut self) {
        if let Some(path) = &self.last_export_path {
            let result = std::process::Command::new("cmd")
                .args(["/C", "start", ""])
                .arg(path)
                .spawn();
            if let Err(e) = result {
                self.status_message = format!("ファイルを開けませんでした: {e}");
            }
        }
    }

    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });

        for path in dropped {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "md" | "txt" | "markdown") {
                // ドロップされた文書は現在のタブを置き換えず、新規タブとして開く
                self.open_new_tab();
                self.load_file_from_path(path);
                break;
            } else if matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp"
            ) {
                self.insert_image_path(&path);
            }
        }
    }

    /// 自分のテンプレートを、新しいタブの未保存の文書として開く（テンプレートのファイル自体は変えない）
    pub(super) fn open_user_template(&mut self, template: &UserTemplate) {
        match std::fs::read_to_string(&template.path) {
            Ok(content) => {
                self.open_new_tab();
                self.markdown_text = content;
                self.revision += 1;
                self.trigger_preview_update();
                self.status_message = format!(
                    "テンプレート「{}」から新しい文書を作りました",
                    template.name
                );
            }
            Err(e) => {
                self.status_message = format!("テンプレートを読み込めませんでした: {e}");
            }
        }
    }

    /// 現在の文書をテンプレートとして保存する。同じ名前があり `overwrite` が偽なら `Err`
    pub(super) fn save_current_as_template(
        &mut self,
        dir: &Path,
        name: &str,
        overwrite: bool,
    ) -> Result<(), String> {
        let path = save_user_template(dir, name, &self.markdown_text, overwrite)?;
        let saved_name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        self.status_message = format!(
            "テンプレート「{saved_name}」として保存しました（📋 テンプレート から使えます）"
        );
        Ok(())
    }

    pub(super) fn open_user_templates_folder(&mut self) {
        let Some(dir) = user_templates_dir() else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        if let Err(e) = std::process::Command::new("explorer").arg(&dir).spawn() {
            self.status_message = format!("フォルダを開けませんでした: {e}");
        }
    }

    /// 未保存の変更があるときの確認ダイアログ（保存して続行 / 保存せずに続行 / キャンセル）
    pub(super) fn draw_unsaved_changes_dialog(&mut self, ctx: &egui::Context) {
        // 未保存確認モーダルダイアログ
        if let Some(action) = self.pending_action.clone() {
            egui::Window::new("⚠ 未保存の変更があります")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("現在の文書にはまだ保存されていない変更があります。");
                    ui.label("変更を保存してから続行しますか？");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let save_btn = egui::Button::new(
                            RichText::new("💾 保存して続行")
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(26, 115, 232));
                        if ui.add(save_btn).clicked() {
                            if self.action_save_file(false) {
                                self.pending_action = None;
                                self.execute_pending_action(action.clone(), ctx);
                            }
                        }
                        if ui.button("保存せずに続行").clicked() {
                            self.pending_action = None;
                            self.execute_pending_action(action.clone(), ctx);
                        }
                        if ui.button("キャンセル").clicked() {
                            self.pending_action = None;
                        }
                    });
                });
        }
    }
}
