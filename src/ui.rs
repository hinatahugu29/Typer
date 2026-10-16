//! デスクトップUI層 (`egui` / `eframe`)
//!
//! - 左ペイン：シンタックスハイライト付きエディタ、カーソル位置への書式挿入・選択範囲ラップ、
//!   検索・置換バー (`Ctrl+F` / `Ctrl+H`)、Undo/Redo履歴、ページ設定GUIバー
//! - 右ペイン：Typst組版結果のリアルタイムページプレビュー（`Ctrl+ホイール` ズーム・複数ページ対応）
//! - 安全性・操作性：未保存変更時の終了・読み込み確認モーダル、ドラッグ＆ドロップ読み込み（文書・画像）

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, Key, Modifiers, RichText, Sense, Stroke, TextEdit, TextureHandle,
    TextureOptions, Vec2, ViewportCommand,
    text::{CCursor, CCursorRange},
};

use crate::docx_export::generate_docx_bytes;
use crate::html_export::generate_html_with_base;
use crate::model::{FontChoice, Orientation};
use crate::parser::{
    extract_headings_outline, parse_document, update_document_settings_in_markdown,
};
use crate::templates::TEMPLATES;
use crate::typst_engine::{PreviewOutput, TypstEngine};
use crate::typst_gen::generate_typst_source_with_base;
#[cfg(test)]
use crate::ui_persistence::recent_files_store_path;
use crate::ui_persistence::{
    autosave_path_for, load_dark_mode_setting, load_recent_files, resolve_startup_document,
    save_dark_mode_setting, save_recent_files, setup_japanese_fonts, setup_visual_theme,
};
use crate::ui_text::{
    byte_to_char_idx, char_idx_to_line_col, char_to_byte_idx, highlight_custom_markdown,
    is_valid_hex_color,
};

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
    CloseTab,
}

/// 非アクティブなタブの状態を丸ごと保持するスナップショット。
/// アクティブなタブのスロットは常に `None`（実体は `TyperApp` 本体のフィールドにある）。
#[derive(Clone)]
struct TabSnapshot {
    markdown_text: String,
    current_file: Option<PathBuf>,
    is_dirty: bool,
    undo_stack: Vec<String>,
    redo_stack: Vec<String>,
    cursor_char_range: Option<(usize, usize)>,
    last_export_path: Option<PathBuf>,
}

const BLANK_DOCUMENT_TEMPLATE: &str = "---\npaper: A4\norientation: portrait\nmargin: 20mm\nfont-size: 10.5pt\nline-height: 1.5\nfont: gothic\n---\n\n# 新規報告書タイトル\n\nここに本文を入力してください。\n";

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
    find_ignore_case: bool,
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

    /// 最近使用したファイルの一覧（新しい順、永続化される）
    recent_files: Vec<PathBuf>,
    /// 自動保存の最終実行時刻
    last_autosave_time: Instant,
    /// 見出しアウトラインパネルの表示フラグ
    show_outline_panel: bool,
    /// カスタム文字色入力欄（HEXコード）
    custom_color_hex: String,
    /// 直近にエクスポートしたファイルのパス（「開く」ボタン用）
    last_export_path: Option<PathBuf>,
    /// ダークモード表示が有効か（永続化される）
    dark_mode: bool,
    /// 設定・履歴をディスクへ永続化するか（テスト時はユーザーの実設定を汚さないよう無効化する）
    persist_settings: bool,

    /// 開いている全タブ。`tabs[active_tab]` は常に `None`（実体は本構造体の他フィールド）で、
    /// それ以外は非アクティブなタブの状態一式を保持する
    tabs: Vec<Option<TabSnapshot>>,
    /// 現在アクティブなタブの `tabs` 内インデックス
    active_tab: usize,
}

impl TyperApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        if let Some(window) = cc.winit_window() {
            window.set_visible(true);
            window.focus_window();
        }
        cc.egui_ctx
            .send_viewport_cmd(ViewportCommand::Visible(true));
        cc.egui_ctx
            .send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);

        let dark_mode = load_dark_mode_setting();
        setup_japanese_fonts(&cc.egui_ctx);
        setup_visual_theme(&cc.egui_ctx, dark_mode);

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

        let recent_files = load_recent_files();
        let (initial_text, initial_file, initial_status) = resolve_startup_document(&recent_files);
        let doc = parse_document(&initial_text);
        let typst_code = generate_typst_source_with_base(&doc, None);

        let mut app = Self {
            markdown_text: initial_text,
            last_typst_code: typst_code,
            current_file: initial_file,
            is_dirty: false,
            last_window_title: String::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            cursor_char_range: None,
            pending_cursor_selection: None,
            show_find_replace: false,
            find_query: String::new(),
            find_ignore_case: false,
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
            status_message: initial_status,
            startup_visibility_ensured: false,
            recent_files,
            last_autosave_time: Instant::now(),
            show_outline_panel: false,
            custom_color_hex: String::new(),
            last_export_path: None,
            dark_mode,
            persist_settings: true,
            tabs: vec![None],
            active_tab: 0,
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

    /// アクティブなタブに限らず、バックグラウンドの他タブも含めて未保存の変更があるか
    fn any_unsaved_changes(&self) -> bool {
        self.is_dirty || self.tabs.iter().flatten().any(|snapshot| snapshot.is_dirty)
    }

    fn execute_pending_action(&mut self, action: PendingAction, ctx: &egui::Context) {
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
    fn reset_to_blank_document(&mut self) {
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

    /// 現在アクティブな文書の状態一式をスナップショットとして取り出す（タブ切替・新規タブ用）
    fn snapshot_active_tab(&self) -> TabSnapshot {
        TabSnapshot {
            markdown_text: self.markdown_text.clone(),
            current_file: self.current_file.clone(),
            is_dirty: self.is_dirty,
            undo_stack: self.undo_stack.clone(),
            redo_stack: self.redo_stack.clone(),
            cursor_char_range: self.cursor_char_range,
            last_export_path: self.last_export_path.clone(),
        }
    }

    /// スナップショットの内容を、現在アクティブな文書のフィールドへ復元する
    fn load_tab_snapshot(&mut self, snapshot: TabSnapshot) {
        self.undo_stack = snapshot.undo_stack;
        self.redo_stack = snapshot.redo_stack;
        self.markdown_text = snapshot.markdown_text;
        self.current_file = snapshot.current_file;
        self.is_dirty = snapshot.is_dirty;
        self.cursor_char_range = snapshot.cursor_char_range;
        self.pending_cursor_selection = snapshot.cursor_char_range;
        self.last_export_path = snapshot.last_export_path;
        self.last_typst_code.clear();
        self.page_textures.clear();
        self.last_error = None;
        self.last_compile_ms = 0.0;
        self.last_autosave_time = Instant::now();
        self.revision += 1;
        self.trigger_preview_update();
    }

    /// 指定インデックスのタブへ切り替える（現在のタブはバックグラウンドへ退避される）
    fn switch_to_tab(&mut self, index: usize) {
        if index == self.active_tab || index >= self.tabs.len() {
            return;
        }
        let current = self.snapshot_active_tab();
        self.tabs[self.active_tab] = Some(current);
        let target = self.tabs[index]
            .take()
            .expect("非アクティブなタブのスロットは必ずSomeのはず");
        self.load_tab_snapshot(target);
        self.active_tab = index;
    }

    /// 現在のタブをバックグラウンドへ退避し、空の新規文書を新しいタブとして開く
    fn open_new_tab(&mut self) {
        let current = self.snapshot_active_tab();
        self.tabs[self.active_tab] = Some(current);
        self.tabs.push(None);
        self.active_tab = self.tabs.len() - 1;
        self.reset_to_blank_document();
        self.status_message = "新規タブを作成しました".to_string();
    }

    /// アクティブなタブを閉じる。残り1枚の場合はタブ自体は閉じず、空の新規文書にリセットする
    fn close_active_tab(&mut self) {
        if self.tabs.len() <= 1 {
            self.reset_to_blank_document();
            self.status_message = "タブを閉じました（新規文書）".to_string();
            return;
        }
        self.tabs.remove(self.active_tab);
        let new_index = self.active_tab.min(self.tabs.len() - 1);
        let snapshot = self.tabs[new_index]
            .take()
            .expect("残っているスロットは必ずSomeのはず（削除前にNoneは1つしかない）");
        self.load_tab_snapshot(snapshot);
        self.active_tab = new_index;
        self.status_message = "タブを閉じました".to_string();
    }

    /// タブバーに表示する短い表示名（ファイル名、未保存なら「新規文書」）を返す
    fn tab_display_title(current_file: &Option<PathBuf>) -> String {
        current_file
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "新規文書".to_string())
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
                self.record_recent_file(&path);
            }
            Err(e) => {
                self.status_message = format!("ファイル読み込みエラー: {e}");
            }
        }
    }

    /// 最近使ったファイル一覧の先頭に追加し、重複排除・件数上限・永続化を行う
    fn record_recent_file(&mut self, path: &Path) {
        let path_buf = path.to_path_buf();
        self.recent_files.retain(|p| p != &path_buf);
        self.recent_files.insert(0, path_buf);
        self.recent_files.truncate(8);
        if self.persist_settings {
            save_recent_files(&self.recent_files);
        }
    }

    /// 編集停止から一定時間が経過していれば自動保存バックアップを書き出す（失敗は無視）
    fn maybe_autosave(&mut self) {
        const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);
        if !self.is_dirty {
            return;
        }
        if self.last_autosave_time.elapsed() < AUTOSAVE_INTERVAL {
            return;
        }
        self.last_autosave_time = Instant::now();
        if let Some(path) = autosave_path_for(&self.current_file) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, &self.markdown_text);
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
                        self.last_export_path = Some(path);
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

    fn toggle_dark_mode(&mut self, ctx: &egui::Context) {
        self.dark_mode = !self.dark_mode;
        setup_visual_theme(ctx, self.dark_mode);
        if self.persist_settings {
            save_dark_mode_setting(self.dark_mode);
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
                    self.last_export_path = Some(path);
                }
                Err(e) => {
                    self.status_message = format!("HTML書き込みエラー: {e}");
                }
            }
        }
    }

    fn action_export_docx(&mut self) {
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
            let doc = parse_document(&self.markdown_text);
            match generate_docx_bytes(&doc) {
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
    fn action_open_last_export(&mut self) {
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

    /// 選択中のテキストがあれば `prefix` と `suffix` で囲み、なければ `prefix + default_inner + suffix` を挿入する
    fn wrap_selection_or_insert(&mut self, prefix: &str, default_inner: &str, suffix: &str) {
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
    fn find_matches(&self) -> Vec<(usize, usize)> {
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
            } else if matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp"
            ) {
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
            if self.any_unsaved_changes() && !self.allow_immediate_close {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.pending_action = Some(PendingAction::CloseWindow);
            }
        }

        // グローバルショートカットキー処理
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::S)) {
            self.action_save_file(true);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) {
            self.action_save_file(false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.request_action(PendingAction::OpenFileDialog, &ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.request_action(PendingAction::NewFile, &ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::P)) {
            self.action_export_pdf();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F))
            || ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::H))
        {
            self.show_find_replace = !self.show_find_replace;
        }
        if self.show_find_replace && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.show_find_replace = false;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::B)) {
            self.wrap_selection_or_insert("**", "太字テキスト", "**");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::I)) {
            self.wrap_selection_or_insert("*", "斜体テキスト", "*");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Y)) {
            self.perform_redo();
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
            self.perform_undo();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::U)) {
            self.wrap_selection_or_insert("[", "下線テキスト", "]{underline}");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::D)) {
            self.toggle_dark_mode(&ctx);
        }

        // 改行(<br>)挿入：Shift+Enter で段落・表セル内の強制改行を挿入する
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Enter)) {
            self.insert_at_cursor("<br>");
        }
        // 改ページ・空白行（Enter系の拡張）
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            self.insert_at_cursor("\n:::pagebreak\n");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter)) {
            self.insert_at_cursor("\n:::space 1em\n");
        }

        // 文字色（Ctrl+Alt+数字）
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::Num1)) {
            self.wrap_selection_or_insert("[", "重要事項", "]{red}");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::Num2)) {
            self.wrap_selection_or_insert("[", "補足事項", "]{blue}");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::Num3)) {
            self.wrap_selection_or_insert("[", "正常", "]{green}");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::Num4)) {
            self.wrap_selection_or_insert("[", "注意", "]{orange}");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::Num5)) {
            self.wrap_selection_or_insert("[", "参考", "]{purple}");
        }

        // 挿入系（Ctrl+Alt+文字）
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::T)) {
            self.insert_at_cursor(
                "\n{widths: 25%, 50%, 25%}\n| 項目 | 内容 | 備考 |\n| :--- | :--- | :--- |\n| サンプル | 内容を入力 | 備考 |\n",
            );
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::R)) {
            self.insert_at_cursor("\n{align: right}\n2026年10月16日\n");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::C)) {
            self.insert_at_cursor("\n{align: center, size: 15pt}\n**文書タイトル**\n");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::W)) {
            self.insert_at_cursor("\n{align: center, block-width: 60%}\n");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::L)) {
            self.insert_at_cursor("\n{line-height: 1.2}\n");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::I)) {
            self.action_insert_image_dialog();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::B)) {
            self.insert_at_cursor("\n{box}\n> 注意事項などをここに記載します。\n");
        }

        // タブ操作（Ctrl+Alt系のショートカットより後に置き、誤って奪わないようにする）
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::T)) {
            self.open_new_tab();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::W)) {
            self.request_action(PendingAction::CloseTab, &ctx);
        }

        // 表示・出力系（Ctrl+Shift+文字 / Ctrl+記号）
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::E)) {
            self.action_export_html();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::Y)) {
            self.show_typst_source = !self.show_typst_source;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::O)) {
            self.show_outline_panel = !self.show_outline_panel;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Comma)) {
            self.show_page_settings_bar = !self.show_page_settings_bar;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Plus))
            || ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Equals))
        {
            self.zoom = (self.zoom + 0.15).clamp(0.5, 2.5);
            self.revision += 1;
            self.trigger_preview_update();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Minus)) {
            self.zoom = (self.zoom - 0.15).clamp(0.5, 2.5);
            self.revision += 1;
            self.trigger_preview_update();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Num0)) {
            self.zoom = 1.0;
            self.revision += 1;
            self.trigger_preview_update();
        }

        // Ctrl + マウスホイールによるプレビュー拡大・縮小
        let zoom_delta = ctx.input(|i| i.zoom_delta());
        if (zoom_delta - 1.0).abs() > 0.001 {
            self.zoom = (self.zoom * zoom_delta).clamp(0.5, 2.5);
            self.revision += 1;
            self.pending_render = true;
            self.last_edit_time = Instant::now();
        }

        self.maybe_autosave();

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

                    ui.menu_button("🕒 最近使ったファイル", |ui| {
                        if self.recent_files.is_empty() {
                            ui.label("（まだ履歴がありません）");
                        } else {
                            let candidates = self.recent_files.clone();
                            for path in candidates {
                                let label = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                                if ui.button(label).on_hover_text(path.to_string_lossy()).clicked() {
                                    self.request_action(PendingAction::LoadFilePath(path), &ctx);
                                    ui.close();
                                }
                            }
                        }
                    });

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
                        .on_hover_text("用紙向き・フォント・余白・文字サイズ・行間の設定バーを開閉します (Ctrl+,)")
                        .clicked()
                    {
                        self.show_page_settings_bar = !self.show_page_settings_bar;
                    }
                    if ui
                        .selectable_label(self.show_outline_panel, "🗂 アウトライン")
                        .on_hover_text("見出し一覧パネルを開閉し、クリックで該当箇所へジャンプします (Ctrl+Shift+O)")
                        .clicked()
                    {
                        self.show_outline_panel = !self.show_outline_panel;
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
                        .on_hover_text("ページ境界に依存しない単一HTMLファイルとして出力します (Ctrl+Shift+E)")
                        .clicked()
                    {
                        self.action_export_html();
                    }

                    let docx_btn = egui::Button::new(
                        RichText::new("📝 Word出力...").color(Color32::from_rgb(23, 78, 166)),
                    )
                    .fill(Color32::from_rgb(232, 240, 254));
                    if ui
                        .add(docx_btn)
                        .on_hover_text(
                            "Microsoft Word (.docx) 形式で出力します（画像埋め込み・実クリックリンクは非対応。目次は右クリック→フィールド更新で表示）",
                        )
                        .clicked()
                    {
                        self.action_export_docx();
                    }

                    ui.separator();
                    ui.checkbox(&mut self.show_typst_source, "Typstコード")
                        .on_hover_text("生成されたTypstコードを表示します (Ctrl+Alt+Y)");

                    let dark_mode_label = if self.dark_mode { "☀ ライト" } else { "🌙 ダーク" };
                    if ui
                        .selectable_label(self.dark_mode, dark_mode_label)
                        .on_hover_text("ダークモード / ライトモードを切り替えます (Ctrl+Alt+D)")
                        .clicked()
                    {
                        self.toggle_dark_mode(&ui.ctx().clone());
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("100%").on_hover_text("表示倍率を100%に戻す (Ctrl+0)").clicked() {
                            self.zoom = 1.0;
                            self.revision += 1;
                            self.trigger_preview_update();
                        }
                        if ui
                            .button("＋")
                            .on_hover_text("プレビュー拡大 (Ctrl+ホイール / Ctrl++)")
                            .clicked()
                        {
                            self.zoom = (self.zoom + 0.15).clamp(0.5, 2.5);
                            self.revision += 1;
                            self.trigger_preview_update();
                        }
                        ui.label(format!("{:.0}%", self.zoom * 100.0));
                        if ui
                            .button("－")
                            .on_hover_text("プレビュー縮小 (Ctrl+ホイール / Ctrl+-)")
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
                        if ui.selectable_label(is_gothic, "ゴシック体").clicked() && !is_gothic
                        {
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
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("生成されたTypst組版コード (自動変換)").strong(),
                                );
                                if ui
                                    .small_button("📋 コピー")
                                    .on_hover_text(
                                        "生成されたTypstコードをクリップボードにコピーします",
                                    )
                                    .clicked()
                                {
                                    ui.ctx().copy_text(self.last_typst_code.clone());
                                    self.status_message =
                                        "Typstコードをクリップボードにコピーしました".to_string();
                                }
                            });
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

        // 見出しアウトラインパネル（クリックで該当見出しへジャンプ）
        if self.show_outline_panel {
            egui::Panel::left("outline_panel")
                .resizable(true)
                .default_size(200.0)
                .size_range(140.0..=420.0)
                .show(ui, |ui| {
                    ui.label(RichText::new("🗂 アウトライン").strong().small());
                    ui.separator();
                    let headings = extract_headings_outline(&self.markdown_text);
                    if headings.is_empty() {
                        ui.label(RichText::new("見出しがありません").small().weak());
                    } else {
                        egui::ScrollArea::vertical()
                            .id_salt("outline_scroll")
                            .show(ui, |ui| {
                                for (level, title, char_idx) in headings {
                                    let indent = (level.saturating_sub(1)) as f32 * 12.0;
                                    ui.horizontal(|ui| {
                                        ui.add_space(indent);
                                        if ui.small_button(&title).clicked() {
                                            self.cursor_char_range = Some((char_idx, char_idx));
                                            self.pending_cursor_selection =
                                                Some((char_idx, char_idx));
                                        }
                                    });
                                }
                            });
                    }
                });
        }

        // 中央（左ペイン）：マークアップエディタ＆クイック挿入バー＆検索置換バー
        egui::CentralPanel::default().show(ui, |ui| {
            // タブバー（複数文書の切り替え・新規タブ・タブを閉じる）
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;

                let mut switch_to: Option<usize> = None;
                let mut close_index: Option<usize> = None;

                for i in 0..self.tabs.len() {
                    let (title, is_dirty) = if i == self.active_tab {
                        (
                            Self::tab_display_title(&self.current_file),
                            self.is_dirty,
                        )
                    } else {
                        let snap = self.tabs[i].as_ref().expect("非アクティブタブはSomeのはず");
                        (Self::tab_display_title(&snap.current_file), snap.is_dirty)
                    };
                    let dirty_mark = if is_dirty { "● " } else { "" };
                    let label = format!("{dirty_mark}{title}");

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.selectable_label(i == self.active_tab, label).clicked() && i != self.active_tab {
                            switch_to = Some(i);
                        }
                        if ui.small_button("×").on_hover_text("このタブを閉じます").clicked() {
                            close_index = Some(i);
                        }
                    });
                }

                if ui
                    .small_button("＋")
                    .on_hover_text("新規タブを開きます (Ctrl+T)")
                    .clicked()
                {
                    self.open_new_tab();
                }

                if let Some(i) = switch_to {
                    self.switch_to_tab(i);
                }
                if let Some(i) = close_index {
                    if i != self.active_tab {
                        self.switch_to_tab(i);
                    }
                    self.request_action(PendingAction::CloseTab, &ui.ctx().clone());
                }
            });
            ui.separator();

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
                    .small_button("U 下線")
                    .on_hover_text("選択範囲（または新規テキスト）に下線を引きます (Ctrl+U)")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "下線テキスト", "]{underline}");
                }
                if ui
                    .small_button("🔴 赤文字")
                    .on_hover_text("選択範囲（または新規テキスト）を赤文字にします (Ctrl+Alt+1)")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "重要事項", "]{red}");
                }
                if ui
                    .small_button("🔵 青文字")
                    .on_hover_text("選択範囲（または新規テキスト）を青文字にします (Ctrl+Alt+2)")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "補足事項", "]{blue}");
                }
                if ui
                    .small_button("🟢 緑文字")
                    .on_hover_text("選択範囲（または新規テキスト）を緑文字にします (Ctrl+Alt+3)")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "正常", "]{green}");
                }
                if ui
                    .small_button("🟠 橙文字")
                    .on_hover_text("選択範囲（または新規テキスト）を橙文字にします (Ctrl+Alt+4)")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "注意", "]{orange}");
                }
                if ui
                    .small_button("🟣 紫文字")
                    .on_hover_text("選択範囲（または新規テキスト）を紫文字にします (Ctrl+Alt+5)")
                    .clicked()
                {
                    self.wrap_selection_or_insert("[", "参考", "]{purple}");
                }

                ui.add(
                    TextEdit::singleline(&mut self.custom_color_hex)
                        .desired_width(60.0)
                        .hint_text("#rrggbb"),
                );
                if ui
                    .small_button("🎨 適用")
                    .on_hover_text("入力したHEXカラーコード（#rrggbb または #rgb）で選択範囲（または新規テキスト）を着色します")
                    .clicked()
                {
                    let hex = self.custom_color_hex.trim().to_string();
                    if is_valid_hex_color(&hex) {
                        self.wrap_selection_or_insert("[", "カスタム色", &format!("]{{color:{hex}}}"));
                    } else {
                        self.status_message =
                            format!("無効なカラーコードです（#rrggbb 形式で入力してください）: {hex}");
                    }
                }

                ui.separator();

                if ui
                    .small_button("📊 列幅付き表")
                    .on_hover_text("カーソル位置に列幅指定付きの表を挿入します (Ctrl+Alt+T)")
                    .clicked()
                {
                    self.insert_at_cursor(
                        "\n{widths: 25%, 50%, 25%}\n| 項目 | 内容 | 備考 |\n| :--- | :--- | :--- |\n| サンプル | 内容を入力 | 備考 |\n",
                    );
                }
                if ui
                    .small_button("➡ 右寄せ段落")
                    .on_hover_text("カーソル位置に日付・署名向けの右寄せ属性を挿入します (Ctrl+Alt+R)")
                    .clicked()
                {
                    self.insert_at_cursor("\n{align: right}\n2026年10月16日\n");
                }
                if ui
                    .small_button("↔ 中央タイトル")
                    .on_hover_text("カーソル位置に中央寄せ＋文字サイズ指定を挿入します (Ctrl+Alt+C)")
                    .clicked()
                {
                    self.insert_at_cursor("\n{align: center, size: 15pt}\n**文書タイトル**\n");
                }
                if ui
                    .small_button("⬌ 幅指定")
                    .on_hover_text("カーソル位置に段落の表示幅（中央寄せ）指定を挿入します (Ctrl+Alt+W)")
                    .clicked()
                {
                    self.insert_at_cursor("\n{align: center, block-width: 60%}\n");
                }
                if ui
                    .small_button("↕ 行間指定")
                    .on_hover_text("カーソル位置に段落の行間指定を挿入します (Ctrl+Alt+L)")
                    .clicked()
                {
                    self.insert_at_cursor("\n{line-height: 1.2}\n");
                }
                if ui
                    .small_button("🖼 画像...")
                    .on_hover_text("ローカル画像ファイルを選択してカーソル位置に挿入します（ドラッグ＆ドロップも可）。挿入後、![alt](path){width: 50%} のようにサイズ指定を追記できます (Ctrl+Alt+I)")
                    .clicked()
                {
                    self.action_insert_image_dialog();
                }
                if ui
                    .small_button("📄 改ページ")
                    .on_hover_text("カーソル位置に手動改ページを挿入します (Ctrl+Enter)")
                    .clicked()
                {
                    self.insert_at_cursor("\n:::pagebreak\n");
                }
                if ui
                    .small_button("↕ 空白行")
                    .on_hover_text("カーソル位置に明示的な垂直方向の空白を挿入します (Ctrl+Shift+Enter)")
                    .clicked()
                {
                    self.insert_at_cursor("\n:::space 1em\n");
                }
                if ui
                    .small_button("🔲 囲み枠")
                    .on_hover_text("カーソル位置に全周を罫線で囲んだボックスを挿入します (Ctrl+Alt+B)")
                    .clicked()
                {
                    self.insert_at_cursor("\n{box}\n> 注意事項などをここに記載します。\n");
                }
                if ui
                    .small_button("📑 目次")
                    .on_hover_text("カーソル位置に見出し一覧から自動生成される目次を挿入します")
                    .clicked()
                {
                    self.insert_at_cursor("\n:::toc\n\n");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::SAMPLE_DOCUMENT;

    #[test]
    fn test_highlight_japanese_sample_document() {
        let job = highlight_custom_markdown(SAMPLE_DOCUMENT, false);
        assert_eq!(job.text, SAMPLE_DOCUMENT);
        assert!(!job.sections.is_empty());
    }

    #[test]
    fn test_highlight_dark_mode_uses_light_enough_default_text_color() {
        // ダークモードの既定文字色が、暗い背景上で十分な明るさ（視認性）を持つことを確認する。
        // 以前はライトモード用のほぼ黒に近い色（rgb 32,33,36）を使い回しており、
        // ダーク背景上でほぼ読めなくなっていた。
        let job = highlight_custom_markdown("通常のテキスト\n", true);
        let color = job
            .sections
            .first()
            .expect("少なくとも1区間はあるはず")
            .format
            .color;
        let brightness = color.r() as u32 + color.g() as u32 + color.b() as u32;
        assert!(
            brightness > 400,
            "ダークモードの既定文字色が暗すぎます: {color:?}"
        );
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
            find_ignore_case: false,
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
            recent_files: Vec::new(),
            last_autosave_time: Instant::now(),
            show_outline_panel: false,
            custom_color_hex: String::new(),
            last_export_path: None,
            dark_mode: false,
            persist_settings: false,
            tabs: vec![None],
            active_tab: 0,
        }
    }

    #[test]
    fn test_open_new_tab_preserves_previous_tab_content() {
        let mut app = test_app("1番目のタブの内容");
        app.is_dirty = true;
        assert_eq!(app.tabs.len(), 1);

        app.open_new_tab();

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, 1);
        assert_eq!(app.markdown_text, BLANK_DOCUMENT_TEMPLATE);
        assert!(!app.is_dirty, "新規タブはクリーンな状態で始まる");
        // 退避された1番目のタブの内容が保持されていることを確認
        let snap0 = app.tabs[0].as_ref().unwrap();
        assert_eq!(snap0.markdown_text, "1番目のタブの内容");
        assert!(snap0.is_dirty);
    }

    #[test]
    fn test_switch_to_tab_swaps_content_both_ways() {
        let mut app = test_app("タブA");
        app.open_new_tab();
        app.markdown_text = "タブB".to_string();

        app.switch_to_tab(0);
        assert_eq!(app.markdown_text, "タブA");
        assert_eq!(app.active_tab, 0);

        app.switch_to_tab(1);
        assert_eq!(app.markdown_text, "タブB");
        assert_eq!(app.active_tab, 1);
    }

    #[test]
    fn test_switch_to_same_tab_is_a_no_op() {
        let mut app = test_app("内容");
        app.switch_to_tab(0);
        assert_eq!(app.markdown_text, "内容");
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn test_close_last_remaining_tab_resets_to_blank_instead_of_removing() {
        let mut app = test_app("内容");
        app.is_dirty = true;
        assert_eq!(app.tabs.len(), 1);

        app.close_active_tab();

        assert_eq!(app.tabs.len(), 1, "最後の1枚は閉じずに残る");
        assert_eq!(app.markdown_text, BLANK_DOCUMENT_TEMPLATE);
        assert!(!app.is_dirty);
    }

    #[test]
    fn test_close_active_tab_activates_neighboring_tab() {
        let mut app = test_app("タブA");
        app.open_new_tab();
        app.markdown_text = "タブB".to_string();
        app.open_new_tab();
        app.markdown_text = "タブC".to_string();
        assert_eq!(app.tabs.len(), 3);
        assert_eq!(app.active_tab, 2);

        // 中央(タブB)に切り替えてから閉じる
        app.switch_to_tab(1);
        assert_eq!(app.markdown_text, "タブB");
        app.close_active_tab();

        assert_eq!(app.tabs.len(), 2);
        // タブBを閉じた後はタブCがアクティブになる（スライドしてきた側）
        assert_eq!(app.markdown_text, "タブC");

        // 残りのタブAが引き続き保持されていることを確認
        app.switch_to_tab(0);
        assert_eq!(app.markdown_text, "タブA");
    }

    #[test]
    fn test_any_unsaved_changes_detects_dirty_background_tab() {
        let mut app = test_app("内容");
        app.is_dirty = false;
        app.open_new_tab();
        assert!(!app.is_dirty);
        // アクティブタブはクリーンでも、裏に退避したタブが未保存ならtrueを返す
        app.tabs[0].as_mut().unwrap().is_dirty = true;
        assert!(app.any_unsaved_changes());

        app.tabs[0].as_mut().unwrap().is_dirty = false;
        assert!(!app.any_unsaved_changes());
    }

    #[test]
    fn test_tab_display_title_uses_file_name_or_placeholder() {
        assert_eq!(
            TyperApp::tab_display_title(&Some(PathBuf::from(r"C:\docs\report.md"))),
            "report.md"
        );
        assert_eq!(TyperApp::tab_display_title(&None), "新規文書");
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
    fn test_insert_at_cursor_inserts_br_tag_for_table_cell_line_break() {
        // Shift+Enter ショートカットが行う挿入と同じ操作を検証する：
        // 表のセルは1物理行でしか書けないため、改行には `<br>` を使う
        let mut app = test_app("| 1行目2行目 |");
        let br_pos = "| 1行目".chars().count();
        app.cursor_char_range = Some((br_pos, br_pos));
        app.insert_at_cursor("<br>");
        assert_eq!(app.markdown_text, "| 1行目<br>2行目 |");
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
    fn test_resolve_startup_document_falls_back_to_sample_when_no_recent_files() {
        let (text, file, status) = resolve_startup_document(&[]);
        assert_eq!(text, SAMPLE_DOCUMENT);
        assert!(file.is_none());
        assert!(status.contains("サンプル"));
    }

    #[test]
    fn test_resolve_startup_document_restores_most_recent_readable_file() {
        let dir = std::env::temp_dir().join(format!(
            "typer_test_{}_{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("restore_me.md");
        std::fs::write(&path, "前回の原稿の内容").unwrap();

        let (text, file, status) = resolve_startup_document(&[path.clone()]);
        assert_eq!(text, "前回の原稿の内容");
        assert_eq!(file, Some(path.clone()));
        assert!(status.contains("自動的に開きました"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_startup_document_falls_back_when_most_recent_file_is_missing() {
        let missing = PathBuf::from("Z:\\does\\not\\exist\\typer_missing_test.md");
        let (text, file, _status) = resolve_startup_document(&[missing]);
        assert_eq!(text, SAMPLE_DOCUMENT);
        assert!(file.is_none());
    }

    #[test]
    fn test_record_recent_file_dedups_and_moves_to_front() {
        let mut app = test_app("");
        app.recent_files = vec![PathBuf::from("a.md"), PathBuf::from("b.md")];
        app.record_recent_file(Path::new("b.md"));
        assert_eq!(
            app.recent_files,
            vec![PathBuf::from("b.md"), PathBuf::from("a.md")]
        );
    }

    #[test]
    fn test_record_recent_file_caps_at_eight_entries() {
        let mut app = test_app("");
        for i in 0..8 {
            app.record_recent_file(Path::new(&format!("{i}.md")));
        }
        app.record_recent_file(Path::new("new.md"));
        assert_eq!(app.recent_files.len(), 8);
        assert_eq!(app.recent_files[0], PathBuf::from("new.md"));
    }

    #[test]
    fn test_test_app_does_not_persist_settings_to_real_appdata() {
        // test_app() はユーザーの実際の %APPDATA%\Typer\ を汚染してはいけない
        let mut app = test_app("");
        assert!(!app.persist_settings);
        app.record_recent_file(Path::new("should-not-be-saved.md"));
        app.dark_mode = true; // toggle_dark_mode を経由しない直接変更なのでディスクには触れない
        if let Some(path) = recent_files_store_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                assert!(
                    !content.contains("should-not-be-saved.md"),
                    "test_app での record_recent_file が実設定ファイルに書き込んでしまっている"
                );
            }
        }
    }

    #[test]
    fn test_shortcut_modifier_is_cross_platform_command_key() {
        // 本アプリの全ショートカットは `Modifiers::CTRL` ではなく `Modifiers::COMMAND` で
        // consume_key している（Mac対応）。egui はプラットフォームに応じて物理キー押下を
        // 次のように解釈するため、どちらの環境でもショートカットが一致することを確認する：
        //   Windows/Linuxで物理Ctrlを押す  → ctrl=true, command=true
        //   Macで物理Cmdを押す             → mac_cmd=true, command=true
        let windows_ctrl_pressed = Modifiers::CTRL | Modifiers::COMMAND;
        let mac_cmd_pressed = Modifiers::MAC_CMD | Modifiers::COMMAND;

        assert!(windows_ctrl_pressed.matches_logically(Modifiers::COMMAND));
        assert!(mac_cmd_pressed.matches_logically(Modifiers::COMMAND));

        // Macで物理Ctrl（コマンドキーではない）を押しても一致してはいけない
        assert!(!Modifiers::CTRL.matches_logically(Modifiers::COMMAND));
    }

    #[test]
    fn test_is_valid_hex_color() {
        assert!(is_valid_hex_color("#1a73e8"));
        assert!(is_valid_hex_color("#fff"));
        assert!(!is_valid_hex_color("banana"));
        assert!(!is_valid_hex_color("1a73e8"));
        assert!(!is_valid_hex_color("#1a73e"));
        assert!(!is_valid_hex_color("#1a73eg"));
        assert!(!is_valid_hex_color(""));
    }

    #[test]
    fn test_find_matches_ignore_case() {
        let mut app = test_app("Hello world, HELLO Rust");
        app.find_query = "hello".to_string();
        app.find_ignore_case = true;
        let matches = app.find_matches();
        assert_eq!(matches, vec![(0, 5), (13, 18)]);
    }

    #[test]
    fn test_find_matches_case_sensitive_by_default() {
        let mut app = test_app("Hello world, HELLO Rust");
        app.find_query = "hello".to_string();
        assert!(app.find_matches().is_empty());
    }

    #[test]
    fn test_replace_all_matches_ignore_case() {
        let mut app = test_app("Hello world, HELLO Rust");
        app.find_query = "hello".to_string();
        app.find_ignore_case = true;
        app.replace_text = "Hi".to_string();
        app.replace_all_matches();
        assert_eq!(app.markdown_text, "Hi world, Hi Rust");
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
                let _ = highlight_custom_markdown(&text, false);
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
