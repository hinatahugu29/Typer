//! デスクトップUI層 (`egui` / `eframe`)
//!
//! - 左ペイン：シンタックスハイライト付きエディタ、カーソル位置への書式挿入・選択範囲ラップ、
//!   検索・置換バー (`Ctrl+F` / `Ctrl+H`)、Undo/Redo履歴、ページ設定GUIバー
//! - 右ペイン：Typst組版結果のリアルタイムページプレビュー（`Ctrl+ホイール` ズーム・複数ページ対応）
//! - 安全性・操作性：未保存変更時の終了・読み込み確認モーダル、ドラッグ＆ドロップ読み込み（文書・画像）

mod editor;
mod file_ops;
mod panels;
mod preview;
mod shortcuts;
mod toolbar;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, Key, Modifiers, RichText, Sense, Stroke, TextEdit, TextureHandle,
    TextureOptions, Vec2, ViewportCommand,
    text::{CCursor, CCursorRange},
};

use crate::docx_export::generate_docx_bytes_with_base;
use crate::html_export::generate_html_with_base;
use crate::lint::{SyntaxWarning, lint_markdown};
use crate::model::{FontChoice, Orientation};
use crate::parser::{
    extract_headings_outline, parse_document, parse_settings, update_document_settings_in_markdown,
};
use crate::templates::{
    TEMPLATES, UserTemplate, list_user_templates, rename_user_template, save_user_template,
    user_templates_dir,
};
use crate::typst_engine::{
    CompileError, DescribedError, HeadingPosition, PreviewOutput, TypstEngine,
    available_font_families, available_latin_font_families,
};
use crate::typst_gen::{
    SourceLineMap, generate_typst_source_with_base, generate_typst_source_with_map,
};
#[cfg(test)]
use crate::ui_persistence::recent_files_store_path;
use crate::ui_persistence::{
    SessionTab, autosave_path_for, load_dark_mode_setting, load_recent_files, load_session,
    resolve_startup_document, sanitize_file_name, save_dark_mode_setting, save_recent_files,
    save_session, setup_japanese_fonts, setup_visual_theme,
};
use crate::ui_text::{
    byte_to_char_idx, char_idx_to_line_col, char_to_byte_idx, highlight_custom_markdown,
    is_valid_hex_color, line_char_range, tsv_to_markdown_table,
};

/// フォント名の入力欄と、入力中の文字で絞り込んだ候補ポップアップを描画する。値が変わったら `true`。
///
/// 開閉状態は `has_focus()` を毎フレーム見るのではなく `open` で保持する。候補をクリックした
/// 瞬間にテキスト欄が同じフレーム内でフォーカスを失うため、フォーカスで判定すると
/// ポップアップごと描画されずクリックが届かない。
///
/// egui既定の「どこをクリックしても閉じる」は使わない。それだと入力欄内のクリック（カーソル移動）
/// でも閉じてしまい、フォーカスが入り直すまで再表示されないため。閉じるのは候補の選択・
/// Escape・入力欄とポップアップ以外の場所のクリックのときだけにしている。
fn font_name_input_with_suggestions(
    ui: &mut egui::Ui,
    value: &mut String,
    open: &mut bool,
    candidates: impl FnOnce() -> Vec<String>,
    hint: &str,
) -> bool {
    let mut changed = false;
    let response = ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(90.0)
            .hint_text(hint),
    );
    changed |= response.changed();
    if response.gained_focus() || response.clicked() || response.changed() {
        *open = true;
    }
    if !*open {
        return changed;
    }
    let query = value.to_ascii_lowercase();
    let suggestions: Vec<String> = candidates()
        .into_iter()
        .filter(|family| query.is_empty() || family.to_ascii_lowercase().contains(&query))
        .take(8)
        .collect();
    // 候補0件でも `open` は維持する（文字を消して再び一致すれば、そのまま表示を再開する）
    if suggestions.is_empty() {
        return changed;
    }
    let mut picked = false;
    let shown = egui::Popup::from_response(&response)
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
        .open_bool(open)
        .show(|ui| {
            for family in suggestions {
                if ui.button(&family).clicked() {
                    *value = family;
                    changed = true;
                    picked = true;
                }
            }
        });
    if picked {
        *open = false;
    } else if let Some(shown) = shown {
        let clicked_elsewhere = ui.input(|i| {
            i.pointer.any_click()
                && i.pointer.interact_pos().is_some_and(|pos| {
                    !shown.response.rect.contains(pos) && !response.rect.contains(pos)
                })
        });
        if clicked_elsewhere {
            *open = false;
        }
    }
    changed
}

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
        result: Result<PreviewOutput, CompileError>,
    },
}

/// 自分のテンプレートの保存・名前の変更・削除の確認ダイアログ
#[derive(Debug, Clone)]
enum TemplateDialog {
    /// 現在の文書をテンプレートとして保存する。`overwrite` は同じ名前があって上書きを確認中
    Save {
        name: String,
        overwrite: bool,
        error: Option<String>,
    },
    Rename {
        template: UserTemplate,
        name: String,
        error: Option<String>,
    },
    Delete {
        template: UserTemplate,
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

/// 見出し文字列を比較用に正規化する（Markdownの装飾記号や空白の差を無視し、文字と数字だけ残す）
fn normalize_heading_text(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// アウトラインの `index` 番目（同名見出しの中では `same_title_before` 番目）の見出しに対応する
/// プレビュー上の位置を探す。まず文字列の一致で探し、見つからなければ出現順で対応させる
fn find_preview_heading(
    headings: &[HeadingPosition],
    index: usize,
    title: &str,
    same_title_before: usize,
) -> Option<(usize, f32)> {
    let key = normalize_heading_text(title);
    headings
        .iter()
        .filter(|h| !key.is_empty() && normalize_heading_text(&h.text) == key)
        .nth(same_title_before)
        .or_else(|| headings.get(index))
        .map(|h| (h.page, h.y_pt))
}

const BLANK_DOCUMENT_TEMPLATE: &str = "---\ntitle:\npaper: A4\norientation: portrait\nmargin: 20mm\nfont-size: 10.5pt\nline-height: 1.5\nfont: gothic\n---\n\n# 新規報告書タイトル\n\nここに本文を入力してください。\n";

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
    /// 直近の組版エラー（原稿の行番号付き）
    last_error: Option<Vec<DescribedError>>,
    /// 直近に生成したTypstコードの各行が原稿の何行目に由来するか（エラー位置の表示用）
    last_line_map: SourceLineMap,
    /// 原稿の書き間違い（効いていない属性指定など）の警告。プレビュー更新のたびに調べ直す
    syntax_warnings: Vec<SyntaxWarning>,

    /// プレビュー表示倍率（1.0 = 100%）
    zoom: f32,
    /// プレビューをウィンドウ全幅に最大化しているか（エディタは非表示になる）
    preview_maximized: bool,
    /// アウトラインパネルの幅（右端のつまみをドラッグして変更する）
    outline_width: f32,
    /// 直近の組版結果に含まれる見出しの位置（出現順）
    preview_headings: Vec<HeadingPosition>,
    /// 次フレームでプレビューをスクロールさせる先（ページ番号, ページ上端からのpt）
    pending_preview_jump: Option<(usize, f32)>,
    /// 次にカーソル位置へエディタをスクロールするとき、その行を表示領域の上端に合わせるか
    /// （アウトラインからのジャンプ用。通常の挿入・検索では最小限のスクロールにとどめる）
    scroll_editor_cursor_to_top: bool,
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
    /// 直近に自動保存したときの `revision`（変更がなければ自動保存を繰り返さない）
    last_autosaved_revision: u64,
    /// 開いているテンプレートのダイアログ（保存・名前の変更・削除）
    template_dialog: Option<TemplateDialog>,
    /// 見出しアウトラインパネルの表示フラグ
    show_outline_panel: bool,
    /// カスタム文字色入力欄（HEXコード）
    custom_color_hex: String,
    /// カスタムフォント名入力欄の候補ポップアップを表示するか。
    /// `TextEdit` の `has_focus()` を毎フレーム直接条件にすると、候補ボタンをクリックした瞬間に
    /// テキスト欄がフォーカスを失ったと判定されて同じフレームでポップアップごと描画されなくなり、
    /// クリックが握りつぶされる。そのため開閉状態をこのフィールドで独立管理する
    show_font_suggestions: bool,
    /// 欧文フォント入力欄の候補ポップアップを表示するか（理由は `show_font_suggestions` と同じ）
    show_latin_font_suggestions: bool,
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
            last_line_map: SourceLineMap::default(),
            syntax_warnings: Vec::new(),
            zoom: 1.0,
            preview_maximized: false,
            outline_width: 220.0,
            preview_headings: Vec::new(),
            pending_preview_jump: None,
            scroll_editor_cursor_to_top: false,
            show_typst_source: false,
            status_message: initial_status,
            startup_visibility_ensured: false,
            recent_files,
            last_autosave_time: Instant::now(),
            last_autosaved_revision: 0,
            template_dialog: None,
            show_outline_panel: false,
            custom_color_hex: String::new(),
            show_font_suggestions: false,
            show_latin_font_suggestions: false,
            last_export_path: None,
            dark_mode,
            persist_settings: true,
            tabs: vec![None],
            active_tab: 0,
        };

        app.restore_session();
        app.trigger_preview_update();
        app
    }

    /// 前回終了時に保存したタブ一式があれば、未保存のものも含めて復元する
    fn restore_session(&mut self) {
        let Some((saved, active)) = load_session() else {
            return;
        };
        let count = saved.len();
        self.tabs = saved
            .into_iter()
            .map(|tab| {
                Some(TabSnapshot {
                    markdown_text: tab.content.unwrap_or_default(),
                    current_file: tab.current_file,
                    is_dirty: tab.is_dirty,
                    undo_stack: Vec::new(),
                    redo_stack: Vec::new(),
                    cursor_char_range: None,
                    last_export_path: None,
                })
            })
            .collect();
        self.active_tab = active;
        let snapshot = self.tabs[active]
            .take()
            .expect("復元直後のスロットは必ずSome");
        self.load_tab_snapshot(snapshot);
        self.status_message = format!("前回のタブを復元しました（{count}件）");
    }

    /// 開いている全タブをセッションとして保存する（テスト時は実設定を汚さないよう何もしない）
    fn save_session_now(&self) {
        if !self.persist_settings {
            return;
        }
        let tabs: Vec<SessionTab> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let snap = match slot {
                    Some(s) => s.clone(),
                    None => {
                        debug_assert_eq!(i, self.active_tab);
                        self.snapshot_active_tab()
                    }
                };
                let keep_text = snap.is_dirty || snap.current_file.is_none();
                SessionTab {
                    content: keep_text.then_some(snap.markdown_text),
                    current_file: snap.current_file,
                    is_dirty: snap.is_dirty,
                }
            })
            .collect();
        save_session(&tabs, self.active_tab);
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

    fn toggle_dark_mode(&mut self, ctx: &egui::Context) {
        self.dark_mode = !self.dark_mode;
        setup_visual_theme(ctx, self.dark_mode);
        if self.persist_settings {
            save_dark_mode_setting(self.dark_mode);
        }
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
            if self.persist_settings {
                // 未保存のタブも含めてセッションへ退避し、次回起動時に復元するので確認なしで閉じる
                self.save_session_now();
            } else if self.any_unsaved_changes() && !self.allow_immediate_close {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.pending_action = Some(PendingAction::CloseWindow);
            }
        }

        self.handle_shortcuts(&ctx);

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

        self.draw_template_dialog(&ctx);

        self.draw_unsaved_changes_dialog(&ctx);

        self.draw_main_toolbar(ui, &ctx);

        // ページ・基本書式設定GUIバー（フロントマターと双方向同期）
        if self.show_page_settings_bar {
            self.draw_page_settings_bar(ui);
        }

        // 下部ステータスバー
        self.draw_status_bar(ui);

        // 最大化中は、プレビューが全幅を取る前にアウトラインを左へ確保しておく（表示/非表示は通常時と共通）
        if self.preview_maximized && self.show_outline_panel {
            self.draw_outline_panel(ui);
        }

        self.draw_preview_panel(ui);

        if self.preview_maximized {
            return;
        }

        // 見出しアウトラインパネル（クリックで該当見出しへジャンプ）
        if self.show_outline_panel && !self.preview_maximized {
            self.draw_outline_panel(ui);
        }

        // 中央（左ペイン）：マークアップエディタ＆クイック挿入バー＆検索置換バー
        egui::CentralPanel::default().show(ui, |ui| {
            self.draw_tab_bar(ui);
            ui.separator();

            self.draw_quick_insert_bar(ui);

            // 検索・置換バー
            if self.show_find_replace {
                ui.separator();
                self.draw_find_replace_bar(ui);
            }

            ui.separator();

            self.draw_editor(ui);
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

    #[test]
    fn test_line_char_range_and_jump_to_source_line() {
        let s = "あいう\r\nえお\n\nか";
        assert_eq!(line_char_range(s, 1), Some((0, 3)));
        assert_eq!(line_char_range(s, 2), Some((5, 7)));
        assert_eq!(line_char_range(s, 3), Some((8, 8)));
        assert_eq!(line_char_range(s, 4), Some((9, 10)));
        assert_eq!(line_char_range(s, 5), None);
        assert_eq!(line_char_range(s, 0), None);

        let mut app = test_app("# 見出し\n\n本文の行\n");
        app.jump_to_source_line(3);
        assert_eq!(app.pending_cursor_selection, Some((7, 11)));
        // 存在しない行へは移動しない
        app.pending_cursor_selection = None;
        app.jump_to_source_line(99);
        assert_eq!(app.pending_cursor_selection, None);
    }

    /// 実際のクリップボードから読んで変換できることの確認（クリップボードを使うため普段は動かさない）。
    /// 事前に Excel でセルをコピーするか、`Set-Clipboard "a`tb"` してから
    /// `cargo test clipboard_table -- --ignored` で実行する
    #[test]
    #[ignore]
    fn clipboard_table_paste_reads_the_system_clipboard() {
        let mut app = test_app("");
        app.paste_table_from_clipboard();
        assert!(
            app.markdown_text.contains("| --- |"),
            "{}",
            app.status_message
        );
    }

    #[test]
    fn test_save_and_open_user_template() {
        let dir = std::env::temp_dir().join(format!("typer-ui-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let original = "---\ntitle: 月次\n---\n# 月次報告\n";
        let mut app = test_app(original);
        app.current_file = Some(PathBuf::from("C:/docs/月次.md"));
        app.save_current_as_template(&dir, "月次報告", false)
            .unwrap();
        assert!(app.status_message.contains("月次報告"));
        // 同じ名前は上書きの確認が要る
        assert!(
            app.save_current_as_template(&dir, "月次報告", false)
                .is_err()
        );

        let template = list_user_templates(&dir).remove(0);
        app.open_user_template(&template);
        // 新しいタブに、ファイル名のない文書として開く（元の文書のタブは残る）
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, 1);
        assert_eq!(app.markdown_text, original);
        assert_eq!(app.current_file, None);
        assert_eq!(
            app.tabs[0].as_ref().unwrap().current_file,
            Some(PathBuf::from("C:/docs/月次.md"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_autosave_targets_cover_dirty_background_tabs() {
        let snapshot = |file: Option<&str>, dirty: bool, text: &str| TabSnapshot {
            markdown_text: text.to_string(),
            current_file: file.map(PathBuf::from),
            is_dirty: dirty,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            cursor_char_range: None,
            last_export_path: None,
        };
        // 表示中のタブ（1番目）は未保存の新規文書で、変更あり
        let mut app = test_app("表示中");
        app.is_dirty = true;
        app.tabs = vec![
            None,
            Some(snapshot(Some("C:/docs/report.md"), true, "裏のファイル")),
            Some(snapshot(None, true, "裏の新規")),
            Some(snapshot(Some("C:/docs/clean.md"), false, "変更なし")),
        ];
        app.active_tab = 0;
        let targets = app.autosave_targets();
        let names: Vec<String> = targets
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        // 新規文書は2つあるので別々の名前になり、変更のないタブは対象外
        assert_eq!(
            names,
            vec![
                "untitled_autosave.md",
                "report.autosave.md",
                "untitled_autosave_2.md"
            ]
        );
        assert_eq!(targets[1].1, "裏のファイル");
        assert_eq!(targets[2].1, "裏の新規");
    }

    #[test]
    fn test_insert_table_from_tsv_at_cursor_and_undo() {
        let mut app = test_app("前\n後\n");
        app.cursor_char_range = Some((2, 2));
        app.insert_table_from_tsv("a\tb\n1\t2\n");
        assert_eq!(
            app.markdown_text,
            "前\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n後\n"
        );
        assert!(app.status_message.contains("1行"));
        app.perform_undo();
        assert_eq!(app.markdown_text, "前\n後\n");

        // 表のデータでなければ原稿は変えない
        app.insert_table_from_tsv("ただの文章");
        assert_eq!(app.markdown_text, "前\n後\n");
        assert!(app.status_message.contains("表のデータがありません"));
    }

    #[test]
    fn test_preview_update_collects_syntax_warnings() {
        let mut app = test_app("# 見出し\n\n{colr: red}\n本文\n");
        app.trigger_preview_update();
        assert_eq!(app.syntax_warnings.len(), 1);
        assert_eq!(app.syntax_warnings[0].line, 3);

        app.markdown_text = "# 見出し\n\n{color: red}\n本文\n".to_string();
        app.trigger_preview_update();
        assert!(app.syntax_warnings.is_empty());
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
            last_line_map: SourceLineMap::default(),
            syntax_warnings: Vec::new(),
            zoom: 1.0,
            preview_maximized: false,
            outline_width: 220.0,
            preview_headings: Vec::new(),
            pending_preview_jump: None,
            scroll_editor_cursor_to_top: false,
            show_typst_source: false,
            status_message: String::new(),
            startup_visibility_ensured: false,
            recent_files: Vec::new(),
            last_autosave_time: Instant::now(),
            last_autosaved_revision: 0,
            template_dialog: None,
            show_outline_panel: false,
            custom_color_hex: String::new(),
            show_font_suggestions: false,
            show_latin_font_suggestions: false,
            last_export_path: None,
            dark_mode: false,
            persist_settings: false,
            tabs: vec![None],
            active_tab: 0,
        }
    }

    #[test]
    fn test_dropping_a_file_opens_it_in_a_new_tab_without_replacing_current() {
        // handle_dropped_files() が .md ファイルに対して行うのと同じ処理列
        // （open_new_tab() → load_file_from_path()）を検証する。
        // egui::Context を要する handle_dropped_files 自体はヘッドレスでは呼べないため、
        // その中核ロジックをここで直接検証する。
        let dir = std::env::temp_dir().join(format!(
            "typer_test_drop_{}_{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dropped.md");
        std::fs::write(&path, "ドロップされた文書の内容").unwrap();

        let mut app = test_app("現在編集中のタブの内容");
        app.is_dirty = true;
        app.current_file = Some(PathBuf::from("current.md"));

        app.open_new_tab();
        app.load_file_from_path(path.clone());

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, 1);
        assert_eq!(app.markdown_text, "ドロップされた文書の内容");
        assert_eq!(app.current_file, Some(path));
        assert!(!app.is_dirty, "読み込み直後は未保存扱いにならない");

        // 退避された元のタブの内容がそのまま保持されていることを確認
        app.switch_to_tab(0);
        assert_eq!(app.markdown_text, "現在編集中のタブの内容");
        assert_eq!(app.current_file, Some(PathBuf::from("current.md")));
        assert!(app.is_dirty);

        let _ = std::fs::remove_dir_all(&dir);
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

    fn heading(text: &str, page: usize, y_pt: f32) -> HeadingPosition {
        HeadingPosition {
            text: text.to_string(),
            page,
            y_pt,
        }
    }

    #[test]
    fn test_find_preview_heading_matches_text_ignoring_markup() {
        let hs = vec![heading("概要", 0, 50.0), heading("太字の見出し", 1, 80.0)];
        assert_eq!(
            find_preview_heading(&hs, 1, "**太字**の見出し", 0),
            Some((1, 80.0))
        );
    }

    #[test]
    fn test_find_preview_heading_picks_nth_duplicate() {
        let hs = vec![
            heading("まとめ", 0, 10.0),
            heading("本文", 0, 40.0),
            heading("まとめ", 2, 30.0),
        ];
        assert_eq!(find_preview_heading(&hs, 2, "まとめ", 1), Some((2, 30.0)));
    }

    #[test]
    fn test_find_preview_heading_falls_back_to_order() {
        let hs = vec![heading("A", 0, 10.0), heading("B", 1, 20.0)];
        assert_eq!(
            find_preview_heading(&hs, 1, "一致しない", 0),
            Some((1, 20.0))
        );
        assert_eq!(find_preview_heading(&hs, 5, "一致しない", 0), None);
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
