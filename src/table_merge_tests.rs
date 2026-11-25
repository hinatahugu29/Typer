//! 表のセル結合（`<` / `^`）と列幅を、原稿の解析から3形式の出力まで通して確かめるテスト

use crate::model::{Block, CellMerge, TableMerges};
use crate::parser::parse_document;
use std::io::Cursor;

/// 承認欄の例：見出しは「申請 | < | 承認」、本文は縦に結合した欄を含む
const APPROVAL: &str = "{widths: 20%, 40%, 40mm}\n| 区分 | 申請 | < |\n| :-- | :-: | :-: |\n| 担当 | 山田 | 印 |\n| ^ | 佐藤 | ^ |\n| 備考 | 特になし | < |\n";

fn merges_of(src: &str) -> TableMerges {
    match &parse_document(src).blocks[0] {
        Block::Table { merges, .. } => merges.clone(),
        other => panic!("expected table, got {other:?}"),
    }
}

fn docx_document(src: &str) -> String {
    let bytes = crate::docx_export::generate_docx_bytes_with_base(&parse_document(src), None)
        .expect("docx generation");
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut file = archive.by_name("word/document.xml").unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut file, &mut s).unwrap();
    s
}

#[test]
fn parser_reads_merge_markers_but_not_escaped_ones() {
    use CellMerge::{Left as L, None as N, Up as U};
    let merges = merges_of(APPROVAL);
    assert_eq!(merges.header, vec![N, N, L]);
    assert_eq!(
        merges.rows,
        vec![vec![N, N, N], vec![U, N, U], vec![N, N, L]]
    );

    // `\<` `\^` は記号として表示し、結合しない
    let escaped = parse_document("| a | b |\n| - | - |\n| \\< | \\^ |\n");
    match &escaped.blocks[0] {
        Block::Table { merges, rows, .. } => {
            assert_eq!(merges.rows, vec![vec![N, N]]);
            assert_eq!(rows[0][0], vec![crate::model::Inline::Text("<".into())]);
        }
        other => panic!("expected table, got {other:?}"),
    }
}

#[test]
fn typst_uses_table_cell_spans_and_compiles() {
    let doc = parse_document(APPROVAL);
    let code = crate::typst_gen::generate_typst_source(&doc);
    assert!(
        code.contains("table.header([*区分*], table.cell(colspan: 2)[*申請*], )"),
        "{code}"
    );
    assert!(
        code.contains("table.cell(rowspan: 2)[担当], [山田], table.cell(rowspan: 2)[印], "),
        "{code}"
    );
    assert!(code.contains("[佐藤], \n"), "{code}");
    assert!(
        code.contains("[備考], table.cell(colspan: 2)[特になし], "),
        "{code}"
    );
    crate::typst_engine::TypstEngine::new()
        .export_pdf(&code)
        .expect("merged table should compile");
}

#[test]
fn typst_combines_spans_with_cell_fill() {
    let code = crate::typst_gen::generate_typst_source(&parse_document(
        "| a | b |\n| - | - |\n| [塗り]{bg: #fff2cc} | < |\n",
    ));
    assert!(
        code.contains("table.cell(colspan: 2, fill: rgb(\"#fff2cc\"))[塗り]"),
        "{code}"
    );
}

#[test]
fn html_uses_colspan_and_rowspan_and_omits_covered_cells() {
    let html = crate::html_export::generate_html_with_base(&parse_document(APPROVAL), None);
    assert!(html.contains("<th colspan=\"2\""), "{html}");
    assert!(
        html.contains("<td rowspan=\"2\" style=\"text-align: left\">担当</td>"),
        "{html}"
    );
    assert!(
        html.contains("<td rowspan=\"2\" style=\"text-align: center\">印</td>"),
        "{html}"
    );
    assert!(
        html.contains("<td colspan=\"2\" style=\"text-align: center\">特になし</td>"),
        "{html}"
    );
    // 結合された側のセル（`<` `^`）は出力しない
    assert!(!html.contains(">&lt;<") && !html.contains(">^<"), "{html}");
    let second_row = html.split("<tr>").find(|r| r.contains("佐藤")).unwrap();
    assert_eq!(second_row.matches("<td").count(), 1, "{second_row}");
}

#[test]
fn docx_uses_grid_span_and_vertical_merge() {
    let doc = docx_document(APPROVAL);
    // 見出し行：2列ぶんの「申請」
    assert!(doc.contains("<w:gridSpan w:val=\"2\"/>"), "{doc}");
    // 縦の結合：始まりのセルと、次の行の続きのセル（空の段落）
    assert_eq!(
        doc.matches("<w:vMerge w:val=\"restart\"/>").count(),
        2,
        "{doc}"
    );
    assert_eq!(doc.matches("<w:vMerge/>").count(), 2, "{doc}");
    // 各行のセル数：見出し2（区分・申請×2）、担当行3、佐藤の行3（続き2つ＋佐藤）、備考行2
    let rows: Vec<&str> = doc.split("<w:tr>").skip(1).collect();
    let cell_counts: Vec<usize> = rows.iter().map(|r| r.matches("<w:tc>").count()).collect();
    assert_eq!(cell_counts, vec![2, 3, 3, 2], "{doc}");
    assert!(!doc.contains(">&lt;<") && !doc.contains(">^<"), "{doc}");
}

#[test]
fn docx_column_widths_follow_the_widths_attribute() {
    let doc = docx_document(APPROVAL);
    // A4縦・余白20mm → 本文幅 9638 twip。40mm ≒ 2268 twip を固定し、残り 7370 を 20:40 で分ける
    assert!(
        doc.contains("<w:tblW w:w=\"9638\" w:type=\"dxa\"/>"),
        "{doc}"
    );
    assert!(
        doc.contains("<w:tblGrid><w:gridCol w:w=\"2457\"/><w:gridCol w:w=\"4913\"/><w:gridCol w:w=\"2268\"/></w:tblGrid>"),
        "{doc}"
    );
    assert!(doc.contains("<w:tblLayout w:type=\"fixed\"/>"));
    // 指定がなければ等分し、幅は固定しない
    let plain = docx_document("| a | b |\n| - | - |\n| 1 | 2 |\n");
    assert!(
        plain.contains("<w:gridCol w:w=\"4819\"/><w:gridCol w:w=\"4819\"/>"),
        "{plain}"
    );
    assert!(!plain.contains("tblLayout"));
}

#[test]
fn lint_reports_merge_markers_without_a_target() {
    let src = "前置き\n\n| < | a |\n| - | - |\n| ^ | b |\n| x | y |\n| < | ^ |\n";
    let warnings = crate::lint::lint_markdown(src);
    let lines: Vec<usize> = warnings.iter().map(|w| w.line).collect();
    // 見出し行の左端の `<`（3行目）、見出し直後の `^`（5行目）、左端の `<`（7行目）。
    // 7行目の2列目の `^` は上の「y」と結合できるので警告しない
    assert_eq!(lines, vec![3, 5, 7], "{warnings:?}");
    assert!(warnings[0].message.contains("「<」"));
    assert!(warnings[1].message.contains("「^」"));
    assert!(crate::lint::lint_markdown(APPROVAL).is_empty());
}
