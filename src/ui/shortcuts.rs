//! キーボードショートカット

use super::*;

impl TyperApp {
    /// グローバルショートカットキーと Ctrl+ホイールによる拡大・縮小を処理する
    pub(super) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // グローバルショートカットキー処理
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::S)) {
            self.action_save_file(true);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) {
            self.action_save_file(false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.request_action(PendingAction::OpenFileDialog, ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.request_action(PendingAction::NewFile, ctx);
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
            self.toggle_dark_mode(ctx);
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
        // Excelなどからコピーしたセルを表として貼り付ける（通常の Ctrl+V はそのまま）
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::V)) {
            self.paste_table_from_clipboard();
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
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::N)) {
            self.insert_at_cursor("\n{indent: 20mm}\n");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::I)) {
            self.action_insert_image_dialog();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::B)) {
            self.insert_at_cursor("\n{box}\n> 注意事項などをここに記載します。\n");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::M)) {
            self.insert_at_cursor(
                "\n:::columns 2\nここに段組みにしたい本文を記載します。\n:::columns-end\n\n",
            );
        }

        // タブ操作（Ctrl+Alt系のショートカットより後に置き、誤って奪わないようにする）
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::T)) {
            self.open_new_tab();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::W)) {
            self.request_action(PendingAction::CloseTab, ctx);
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
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F11)) {
            self.preview_maximized = !self.preview_maximized;
        }
        if self.preview_maximized && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.preview_maximized = false;
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
    }
}
