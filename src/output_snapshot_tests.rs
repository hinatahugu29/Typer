//! 出力の回帰テスト（ロードマップ 5-2）。
//!
//! 確認用の原稿から作る Typst コード・HTML・Word 内部の XML を `tests/snapshots/` に保存しておき、
//! 毎回のテストで今の出力と比べる。リファクタリングで出力が変わっていないことを確かめるためのもの。
//!
//! 出力を意図して変えたときは、保存済みのファイルを作り直してから差分を確認してコミットする：
//! `$env:TYPER_UPDATE_SNAPSHOTS = "1"; cargo test output_snapshot; Remove-Item Env:TYPER_UPDATE_SNAPSHOTS`
//!
//! 環境で変わる部分は比べる前に置き換える：
//! - 原稿フォルダの絶対パス（Typst の画像の仮想パスに16進で入る）→ `<ROOT>`
//! - HTML の data URI（数式SVGはインストール済みフォントで中身が変わる）→ `<data:種類>`
//! - Word の画像ファイル（数式PNGなど）は中身を比べず、ファイル名だけを記録する

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 比べる対象の原稿：（スナップショット名, 原稿, 画像を探す基準フォルダ）
fn cases() -> Vec<(&'static str, String, Option<PathBuf>)> {
    let word_check = root().join("docs/word-check");
    vec![
        (
            "word_check_sample",
            std::fs::read_to_string(word_check.join("sample.md")).unwrap(),
            Some(word_check),
        ),
        (
            "builtin_sample",
            crate::sample::SAMPLE_DOCUMENT.to_string(),
            None,
        ),
    ]
}

/// 絶対パスを `<ROOT>` に置き換え、区切りを `/` にそろえる
fn normalize_path_text(text: &str) -> String {
    let root = root().to_string_lossy().to_string();
    text.replace(&root, "<ROOT>")
        .replace(&root.replace('\\', "/"), "<ROOT>")
        .replace("<ROOT>\\", "<ROOT>/")
}

/// Typst の `__local_img__/<16進>.<拡張子>` を、読めるパスに戻して正規化する
fn normalize_typst(code: &str) -> String {
    let prefix = crate::typst_gen::LOCAL_IMAGE_VPATH_PREFIX;
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some(pos) = rest.find(prefix) {
        out.push_str(&rest[..pos + prefix.len()]);
        rest = &rest[pos + prefix.len()..];
        let hex_len = rest.bytes().take_while(|b| b.is_ascii_hexdigit()).count();
        let bytes: Vec<u8> = (0..hex_len / 2)
            .map(|i| u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let decoded = String::from_utf8_lossy(&bytes).replace('\\', "/");
        out.push_str(&format!("{{{}}}", normalize_path_text(&decoded)));
        rest = &rest[hex_len..];
    }
    out.push_str(rest);
    normalize_path_text(&out)
}

/// `data:<種類>;base64,...` の中身を省く
fn normalize_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(pos) = rest.find("data:") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        match after.find(";base64,") {
            Some(semi) if !after[..semi].contains(|c: char| c.is_whitespace() || c == '"') => {
                out.push_str(&format!("<{}>", &after[..semi]));
                let data = &after[semi + ";base64,".len()..];
                let len = data
                    .bytes()
                    .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
                    .count();
                rest = &data[len..];
            }
            _ => {
                out.push_str("data:");
                rest = &after["data:".len()..];
            }
        }
    }
    out.push_str(rest);
    normalize_path_text(&out)
}

/// Word の XML は1行なので、段落・表などの区切りで改行して差分を読みやすくする
fn split_ooxml(xml: &str) -> String {
    let mut out = xml.to_string();
    for tag in [
        "<w:p>",
        "<w:p ",
        "<w:tbl>",
        "<w:tr>",
        "<w:tr ",
        "<w:sectPr",
        "<w:style ",
        "<w:abstractNum ",
        "<w:num ",
        "<w:footnote ",
        "<Relationship ",
        "<Override ",
    ] {
        out = out.replace(tag, &format!("\n{tag}"));
    }
    out
}

/// docx（zip）の中身を1つのテキストにまとめる。XMLは本文、画像などはファイル名だけ。
fn docx_text(bytes: Vec<u8>) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = String::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let name = file.name().unwrap().to_string();
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let mut s = String::new();
            file.read_to_string(&mut s).unwrap();
            out.push_str(&format!("===== {name} =====\n{}\n", split_ooxml(&s)));
        } else {
            out.push_str(&format!("===== {name} (binary, not compared) =====\n"));
        }
    }
    normalize_path_text(&out)
}

/// 保存済みと比べる。違えば最初に違う行を示して失敗する。
/// `TYPER_UPDATE_SNAPSHOTS=1` のときは保存し直す。
fn check(file_name: &str, actual: &str) -> Option<String> {
    let path = root().join("tests/snapshots").join(file_name);
    let actual = actual.replace("\r\n", "\n");
    if std::env::var_os("TYPER_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return None;
    }
    let expected = match std::fs::read_to_string(&path) {
        Ok(s) => s.replace("\r\n", "\n"),
        Err(_) => {
            return Some(format!(
                "{file_name}: 保存済みのファイルがありません（TYPER_UPDATE_SNAPSHOTS=1 で作成）"
            ));
        }
    };
    if expected == actual {
        return None;
    }
    let (exp_lines, act_lines): (Vec<_>, Vec<_>) =
        (expected.lines().collect(), actual.lines().collect());
    let line = (0..exp_lines.len().max(act_lines.len()))
        .find(|&i| exp_lines.get(i) != act_lines.get(i))
        .unwrap_or(0);
    Some(format!(
        "{file_name}: {} 行目から出力が変わりました\n  保存済み: {}\n  今の出力: {}",
        line + 1,
        exp_lines.get(line).unwrap_or(&"(なし)"),
        act_lines.get(line).unwrap_or(&"(なし)"),
    ))
}

#[test]
fn outputs_match_saved_snapshots() {
    let mut failures = Vec::new();
    for (name, src, base) in cases() {
        let base: Option<&Path> = base.as_deref();
        let doc = crate::parser::parse_document(&src);

        let typst = crate::typst_gen::generate_typst_source_with_base(&doc, base);
        failures.extend(check(&format!("{name}.typ"), &normalize_typst(&typst)));

        let html = crate::html_export::generate_html_with_base(&doc, base);
        failures.extend(check(&format!("{name}.html"), &normalize_html(&html)));

        let docx = crate::docx_export::generate_docx_bytes_with_base(&doc, base).unwrap();
        failures.extend(check(&format!("{name}.docx.txt"), &docx_text(docx)));
    }
    assert!(
        failures.is_empty(),
        "{}\n\n意図した変更なら TYPER_UPDATE_SNAPSHOTS=1 で保存し直し、tests/snapshots の差分を確認してください。",
        failures.join("\n\n")
    );
}

#[test]
fn normalizers_remove_machine_specific_parts() {
    let path = root().join("assets").join("icon.png");
    let vpath = crate::typst_gen::encode_local_image_vpath(&path);
    let typst = normalize_typst(&format!("#image(\"/{vpath}\")"));
    assert!(typst.contains("{<ROOT>/assets/icon.png}"), "{typst}");

    let html = normalize_html("<img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"x\">");
    assert_eq!(html, "<img src=\"<data:image/png>\" alt=\"x\">");
}
