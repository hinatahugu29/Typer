//! ツールバー：上部メインツールバー、タブバー、書式・要素クイック挿入バー

use super::*;

impl TyperApp {
    /// 上部メインツールバー（ファイル操作・表示切り替え・出力・表示倍率）
    pub(super) fn draw_main_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
                        self.request_action(PendingAction::NewFile, ctx);
                    }
                    if ui
                        .button("📂 開く...")
                        .on_hover_text("Markdownファイルを読み込みます (Ctrl+O / ドロップ対応)")
                        .clicked()
                    {
                        self.request_action(PendingAction::OpenFileDialog, ctx);
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
                                    self.request_action(PendingAction::LoadFilePath(path), ctx);
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
                    if ui
                        .selectable_label(self.preview_maximized, "🔍 プレビュー最大化")
                        .on_hover_text("プレビューをウィンドウ全体に広げます。もう一度押すかEscで元に戻ります (F11)")
                        .clicked()
                    {
                        self.preview_maximized = !self.preview_maximized;
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
    }

    /// タブバー（複数文書の切り替え・新規タブ・タブを閉じる）
    pub(super) fn draw_tab_bar(&mut self, ui: &mut egui::Ui) {
        // タブバー（複数文書の切り替え・新規タブ・タブを閉じる）
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;

            let mut switch_to: Option<usize> = None;
            let mut close_index: Option<usize> = None;

            for i in 0..self.tabs.len() {
                let (title, is_dirty) = if i == self.active_tab {
                    (Self::tab_display_title(&self.current_file), self.is_dirty)
                } else {
                    let snap = self.tabs[i].as_ref().expect("非アクティブタブはSomeのはず");
                    (Self::tab_display_title(&snap.current_file), snap.is_dirty)
                };
                let dirty_mark = if is_dirty { "● " } else { "" };
                let label = format!("{dirty_mark}{title}");

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if ui.selectable_label(i == self.active_tab, label).clicked()
                        && i != self.active_tab
                    {
                        switch_to = Some(i);
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("このタブを閉じます")
                        .clicked()
                    {
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
    }

    /// 書式・要素クイック挿入バー（カーソル位置への挿入・選択テキストのラップ・テンプレートメニュー）
    pub(super) fn draw_quick_insert_bar(&mut self, ui: &mut egui::Ui) {
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
                    .small_button("📥 Excelから表")
                    .on_hover_text(
                        "Excelなどでコピーしたセルを、Markdownの表にしてカーソル位置に貼り付けます (Ctrl+Shift+V)",
                    )
                    .clicked()
                {
                    self.paste_table_from_clipboard();
                }
                if ui
                    .small_button("📏 行高さ指定")
                    .on_hover_text("直前の表に行の上下パディング（セル内余白）指定を挿入します")
                    .clicked()
                {
                    self.insert_at_cursor("\n{row-padding: 14pt}\n");
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
                    .small_button("⇥ インデント")
                    .on_hover_text("カーソル位置に段落の左インデント（段下げ）指定を挿入します。左右別指定は {indent-left: ..., indent-right: ...} を使用してください (Ctrl+Alt+N)")
                    .clicked()
                {
                    self.insert_at_cursor("\n{indent: 20mm}\n");
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
                if ui
                    .small_button("📰 段組み")
                    .on_hover_text("カーソル位置に、その範囲だけを段組み表示する :::columns 〜 :::columns-end を挿入します (Ctrl+Alt+M)")
                    .clicked()
                {
                    self.insert_at_cursor(
                        "\n:::columns 2\nここに段組みにしたい本文を記載します。\n:::columns-end\n\n",
                    );
                }

                ui.separator();

                ui.menu_button("📋 テンプレート", |ui| {
                    ui.label(RichText::new("カーソル位置に挿入").small().weak());
                    for (name, body) in TEMPLATES {
                        if ui.button(*name).clicked() {
                            self.insert_at_cursor(body);
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new("自分のテンプレート（新しいタブで開く）").small().weak());
                    let user_templates = user_templates_dir()
                        .map(|dir| list_user_templates(&dir))
                        .unwrap_or_default();
                    if user_templates.is_empty() {
                        ui.label(RichText::new("まだありません").small().weak());
                    }
                    for template in user_templates {
                        ui.horizontal(|ui| {
                            if ui.button(format!("📄 {}", template.name)).clicked() {
                                self.open_user_template(&template);
                                ui.close();
                            }
                            if ui.small_button("✏").on_hover_text("名前を変更").clicked() {
                                self.template_dialog = Some(TemplateDialog::Rename {
                                    name: template.name.clone(),
                                    template: template.clone(),
                                    error: None,
                                });
                                ui.close();
                            }
                            if ui.small_button("🗑").on_hover_text("削除").clicked() {
                                self.template_dialog =
                                    Some(TemplateDialog::Delete { template: template.clone() });
                                ui.close();
                            }
                        });
                    }
                    ui.separator();
                    if ui.button("💾 現在の文書をテンプレートとして保存...").clicked() {
                        let name = self
                            .suggest_file_name()
                            .trim_end_matches(".md")
                            .to_string();
                        self.template_dialog = Some(TemplateDialog::Save {
                            name,
                            overwrite: false,
                            error: None,
                        });
                        ui.close();
                    }
                    if ui.button("📂 テンプレートのフォルダを開く").clicked() {
                        self.open_user_templates_folder();
                        ui.close();
                    }
                })
                .response
                .on_hover_text(
                    "定型の業務文書の挿入と、自分で保存したテンプレートからの新規作成・管理を行います",
                );

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
    }
}
