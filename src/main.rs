#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod docx_export;
mod html_export;
mod lint;
mod math;
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
use eframe::egui_wgpu::{self, wgpu};
use ui::TyperApp;

/// 描画に使うwgpuバックエンドを1つに絞る。
///
/// 既定では Vulkan/DX12/OpenGL/ソフトウェア描画をすべて初期化したうえで1つを選ぶが、
/// Intel内蔵GPU環境ではこの状態だとマウス移動時に画面がちらつくことがあった
/// （`WGPU_BACKEND` で1つに限定すると再現しない）。
/// そのためWindowsでは DX12 のみを使う。`WGPU_BACKEND` が設定されていればそちらを優先する。
fn wgpu_configuration() -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    if cfg!(windows) && wgpu::Backends::from_env().is_none() {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
    }
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

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
        wgpu_options: wgpu_configuration(),
        ..Default::default()
    };

    eframe::run_native(
        "typer",
        native_options,
        Box::new(|cc| Ok(Box::new(TyperApp::new(cc)))),
    )
}
