//! `assets/icon.png` から Windows用 `.ico`（マルチサイズ: 16/32/48/256px）を生成する開発用ツール。
//! アイコン画像を差し替えた際は `cargo run --example gen_icon` を再実行してください。

use image::ImageEncoder;

const SIZES: [u32; 4] = [16, 32, 48, 256];

fn main() {
    let src_path = "assets/icon.png";
    let out_path = "assets/icon.ico";

    let img =
        image::open(src_path).unwrap_or_else(|e| panic!("{src_path} を開けませんでした: {e}"));
    println!("読み込み完了: {}x{}", img.width(), img.height());

    let mut png_per_size = Vec::new();
    for &size in &SIZES {
        let resized = img.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
        let rgba = resized.to_rgba8();
        let mut png_bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png_bytes)
            .write_image(&rgba, size, size, image::ExtendedColorType::Rgba8)
            .unwrap();
        png_per_size.push(png_bytes);
    }

    // ICO形式を手組みする（ICONDIR + ICONDIRENTRY[] + PNG形式の画像データを並べるだけの単純な構造）
    let header_len = 6 + 16 * SIZES.len();
    let mut ico = Vec::<u8>::new();
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type: 1 = icon
    ico.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());

    let mut offset = header_len as u32;
    for (i, &size) in SIZES.iter().enumerate() {
        let png_len = png_per_size[i].len() as u32;
        let dim_byte = if size == 256 { 0u8 } else { size as u8 }; // 0は256pxを意味する慣習
        ico.push(dim_byte);
        ico.push(dim_byte);
        ico.push(0); // パレットなし
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // color planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&png_len.to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png_len;
    }
    for png_bytes in &png_per_size {
        ico.extend_from_slice(png_bytes);
    }

    std::fs::write(out_path, &ico)
        .unwrap_or_else(|e| panic!("{out_path} へ書き込めませんでした: {e}"));
    println!(
        "生成完了: {out_path} ({} バイト, {} サイズ収録)",
        ico.len(),
        SIZES.len()
    );
}
