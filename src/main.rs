#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod html_export;
mod model;
mod parser;
mod sample;
mod typst_engine;
mod typst_gen;
mod ui;

use eframe::egui;
use ui::TyperApp;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Typer — 軽量マークアップ報告書作成アプリ (MVP)")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([800.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "typer",
        native_options,
        Box::new(|cc| Ok(Box::new(TyperApp::new(cc)))),
    )
}
