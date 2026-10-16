//! デスクトップUI層 (`egui` / `eframe`)
//!
//! - 左ペイン：シンタックスハイライト付きエディタ、カーソル位置への書式挿入・選択範囲ラップ、
//!   検索・置換バー (`Ctrl+F` / `Ctrl+H`)、Undo/Redo履歴、ページ設定GUIバー
//! - 右ペイン：Typst組版結果のリアルタイムページプレビュー（`Ctrl+ホイール` ズーム・複数ページ対応）
//! - 安全性・操作性：未保存変更時の終了・読み込み確認モーダル、ドラッグ＆ドロップ読み込み（文書・画像）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, FontData, FontDefinitions, FontFamily, Key, Modifiers, RichText,
    Sense, Stroke, TextEdit, TextFormat, TextureHandle, TextureOptions, Vec2, ViewportCommand,
    text::{CCursor, CCursorRange, LayoutJob},
};

use crate::html_export::generate_html_with_base;
use crate::model::{FontChoice, Orientation};
use crate::parser::{parse_document, update_document_settings_in_markdown};
use crate::sample::SAMPLE_DOCUMENT;
use crate::templates::TEMPLATES;
use crate::typst_engine::{PreviewOutput, TypstEngine};
use crate::typst_gen::generate_typst_source_with_base;

enum WorkerRequest {
    RenderPreview {
        revision: u64,
        typst_code: String,
        pixel_per_pt: f32,
    },
}

enum WorkerResponse {
    PreviewReady {
        revision: u64,
        result: Result<PreviewOutput, String>,
    },
}

/// 未保存の変更がある状態で実行しようとした保留アクション
#[derive(Debug, Clone)]
enum PendingAction {
    NewFile,
    OpenFileDialog,
    LoadFilePath(PathBuf),
    CloseWindow,
}

pub struct TyperApp {
    /// 編集中の原稿テキスト
    markdown_text: String,
    /// 直近生成されたTypstソース（デバッグ・確認表示用）
    last_typst_code: String,
    /// 現在開いているファイルパス
    current_file: Option<PathBuf>,
    /// 未保存の変更があるか
    is_dirty: bool,
    /// 直近設定したウィンドウタイトル
    last_window_title: String,

    /// Undo / Redo スタック
    undo_stack: Vec<String>,
    redo_stack: Vec<String>,

    /// エディタの現在のカーソル・選択範囲（文字インデックス単位）
    cursor_char_range: Option<(usize, usize)>,
    /// 次フレームでエディタに適用する新しいカーソル・選択範囲（文字インデックス単位）
    pending_cursor_selection: Option<(usize, usize)>,

    /// 検索・置換パネルの状態
    show_find_replace: bool,
    find_query: String,
    replace_text: String,
    current_match_index: usize,

    /// ページ・基本書式設定GUIバーの表示フラグ
    show_page_settings_bar: bool,

    /// 未保存確認ダイアログ用の保留アクション
    pending_action: Option<PendingAction>,
    allow_immediate_close: bool,

    /// プレビュー更新のデバウンス管理
    revision: u64,
    pending_render: bool,
    last_edit_time: Instant,

    /// バックグラウンド組版ワーカーとの通信チャネル
    worker_tx: Sender<WorkerRequest>,
    worker_rx: Receiver<WorkerResponse>,
    is_compiling: bool,

    /// レンダリング済みページテクスチャ
    page_textures: Vec<(TextureHandle, Vec2)>,
    last_compile_ms: f64,
    last_error: Option<String>,

    /// プレビュー表示倍率（1.0 = 100%）
    zoom: f32,
    /// 生成されたTypstコードを確認するサブパネル表示フラグ
    show_typst_source: bool,
    /// ステータスバー通知メッセージ
    status_message: String,
    /// eframe 0.37 Windowsの初期非表示デッドロック回避用フラグ
    startup_visibility_ensured: bool,
}

impl TyperApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        if let Some(window) = cc.winit_window() {
            window.set_visible(true);
            window.focus_window();
        }
        cc.egui_ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        cc.egui_ctx.send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);

        setup_japanese_fonts(&cc.egui_ctx);
        setup_visual_theme(&cc.egui_ctx);

        let (req_tx, req_rx) = mpsc::channel::<WorkerRequest>();
        let (res_tx, res_rx) = mpsc::channel::<WorkerResponse>();
        let ctx_clone = cc.egui_ctx.clone();

        // 組版処理をUIスレッドから完全に分離するワーカースレッド
        std::thread::spawn(move || {
            let mut engine = TypstEngine::new();
            while let Ok(req) = req_rx.recv() {
                match req {
                    WorkerRequest::RenderPreview {
                        mut revision,
                        mut typst_code,
                        mut pixel_per_pt,
                    } => {
                        while let Ok(newer) = req_rx.try_recv() {
                            match newer {
                                WorkerRequest::RenderPreview {
                                    revision: r,
                                    typst_code: c,
                                    pixel_per_pt: p,
                                } => {
                                    revision = r;
                                    typst_code = c;
                                    pixel_per_pt = p;
                                }
                            }
                        }

                        let result = engine.render_preview(&typst_code, pixel_per_pt);
                        let _ = res_tx.send(WorkerResponse::PreviewReady { revision, result });
                        ctx_clone.request_repaint();
                    }
                }
            }
        });

        let initial_text = SAMPLE_DOCUMENT.to_string();
        let doc = parse_document(&initial_text);
        let typst_code = generate_typst_source_with_base(&doc, None);

        let mut app = Self {
            markdown_text: initial_text,
            last_typst_code: typst_code,
            current_file: None,
            is_dirty: false,
            last_window_title: String::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            cursor_char_range: None,
            pending_cursor_selection: None,
            show_find_replace: false,
            find_query: String::new(),
            replace_text: String::new(),
            current_match_index: 0,
            show_page_settings_bar: true,
            pending_action: None,
            allow_immediate_close: false,
            revision: 1,
            pending_render: false,
            last_edit_time: Instant::now(),
            worker_tx: req_tx,
            worker_rx: res_rx,
            is_compiling: true,
            page_textures: Vec::new(),
            last_compile_ms: 0.0,
            last_error: None,
            zoom: 1.0,
            show_typst_source: false,
            status_message: "準備完了 — サンプル報告書を読み込みました".to_string(),
            startup_visibility_ensured: false,
        };

        app.trigger_preview_update();
        app
    }

    fn base_dir(&self) -> Option<PathBuf> {
        self.current_file
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
    }

    fn push_undo_snapshot(&mut self) {
        if self.undo_stack.last().map(|s| s.as_str()) != Some(self.markdown_text.as_str()) {
            if self.undo_stack.len() >= 100 {
                self.undo_stack.remove(0);
            }
            self.undo_stack.push(self.markdown_text.clone());
            self.redo_stack.clear();
        }
    }

    fn perform_undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.markdown_text.clone());
            self.markdown_text = prev;
            self.is_dirty = true;
            self.revision += 1;
            self.pending_render = true;
            self.last_edit_time = Instant::now();
            self.status_message = "元に戻しました (Undo)".to_string();
        }
    }

    fn perform_redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.markdown_text.clone());
            self.markdown_text = next;
            self.is_dirty = true;
            self.revision += 1;
            self.pending_render = true;
            self.last_edit_time = Instant::now();
            self.status_message = "やり直しました (Redo)".to_string();
        }
    }

    fn trigger_preview_update(&mut self) {
        let doc = parse_document(&self.markdown_text);
        let base = self.base_dir();
        self.last_typst_code = generate_typst_source_with_base(&doc, base.as_deref());
        self.is_compiling = true;
        self.pending_render = false;

        let pixel_per_pt = (2.0 * self.zoom).clamp(1.2, 3.5);
        let _ = self.worker_tx.send(WorkerRequest::RenderPreview {
            revision: self.revision,
            typst_code: self.last_typst_code.clone(),
            pixel_per_pt,
        });
    }

    fn poll_worker_responses(&mut self, ctx: &egui::Context) {
        while let Ok(resp) = self.worker_rx.try_recv() {
            match resp {
                WorkerResponse::PreviewReady { revision, result } => {
                    if revision < self.revision {
                        continue;
                    }
                    self.is_compiling = false;
                    match result {
                        Ok(output) => {
                            self.last_compile_ms = output.compile_ms;
                            self.last_error = None;
                            self.page_textures.clear();

                            let scale_divisor = (2.0 * self.zoom).clamp(1.2, 3.5);
                            for (idx, page) in output.pages.into_iter().enumerate() {
                                let size = [page.width as usize, page.height as usize];
                                let color_image = ColorImage::from_rgba_premultiplied(
                                    size,
                                    &page.premultiplied_rgba,
                                );
                                let tex = ctx.load_texture(
                                    format!("preview_page_{idx}"),
                                    color_image,
                                    TextureOptions::LINEAR,
                                );
                                let logical_size = Vec2::new(
                                    page.width as f32 / scale_divisor,
                                    page.height as f32 / scale_divisor,
                                );
                                self.page_textures.push((tex, logical_size));
                            }
                        }
                        Err(err) => {
                            self.last_error = Some(err);
                        }
                    }
                }
            }
        }
    }

    fn request_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        if self.is_dirty {
            self.pending_action = Some(action);
        } else {
            self.execute_pending_action(action, ctx);
        }
    }

    fn execute_pending_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        match action {
            PendingAction::NewFile => {
                self.undo_stack.clear();
                self.redo_stack.clear();
                self.markdown_text = "---\npaper: A4\norientation: portrait\nmargin: 20mm\nfont-size: 10.5pt\nline-height: 1.5\nfont: gothic\n---\n\n# 新規報告書タイトル\n\nここに本文を入力してください。\n".to_string();
                self.current_file = None;
                self.is_dirty = false;
                self.revision += 1;
                self.trigger_preview_update();
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
        }
    }

    fn load_file_from_path(&mut self, path: PathBuf) {
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
            }
            Err(e) => {
                self.status_message = format!("ファイル読み込みエラー: {e}");
            }
        }
    }

    fn action_save_file(&mut self, save_as: bool) -> bool {
        let target_path = if save_as || self.current_file.is_none() {
            rfd::FileDialog::new()
                .add_filter("Markdown文書 (*.md)", &["md"])
                .set_file_name("report.md")
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
                    return true;
                }
                Err(e) => {
                    self.status_message = format!("保存エラー: {e}");
                }
            }
        }
        false
    }

    fn action_export_pdf(&mut self) {
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
            let doc = parse_document(&self.markdown_text);
            let base = self.base_dir();
            let typst_code = generate_typst_source_with_base(&doc, base.as_deref());
            let mut engine = TypstEngine::new();
            match engine.export_pdf(&typst_code) {
                Ok(pdf_bytes) => match std::fs::write(&path, pdf_bytes) {
                    Ok(()) => {
                        let ms = start.elapsed().as_secs_f64() * 1000.0;
                        self.status_message =
                            format!("PDFを出力しました ({ms:.1} ms): {}", path.display());
                    }
                    Err(e) => {
                        self.status_message = format!("PDF書き込みエラー: {e}");
                    }
                },
                Err(e) => {
                    self.status_message = format!("PDF組版エラー: {e}");
                }
            }
        }
    }

    fn action_export_html(&mut self) {
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
            let doc = parse_document(&self.markdown_text);
            let base = self.base_dir();
            let html = generate_html_with_base(&doc, base.as_deref());
            match std::fs::write(&path, html) {
                Ok(()) => {
                    self.status_message = format!("HTMLを出力しました: {}", path.display());
                }
                Err(e) => {
                    self.status_message = format!("HTML書き込みエラー: {e}");
                }
            }
        }
    }

    fn action_insert_image_dialog(&mut self) {
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

    fn insert_image_path(&mut self, img_path: &Path) {
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
    fn insert_at_cursor(&mut self, snippet: &str) {
        self.push_undo_snapshot();

        let total_chars = self.markdown_text.chars().count();
        let (start_char, end_char) = self
            .cursor_char_range
            .unwrap_or((total_chars, total_chars));
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

    /// 選択中のテキストがあれば `prefix` と `suffix` で囲み、なければ `prefix + default_inner + suffix` を挿入する
    fn wrap_selection_or_insert(&mut self, prefix: &str, default_inner: &str, suffix: &str) {
        self.push_undo_snapshot();

        let total_chars = self.markdown_text.chars().count();
        let (start_char, end_char) = self
            .cursor_char_range
            .unwrap_or((total_chars, total_chars));
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
    fn find_matches(&self) -> Vec<(usize, usize)> {
        if self.find_query.is_empty() {
            return Vec::new();
        }
        let mut results = Vec::new();
        let q_chars = self.find_query.chars().count();
        for (byte_idx, _) in self.markdown_text.match_indices(&self.find_query) {
            let start_char = byte_to_char_idx(&self.markdown_text, byte_idx);
            results.push((start_char, start_char + q_chars));
        }
        results
    }

    fn select_find_match(&mut self, match_idx: usize) {
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

    fn replace_current_match(&mut self) {
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

    fn replace_all_matches(&mut self) {
        if self.find_query.is_empty() || !self.markdown_text.contains(&self.find_query) {
            return;
        }
        let count = self.markdown_text.matches(&self.find_query).count();
        self.push_undo_snapshot();
        self.markdown_text = self
            .markdown_text
            .replace(&self.find_query, &self.replace_text);
        self.is_dirty = true;
        self.revision += 1;
        self.pending_render = true;
        self.last_edit_time = Instant::now();
        self.status_message = format!("{count} 件をすべて置換しました");
    }

    fn update_window_title(&mut self, ctx: &egui::Context) {
        let name = self
            .current_file
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("未保存の文書");
        let dirty = if self.is_dirty { " *" } else { "" };
        let title = format!("Typer — {name}{dirty}");
        if title != self.last_window_title {
            self.last_window_title = title.clone();
            ctx.send_viewport_cmd(ViewportCommand::Title(title));
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
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
                self.request_action(PendingAction::LoadFilePath(path), ctx);
                break;
            } else if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp") {
                self.insert_image_path(&path);
            }
        }
    }
}

impl eframe::App for TyperApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.startup_visibility_ensured {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            self.startup_visibility_ensured = true;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_worker_responses(&ctx);
        self.update_window_title(&ctx);
        self.handle_dropped_files(&ctx);

        // ウィンドウの「×」ボタンによる終了要求のインターセプト（未保存保護）
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.is_dirty && !self.allow_immediate_close {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.pending_action = Some(PendingAction::CloseWindow);
            }
        }

        // グローバルショートカットキー処理
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::S)) {
            self.action_save_file(true);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::S)) {
            self.action_save_file(false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::O)) {
            self.request_action(PendingAction::OpenFileDialog, &ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::N)) {
            self.request_action(PendingAction::NewFile, &ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::P)) {
            self.action_export_pdf();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::F))
            || ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::H))
        {
            self.show_find_replace = !self.show_find_replace;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::B)) {
            self.wrap_selection_or_insert("**", "太字テキスト", "**");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::I)) {
            self.wrap_selection_or_insert("*", "斜体テキスト", "*");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Y)) {
            self.perform_redo();
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Z)) {
            self.perform_undo();
        }

        // Ctrl + マウスホイールによるプレビュー拡大・縮小
        let zoom_delta = ctx.input(|i| i.zoom_delta());
        if (zoom_delta - 1.0).abs() > 0.001 {
            self.zoom = (self.zoom * zoom_delta).clamp(0.5, 2.5);
            self.revision += 1;
            self.pending_render = true;
            self.last_edit_time = Instant::now();
        }

        // デバウンスされたプレビュー更新チェック（入力停止から120ms後に組版開始）
        if self.pending_render {
            let elapsed = self.last_edit_time.elapsed();
            if elapsed >= Duration::from_millis(120) {
                self.trigger_preview_update();
            } else {
                ctx.request_repaint_after(Duration::from_millis(120) - elapsed);
            }
        }

        // 未保存確認モーダルダイアログ
        if let Some(action) = self.pending_action.clone() {
            egui::Window::new("⚠ 未保存の変更があります")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
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
                                self.execute_pending_action(action.clone(), &ctx);
                            }
                        }
                        if ui.button("保存せずに続行").clicked() {
                            self.pending_action = None;
                            self.execute_pending_action(action.clone(), &ctx);
                        }
                        if ui.button("キャンセル").clicked() {
                            self.pending_action = None;
                        }
                    });
                });
        }

        // 上部メインツールバー
        egui::Panel::top("top_toolbar")
            .exact_size(42.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;

                    if ui
                        .button("🗋 新規")
                        .on_hover_text("新規文書を作成します (Ctrl+N)")
                        .clicked()
                    {
                        self.request_action(PendingAction::NewFile, &ctx);
                    }
                    if ui
                        .button("📂 開く...")
                        .on_hover_text("Markdownファイルを読み込みます (Ctrl+O / ドロップ対応)")
                        .clicked()
                    {
                        self.request_action(PendingAction::OpenFileDialog, &ctx);
                    }
                    if ui
                        .button("💾 保存")
                        .on_hover_text("ファイルを上書き保存します (Ctrl+S)")
                        .clicked()
                    {
                        self.action_save_file(false);
                    }
                    if ui
                        .button("別名保存...")
                        .on_hover_text("名前を付けて保存します (Ctrl+Shift+S)")
                        .clicked()
                    {
                        self.action_save_file(true);
                    }

                    ui.separator();

                    if ui
                        .add_enabled(!self.undo_stack.is_empty(), egui::Button::new("↶"))
                        .on_hover_text("元に戻す (Ctrl+Z)")
                        .clicked()
                    {
                        self.perform_undo();
                    }
                    if ui
                        .add_enabled(!self.redo_stack.is_empty(), egui::Button::new("↷"))
                        .on_hover_text("やり直し (Ctrl+Y)")
                        .clicked()
                    {
                        self.perform_redo();
                    }
                    if ui
                        .selectable_label(self.show_find_replace, "🔍 検索・置換")
                        .on_hover_text("検索・置換バーを開閉します (Ctrl+F)")
                        .clicked()
                    {
                        self.show_find_replace = !self.show_find_replace;
                    }
                    if ui
                        .selectable_label(self.show_page_settings_bar, "⚙ ページ設定")
                        .on_hover_text("用紙向き・フォント・余白・文字サイズ・行間の設定バーを開閉します")
                        .clicked()
                    {
                        self.show_page_settings_bar = !self.show_page_settings_bar;
                    }

                    ui.separator();

                    let pdf_btn = egui::Button::new(
                        RichText::new("📄 PDF出力...").strong().color(Color32::WHITE),
                    )
                    .fill(Color32::from_rgb(26, 115, 232));
                    if ui
                        .add(pdf_btn)
                        .on_hover_text("現在の文書をPDFとして出力します (Ctrl+P)")
                        .clicked()
                    {
                        self.action_export_pdf();
                    }

                    let html_btn = egui::Button::new(
                        RichText::new("🌐 HTML出力...").color(Color32::from_rgb(23, 78, 166)),
                    )
                    .fill(Color32::from_rgb(232, 240, 254));
                    if ui
                        .add(html_btn)
                        .on_hover_text("ページ境界に依存しない単一HTMLファイルとして出力します")
                        .clicked()
                    {
                        self.action_export_html();
                    }

                    ui.separator();
                    ui.checkbox(&mut self.show_typst_source, "Typstコード");

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("100%").on_hover_text("表示倍率を100%に戻す").clicked() {
                            self.zoom = 1.0;
                            self.revision += 1;
                            self.trigger_preview_update();
                        }
                        if ui
                            .button("＋")
                            .on_hover_text("プレビュー拡大 (Ctrl+ホイール)")
                            .clicked()
                        {
                            self.zoom = (self.zoom + 0.15).clamp(0.5, 2.5);
                            self.revision += 1;
                            self.trigger_preview_update();
                        }
                        ui.label(format!("{:.0}%", self.zoom * 100.0));
                        if ui
                            .button("－")
                            .on_hover_text("プレビュー縮小 (Ctrl+ホイール)")
                            .clicked()
                        {
                            self.zoom = (self.zoom - 0.15).clamp(0.5, 2.5);
                            self.revision += 1;
                            self.trigger_preview_update();
                        }
                    });
                });
            });

        // ページ・基本書式設定GUIバー（フロントマターと双方向同期）
        if self.show_page_settings_bar {
            egui::Panel::top("page_settings_bar")
                .exact_size(34.0)
                .show(ui, |ui| {
                    let mut doc_settings = parse_document(&self.markdown_text).settings;
                    let mut changed = false;

                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(RichText::new("⚙ 文書設定:").small().strong());

                        // 用紙向き
                        let is_portrait = doc_settings.orientation == Orientation::Portrait;
                        if ui.selectable_label(is_portrait, "A4 縦").clicked() && !is_portrait {
                            doc_settings.orientation = Orientation::Portrait;
                            changed = true;
                        }
                        if ui.selectable_label(!is_portrait, "A4 横").clicked() && is_portrait {
                            doc_settings.orientation = Orientation::Landscape;
                            changed = true;
                        }

                        ui.separator();

                        // フォント選択
                        let is_gothic = doc_settings.font_choice == FontChoice::Gothic;
                        if ui.selectable_label(is_gothic, "ゴシック体").clicked() && !is_gothic {
                            doc_settings.font_choice = FontChoice::Gothic;
                            changed = true;
                        }
                        if ui.selectable_label(!is_gothic, "明朝体").clicked() && is_gothic {
                            doc_settings.font_choice = FontChoice::Mincho;
                            changed = true;
                        }

                        ui.separator();

                        // 余白選択
                        ui.label(RichText::new("余白:").small());
                        for (label, val) in [("狭 15mm", "15mm"), ("標準 20mm", "20mm"), ("広 25mm", "25mm")] {
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
                    });

                    if changed {
                        self.push_undo_snapshot();
                        self.markdown_text = update_document_settings_in_markdown(
                            &self.markdown_text,
                            &doc_settings,
                        );
                        self.is_dirty = true;
                        self.revision += 1;
                        self.pending_render = true;
                        self.last_edit_time = Instant::now();
                    }
                });
        }

        // 下部ステータスバー
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
                });
            });

        // 右ペイン：ページプレビュー領域
        egui::Panel::right("preview_panel")
            .resizable(true)
            .default_size(580.0)
            .size_range(320.0..=1200.0)
            .show(ui, |ui| {
                if let Some(err) = &self.last_error {
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(253, 232, 230))
                        .inner_margin(8.0)
                        .corner_radius(4.0)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("⚠ 構文・組版メッセージ: {err}"))
                                    .small()
                                    .color(Color32::from_rgb(197, 34, 31)),
                            );
                        });
                }

                if self.show_typst_source {
                    egui::Panel::bottom("typst_source_inspector")
                        .resizable(true)
                        .default_size(180.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("生成されたTypst組版コード (自動変換)").strong());
                            egui::ScrollArea::vertical()
                                .id_salt("typst_code_scroll")
                                .show(ui, |ui| {
                                    let mut code_view = self.last_typst_code.as_str();
                                    ui.add(
                                        TextEdit::multiline(&mut code_view)
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(f32::INFINITY),
                                    );
                                });
                        });
                }

                egui::Frame::NONE
                    .fill(Color32::from_rgb(228, 231, 235))
                    .inner_margin(16.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::both()
                            .id_salt("page_preview_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    ui.spacing_mut().item_spacing.y = 20.0;

                                    for (idx, (tex, base_size)) in
                                        self.page_textures.iter().enumerate()
                                    {
                                        let display_size = *base_size * self.zoom * 0.85;

                                        let (rect, _) =
                                            ui.allocate_exact_size(display_size, Sense::hover());
                                        let shadow_rect = rect.translate(Vec2::new(4.0, 5.0));
                                        ui.painter().rect_filled(
                                            shadow_rect,
                                            2.0,
                                            Color32::from_black_alpha(38),
                                        );
                                        ui.painter().image(
                                            tex.id(),
                                            rect,
                                            egui::Rect::from_min_max(
                                                egui::pos2(0.0, 0.0),
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            Color32::WHITE,
                                        );
                                        ui.painter().rect_stroke(
                                            rect,
                                            0.0,
                                            Stroke::new(1.0, Color32::from_rgb(189, 193, 198)),
                                            egui::StrokeKind::Outside,
                                        );

                                        ui.label(
                                            RichText::new(format!(
                                                "- {} / {} -",
                                                idx + 1,
                                                self.page_textures.len()
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                    }
                                });
                            });
                    });
            });

        // 中央（左ペイン）：マークアップエディタ＆クイック挿入バー＆検索置換バー
        egui::CentralPanel::default().show(ui, |ui| {
            // 書式・要素クイック挿入バー（カーソル位置への挿入・選択テキストのラップに対応）
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(RichText::new("書式挿入:").small().weak());

                if ui
                    .small_button("B 太字")
                    .on_hover_text("選択範囲を太字で囲みます (Ctrl+B)")
                    .clicked()
                {
                    self.wrap_selection_or_insert("**", "太字", "**");
                }
                if ui
                    .small_button("I 斜体")
                    .on_hover_text("選択範囲を斜体で囲みます (Ctrl+I)")
                    .clicked()
                {
                    self.wrap_selection_or_insert("*", "斜体", "*");
                }
                if ui
                    .small_button("🔴 赤文字")
                    .on_hover_text("選択範囲（または新規テキスト）を赤文字にします")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "重要事項", "]{red}");
                }
                if ui
                    .small_button("🔵 青文字")
                    .on_hover_text("選択範囲（または新規テキスト）を青文字にします")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "補足事項", "]{blue}");
                }
                if ui
                    .small_button("🟢 緑文字")
                    .on_hover_text("選択範囲（または新規テキスト）を緑文字にします")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "正常", "]{green}");
                }

                ui.separator();

                if ui
                    .small_button("📊 列幅付き表")
                    .on_hover_text("カーソル位置に列幅指定付きの表を挿入します")
                    .clicked()
                {
                    self.insert_at_cursor(
                        "\n{widths: 25%, 50%, 25%}\n| 項目 | 内容 | 備考 |\n| :--- | :--- | :--- |\n| サンプル | 内容を入力 | 備考 |\n",
                    );
                }
                if ui
                    .small_button("➡ 右寄せ段落")
                    .on_hover_text("カーソル位置に日付・署名向けの右寄せ属性を挿入します")
                    .clicked()
                {
                    self.insert_at_cursor("\n{align: right}\n2026年10月16日\n");
                }
                if ui
                    .small_button("↔ 中央タイトル")
                    .on_hover_text("カーソル位置に中央寄せ＋文字サイズ指定を挿入します")
                    .clicked()
                {
                    self.insert_at_cursor("\n{align: center, size: 15pt}\n**文書タイトル**\n");
                }
                if ui
                    .small_button("↕ 行間指定")
                    .on_hover_text("カーソル位置に段落の行間指定を挿入します")
                    .clicked()
                {
                    self.insert_at_cursor("\n{line-height: 1.2}\n");
                }
                if ui
                    .small_button("🖼 画像...")
                    .on_hover_text("ローカル画像ファイルを選択してカーソル位置に挿入します（ドラッグ＆ドロップも可）")
                    .clicked()
                {
                    self.action_insert_image_dialog();
                }
                if ui
                    .small_button("📄 改ページ")
                    .on_hover_text("カーソル位置に手動改ページを挿入します")
                    .clicked()
                {
                    self.insert_at_cursor("\n:::pagebreak\n");
                }

                ui.separator();

                ui.menu_button("📋 テンプレート", |ui| {
                    for (name, body) in TEMPLATES {
                        if ui.button(*name).clicked() {
                            self.insert_at_cursor(body);
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("定型の業務文書（議事録・障害報告書など）をカーソル位置に挿入します");

                if ui
                    .small_button("🧩 Typstコード")
                    .on_hover_text(
                        "上級者向け：独自構文では表現できない組版をTypstコードで直接記述できます（HTML出力では評価されません）",
                    )
                    .clicked()
                {
                    self.insert_at_cursor("\n```typst\n#line(length: 50%, stroke: 1pt)\n```\n");
                }
            });

            // 検索・置換バー
            if self.show_find_replace {
                ui.separator();
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
                                .add_enabled(
                                    match_count > 0,
                                    egui::Button::new("すべて置換").small(),
                                )
                                .clicked()
                            {
                                self.replace_all_matches();
                            }

                            if ui.small_button("✖").on_hover_text("検索バーを閉じる").clicked() {
                                self.show_find_replace = false;
                            }
                        });
                    });
            }

            ui.separator();

            // カスタムシンタックスハイライト付きエディタ
            let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                let mut layout_job = highlight_custom_markdown(text.as_str());
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
                    }

                    // 現在のカーソル・選択位置を記録
                    if let Some(range) = output.cursor_range {
                        let sorted = range.as_sorted_char_range();
                        self.cursor_char_range = Some((sorted.start.into(), sorted.end.into()));
                    }

                    if output.response.changed() {
                        if self.undo_stack.last().map(|s| s.as_str()) != Some(prev_snapshot.as_str())
                        {
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
        });
    }
}

/// 文字インデックス (`chars()` の順番) をUTF-8バイトオフセットに変換する
fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte_i, _)| byte_i)
        .unwrap_or(s.len())
}

/// UTF-8バイトオフセットを文字インデックスに変換する
fn byte_to_char_idx(s: &str, byte_idx: usize) -> usize {
    let clamped = byte_idx.min(s.len());
    s[..clamped].chars().count()
}

/// 文字インデックスから 1始まりの `(行番号, 列番号)` を計算する
fn char_idx_to_line_col(s: &str, char_idx: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in s.chars().enumerate() {
        if i >= char_idx {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// 標準Markdownおよび独自拡張構文（`{widths: ...}`, `[...]{red}`, `:::pagebreak` 等）の
/// シンタックスハイライトを行う
fn highlight_custom_markdown(text: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font_id = egui::FontId::new(14.5, FontFamily::Monospace);

    let default_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(32, 33, 36),
        ..Default::default()
    };
    let heading_fmt = TextFormat {
        font_id: egui::FontId::new(15.0, FontFamily::Monospace),
        color: Color32::from_rgb(23, 78, 166),
        ..Default::default()
    };
    let attr_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(147, 52, 230),
        background: Color32::from_rgb(248, 242, 254),
        ..Default::default()
    };
    let pagebreak_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(227, 116, 0),
        background: Color32::from_rgb(254, 247, 224),
        ..Default::default()
    };
    let frontmatter_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(13, 101, 45),
        ..Default::default()
    };
    let table_pipe_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(26, 115, 232),
        ..Default::default()
    };

    let mut in_frontmatter = false;
    for (idx, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        if idx == 0 && trimmed == "---" {
            in_frontmatter = true;
            job.append(line, 0.0, frontmatter_fmt.clone());
            continue;
        }
        if in_frontmatter {
            job.append(line, 0.0, frontmatter_fmt.clone());
            if trimmed == "---" {
                in_frontmatter = false;
            }
            continue;
        }

        if trimmed.starts_with('#') {
            job.append(line, 0.0, heading_fmt.clone());
        } else if trimmed.eq_ignore_ascii_case(":::pagebreak")
            || trimmed.eq_ignore_ascii_case("{pagebreak}")
        {
            job.append(line, 0.0, pagebreak_fmt.clone());
        } else if trimmed.starts_with('{') && trimmed.ends_with('}') && !trimmed.contains('|') {
            job.append(line, 0.0, attr_fmt.clone());
        } else if trimmed.starts_with('|') {
            append_line_with_inline_highlights(
                &mut job,
                line,
                &default_fmt,
                &attr_fmt,
                Some(&table_pipe_fmt),
            );
        } else {
            append_line_with_inline_highlights(&mut job, line, &default_fmt, &attr_fmt, None);
        }
    }

    if text.is_empty() {
        job.append("", 0.0, default_fmt);
    }

    job
}

fn append_line_with_inline_highlights(
    job: &mut LayoutJob,
    line: &str,
    default_fmt: &TextFormat,
    attr_fmt: &TextFormat,
    pipe_fmt: Option<&TextFormat>,
) {
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(pf) = pipe_fmt {
            if rest.starts_with('|') {
                job.append("|", 0.0, pf.clone());
                rest = &rest[1..];
                continue;
            }
        }
        if rest.starts_with('[') {
            if let Some(close_bracket) = rest.find("]{") {
                let after = close_bracket + 2;
                if let Some(close_brace) = rest[after..].find('}') {
                    let total = after + close_brace + 1;
                    job.append(&rest[..total], 0.0, attr_fmt.clone());
                    rest = &rest[total..];
                    continue;
                }
            }
        }
        if rest.starts_with('{') {
            if let Some(close_brace) = rest.find('}') {
                let inner = &rest[1..close_brace];
                if inner.contains(':') {
                    let total = close_brace + 1;
                    job.append(&rest[..total], 0.0, attr_fmt.clone());
                    rest = &rest[total..];
                    continue;
                }
            }
        }

        let first_char_len = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        let next_pos = if pipe_fmt.is_some() {
            rest[first_char_len..]
                .find(['|', '[', '{'])
                .map(|p| p + first_char_len)
                .unwrap_or(rest.len())
        } else {
            rest[first_char_len..]
                .find(['[', '{'])
                .map(|p| p + first_char_len)
                .unwrap_or(rest.len())
        };

        job.append(&rest[..next_pos], 0.0, default_fmt.clone());
        rest = &rest[next_pos..];
    }
}

/// Windowsの日本語システムフォントを `egui` に登録し、UIおよびエディタでの日本語表示・IME入力を最適化する
fn setup_japanese_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let candidates = [
        r"C:\Windows\Fonts\BIZ-UDGothicR.ttc",
        r"C:\Windows\Fonts\YuGothM.ttc",
        r"C:\Windows\Fonts\meiryo.ttc",
        r"C:\Windows\Fonts\msgothic.ttc",
    ];

    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "jp_system_font".to_owned(),
                Arc::new(FontData::from_owned(bytes)),
            );
            if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
                prop.insert(0, "jp_system_font".to_owned());
            }
            if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
                mono.push("jp_system_font".to_owned());
            }
            break;
        }
    }

    ctx.set_fonts(fonts);
}

fn setup_visual_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = Color32::from_rgb(248, 249, 250);
    visuals.extreme_bg_color = Color32::WHITE;
    ctx.set_visuals(visuals);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::SAMPLE_DOCUMENT;

    #[test]
    fn test_highlight_japanese_sample_document() {
        let job = highlight_custom_markdown(SAMPLE_DOCUMENT);
        assert_eq!(job.text, SAMPLE_DOCUMENT);
        assert!(!job.sections.is_empty());
    }

    #[test]
    fn test_char_byte_conversion_and_line_col() {
        let s = "あいう\nえお";
        assert_eq!(char_to_byte_idx(s, 0), 0);
        assert_eq!(char_to_byte_idx(s, 1), 3);
        assert_eq!(byte_to_char_idx(s, 3), 1);
        assert_eq!(char_idx_to_line_col(s, 0), (1, 1));
        assert_eq!(char_idx_to_line_col(s, 2), (1, 3));
        assert_eq!(char_idx_to_line_col(s, 4), (2, 1));
    }

    /// `TyperApp::new` はウィンドウ・GPUコンテキストを要するため、
    /// テキスト編集系ロジックのみを検証するための軽量インスタンスを直接構築する
    fn test_app(markdown: &str) -> TyperApp {
        let (worker_tx, _worker_req_rx) = mpsc::channel::<WorkerRequest>();
        let (_worker_res_tx, worker_rx) = mpsc::channel::<WorkerResponse>();
        TyperApp {
            markdown_text: markdown.to_string(),
            last_typst_code: String::new(),
            current_file: None,
            is_dirty: false,
            last_window_title: String::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            cursor_char_range: None,
            pending_cursor_selection: None,
            show_find_replace: false,
            find_query: String::new(),
            replace_text: String::new(),
            current_match_index: 0,
            show_page_settings_bar: true,
            pending_action: None,
            allow_immediate_close: false,
            revision: 1,
            pending_render: false,
            last_edit_time: Instant::now(),
            worker_tx,
            worker_rx,
            is_compiling: false,
            page_textures: Vec::new(),
            last_compile_ms: 0.0,
            last_error: None,
            zoom: 1.0,
            show_typst_source: false,
            status_message: String::new(),
            startup_visibility_ensured: false,
        }
    }

    #[test]
    fn test_undo_redo_roundtrip() {
        let mut app = test_app("hello");
        app.push_undo_snapshot();
        app.markdown_text = "hello world".to_string();

        app.perform_undo();
        assert_eq!(app.markdown_text, "hello");
        assert!(app.is_dirty);

        app.perform_redo();
        assert_eq!(app.markdown_text, "hello world");

        // undo/redoスタックが空の場合は何もしない
        app.undo_stack.clear();
        app.redo_stack.clear();
        app.perform_undo();
        assert_eq!(app.markdown_text, "hello world");
        app.perform_redo();
        assert_eq!(app.markdown_text, "hello world");
    }

    #[test]
    fn test_wrap_selection_with_selection() {
        let mut app = test_app("あいうえお");
        // "いう" (char index 1..3) を選択した状態で太字化
        app.cursor_char_range = Some((1, 3));
        app.wrap_selection_or_insert("**", "太字", "**");
        assert_eq!(app.markdown_text, "あ**いう**えお");
        assert_eq!(app.cursor_char_range, Some((3, 5)));
    }

    #[test]
    fn test_wrap_selection_without_selection_inserts_default() {
        let mut app = test_app("あ");
        app.cursor_char_range = Some((1, 1));
        app.wrap_selection_or_insert("**", "太字", "**");
        assert_eq!(app.markdown_text, "あ**太字**");
        assert_eq!(app.cursor_char_range, Some((3, 5)));
    }

    #[test]
    fn test_find_matches_with_multibyte_text() {
        let mut app = test_app("あいうえおあいう");
        app.find_query = "あい".to_string();
        let matches = app.find_matches();
        assert_eq!(matches, vec![(0, 2), (5, 7)]);
    }

    #[test]
    fn test_find_matches_empty_query_returns_none() {
        let app = test_app("あいうえお");
        assert!(app.find_matches().is_empty());
    }

    #[test]
    fn test_replace_current_match() {
        let mut app = test_app("あいうえおあいう");
        app.find_query = "あい".to_string();
        app.replace_text = "X".to_string();
        app.current_match_index = 0;
        app.replace_current_match();
        assert_eq!(app.markdown_text, "Xうえおあいう");
        assert_eq!(app.cursor_char_range, Some((0, 1)));
    }

    #[test]
    fn test_replace_all_matches() {
        let mut app = test_app("あいうえおあいう");
        app.find_query = "あい".to_string();
        app.replace_text = "X".to_string();
        app.replace_all_matches();
        assert_eq!(app.markdown_text, "XうえおXう");
    }

    #[test]
    fn test_replace_all_matches_no_match_leaves_text_unchanged() {
        let mut app = test_app("あいうえお");
        app.find_query = "存在しない".to_string();
        app.replace_text = "X".to_string();
        app.replace_all_matches();
        assert_eq!(app.markdown_text, "あいうえお");
        assert!(app.undo_stack.is_empty());
    }

    #[test]
    #[ignore]
    fn bench_editor_frame_cost_at_scale() {
        use crate::sample::SAMPLE_DOCUMENT;
        use std::time::Instant;

        for repeat in [1usize, 10, 50, 200] {
            let text = SAMPLE_DOCUMENT.repeat(repeat);
            let char_len = text.chars().count();

            // 1フレームあたり実際に走る「シンタックスハイライトの再計算」コスト
            let start = Instant::now();
            for _ in 0..30 {
                let _ = highlight_custom_markdown(&text);
            }
            let highlight_30frames = start.elapsed();

            // 1フレームあたり実際に走る「undo判定用の全文クローン」コスト
            let start = Instant::now();
            for _ in 0..30 {
                let _ = text.clone();
            }
            let clone_30frames = start.elapsed();

            let mut app = test_app(&text);
            app.find_query = "情報システム部".to_string();
            let start = Instant::now();
            let matches = app.find_matches();
            let find_elapsed = start.elapsed();

            println!(
                "repeat={repeat:>4} chars={char_len:>9} | highlight(30f)={highlight_30frames:>10?} ({:>8?}/frame) | clone(30f)={clone_30frames:>10?} ({:>8?}/frame) | find_matches={find_elapsed:>10?} (hits={})",
                highlight_30frames / 30,
                clone_30frames / 30,
                matches.len()
            );
        }
    }
}
