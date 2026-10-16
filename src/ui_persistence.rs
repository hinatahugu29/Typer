//! 設定・履歴の永続化、起動時文書の決定、フォント/テーマのセットアップ
//!
//! `ui.rs` から分離した、`TyperApp` の状態を直接持たない純粋な永続化・初期化ヘルパー群。
//! いずれも `%APPDATA%\Typer\` 配下への読み書きか、`egui::Context` への一度きりの設定適用のみを行う。

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};

use crate::sample::SAMPLE_DOCUMENT;

/// 最近使ったファイル一覧を永続化するテキストファイルのパス（1行1パス、新しい順）
pub(crate) fn recent_files_store_path() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(
        PathBuf::from(appdata)
            .join("Typer")
            .join("recent_files.txt"),
    )
}

/// 起動時に表示する初期文書を決定する：直近に開いていたファイル（先頭）が読み込めればそれを、
/// 読み込めない・1件もない場合はサンプル報告書を返す
pub(crate) fn resolve_startup_document(
    recent_files: &[PathBuf],
) -> (String, Option<PathBuf>, String) {
    recent_files
        .first()
        .and_then(|path| {
            std::fs::read_to_string(path).ok().map(|content| {
                (
                    content,
                    Some(path.clone()),
                    format!("前回のファイルを自動的に開きました: {}", path.display()),
                )
            })
        })
        .unwrap_or_else(|| {
            (
                SAMPLE_DOCUMENT.to_string(),
                None,
                "準備完了 — サンプル報告書を読み込みました".to_string(),
            )
        })
}

/// 保存されている最近使ったファイル一覧を読み込む（既に存在しないファイルは除外する）
pub(crate) fn load_recent_files() -> Vec<PathBuf> {
    let Some(path) = recent_files_store_path() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .take(8)
        .collect()
}

/// 最近使ったファイル一覧をテキストファイルへ保存する（失敗しても致命的ではないため無視する）
pub(crate) fn save_recent_files(files: &[PathBuf]) {
    let Some(path) = recent_files_store_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(&path, content);
}

/// 現在編集中のファイルに対応する自動保存バックアップファイルのパスを決定する。
/// 保存済みファイルの場合は同じフォルダに `.autosave.md` 拡張で置き、
/// 未保存の新規文書の場合はアプリ設定フォルダ内の固定ファイルへ保存する。
pub(crate) fn autosave_path_for(current_file: &Option<PathBuf>) -> Option<PathBuf> {
    match current_file {
        Some(path) => {
            let stem = path.file_stem()?.to_string_lossy().to_string();
            Some(path.with_file_name(format!("{stem}.autosave.md")))
        }
        None => {
            let appdata = std::env::var("APPDATA").ok()?;
            Some(
                PathBuf::from(appdata)
                    .join("Typer")
                    .join("untitled_autosave.md"),
            )
        }
    }
}

/// Windowsの日本語システムフォントを `egui` に登録し、UIおよびエディタでの日本語表示・IME入力を最適化する
pub(crate) fn setup_japanese_fonts(ctx: &egui::Context) {
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

pub(crate) fn setup_visual_theme(ctx: &egui::Context, dark_mode: bool) {
    let mut visuals = if dark_mode {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    if dark_mode {
        visuals.panel_fill = Color32::from_rgb(30, 31, 34);
        visuals.extreme_bg_color = Color32::from_rgb(18, 19, 21);
    } else {
        visuals.panel_fill = Color32::from_rgb(248, 249, 250);
        visuals.extreme_bg_color = Color32::WHITE;
    }
    ctx.set_visuals(visuals);
}

/// ダークモード設定の永続化ファイルのパス（最近使ったファイル一覧と同じ設定フォルダ）
pub(crate) fn dark_mode_store_path() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(appdata).join("Typer").join("dark_mode.txt"))
}

/// 保存されているダークモード設定を読み込む（未設定時は既定でライトモード）
pub(crate) fn load_dark_mode_setting() -> bool {
    let Some(path) = dark_mode_store_path() else {
        return false;
    };
    std::fs::read_to_string(&path)
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// ダークモード設定をテキストファイルへ保存する（失敗しても致命的ではないため無視する）
pub(crate) fn save_dark_mode_setting(enabled: bool) {
    let Some(path) = dark_mode_store_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, if enabled { "1" } else { "0" });
}
