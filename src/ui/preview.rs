//! プレビュー：組版ワーカーへの依頼と結果の受け取り、ページ画像の表示、組版エラー・書き方の確認の表示

use super::*;

impl TyperApp {
    pub(super) fn trigger_preview_update(&mut self) {
        let doc = parse_document(&self.markdown_text);
        let base = self.base_dir();
        let (typst_code, line_map) = generate_typst_source_with_map(&doc, base.as_deref());
        self.last_typst_code = typst_code;
        self.last_line_map = line_map;
        self.syntax_warnings = lint_markdown(&self.markdown_text);
        self.is_compiling = true;
        self.pending_render = false;

        let pixel_per_pt = (2.0 * self.zoom).clamp(1.2, 3.5);
        let _ = self.worker_tx.send(WorkerRequest::RenderPreview {
            revision: self.revision,
            typst_code: self.last_typst_code.clone(),
            pixel_per_pt,
        });
    }

    pub(super) fn poll_worker_responses(&mut self, ctx: &egui::Context) {
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
                            self.preview_headings = output.headings;

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
                            // 古い結果は上で捨てているので、この対応表は同じ版の原稿から作ったもの
                            self.last_error = Some(err.describe(Some(&self.last_line_map)));
                        }
                    }
                }
            }
        }
    }

    /// プレビュー上部の組版エラー欄と書き間違いの警告欄を描画する。
    /// 原稿の行が分かるものはクリックでその行へ移動できる
    pub(super) fn draw_message_panel(&mut self, ui: &mut egui::Ui) {
        let mut jump_to: Option<usize> = None;
        self.draw_compile_errors(ui, &mut jump_to);
        self.draw_syntax_warnings(ui, &mut jump_to);
        if let Some(line) = jump_to {
            self.jump_to_source_line(line);
        }
    }

    /// 書き間違いの警告欄（組版は止まらないが、指定が効いていない箇所）
    pub(super) fn draw_syntax_warnings(&self, ui: &mut egui::Ui, jump_to: &mut Option<usize>) {
        const MAX_SHOWN: usize = 5;
        if self.syntax_warnings.is_empty() {
            return;
        }
        let warn_color = Color32::from_rgb(150, 85, 0);
        egui::Frame::NONE
            .fill(Color32::from_rgb(254, 247, 224))
            .inner_margin(8.0)
            .corner_radius(4.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "✎ 書き方の確認（{}件）：次の指定は効いていません",
                        self.syntax_warnings.len()
                    ))
                    .small()
                    .strong()
                    .color(warn_color),
                );
                for w in self.syntax_warnings.iter().take(MAX_SHOWN) {
                    let text = RichText::new(format!("[原稿 {}行目] {}", w.line, w.message))
                        .small()
                        .color(warn_color)
                        .underline();
                    if ui
                        .add(egui::Label::new(text).sense(Sense::click()))
                        .on_hover_text("クリックで原稿のこの行へ移動します")
                        .clicked()
                    {
                        *jump_to = Some(w.line);
                    }
                }
                if self.syntax_warnings.len() > MAX_SHOWN {
                    ui.label(
                        RichText::new(format!(
                            "…ほか {}件（上から順に直すと続きが表示されます）",
                            self.syntax_warnings.len() - MAX_SHOWN
                        ))
                        .small()
                        .color(warn_color),
                    );
                }
            });
    }

    /// 組版エラー欄（組版できず、直前のプレビューのまま）
    pub(super) fn draw_compile_errors(&self, ui: &mut egui::Ui, jump_to: &mut Option<usize>) {
        let Some(errors) = &self.last_error else {
            return;
        };
        let error_color = Color32::from_rgb(197, 34, 31);
        egui::Frame::NONE
            .fill(Color32::from_rgb(253, 232, 230))
            .inner_margin(8.0)
            .corner_radius(4.0)
            .show(ui, |ui| {
                let heading = if self.page_textures.is_empty() {
                    "⚠ 組版エラー（直すとプレビューが表示されます）"
                } else {
                    "⚠ 組版エラー（直前のプレビューを表示しています）"
                };
                ui.label(RichText::new(heading).small().strong().color(error_color));
                for err in errors {
                    let text = RichText::new(&err.text).small().color(error_color);
                    match err.source_line {
                        Some(line) => {
                            if ui
                                .add(egui::Label::new(text.underline()).sense(Sense::click()))
                                .on_hover_text("クリックで原稿のこの行へ移動します")
                                .clicked()
                            {
                                *jump_to = Some(line);
                            }
                        }
                        None => {
                            ui.label(text);
                        }
                    }
                }
            });
    }

    /// 右ペインのページプレビュー（最大化中はウィンドウ全幅）と生成Typstコードの表示
    pub(super) fn draw_preview_panel(&mut self, ui: &mut egui::Ui) {
        // 右ペイン：ページプレビュー領域（最大化中はウィンドウ全幅を占有し、エディタは描かない）。
        // 最大化時は別IDのパネルにして、通常時にユーザーが調整した幅を上書きしないようにする
        let preview_panel = if self.preview_maximized {
            egui::Panel::right("preview_panel_maximized")
                .resizable(false)
                .exact_size(ui.available_width())
        } else {
            egui::Panel::right("preview_panel")
                .resizable(true)
                .default_size(580.0)
                .size_range(320.0..=1200.0)
        };
        preview_panel.show(ui, |ui| {
            self.draw_message_panel(ui);

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

                                for (idx, (tex, base_size)) in self.page_textures.iter().enumerate()
                                {
                                    let display_size = *base_size * self.zoom * 0.85;

                                    let (rect, _) =
                                        ui.allocate_exact_size(display_size, Sense::hover());
                                    if let Some((page, y_pt)) = self.pending_preview_jump
                                        && page == idx
                                    {
                                        // base_size はpt単位なので、表示倍率を掛ければ画面上の位置になる
                                        let y = rect.top() + y_pt * self.zoom * 0.85;
                                        let target = egui::Rect::from_min_max(
                                            egui::pos2(rect.left(), y - 12.0),
                                            egui::pos2(rect.right(), y),
                                        );
                                        ui.scroll_to_rect(target, Some(egui::Align::Min));
                                        self.pending_preview_jump = None;
                                    }
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
    }
}
