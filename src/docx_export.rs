//! 内部文書モデル (`Document`) からMicrosoft Word (.docx) バイナリへの変換器
//!
//! 外部のdocx生成専用クレートに頼らず、最小限の有効なOOXML（Office Open XML）を直接組み立てて
//! `zip` クレートでパッケージングする。見出し・段落・表・リスト・改ページ・目次（Wordのフィールド
//! コードによる自動生成TOC）、PNG/JPEG/GIFのローカル画像埋め込みに対応する。
//! SVG/WebP画像、画像の浮動配置（`float: true`）、実クリック可能なハイパーリンクは
//! 今回も非対応（代替としてテキスト表記で出力する）。

use std::io::{Cursor, Write};
use std::path::Path;

use crate::model::{
    Alignment, Block, Document, DocumentSettings, FontChoice, Inline, InlineStyle, Orientation,
};
use crate::typst_gen::resolve_local_image_path;

/// 埋め込み対象として採用したローカル画像1件分のデータ
struct EmbeddedImage {
    relationship_id: String,
    file_name: String,
    content_type: &'static str,
    bytes: Vec<u8>,
}

/// 文書中の画像を集め、Word文書パッケージへ埋め込むための情報を蓄積するコンテキスト
#[derive(Default)]
struct DocxImageCollector {
    images: Vec<EmbeddedImage>,
    /// 数式を本文と同じフォント・文字サイズで画像化するために使う
    settings: DocumentSettings,
}

impl DocxImageCollector {
    fn push_image(&mut self, content_type: &'static str, ext: &str, bytes: Vec<u8>) -> String {
        let index = self.images.len() + 1;
        let relationship_id = format!("rIdImg{index}");
        self.images.push(EmbeddedImage {
            relationship_id: relationship_id.clone(),
            file_name: format!("image{index}.{ext}"),
            content_type,
            bytes,
        });
        relationship_id
    }

    /// 数式をPNG画像として登録し、その画像を本文に置くランXMLを返す。組版できなければ `None`
    fn register_math(
        &mut self,
        tex: &str,
        display: bool,
        style: &RunStyle,
    ) -> Option<String> {
        let mut settings = self.settings.clone();
        // 見出し・`{size: ...}` の中では、その文字サイズで組む
        if let Some(half_pt) = style.size_half_pt {
            settings.font_size = format!("{}pt", half_pt as f64 / 2.0);
        }
        let color = style.color.as_ref().map(|c| format!("#{c}"));
        let source = crate::typst_gen::math_snippet_source(tex, display, &settings, color.as_deref());
        let math = crate::typst_engine::render_math(&source)?;
        let rel_id = self.push_image("image/png", "png", math.png.clone());
        let delim = if display { "$$" } else { "$" };
        let alt = format!("{delim}{}{delim}", tex.trim());
        // 画像の下端がベースラインに来るため、ベースラインより下にはみ出す分だけ下げる
        let lower_half_pt = (math.depth_pt * 2.0).round() as i64;
        Some(drawing_run_xml(
            &rel_id,
            self.images.len(),
            pt_to_emu(math.width_pt),
            pt_to_emu(math.height_pt),
            &alt,
            (!display && lower_half_pt > 0).then_some(-lower_half_pt),
        ))
    }

    /// ローカル画像を読み込んで登録し、本文から参照するための relationship ID を返す。
    /// 読み込めない画像（リンク切れ・SVG/WebP等の非対応形式・外部URL）は `None` を返す。
    fn register(&mut self, url: &str, base_dir: Option<&Path>) -> Option<(String, u32, u32)> {
        let resolved = resolve_local_image_path(url, base_dir)?;
        let bytes = std::fs::read(&resolved).ok()?;
        let ext = resolved
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let content_type: &'static str = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            _ => return None, // SVG/WebP等はWordのレガシーメディア形式として非対応
        };
        let (px_w, px_h) = probe_image_pixel_size(&bytes, &ext).unwrap_or((400, 300));
        let ext = if ext == "jpg" { "jpeg".to_string() } else { ext };
        let relationship_id = self.push_image(content_type, &ext, bytes);
        Some((relationship_id, px_w, px_h))
    }
}

fn is_display_math_only(content: &[Inline]) -> bool {
    let mut has_display = false;
    for inline in content {
        match inline {
            Inline::Math { display: true, .. } => has_display = true,
            Inline::SoftBreak | Inline::HardBreak => {}
            Inline::Text(t) if t.trim().is_empty() => {}
            _ => return false,
        }
    }
    has_display
}

fn pt_to_emu(pt: f64) -> i64 {
    (pt * 12700.0).round() as i64
}

/// PNG/JPEG/GIFのヘッダーからピクセルサイズを読み取る（ファイル全体のデコードは行わない軽量実装）
fn probe_image_pixel_size(bytes: &[u8], ext: &str) -> Option<(u32, u32)> {
    match ext {
        "png" => {
            if bytes.len() >= 24 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
                let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
                let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
                Some((w, h))
            } else {
                None
            }
        }
        "gif" => {
            if bytes.len() >= 10 && (&bytes[0..3] == b"GIF") {
                let w = u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32;
                let h = u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32;
                Some((w, h))
            } else {
                None
            }
        }
        "jpg" | "jpeg" => probe_jpeg_pixel_size(bytes),
        _ => None,
    }
}

/// JPEGのSOFセグメントを走査して幅・高さを取得する
fn probe_jpeg_pixel_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut pos = 2usize;
    while pos + 4 <= bytes.len() {
        if bytes[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = bytes[pos + 1];
        // SOF0-SOF3, SOF5-SOF7, SOF9-SOF11, SOF13-SOF15 のいずれか（高さ・幅を持つセグメント）
        let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
        let segment_len = u16::from_be_bytes(bytes[pos + 2..pos + 4].try_into().ok()?) as usize;
        if is_sof {
            if pos + 9 > bytes.len() {
                return None;
            }
            let h = u16::from_be_bytes(bytes[pos + 5..pos + 7].try_into().ok()?) as u32;
            let w = u16::from_be_bytes(bytes[pos + 7..pos + 9].try_into().ok()?) as u32;
            return Some((w, h));
        }
        if marker == 0xD8 || marker == 0xD9 {
            pos += 2;
            continue;
        }
        pos += 2 + segment_len;
    }
    None
}

/// 内部文書モデルから .docx ファイルのバイト列を生成する（相対パス画像の解決基準ディレクトリ付き）
pub fn generate_docx_bytes_with_base(doc: &Document, base_dir: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut images = DocxImageCollector {
        settings: doc.settings.clone(),
        ..Default::default()
    };
    let body_xml = render_docx_body(&doc.blocks, base_dir, &mut images);
    let document_xml = wrap_document_xml(&doc, &body_xml);
    build_docx_zip(&document_xml, &styles_xml(doc), &images.images)
}

/// 文書既定フォント（和文=eastAsia、欧文=ascii/hAnsi）を `docDefaults` に定義する
fn styles_xml(doc: &Document) -> String {
    let s = &doc.settings;
    let east_asia = match &s.font_choice {
        FontChoice::Gothic => "Yu Gothic".to_string(),
        FontChoice::Mincho => "Yu Mincho".to_string(),
        FontChoice::Custom(name) if !name.trim().is_empty() => xml_escape(name.trim()),
        FontChoice::Custom(_) => "Yu Gothic".to_string(),
    };
    let latin = match s.latin_font.as_deref().map(str::trim) {
        Some(latin) if !latin.is_empty() => xml_escape(latin),
        _ => east_asia.clone(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:docDefaults>
    <w:rPrDefault>
      <w:rPr>
        <w:rFonts w:ascii="{latin}" w:hAnsi="{latin}" w:eastAsia="{east_asia}" w:cs="{latin}"/>
        <w:lang w:val="en-US" w:eastAsia="ja-JP"/>
      </w:rPr>
    </w:rPrDefault>
  </w:docDefaults>
</w:styles>
"#
    )
}

fn wrap_document_xml(doc: &Document, body_xml: &str) -> String {
    let s = &doc.settings;
    let (page_w, page_h) = match s.orientation {
        Orientation::Portrait => (11906, 16838),  // A4 in twips (縦)
        Orientation::Landscape => (16838, 11906), // A4 in twips (横)
    };
    let margin_twips = length_to_twips(&s.margin, 1134); // 既定20mm相当

    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
{body_xml}
    <w:sectPr>
      <w:pgSz w:w="{page_w}" w:h="{page_h}"/>
      <w:pgMar w:top="{margin_twips}" w:right="{margin_twips}" w:bottom="{margin_twips}" w:left="{margin_twips}"/>    </w:sectPr>
  </w:body>
</w:document>
"#
    )
}

/// mm/cm/pt等の長さ文字列をtwips（1pt=20twips、1inch=1440twips）に概算変換する。
/// 解釈できない場合は `default_twips` を返す。
fn length_to_twips(raw: &str, default_twips: i64) -> i64 {
    let trimmed = raw.trim();
    if let Some(v) = trimmed.strip_suffix("mm") {
        if let Ok(mm) = v.trim().parse::<f64>() {
            return (mm * 56.6929).round() as i64; // 1mm ≈ 56.6929twips
        }
    }
    if let Some(v) = trimmed.strip_suffix("cm") {
        if let Ok(cm) = v.trim().parse::<f64>() {
            return (cm * 566.929).round() as i64;
        }
    }
    if let Some(v) = trimmed.strip_suffix("pt") {
        if let Ok(pt) = v.trim().parse::<f64>() {
            return (pt * 20.0).round() as i64;
        }
    }
    if let Some(v) = trimmed.strip_suffix("em") {
        if let Ok(em) = v.trim().parse::<f64>() {
            return (em * 10.5 * 20.0).round() as i64; // 1em ≈ 本文既定サイズ10.5pt相当
        }
    }
    default_twips
}

/// リスト項目を再帰的にDOCXの段落として出力する（入れ子リスト対応）
/// 深さごとに左インデント（`w:left`）を360twip刻みで増やして階層を表現する
fn render_docx_list_items(
    out: &mut String,
    items: &[crate::model::ListItem],
    ordered: bool,
    start: u64,
    base_dir: Option<&Path>,
    images: &mut DocxImageCollector,
    depth: u32,
) {
    let left = 360 * (depth + 1);
    for (i, item) in items.iter().enumerate() {
        let prefix = if ordered {
            format!("{}. ", start + i as u64)
        } else {
            "• ".to_string()
        };
        out.push_str(&format!(
            "    <w:p><w:pPr><w:ind w:left=\"{left}\"/></w:pPr>"
        ));
        out.push_str(&format!(
            "<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r>",
            xml_escape(&prefix)
        ));
        render_docx_runs(out, &item.content, RunStyle::default(), base_dir, images);
        out.push_str("</w:p>\n");
        for child in &item.children {
            if let Block::List {
                ordered: c_ordered,
                start: c_start,
                items: c_items,
                ..
            } = child
            {
                render_docx_list_items(
                    out, c_items, *c_ordered, *c_start, base_dir, images, depth + 1,
                );
            }
        }
    }
}

fn render_docx_body(
    blocks: &[Block],
    base_dir: Option<&Path>,
    images: &mut DocxImageCollector,
) -> String {
    let mut out = String::new();
    let mut heading_index = 0usize;
    for block in blocks {
        render_docx_block(&mut out, block, &mut heading_index, base_dir, images);
    }
    out
}

fn render_docx_block(
    out: &mut String,
    block: &Block,
    heading_index: &mut usize,
    base_dir: Option<&Path>,
    images: &mut DocxImageCollector,
) {
    match block {
        Block::Heading {
            level,
            content,
            attrs,
        } => {
            let lvl = (*level).clamp(1, 6);
            let outline_lvl = lvl as u32 - 1;
            let sz = match lvl {
                1 => 32,
                2 => 28,
                3 => 24,
                _ => 22,
            };
            let ind = indent_ind_attr(attrs);
            out.push_str(&format!(
                "    <w:p><w:pPr><w:outlineLvl w:val=\"{outline_lvl}\"/><w:spacing w:before=\"240\" w:after=\"120\"/>{ind}</w:pPr>"
            ));
            render_docx_runs(
                out,
                content,
                RunStyle {
                    bold: true,
                    size_half_pt: Some(sz),
                    ..Default::default()
                },
                base_dir,
                images,
            );
            out.push_str("</w:p>\n");
            *heading_index += 1;
        }
        Block::Paragraph { content, attrs } => {
            let ind = indent_ind_attr(attrs);
            // `$$...$$` だけの段落は、PDFと同じく独立した数式として中央に置く
            let jc = if is_display_math_only(content) {
                "<w:jc w:val=\"center\"/>"
            } else {
                ""
            };
            if ind.is_empty() && jc.is_empty() {
                out.push_str("    <w:p>");
            } else {
                out.push_str(&format!("    <w:p><w:pPr>{jc}{ind}</w:pPr>"));
            }
            render_docx_runs(out, content, RunStyle::default(), base_dir, images);
            out.push_str("</w:p>\n");
        }
        Block::List {
            ordered,
            start,
            items,
            ..
        } => {
            render_docx_list_items(out, items, *ordered, *start, base_dir, images, 0);
        }
        Block::Table {
            headers,
            alignments,
            rows,
            attrs,
        } => {
            let col_count = headers
                .len()
                .max(rows.first().map(|r| r.len()).unwrap_or(1))
                .max(1);
            out.push_str("    <w:tbl><w:tblPr><w:tblBorders>");
            for edge in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                out.push_str(&format!(
                    "<w:{edge} w:val=\"single\" w:sz=\"4\" w:color=\"9AA0A6\"/>"
                ));
            }
            out.push_str("</w:tblBorders></w:tblPr><w:tblGrid>");
            for _ in 0..col_count {
                out.push_str("<w:gridCol/>");
            }
            out.push_str("</w:tblGrid>\n");

            let row_padding_twips = attrs
                .row_padding
                .as_deref()
                .map(|v| length_to_twips(v, 120));
            if !headers.is_empty() {
                out.push_str("      <w:tr>\n");
                for i in 0..col_count {
                    let cell = headers.get(i);
                    let bg = attrs.header_bg.as_deref();
                    render_docx_table_cell(
                        out,
                        cell,
                        alignments.get(i).copied().flatten(),
                        true,
                        bg,
                        row_padding_twips,
                        base_dir,
                        images,
                    );
                }
                out.push_str("      </w:tr>\n");
            }
            for row in rows {
                out.push_str("      <w:tr>\n");
                for i in 0..col_count {
                    let cell = row.get(i);
                    render_docx_table_cell(
                        out,
                        cell,
                        alignments.get(i).copied().flatten(),
                        false,
                        None,
                        row_padding_twips,
                        base_dir,
                        images,
                    );
                }
                out.push_str("      </w:tr>\n");
            }
            out.push_str("    </w:tbl>\n    <w:p/>\n");
        }
        Block::BlockQuote { blocks, .. } => {
            for b in blocks {
                // 引用は左インデントを付けて表現する（罫線囲みの再現は見送り）
                let mut inner = String::new();
                render_docx_block(&mut inner, b, heading_index, base_dir, images);
                out.push_str(&inner.replace("<w:pPr>", "<w:pPr><w:ind w:left=\"720\"/>"));
            }
        }
        Block::CodeBlock { code, .. } => {
            out.push_str("    <w:p><w:pPr><w:shd w:val=\"clear\" w:fill=\"F8F9FA\"/></w:pPr>");
            for (i, line) in code.lines().enumerate() {
                if i > 0 {
                    out.push_str("<w:r><w:br/></w:r>");
                }
                out.push_str(&format!(
                    "<w:r><w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>",
                    xml_escape(line)
                ));
            }
            out.push_str("</w:p>\n");
        }
        Block::RawTypst { .. } => {
            out.push_str("    <w:p><w:r><w:rPr><w:i/><w:color w:val=\"5F6368\"/></w:rPr><w:t xml:space=\"preserve\">[Typstコードブロック：Word出力では評価されないため省略されました]</w:t></w:r></w:p>\n");
        }
        Block::Spacer { .. } => {
            out.push_str("    <w:p><w:pPr><w:spacing w:before=\"200\"/></w:pPr></w:p>\n");
        }
        Block::HorizontalRule => {
            out.push_str("    <w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:color=\"BDC1C6\"/></w:pBdr></w:pPr></w:p>\n");
        }
        Block::PageBreak => {
            out.push_str("    <w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>\n");
        }
        Block::TableOfContents => {
            out.push_str(
                "    <w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> TOC \\o \"1-3\" \\h \\z \\u </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t xml:space=\"preserve\">目次（Wordで右クリック→「フィールド更新」、またはF9キーで表示されます）</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>\n",
            );
        }
        Block::Columns { blocks, .. } => {
            // Word出力は段組みを再現できないため、範囲内のブロックを通常の縦積みとして展開する
            for b in blocks {
                render_docx_block(out, b, heading_index, base_dir, images);
            }
        }
    }
}

/// `attrs` の `indent_left` / `indent_right` から `<w:ind .../>` 断片を生成する（指定がなければ空文字列）
fn indent_ind_attr(attrs: &crate::model::BlockAttributes) -> String {
    let left = attrs
        .indent_left
        .as_deref()
        .map(|v| length_to_twips(v, 0));
    let right = attrs
        .indent_right
        .as_deref()
        .map(|v| length_to_twips(v, 0));
    if left.is_none() && right.is_none() {
        return String::new();
    }
    let mut s = String::from("<w:ind");
    if let Some(l) = left {
        s.push_str(&format!(" w:left=\"{l}\""));
    }
    if let Some(r) = right {
        s.push_str(&format!(" w:right=\"{r}\""));
    }
    s.push_str("/>");
    s
}

const DEFAULT_CELL_PADDING_TWIPS: i64 = 80;

fn render_docx_table_cell(
    out: &mut String,
    cell: Option<&Vec<Inline>>,
    align: Option<Alignment>,
    is_header: bool,
    header_bg: Option<&str>,
    row_padding_twips: Option<i64>,
    base_dir: Option<&Path>,
    images: &mut DocxImageCollector,
) {
    out.push_str("        <w:tc><w:tcPr>");
    if let Some(bg) = header_bg {
        let safe = sanitize_hex(bg);
        out.push_str(&format!("<w:shd w:val=\"clear\" w:fill=\"{safe}\"/>"));
    }
    // Wordのセルは既定で上下の余白が0のため、背の高い数式画像などが罫線に接してしまう。
    // PDF/HTMLの表と同じく、指定がなくても少し（4pt）空ける
    let pad = row_padding_twips.unwrap_or(DEFAULT_CELL_PADDING_TWIPS);
    out.push_str(&format!(
        "<w:tcMar><w:top w:w=\"{pad}\" w:type=\"dxa\"/><w:bottom w:w=\"{pad}\" w:type=\"dxa\"/></w:tcMar>"
    ));
    out.push_str("</w:tcPr>\n          <w:p>");
    let jc = match align {
        Some(Alignment::Center) => "<w:pPr><w:jc w:val=\"center\"/></w:pPr>",
        Some(Alignment::Right) => "<w:pPr><w:jc w:val=\"right\"/></w:pPr>",
        _ => "",
    };
    out.push_str(jc);
    let base_style = RunStyle {
        bold: is_header,
        ..Default::default()
    };
    if let Some(content) = cell {
        render_docx_runs(out, content, base_style, base_dir, images);
    }
    out.push_str("</w:p>\n        </w:tc>\n");
}

#[derive(Clone, Default)]
struct RunStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    monospace: bool,
    color: Option<String>,
    size_half_pt: Option<u32>,
}

enum RunItem {
    Text(String, RunStyle),
    Break,
    Drawing(String),
}

fn render_docx_runs(
    out: &mut String,
    inlines: &[Inline],
    base_style: RunStyle,
    base_dir: Option<&Path>,
    images: &mut DocxImageCollector,
) {
    let mut items = Vec::new();
    collect_runs(inlines, base_style, base_dir, images, &mut items);
    for item in items {
        match item {
            RunItem::Break => out.push_str("<w:r><w:br/></w:r>"),
            RunItem::Drawing(xml) => out.push_str(&xml),
            RunItem::Text(text, style) => {
                if text.is_empty() {
                    continue;
                }
                out.push_str("<w:r><w:rPr>");
                if style.bold {
                    out.push_str("<w:b/>");
                }
                if style.italic {
                    out.push_str("<w:i/>");
                }
                if style.underline {
                    out.push_str("<w:u w:val=\"single\"/>");
                }
                if style.strikethrough {
                    out.push_str("<w:strike/>");
                }
                if style.monospace {
                    out.push_str("<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/>");
                }
                if let Some(color) = &style.color {
                    out.push_str(&format!("<w:color w:val=\"{}\"/>", sanitize_hex(color)));
                }
                if let Some(sz) = style.size_half_pt {
                    out.push_str(&format!("<w:sz w:val=\"{sz}\"/>"));
                }
                out.push_str("</w:rPr><w:t xml:space=\"preserve\">");
                out.push_str(&xml_escape(&text));
                out.push_str("</w:t></w:r>");
            }
        }
    }
}

fn collect_runs(
    inlines: &[Inline],
    style: RunStyle,
    base_dir: Option<&Path>,
    images: &mut DocxImageCollector,
    out: &mut Vec<RunItem>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push(RunItem::Text(t.clone(), style.clone())),
            // Wordの数式（OMML）ではなく、PDFと同じTypstで組んだPNG画像として埋め込む
            Inline::Math { tex, display } => match images.register_math(tex, *display, &style) {
                Some(xml) => out.push(RunItem::Drawing(xml)),
                None => {
                    // 組版できなかった数式は、元の記法のまま等幅で出す
                    let delim = if *display { "$$" } else { "$" };
                    let mut s = style.clone();
                    s.monospace = true;
                    out.push(RunItem::Text(format!("{delim}{tex}{delim}"), s));
                }
            },
            Inline::Code(t) => {
                let mut s = style.clone();
                s.monospace = true;
                out.push(RunItem::Text(t.clone(), s));
            }
            Inline::Strong(c) => {
                let mut s = style.clone();
                s.bold = true;
                collect_runs(c, s, base_dir, images, out);
            }
            Inline::Emphasis(c) => {
                let mut s = style.clone();
                s.italic = true;
                collect_runs(c, s, base_dir, images, out);
            }
            Inline::Strikethrough(c) => {
                let mut s = style.clone();
                s.strikethrough = true;
                collect_runs(c, s, base_dir, images, out);
            }
            Inline::Styled { content, style: st } => {
                let mut s = style.clone();
                apply_inline_style(&mut s, st);
                collect_runs(content, s, base_dir, images, out);
            }
            Inline::Link { text, url: _ } => {
                // 実クリック可能なハイパーリンクは今回のv1では非対応のため、装飾のみ再現する
                let mut s = style.clone();
                s.underline = true;
                s.color = Some("1A73E8".to_string());
                collect_runs(text, s, base_dir, images, out);
            }
            Inline::Image { alt, url, width, .. } => {
                // floatによる絶対配置はWord出力では再現しないため、通常の本文内画像として埋め込む
                match images.register(url, base_dir) {
                    Some((rel_id, px_w, px_h)) => {
                        out.push(RunItem::Drawing(render_docx_image_drawing(
                            &rel_id,
                            images.images.len(),
                            px_w,
                            px_h,
                            width.as_deref(),
                            alt,
                        )));
                    }
                    None => {
                        out.push(RunItem::Text(format!("[画像: {alt}]"), style.clone()));
                    }
                }
            }
            Inline::SoftBreak => out.push(RunItem::Text(" ".to_string(), style.clone())),
            Inline::HardBreak => out.push(RunItem::Break),
        }
    }
}

/// 画像1枚分の `<w:drawing>` を含むランXMLを生成する。
/// `width_attr`（`{width: 50%}` 等）が指定されていればページ幅に対する比率として解釈し、
/// 指定がなければ元画像のピクセルサイズをそのまま96dpi換算で採用する（過度に大きい場合は既定最大幅に収める）。
fn render_docx_image_drawing(
    rel_id: &str,
    drawing_id: usize,
    px_w: u32,
    px_h: u32,
    width_attr: Option<&str>,
    alt: &str,
) -> String {
    const EMU_PER_PX_AT_96DPI: f64 = 9525.0;
    const DEFAULT_CONTENT_WIDTH_EMU: f64 = 6_000_000.0; // A4縦・余白20mm相当の目安本文幅
    const MAX_WIDTH_EMU: f64 = DEFAULT_CONTENT_WIDTH_EMU;

    let aspect = if px_w > 0 {
        px_h as f64 / px_w as f64
    } else {
        0.75
    };

    let width_emu = if let Some(pct) = width_attr
        .and_then(|w| w.trim().strip_suffix('%'))
        .and_then(|v| v.trim().parse::<f64>().ok())
    {
        DEFAULT_CONTENT_WIDTH_EMU * (pct / 100.0)
    } else {
        (px_w as f64 * EMU_PER_PX_AT_96DPI).min(MAX_WIDTH_EMU)
    };
    let height_emu = width_emu * aspect;
    let (width_emu, height_emu) = (width_emu.round() as i64, height_emu.round() as i64);
    drawing_run_xml(rel_id, drawing_id, width_emu, height_emu, alt, None)
}

/// 画像1枚を置くランXML。`drawing_id` は文書内で一意な番号（重複するとWordが修復を求めることがある）。
/// `position_half_pt` を指定すると、ランをその分だけ上下にずらす（負で下げる）
fn drawing_run_xml(
    rel_id: &str,
    drawing_id: usize,
    width_emu: i64,
    height_emu: i64,
    alt: &str,
    position_half_pt: Option<i64>,
) -> String {
    let escaped_alt = xml_escape(alt);
    let run_props = position_half_pt
        .map(|p| format!("<w:rPr><w:position w:val=\"{p}\"/></w:rPr>"))
        .unwrap_or_default();
    format!(
        "<w:r>{run_props}<w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" \
xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\">\
<wp:extent cx=\"{width_emu}\" cy=\"{height_emu}\"/>\
<wp:docPr id=\"{drawing_id}\" name=\"{escaped_alt}\" descr=\"{escaped_alt}\"/>\
<a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">\
<a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<pic:nvPicPr><pic:cNvPr id=\"{drawing_id}\" name=\"{escaped_alt}\"/><pic:cNvPicPr/></pic:nvPicPr>\
<pic:blipFill><a:blip r:embed=\"{rel_id}\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"/>\
<a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{width_emu}\" cy=\"{height_emu}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
    )
}

fn apply_inline_style(target: &mut RunStyle, style: &InlineStyle) {
    if let Some(color) = &style.color {
        target.color = Some(sanitize_hex(color));
    }
    if style.underline {
        target.underline = true;
    }
    if let Some(size) = &style.size {
        if let Some(pt) = parse_pt_size(size) {
            target.size_half_pt = Some((pt * 2.0).round() as u32);
        }
    }
    // 背景ハイライト（{bg:...}）はdocxの定型ハイライト色パレットに収まらないため、
    // v1では反映を見送る（本文の可読性に影響しないテキスト色・下線等を優先）
}

fn parse_pt_size(raw: &str) -> Option<f64> {
    raw.trim().strip_suffix("pt")?.trim().parse::<f64>().ok()
}

fn sanitize_hex(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('#');
    if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        trimmed.to_ascii_uppercase()
    } else if trimmed.len() == 3 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        trimmed
            .chars()
            .flat_map(|c| [c, c])
            .collect::<String>()
            .to_ascii_uppercase()
    } else {
        "C5221F".to_string()
    }
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

const ROOT_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>
"#;

fn content_types_xml(images: &[EmbeddedImage]) -> String {
    let mut extensions: Vec<&str> = Vec::new();
    for img in images {
        let ext = match img.content_type {
            "image/png" => "png",
            "image/jpeg" => "jpeg",
            "image/gif" => "gif",
            _ => continue,
        };
        if !extensions.contains(&ext) {
            extensions.push(ext);
        }
    }
    let image_defaults: String = extensions
        .iter()
        .map(|ext| {
            let content_type = match *ext {
                "png" => "image/png",
                "gif" => "image/gif",
                _ => "image/jpeg",
            };
            format!(r#"  <Default Extension="{ext}" ContentType="{content_type}"/>"#)
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
{image_defaults}
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>
"#
    )
}

fn document_rels_xml(images: &[EmbeddedImage]) -> String {
    let mut relationships: Vec<String> = images
        .iter()
        .map(|img| {
            format!(
                r#"  <Relationship Id="{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/{}"/>"#,
                img.relationship_id, img.file_name
            )
        })
        .collect();
    relationships.push(
        r#"  <Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#
            .to_string(),
    );
    let relationships = relationships.join("\n");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
{relationships}
</Relationships>
"#
    )
}

fn build_docx_zip(
    document_xml: &str,
    styles_xml: &str,
    images: &[EmbeddedImage],
) -> Result<Vec<u8>, String> {
    let buf = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let options: zip::write::FileOptions<'_, '_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("[Content_Types].xml", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(content_types_xml(images).as_bytes())
        .map_err(|e| e.to_string())?;

    zip.start_file("_rels/.rels", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(ROOT_RELS_XML.as_bytes())
        .map_err(|e| e.to_string())?;

    zip.start_file("word/document.xml", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(document_xml.as_bytes())
        .map_err(|e| e.to_string())?;

    zip.start_file("word/styles.xml", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(styles_xml.as_bytes())
        .map_err(|e| e.to_string())?;

    zip.start_file("word/_rels/document.xml.rels", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(document_rels_xml(images).as_bytes())
        .map_err(|e| e.to_string())?;

    for img in images {
        zip.start_file(format!("word/media/{}", img.file_name), options)
            .map_err(|e| e.to_string())?;
        zip.write_all(&img.bytes).map_err(|e| e.to_string())?;
    }

    let cursor = zip.finish().map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_document;

    fn read_docx_part(doc: &Document, part: &str) -> String {
        let bytes = generate_docx_bytes_with_base(doc, None).expect("docx generation should succeed");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("valid zip archive");
        let mut file = archive.by_name(part).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut file, &mut s).unwrap();
        s
    }

    #[test]
    fn test_custom_font_choice_empty_name_falls_back_to_yu_gothic() {
        let mut doc = parse_document("本文\n");
        doc.settings.font_choice = crate::model::FontChoice::Custom(String::new());
        let styles = read_docx_part(&doc, "word/styles.xml");
        assert!(styles.contains(r#"w:eastAsia="Yu Gothic""#));
    }

    #[test]
    fn test_custom_font_choice_is_xml_escaped_in_document_defaults() {
        let mut doc = parse_document("本文\n");
        doc.settings.font_choice =
            crate::model::FontChoice::Custom(r#"Evil"&<font>"#.to_string());
        let styles = read_docx_part(&doc, "word/styles.xml");
        assert!(styles.contains("Evil&quot;&amp;&lt;font&gt;"));
        assert!(!styles.contains(r#"Evil"&<font>"#));
    }

    #[test]
    fn test_math_is_embedded_as_png_with_unique_drawing_ids() {
        let doc = parse_document("文中 $x^2$ と $\\frac{1}{2}$\n\n$$\n\\sqrt{2}\n$$\n");
        let bytes = generate_docx_bytes_with_base(&doc, None).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        for i in 1..=3 {
            assert!(names.contains(&format!("word/media/image{i}.png")), "{names:?}");
        }
        let document = read_docx_part(&doc, "word/document.xml");
        assert!(document.contains(r#"descr="$x^2$""#));
        assert!(document.contains(r#"<w:position w:val="-"#), "inline math is lowered to the baseline");
        assert!(document.contains(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:drawing>"#));
        for id in 1..=3 {
            assert_eq!(document.matches(&format!(r#"<wp:docPr id="{id}""#)).count(), 1);
        }
        assert!(!document.contains(r#"<wp:docPr id="0""#));
    }

    #[test]
    fn test_latin_font_maps_to_ascii_and_japanese_font_to_east_asia() {
        let doc = parse_document("---\nfont: mincho\nfont-latin: Times New Roman\n---\n\n本文\n");
        let styles = read_docx_part(&doc, "word/styles.xml");
        assert!(styles.contains(
            r#"w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:eastAsia="Yu Mincho""#
        ));
        let rels = read_docx_part(&doc, "word/_rels/document.xml.rels");
        assert!(rels.contains(r#"Target="styles.xml""#));
    }

    #[test]
    fn test_generate_docx_bytes_is_a_valid_zip_with_required_parts() {
        let doc =
            parse_document("# 見出し\n\n本文です。\n\n| A | B |\n| :--- | :--- |\n| 1 | 2 |\n");
        let bytes = generate_docx_bytes_with_base(&doc, None).expect("docx generation should succeed");
        assert!(
            bytes.starts_with(b"PK"),
            "docx must be a zip (PK magic bytes)"
        );

        let cursor = Cursor::new(bytes);
        let mut archive =
            zip::ZipArchive::new(cursor).expect("generated bytes should be a valid zip archive");
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        assert!(names.contains(&"[Content_Types].xml".to_string()));
        assert!(names.contains(&"_rels/.rels".to_string()));
        assert!(names.contains(&"word/document.xml".to_string()));

        let mut doc_xml_file = archive.by_name("word/document.xml").unwrap();
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut doc_xml_file, &mut contents).unwrap();
        assert!(contents.contains("見出し"));
        assert!(contents.contains("本文です"));
    }

    #[test]
    fn test_docx_bold_italic_underline_runs_are_distinct() {
        let doc = parse_document("**太字** *斜体* [下線]{underline}\n");
        let mut out = String::new();
        let mut idx = 0usize;
        let mut images = DocxImageCollector::default();
        render_docx_block(&mut out, &doc.blocks[0], &mut idx, None, &mut images);
        assert!(out.contains("<w:b/>"));
        assert!(out.contains("<w:i/>"));
        assert!(out.contains("<w:u w:val=\"single\"/>"));
    }

    #[test]
    fn test_docx_indent_attributes_emit_ind_twips() {
        let doc = parse_document("{indent-left: 10mm, indent-right: 5mm}\n段落\n");
        let mut out = String::new();
        let mut idx = 0usize;
        let mut images = DocxImageCollector::default();
        render_docx_block(&mut out, &doc.blocks[0], &mut idx, None, &mut images);
        assert!(out.contains("<w:ind w:left=\"567\" w:right=\"283\"/>"));
    }

    #[test]
    fn test_docx_ordered_list_uses_custom_start_number() {
        let doc = parse_document("5. 五番目\n6. 六番目\n");
        let mut out = String::new();
        let mut idx = 0usize;
        let mut images = DocxImageCollector::default();
        render_docx_block(&mut out, &doc.blocks[0], &mut idx, None, &mut images);
        assert!(out.contains(">5. <"));
        assert!(out.contains(">6. <"));
    }

    #[test]
    fn test_generated_xml_parts_are_well_formed_xml() {
        // ZIPとして妥当なだけでなく、各XMLパートがXMLとしてパース可能（整形式）であることまで検証する。
        // 表・リスト・見出し・目次・改ページ・コードブロック・引用・画像・リンクなど
        // 主要なブロック/インライン種別を一通り含む文書で検証する。
        let src = "---\nheader: 社内限定資料\nfooter: 社外秘\n---\n\n\
            # 見出し1\n\n:::toc\n\n## 見出し2\n\n\
            **太字** *斜体* ~~取消線~~ [下線]{underline} [色付き]{red} [リンク](https://example.com) テキスト\\\n改行\n\n\
            5. 五番目\n6. 六番目\n\n\
            {widths: 50%, 50%}\n{header-bg: #fdf0e6}\n| A | B |\n| :--- | :--- |\n| [全体塗り]{bg: #fff2cc} | 2 |\n\n\
            > 引用文\n\n\
            ```\nコードブロック\n複数行\n```\n\n\
            ![画像](x.png)\n\n\
            :::pagebreak\n\n\
            ---\n\n\
            :::space 1em\n\n\
            ```typst\n#line(length: 50%)\n```\n";
        let doc = parse_document(src);
        let bytes = generate_docx_bytes_with_base(&doc, None).expect("docx generation should succeed");

        let cursor = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).unwrap();

        for part in ["[Content_Types].xml", "_rels/.rels", "word/document.xml"] {
            let mut file = archive.by_name(part).unwrap();
            let mut contents = String::new();
            std::io::Read::read_to_string(&mut file, &mut contents).unwrap();
            roxmltree::Document::parse(&contents)
                .unwrap_or_else(|e| panic!("{part} should be well-formed XML: {e}"));
        }
    }

    /// 1x1透過PNGの最小バイト列（テスト用フィクスチャ）
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn test_probe_png_pixel_size_reads_width_and_height() {
        assert_eq!(probe_image_pixel_size(TINY_PNG, "png"), Some((1, 1)));
    }

    #[test]
    fn test_docx_embeds_local_png_as_media_with_relationship() {
        let dir = std::env::temp_dir().join(format!(
            "typer_docx_image_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("pic.png");
        std::fs::write(&img_path, TINY_PNG).unwrap();

        let doc = parse_document("![テスト画像](pic.png)\n");
        let bytes = generate_docx_bytes_with_base(&doc, Some(dir.as_path()))
            .expect("docx generation with image should succeed");

        let cursor = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        assert!(names.contains(&"word/media/image1.png".to_string()));
        assert!(names.contains(&"word/_rels/document.xml.rels".to_string()));

        let mut contents = String::new();
        {
            let mut doc_xml_file = archive.by_name("word/document.xml").unwrap();
            std::io::Read::read_to_string(&mut doc_xml_file, &mut contents).unwrap();
        }
        assert!(contents.contains("<w:drawing>"));
        assert!(contents.contains("r:embed=\"rIdImg1\""));

        let mut rels_contents = String::new();
        {
            let mut rels_file = archive.by_name("word/_rels/document.xml.rels").unwrap();
            std::io::Read::read_to_string(&mut rels_file, &mut rels_contents).unwrap();
        }
        assert!(rels_contents.contains("rIdImg1"));
        assert!(rels_contents.contains("media/image1.png"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_docx_row_padding_attribute_emits_cell_margin_twips() {
        let doc = parse_document("{row-padding: 14pt}\n| A |\n| :--- |\n| 1 |\n");
        let mut out = String::new();
        let mut idx = 0usize;
        let mut images = DocxImageCollector::default();
        render_docx_block(&mut out, &doc.blocks[0], &mut idx, None, &mut images);
        assert!(out.contains("<w:tcMar><w:top w:w=\"280\" w:type=\"dxa\"/><w:bottom w:w=\"280\" w:type=\"dxa\"/></w:tcMar>"));
    }

    #[test]
    fn test_docx_toc_directive_renders_field_code() {
        let doc = parse_document("# 見出し\n\n:::toc\n");
        let bytes = generate_docx_bytes_with_base(&doc, None).unwrap();
        let cursor = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).unwrap();
        let mut doc_xml_file = archive.by_name("word/document.xml").unwrap();
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut doc_xml_file, &mut contents).unwrap();
        assert!(contents.contains("TOC \\o"));
    }
}
