#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod docx_export;
mod html_export;
mod model;
mod parser;
mod sample;
mod templates;
mod typst_engine;
mod typst_gen;
mod ui;
mod ui_persistence;
mod ui_text;

use eframe::egui;
use ui::TyperApp;

fn main() -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("assets/icon.png は有効なPNGであるはず（ビルド時に同梱）");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Typer — 軽量マークアップ報告書作成アプリ (MVP)")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([800.0, 520.0])
            .with_visible(true)
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "typer",
        native_options,
        Box::new(|cc| Ok(Box::new(TyperApp::new(cc)))),
    )
}
