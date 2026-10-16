//! デスクトップUI層 (`egui` / `eframe`)
//!
//! - 左ペイン：独自拡張構文のシンタックスハイライト付きテキストエディタ＋クイック挿入バー
//! - 右ペイン：Typst組版結果のリアルタイムページプレビュー（ズーム・複数ページスクロール対応）
//! - バックグラウンドワーカースレッドによる非同期組版（エディタのキー入力を一切ブロックしない構成）

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, FontData, FontDefinitions, FontFamily, Key, Modifiers, RichText,
    Sense, Stroke, TextEdit, TextFormat, TextureHandle, TextureOptions, Vec2, text::LayoutJob,
};

use crate::html_export::generate_html;
use crate::parser::parse_document;
use crate::sample::SAMPLE_DOCUMENT;
use crate::typst_engine::{PreviewOutput, TypstEngine};
use crate::typst_gen::generate_typst_source;

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

pub struct TyperApp {
    /// 編集中の原稿テキスト
    markdown_text: String,
    /// 直近生成されたTypstソース（デバッグ・確認表示用）
    last_typst_code: String,
    /// 現在開いているファイルパス
    current_file: Option<PathBuf>,
    /// 未保存の変更があるか
    is_dirty: bool,

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
}

impl TyperApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
                        // キューに後続のリクエストが溜まっていれば最新のものだけを処理する
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
        let typst_code = generate_typst_source(&doc);

        let mut app = Self {
            markdown_text: initial_text,
            last_typst_code: typst_code,
            current_file: None,
            is_dirty: false,
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
        };

        app.trigger_preview_update();
        app
    }

    fn trigger_preview_update(&mut self) {
        let doc = parse_document(&self.markdown_text);
        self.last_typst_code = generate_typst_source(&doc);
        self.is_compiling = true;
        self.pending_render = false;

        // 高DPI環境やズーム時にも文字がくっきり見えるよう 2.0 px/pt を基準にする
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
                                // A4の基本表示サイズ（pt単位に近い論理サイズ）
                                let logical_size = Vec2::new(
                                    page.width as f32 / (2.0 * self.zoom).clamp(1.2, 3.5),
                                    page.height as f32 / (2.0 * self.zoom).clamp(1.2, 3.5),
                                );
                                self.page_textures.push((tex, logical_size));
                            }
                        }
                        Err(err) => {
                            // エラー発生時も直前の有効なプレビューは維持し、エラー内容のみ保持する
                            self.last_error = Some(err);
                        }
                    }
                }
            }
        }
    }

    fn action_new_file(&mut self) {
        self.markdown_text = "---\npaper: A4\norientation: portrait\nmargin: 20mm\nfont-size: 10.5pt\nline-height: 1.5\n---\n\n# 新規報告書タイトル\n\nここに本文を入力してください。\n".to_string();
        self.current_file = None;
        self.is_dirty = false;
        self.revision += 1;
        self.trigger_preview_update();
        self.status_message = "新規文書を作成しました".to_string();
    }

    fn action_open_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Markdown文書 (*.md, *.txt)", &["md", "txt"])
            .pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
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
    }

    fn action_save_file(&mut self, save_as: bool) {
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
                    self.status_message = format!("保存しました: {}", path.display());
                }
                Err(e) => {
                    self.status_message = format!("保存エラー: {e}");
                }
            }
        }
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
            let doc = parse_document(&self.markdown_text);
            let typst_code = generate_typst_source(&doc);
            let mut engine = TypstEngine::new();
            match engine.export_pdf(&typst_code) {
                Ok(pdf_bytes) => match std::fs::write(&path, pdf_bytes) {
                    Ok(()) => {
                        self.status_message = format!("PDFを出力しました: {}", path.display());
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
            let html = generate_html(&doc);
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

    fn insert_snippet(&mut self, snippet: &str) {
        if !self.markdown_text.ends_with('\n') && !self.markdown_text.is_empty() {
            self.markdown_text.push('\n');
        }
        self.markdown_text.push_str(snippet);
        self.is_dirty = true;
        self.revision += 1;
        self.pending_render = true;
        self.last_edit_time = Instant::now();
    }
}

impl eframe::App for TyperApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_worker_responses(&ctx);

        // ショートカットキー処理
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::S)) {
            self.action_save_file(false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::O)) {
            self.action_open_file();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::P)) {
            self.action_export_pdf();
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

        // 上部メインツールバー
        egui::Panel::top("top_toolbar")
            .exact_size(42.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    if ui.button("🗋 新規").on_hover_text("新規文書を作成します").clicked() {
                        self.action_new_file();
                    }
                    if ui
                        .button("📂 開く...")
                        .on_hover_text("Markdownファイルを読み込みます (Ctrl+O)")
                        .clicked()
                    {
                        self.action_open_file();
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
                        .on_hover_text("名前を付けて保存します")
                        .clicked()
                    {
                        self.action_save_file(true);
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

                    ui.checkbox(&mut self.show_typst_source, "Typst変換コード表示");

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("100%").clicked() {
                            self.zoom = 1.0;
                            self.revision += 1;
                            self.trigger_preview_update();
                        }
                        if ui.button("＋").on_hover_text("プレビュー拡大").clicked() {
                            self.zoom = (self.zoom + 0.15).clamp(0.5, 2.5);
                            self.revision += 1;
                            self.trigger_preview_update();
                        }
                        ui.label(format!("{:.0}%", self.zoom * 100.0));
                        if ui.button("－").on_hover_text("プレビュー縮小").clicked() {
                            self.zoom = (self.zoom - 0.15).clamp(0.5, 2.5);
                            self.revision += 1;
                            self.trigger_preview_update();
                        }
                        ui.label(RichText::new("表示倍率:").weak());
                    });
                });
            });

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
                    ui.label(
                        RichText::new(format!("{line_count} 行 / {char_count} 文字")).small(),
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

                // 生成Typstコード確認用折りたたみビュー
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

                // プレビュー背景キャンバス
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

                                        // 用紙のドロップシャドウと枠線を描画
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

        // 中央（左ペイン）：マークアップエディタ＆クイック挿入バー
        egui::CentralPanel::default().show(ui, |ui| {
            // 拡張構文クイック挿入バー
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(RichText::new("拡張書式挿入:").small().weak());

                if ui
                    .small_button("📊 列幅付き表")
                    .on_hover_text("列幅比率を指定した表テンプレートを末尾に挿入します")
                    .clicked()
                {
                    self.insert_snippet(
                        "\n{widths: 25%, 50%, 25%}\n| 項目 | 内容 | 備考 |\n| :--- | :--- | :--- |\n| サンプル | 内容を入力 | 備考 |\n",
                    );
                }
                if ui
                    .small_button("➡ 右寄せ段落")
                    .on_hover_text("日付や署名向けの右寄せ属性を挿入します")
                    .clicked()
                {
                    self.insert_snippet("\n{align: right}\n2026年10月16日\n");
                }
                if ui
                    .small_button("↔ 中央タイトル")
                    .on_hover_text("中央寄せ＋文字サイズ指定の属性を挿入します")
                    .clicked()
                {
                    self.insert_snippet("\n{align: center, size: 15pt}\n**文書タイトル**\n");
                }
                if ui
                    .small_button("↕ 行間指定")
                    .on_hover_text("段落の行間を調整する属性を挿入します")
                    .clicked()
                {
                    self.insert_snippet("\n{line-height: 1.2}\nここに行間を調整したい文章を入力します。\n");
                }
                if ui
                    .small_button("🔴 赤文字")
                    .on_hover_text("インライン赤文字記法を挿入します")
                    .clicked()
                {
                    self.insert_snippet("[重要事項]{red}");
                }
                if ui
                    .small_button("🔵 青文字")
                    .on_hover_text("インライン青文字記法を挿入します")
                    .clicked()
                {
                    self.insert_snippet("[補足事項]{blue}");
                }
                if ui
                    .small_button("📄 改ページ")
                    .on_hover_text("手動改ページ指定を挿入します")
                    .clicked()
                {
                    self.insert_snippet("\n:::pagebreak\n");
                }
            });

            ui.separator();

            // カスタムシンタックスハイライト付きエディタ
            let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                let mut layout_job = highlight_custom_markdown(text.as_str());
                layout_job.wrap.max_width = wrap_width;
                ui.fonts_mut(|f| f.layout_job(layout_job))
            };

            egui::ScrollArea::vertical()
                .id_salt("editor_scroll")
                .show(ui, |ui| {
                    let response = ui.add_sized(
                        ui.available_size(),
                        TextEdit::multiline(&mut self.markdown_text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .lock_focus(true)
                            .layouter(&mut layouter),
                    );

                    if response.changed() {
                        self.is_dirty = true;
                        self.revision += 1;
                        self.pending_render = true;
                        self.last_edit_time = Instant::now();
                    }
                });
        });
    }
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
            // 表の行はパイプ記号とインライン属性を色分けする
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
}

