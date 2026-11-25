//! 内部文書モデル (`Document`) からMicrosoft Word (.docx) バイナリへの変換器
//!
//! 外部のdocx生成専用クレートに頼らず、最小限の有効なOOXML（Office Open XML）を直接組み立てて
//! `zip` クレートでパッケージングする。見出し・段落・表・リスト・改ページ・目次（Wordのフィールド
//! コードによる自動生成TOC）、PNG/JPEG/GIFのローカル画像埋め込み、クリックできるリンク、
//! 文字・セルの背景色に対応する。SVG/WebP画像と画像の浮動配置（`float: true`）は非対応
//! （代替としてテキスト表記・通常の配置で出力する）。文書内の見出しへのリンク（`#...`）は見た目のみ。

use std::io::{Cursor, Write};
use std::path::Path;

use crate::model::{
    Alignment, Block, CellSlot, Document, DocumentSettings, FontChoice, Inline, InlineStyle,
    Orientation, resolve_table_layout,
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
    /// いま描画中のブロックに足す左インデント（引用の中・リスト項目の子ブロックなど。twip）
    indent_twips: i64,
    /// いま描画中のリストの入れ子の深さ（Wordの段落番号のレベル `ilvl` に使う）
    list_depth: u32,
    /// `numbering.xml` に書き出す番号付け定義。リスト1つにつき1件（番号を振り直すため）
    nums: Vec<NumInstance>,
    /// `footnotes.xml` に書き出す脚注の段落内ランXML（出現順。脚注IDは1始まり）
    footnotes: Vec<String>,
    /// 文書内の脚注の通し番号 → Wordの脚注ID（同じ脚注への2回目以降の参照に使う）
    footnote_ids: std::collections::HashMap<usize, usize>,
    /// クリックできるリンクの (relationship ID, リンク先) 。`document.xml.rels` に書き出す
    links: Vec<(String, String)>,
}

/// Wordの段落番号の定義1件分（`<w:num>`）
struct NumInstance {
    ordered: bool,
    level: u32,
    start: u64,
}

impl DocxImageCollector {
    /// リスト1つ分の番号付け定義を登録し、段落から参照する `numId` を返す
    fn add_num(&mut self, ordered: bool, level: u32, start: u64) -> usize {
        self.nums.push(NumInstance {
            ordered,
            level,
            start,
        });
        self.nums.len()
    }

    /// 脚注を登録し、本文から参照する脚注IDを返す
    fn add_footnote(&mut self, content: &[Inline], base_dir: Option<&Path>) -> usize {
        let mut runs = String::new();
        render_docx_runs(
            &mut runs,
            content,
            RunStyle {
                size_half_pt: Some(18),
                ..Default::default()
            },
            base_dir,
            self,
        );
        self.footnotes.push(runs);
        self.footnotes.len()
    }

    /// クリックできるリンクとして登録し、本文から参照する relationship ID を返す。
    /// 文書内の見出しへのリンク（`#...`）や空のリンク先は、Wordではしおりが必要なため登録しない
    fn register_link(&mut self, url: &str) -> Option<String> {
        let url = url.trim();
        if url.is_empty() || url.starts_with('#') {
            return None;
        }
        if let Some((id, _)) = self.links.iter().find(|(_, u)| u == url) {
            return Some(id.clone());
        }
        let id = format!("rIdLink{}", self.links.len() + 1);
        self.links.push((id.clone(), url.to_string()));
        Some(id)
    }

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
    fn register_math(&mut self, tex: &str, display: bool, style: &RunStyle) -> Option<String> {
        let mut settings = self.settings.clone();
        // 見出し・`{size: ...}` の中では、その文字サイズで組む
        if let Some(half_pt) = style.size_half_pt {
            settings.font_size = format!("{}pt", half_pt as f64 / 2.0);
        }
        let color = style.color.as_ref().map(|c| format!("#{c}"));
        let source =
            crate::typst_gen::math_snippet_source(tex, display, &settings, color.as_deref());
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
        let ext = if ext == "jpg" {
            "jpeg".to_string()
        } else {
            ext
        };
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
pub fn generate_docx_bytes_with_base(
    doc: &Document,
    base_dir: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let mut images = DocxImageCollector {
        settings: doc.settings.clone(),
        ..Default::default()
    };
    let body_xml = render_docx_body(&doc.blocks, base_dir, &mut images);
    let document_xml = wrap_document_xml(&doc, &body_xml);
    let header_footer = HeaderFooter {
        header: header_xml(doc),
        footer: footer_xml(doc),
    };
    build_docx_zip(&document_xml, &styles_xml(doc), &images, &header_footer)
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
    // ヘッダー・フッターは用紙の端から余白の半分の位置に置く（本文と重ならないように）
    let header_distance = margin_twips / 2;
    let mut references = String::new();
    if header_xml(doc).is_some() {
        references.push_str("\n      <w:headerReference w:type=\"default\" r:id=\"rIdHeader1\"/>");
    }
    if footer_xml(doc).is_some() {
        references.push_str("\n      <w:footerReference w:type=\"default\" r:id=\"rIdFooter1\"/>");
    }

    // `w:sectPr` の子要素は headerReference → footerReference → pgSz → pgMar の順。
    // `w:pgMar` の header / footer / gutter はスキーマ上の必須属性
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
{body_xml}
    <w:sectPr>{references}
      <w:pgSz w:w="{page_w}" w:h="{page_h}"/>
      <w:pgMar w:top="{margin_twips}" w:right="{margin_twips}" w:bottom="{margin_twips}" w:left="{margin_twips}" w:header="{header_distance}" w:footer="{header_distance}" w:gutter="0"/>
    </w:sectPr>
  </w:body>
</w:document>
"#
    )
}

/// ヘッダー・フッターの文字の書式（PDFと同じ灰色・8pt）
const HEADER_FOOTER_RPR: &str = "<w:rPr><w:color w:val=\"5F6368\"/><w:sz w:val=\"16\"/></w:rPr>";

fn header_footer_text_run(text: &str) -> String {
    format!(
        "<w:r>{HEADER_FOOTER_RPR}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
        xml_escape(text)
    )
}

/// 「現在のページ / 総ページ数」をWordのフィールドで書く（Wordが開いたときに計算する）
fn page_number_runs() -> String {
    format!(
        "<w:fldSimple w:instr=\" PAGE \"><w:r>{HEADER_FOOTER_RPR}<w:t>1</w:t></w:r></w:fldSimple>\
<w:r>{HEADER_FOOTER_RPR}<w:t xml:space=\"preserve\"> / </w:t></w:r>\
<w:fldSimple w:instr=\" NUMPAGES \"><w:r>{HEADER_FOOTER_RPR}<w:t>1</w:t></w:r></w:fldSimple>"
    )
}

/// 全ページ共通のヘッダー（PDFと同じく右寄せ）。指定がなければ `None`
fn header_xml(doc: &Document) -> Option<String> {
    let text = doc.settings.header_text.as_deref()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:pPr><w:jc w:val="right"/></w:pPr>{}</w:p></w:hdr>
"#,
        header_footer_text_run(text)
    ))
}

/// 全ページ共通のフッター。PDFと同じ配置にする：
/// フッター文字列とページ番号の両方 → 文字列を左・番号を右、番号だけ・文字列だけ → 中央。
/// どちらもなければ `None`
fn footer_xml(doc: &Document) -> Option<String> {
    let s = &doc.settings;
    let text = s
        .footer_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let content = match (text, s.show_page_number) {
        (Some(text), true) => {
            // 右端のタブ位置 = 本文の幅（用紙幅 − 左右の余白）
            let page_w = match s.orientation {
                Orientation::Portrait => 11906,
                Orientation::Landscape => 16838,
            };
            let width = page_w - 2 * length_to_twips(&s.margin, 1134);
            format!(
                "<w:pPr><w:tabs><w:tab w:val=\"right\" w:pos=\"{width}\"/></w:tabs></w:pPr>{}<w:r>{HEADER_FOOTER_RPR}<w:tab/></w:r>{}",
                header_footer_text_run(text),
                page_number_runs()
            )
        }
        (Some(text), false) => format!(
            "<w:pPr><w:jc w:val=\"center\"/></w:pPr>{}",
            header_footer_text_run(text)
        ),
        (None, true) => format!(
            "<w:pPr><w:jc w:val=\"center\"/></w:pPr>{}",
            page_number_runs()
        ),
        (None, false) => return None,
    };
    Some(format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p>{content}</w:p></w:ftr>
"#
    ))
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

/// 1段分のリストのインデント幅（twip）。行頭記号・番号はこの幅にぶら下げる
const LIST_INDENT_TWIPS: i64 = 360;

/// リストをWordの箇条書き・段落番号（`numbering.xml`）を使った段落として出力する。
/// 項目の子ブロック（入れ子リスト・続きの段落・コード等）は1段深いインデントで続ける
fn render_docx_list(
    out: &mut String,
    ordered: bool,
    start: u64,
    items: &[crate::model::ListItem],
    heading_index: &mut usize,
    base_dir: Option<&Path>,
    images: &mut DocxImageCollector,
) {
    let level = images.list_depth.min(8);
    let num_id = images.add_num(ordered, level, start);
    let base = images.indent_twips;
    let left = base + LIST_INDENT_TWIPS;
    for item in items {
        match item.checked {
            // チェックリストの項目は段落番号を付けず、枠の文字を行頭に置く
            Some(checked) => {
                let mark = if checked { "☑" } else { "☐" };
                out.push_str(&format!(
                    "    <w:p><w:pPr><w:ind w:left=\"{left}\" w:hanging=\"{LIST_INDENT_TWIPS}\"/></w:pPr>\
<w:r><w:rPr><w:rFonts w:ascii=\"Segoe UI Symbol\" w:hAnsi=\"Segoe UI Symbol\" w:eastAsia=\"Segoe UI Symbol\"/></w:rPr><w:t xml:space=\"preserve\">{mark} </w:t></w:r>"
                ));
            }
            None => {
                out.push_str(&format!(
                    "    <w:p><w:pPr><w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"{num_id}\"/></w:numPr><w:ind w:left=\"{left}\" w:hanging=\"{LIST_INDENT_TWIPS}\"/></w:pPr>"
                ));
            }
        }
        render_docx_runs(out, &item.content, RunStyle::default(), base_dir, images);
        out.push_str("</w:p>\n");
        if !item.children.is_empty() {
            images.indent_twips = left;
            images.list_depth += 1;
            for child in &item.children {
                render_docx_block(out, child, heading_index, base_dir, images);
            }
            images.list_depth -= 1;
            images.indent_twips = base;
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
            let ppr = paragraph_properties(ParagraphProps {
                // PDFの見出し（sticky）と同じく、見出しだけがページ末尾に取り残されないようにする
                keep_next: true,
                spacing: Some("<w:spacing w:before=\"240\" w:after=\"120\"/>"),
                ind: indent_ind_attr(attrs, images.indent_twips),
                align: attrs.align,
                outline_level: Some(outline_lvl),
            });
            out.push_str(&format!("    <w:p>{ppr}"));
            let base = block_run_style(
                attrs,
                RunStyle {
                    bold: true,
                    size_half_pt: Some(sz),
                    ..Default::default()
                },
            );
            render_docx_runs(out, content, base, base_dir, images);
            out.push_str("</w:p>\n");
            *heading_index += 1;
        }
        Block::Paragraph { content, attrs } => {
            // `$$...$$` だけの段落は、PDFと同じく独立した数式として中央に置く
            let align = if is_display_math_only(content) {
                Some(Alignment::Center)
            } else {
                attrs.align
            };
            let ppr = paragraph_properties(ParagraphProps {
                ind: indent_ind_attr(attrs, images.indent_twips),
                align,
                ..Default::default()
            });
            out.push_str(&format!("    <w:p>{ppr}"));
            let base = block_run_style(attrs, RunStyle::default());
            render_docx_runs(out, content, base, base_dir, images);
            out.push_str("</w:p>\n");
        }
        Block::List {
            ordered,
            start,
            items,
            ..
        } => {
            render_docx_list(
                out,
                *ordered,
                *start,
                items,
                heading_index,
                base_dir,
                images,
            );
        }
        Block::Table {
            headers,
            alignments,
            rows,
            attrs,
            merges,
        } => {
            let col_count = headers
                .len()
                .max(rows.first().map(|r| r.len()).unwrap_or(1))
                .max(1);
            let s = &images.settings;
            let page_w = match s.orientation {
                Orientation::Portrait => 11906,
                Orientation::Landscape => 16838,
            };
            let available =
                (page_w - 2 * length_to_twips(&s.margin, 1134) - images.indent_twips).max(1440);
            let col_widths = column_widths_twips(attrs.widths.as_deref(), col_count, available);
            let total_width: i64 = col_widths.iter().sum();

            // `w:tblPr` の子要素の順序：tblW → tblInd → tblBorders → tblLayout
            out.push_str(&format!(
                "    <w:tbl><w:tblPr><w:tblW w:w=\"{total_width}\" w:type=\"dxa\"/>"
            ));
            if images.indent_twips > 0 {
                out.push_str(&format!(
                    "<w:tblInd w:w=\"{}\" w:type=\"dxa\"/>",
                    images.indent_twips
                ));
            }
            out.push_str("<w:tblBorders>");
            for edge in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                out.push_str(&format!(
                    "<w:{edge} w:val=\"single\" w:sz=\"4\" w:color=\"9AA0A6\"/>"
                ));
            }
            out.push_str("</w:tblBorders>");
            // 列幅を指定した表は、Wordが中身に合わせて幅を変えないよう固定する
            if attrs.widths.is_some() {
                out.push_str("<w:tblLayout w:type=\"fixed\"/>");
            }
            out.push_str("</w:tblPr><w:tblGrid>");
            for w in &col_widths {
                out.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
            }
            out.push_str("</w:tblGrid>\n");

            let row_padding_twips = attrs
                .row_padding
                .as_deref()
                .map(|v| length_to_twips(v, 120));
            let (header_layout, row_layout) =
                resolve_table_layout(merges, !headers.is_empty(), rows.len(), col_count);
            let mut all_rows: Vec<(&[Vec<Inline>], bool)> = Vec::new();
            if !headers.is_empty() {
                all_rows.push((headers.as_slice(), true));
            }
            all_rows.extend(rows.iter().map(|r| (r.as_slice(), false)));
            let all_layouts: Vec<&Vec<CellSlot>> = std::iter::once(&header_layout)
                .filter(|_| !headers.is_empty())
                .chain(row_layout.iter())
                .collect();
            let cells = docx_table_cells(&all_layouts);

            for (r, (row, is_header)) in all_rows.iter().enumerate() {
                out.push_str("      <w:tr>\n");
                for cell in &cells[r] {
                    let content = match cell.kind {
                        DocxCellKind::Content { orphan: false }
                        | DocxCellKind::ContentRestart { orphan: false } => row.get(cell.col),
                        _ => None,
                    };
                    render_docx_table_cell(
                        out,
                        content,
                        alignments.get(cell.col).copied().flatten(),
                        *is_header,
                        if *is_header {
                            attrs.header_bg.as_deref()
                        } else {
                            None
                        },
                        row_padding_twips,
                        cell,
                        base_dir,
                        images,
                    );
                }
                out.push_str("      </w:tr>\n");
            }
            out.push_str("    </w:tbl>\n    <w:p/>\n");
        }
        Block::BlockQuote { blocks, .. } => {
            // 引用は左インデントを付けて表現する（罫線囲みの再現は見送り）。
            // 引用の中の引用・リストは、さらに内側へずらして入れ子を表す
            let base = images.indent_twips;
            images.indent_twips = base + 720;
            for b in blocks {
                render_docx_block(out, b, heading_index, base_dir, images);
            }
            images.indent_twips = base;
        }
        Block::CodeBlock { code, .. } => {
            let ind = if images.indent_twips > 0 {
                format!("<w:ind w:left=\"{}\"/>", images.indent_twips)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "    <w:p><w:pPr><w:shd w:val=\"clear\" w:fill=\"F8F9FA\"/>{ind}</w:pPr>"
            ));
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

/// `attrs` の `indent_left` / `indent_right` から `<w:ind .../>` 断片を生成する（指定がなければ空文字列）。
/// `extra_left` は引用やリスト項目の中にいる分の左インデント（twip）で、`indent_left` に足し込む
/// 段落の書式（`w:pPr`）の材料
#[derive(Default)]
struct ParagraphProps {
    keep_next: bool,
    spacing: Option<&'static str>,
    ind: String,
    align: Option<Alignment>,
    outline_level: Option<u32>,
}

/// `w:pPr` を、OOXMLのスキーマが定める子要素の順序（keepNext → spacing → ind → jc → outlineLvl）で書く。
/// 順序が違うと、Wordが「読み取れない内容」として修復を求めることがある。指定が何もなければ空文字列
fn paragraph_properties(p: ParagraphProps) -> String {
    let mut inner = String::new();
    if p.keep_next {
        inner.push_str("<w:keepNext/>");
    }
    if let Some(spacing) = p.spacing {
        inner.push_str(spacing);
    }
    inner.push_str(&p.ind);
    match p.align {
        Some(Alignment::Center) => inner.push_str("<w:jc w:val=\"center\"/>"),
        Some(Alignment::Right) => inner.push_str("<w:jc w:val=\"right\"/>"),
        Some(Alignment::Left) => inner.push_str("<w:jc w:val=\"left\"/>"),
        None => {}
    }
    if let Some(level) = p.outline_level {
        inner.push_str(&format!("<w:outlineLvl w:val=\"{level}\"/>"));
    }
    if inner.is_empty() {
        inner
    } else {
        format!("<w:pPr>{inner}</w:pPr>")
    }
}

/// 段落・見出しの属性（`{color: ...}` / `{size: ...}`）を、その中の文字の既定の書式に反映する
fn block_run_style(attrs: &crate::model::BlockAttributes, mut base: RunStyle) -> RunStyle {
    if let Some(color) = &attrs.color {
        base.color = Some(sanitize_hex(color));
    }
    if let Some(pt) = attrs.font_size.as_deref().and_then(parse_pt_size) {
        base.size_half_pt = Some((pt * 2.0).round() as u32);
    }
    base
}

fn indent_ind_attr(attrs: &crate::model::BlockAttributes, extra_left: i64) -> String {
    let left = match attrs.indent_left.as_deref().map(|v| length_to_twips(v, 0)) {
        Some(l) => Some(l + extra_left),
        None if extra_left > 0 => Some(extra_left),
        None => None,
    };
    let right = attrs.indent_right.as_deref().map(|v| length_to_twips(v, 0));
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

/// `{widths: ...}` を、Wordの列幅（twip）に換算する。PDF（Typst）と同じく `%` と `fr` は
/// 残りの幅を比で分け合い、`mm` などの長さはその幅で固定する。`auto` と指定のない列は
/// 比の平均（比の指定がなければ1）の重みで分け合う。指定がなければ全列を等分する
fn column_widths_twips(widths: Option<&[String]>, col_count: usize, available: i64) -> Vec<i64> {
    enum Spec {
        Fixed(i64),
        Weight(f64),
        Auto,
    }
    let specs: Vec<Spec> = (0..col_count)
        .map(|i| {
            let raw = widths
                .and_then(|w| w.get(i))
                .map(|s| s.trim().to_ascii_lowercase())
                .unwrap_or_default();
            for unit in ["fr", "%"] {
                if let Some(v) = raw
                    .strip_suffix(unit)
                    .and_then(|n| n.trim().parse::<f64>().ok())
                {
                    if v > 0.0 {
                        return Spec::Weight(v);
                    }
                }
            }
            match length_to_twips(&raw, -1) {
                t if t > 0 => Spec::Fixed(t),
                _ => Spec::Auto,
            }
        })
        .collect();
    let weights: Vec<f64> = specs
        .iter()
        .filter_map(|s| match s {
            Spec::Weight(w) => Some(*w),
            _ => None,
        })
        .collect();
    let auto_weight = if weights.is_empty() {
        1.0
    } else {
        weights.iter().sum::<f64>() / weights.len() as f64
    };
    let fixed: i64 = specs
        .iter()
        .map(|s| if let Spec::Fixed(t) = s { *t } else { 0 })
        .sum();
    let total_weight: f64 = specs
        .iter()
        .map(|s| match s {
            Spec::Weight(w) => *w,
            Spec::Auto => auto_weight,
            Spec::Fixed(_) => 0.0,
        })
        .sum();
    let remaining = (available - fixed).max(0) as f64;
    specs
        .iter()
        .map(|s| match s {
            Spec::Fixed(t) => *t,
            Spec::Weight(w) => (remaining * w / total_weight).round() as i64,
            Spec::Auto => (remaining * auto_weight / total_weight).round() as i64,
        })
        .map(|t| t.max(200))
        .collect()
}

/// Wordの表の行に置くセル1つ分
#[derive(Debug, Clone, PartialEq)]
struct DocxCell {
    /// このセルが始まる列
    col: usize,
    /// 横に何列ぶん占めるか（`w:gridSpan`）
    colspan: usize,
    kind: DocxCellKind,
}

#[derive(Debug, Clone, PartialEq)]
enum DocxCellKind {
    /// 中身を書くセル（`orphan` は結合先のない `<` / `^` で、中身を空にする）
    Content { orphan: bool },
    /// 中身を書き、下の行と縦に結合し始めるセル（`w:vMerge w:val="restart"`）
    ContentRestart { orphan: bool },
    /// 上のセルと縦に結合した続きのセル（Wordではこの位置にも空のセルが要る）
    Continue,
}

/// 結合を解決した表の格子から、行ごとにWordへ書くセルの並びを作る。
/// 横に結合した分は1つのセルにまとめ（`gridSpan`）、縦に結合した下の行には
/// 続きのセル（`vMerge`）を置く
fn docx_table_cells(layouts: &[&Vec<CellSlot>]) -> Vec<Vec<DocxCell>> {
    let mut cells: Vec<Vec<DocxCell>> = vec![Vec::new(); layouts.len()];
    // 縦の結合の続き：行 → (列, 横幅)
    let mut continuations: Vec<Vec<(usize, usize)>> = vec![Vec::new(); layouts.len()];
    for (r, row) in layouts.iter().enumerate() {
        for (c, slot) in row.iter().enumerate() {
            if let CellSlot::Origin {
                colspan, rowspan, ..
            } = *slot
            {
                for below in continuations.iter_mut().skip(r + 1).take(rowspan - 1) {
                    below.push((c, colspan));
                }
            }
        }
    }
    for (r, row) in layouts.iter().enumerate() {
        for (c, slot) in row.iter().enumerate() {
            match *slot {
                CellSlot::Origin {
                    colspan,
                    rowspan,
                    orphan,
                } => cells[r].push(DocxCell {
                    col: c,
                    colspan,
                    kind: if rowspan > 1 {
                        DocxCellKind::ContentRestart { orphan }
                    } else {
                        DocxCellKind::Content { orphan }
                    },
                }),
                CellSlot::Covered => {
                    if let Some(&(_, colspan)) =
                        continuations[r].iter().find(|(start, _)| *start == c)
                    {
                        cells[r].push(DocxCell {
                            col: c,
                            colspan,
                            kind: DocxCellKind::Continue,
                        });
                    }
                }
            }
        }
    }
    cells
}

#[allow(clippy::too_many_arguments)]
fn render_docx_table_cell(
    out: &mut String,
    cell: Option<&Vec<Inline>>,
    align: Option<Alignment>,
    is_header: bool,
    header_bg: Option<&str>,
    row_padding_twips: Option<i64>,
    layout: &DocxCell,
    base_dir: Option<&Path>,
    images: &mut DocxImageCollector,
) {
    // セルの中身を丸ごと `[...]{bg: ...}` で囲んだ場合は、PDF/HTMLと同じくセル全体を塗る
    // （文字の背景としてではなく、セルの網かけにする）
    let whole_cell = cell.and_then(|c| match c.as_slice() {
        [Inline::Styled { content, style }] if style.bg.is_some() => {
            let mut rest = style.clone();
            let bg = rest.bg.take();
            let inner = if rest == InlineStyle::default() {
                content.clone()
            } else {
                vec![Inline::Styled {
                    content: content.clone(),
                    style: rest,
                }]
            };
            bg.map(|bg| (bg, inner))
        }
        _ => None,
    });
    let cell_fill = whole_cell.as_ref().map(|(bg, _)| bg.as_str()).or(header_bg);
    let cell = whole_cell.as_ref().map(|(_, inner)| inner).or(cell);

    // `w:tcPr` の子要素も順序が決まっている（gridSpan → vMerge → shd → tcMar）
    out.push_str("        <w:tc><w:tcPr>");
    if layout.colspan > 1 {
        out.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", layout.colspan));
    }
    match layout.kind {
        DocxCellKind::ContentRestart { .. } => out.push_str("<w:vMerge w:val=\"restart\"/>"),
        DocxCellKind::Continue => out.push_str("<w:vMerge/>"),
        DocxCellKind::Content { .. } => {}
    }
    if let Some(bg) = cell_fill {
        let safe = sanitize_hex(bg);
        out.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{safe}\"/>"
        ));
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
    /// 文字の背景色（`[文字]{bg: ...}`。16進6桁）
    bg: Option<String>,
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
    out.push_str(&runs_xml(items));
}

/// ランの並びをXMLにする
fn runs_xml(items: Vec<RunItem>) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            RunItem::Break => out.push_str("<w:r><w:br/></w:r>"),
            RunItem::Drawing(xml) => out.push_str(&xml),
            RunItem::Text(text, style) => {
                if text.is_empty() {
                    continue;
                }
                // `w:rPr` の子要素はOOXMLのスキーマが定める順序で書く
                // （rFonts → b → i → strike → color → sz → u → shd）。順序が違うと、
                // Wordが「読み取れない内容」として修復を求めることがある
                out.push_str("<w:r><w:rPr>");
                if style.monospace {
                    out.push_str("<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/>");
                }
                if style.bold {
                    out.push_str("<w:b/>");
                }
                if style.italic {
                    out.push_str("<w:i/>");
                }
                if style.strikethrough {
                    out.push_str("<w:strike/>");
                }
                if let Some(color) = &style.color {
                    out.push_str(&format!("<w:color w:val=\"{}\"/>", sanitize_hex(color)));
                }
                if let Some(sz) = style.size_half_pt {
                    out.push_str(&format!("<w:sz w:val=\"{sz}\"/>"));
                }
                if style.underline {
                    out.push_str("<w:u w:val=\"single\"/>");
                }
                if let Some(bg) = &style.bg {
                    out.push_str(&format!(
                        "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
                        sanitize_hex(bg)
                    ));
                }
                out.push_str("</w:rPr><w:t xml:space=\"preserve\">");
                out.push_str(&xml_escape(&text));
                out.push_str("</w:t></w:r>");
            }
        }
    }
    out
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
            Inline::Link { text, url } => {
                let mut s = style.clone();
                s.underline = true;
                s.color = Some("1A73E8".to_string());
                let mut inner = Vec::new();
                collect_runs(text, s, base_dir, images, &mut inner);
                match images.register_link(url) {
                    // Word本来のハイパーリンク（Ctrl+クリックで開く）。脚注の中でも使えるよう、
                    // relationship の名前空間はこの要素に直接宣言する
                    Some(rel_id) => out.push(RunItem::Drawing(format!(
                        "<w:hyperlink xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"{rel_id}\" w:history=\"1\">{}</w:hyperlink>",
                        runs_xml(inner)
                    ))),
                    // 文書内の見出しへのリンクは、見た目だけリンクにする
                    None => out.extend(inner),
                }
            }
            Inline::Image {
                alt, url, width, ..
            } => {
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
            // Word本来の脚注（ページ下部に番号付きで入る）として登録する
            Inline::Footnote { id, content, .. } => {
                match images.footnote_ids.get(id).copied() {
                    // 同じ脚注の2回目以降の参照：Wordの脚注は1つの注に1か所からしか参照できないため、
                    // 最初の参照と同じ番号を上付き文字で書く（Wordは脚注を出現順に1から番号付けする）
                    Some(word_id) => out.push(RunItem::Drawing(format!(
                        "<w:r><w:rPr><w:vertAlign w:val=\"superscript\"/></w:rPr><w:t>{word_id}</w:t></w:r>"
                    ))),
                    None => {
                        let word_id = images.add_footnote(content, base_dir);
                        images.footnote_ids.insert(*id, word_id);
                        out.push(RunItem::Drawing(format!(
                            "<w:r><w:rPr><w:vertAlign w:val=\"superscript\"/></w:rPr><w:footnoteReference w:id=\"{word_id}\"/></w:r>"
                        )));
                    }
                }
            }
            Inline::FootnoteRef(label) => {
                out.push(RunItem::Text(format!("[^{label}]"), style.clone()))
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
    // 背景色は定型の蛍光ペン（w:highlight）の色に収まらないため、任意の色を指定できる
    // 網かけ（w:shd）で表す
    if let Some(bg) = &style.bg {
        target.bg = Some(sanitize_hex(bg));
    }
}

fn link_relationships(links: &[(String, String)]) -> Vec<String> {
    links
        .iter()
        .map(|(id, url)| {
            format!(
                r#"  <Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="{}" TargetMode="External"/>"#,
                xml_escape(url)
            )
        })
        .collect()
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

fn content_types_xml(
    images: &[EmbeddedImage],
    has_footnotes: bool,
    header_footer: &HeaderFooter,
) -> String {
    let mut header_footer_overrides = String::new();
    if header_footer.header.is_some() {
        header_footer_overrides.push_str(
            r#"
  <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#,
        );
    }
    if header_footer.footer.is_some() {
        header_footer_overrides.push_str(
            r#"
  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>"#,
        );
    }
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
    let footnote_overrides = if has_footnotes {
        r#"
  <Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>
  <Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>"#
    } else {
        ""
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
{image_defaults}
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
  <Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>{footnote_overrides}{header_footer_overrides}
</Types>
"#
    )
}

fn image_relationships(images: &[EmbeddedImage]) -> Vec<String> {
    images
        .iter()
        .map(|img| {
            format!(
                r#"  <Relationship Id="{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/{}"/>"#,
                img.relationship_id, img.file_name
            )
        })
        .collect()
}

fn wrap_relationships(relationships: Vec<String>) -> String {
    let relationships = relationships.join("\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
{relationships}
</Relationships>
"#
    )
}

/// 全ページ共通のヘッダー・フッターのパート（指定がなければ `None`）
struct HeaderFooter {
    header: Option<String>,
    footer: Option<String>,
}

fn document_rels_xml(
    images: &[EmbeddedImage],
    links: &[(String, String)],
    has_footnotes: bool,
    header_footer: &HeaderFooter,
) -> String {
    let mut relationships = image_relationships(images);
    relationships.extend(link_relationships(links));
    if header_footer.header.is_some() {
        relationships.push(
            r#"  <Relationship Id="rIdHeader1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>"#
                .to_string(),
        );
    }
    if header_footer.footer.is_some() {
        relationships.push(
            r#"  <Relationship Id="rIdFooter1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>"#
                .to_string(),
        );
    }
    relationships.push(
        r#"  <Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#
            .to_string(),
    );
    relationships.push(
        r#"  <Relationship Id="rIdNumbering" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#
            .to_string(),
    );
    if has_footnotes {
        relationships.push(
            r#"  <Relationship Id="rIdFootnotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>"#
                .to_string(),
        );
        relationships.push(
            r#"  <Relationship Id="rIdSettings" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>"#
                .to_string(),
        );
    }
    wrap_relationships(relationships)
}

/// 箇条書き（abstractNum 0）と段落番号（abstractNum 1）の定義、
/// およびリスト1つごとの番号付け（開始番号の指定を含む）を書き出す
fn numbering_xml(nums: &[NumInstance]) -> String {
    const BULLETS: [&str; 3] = ["•", "◦", "▪"];
    let mut bullet_levels = String::new();
    let mut decimal_levels = String::new();
    for lvl in 0..9i64 {
        let left = LIST_INDENT_TWIPS * (lvl + 1);
        let bullet = BULLETS[lvl as usize % BULLETS.len()];
        bullet_levels.push_str(&format!(
            "<w:lvl w:ilvl=\"{lvl}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"{bullet}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{left}\" w:hanging=\"{LIST_INDENT_TWIPS}\"/></w:pPr></w:lvl>"
        ));
        decimal_levels.push_str(&format!(
            "<w:lvl w:ilvl=\"{lvl}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%{}.\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{left}\" w:hanging=\"{LIST_INDENT_TWIPS}\"/></w:pPr></w:lvl>",
            lvl + 1
        ));
    }
    let mut num_xml = String::new();
    for (i, num) in nums.iter().enumerate() {
        let id = i + 1;
        if num.ordered {
            num_xml.push_str(&format!(
                "  <w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"1\"/><w:lvlOverride w:ilvl=\"{}\"><w:startOverride w:val=\"{}\"/></w:lvlOverride></w:num>\n",
                num.level, num.start
            ));
        } else {
            num_xml.push_str(&format!(
                "  <w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"0\"/></w:num>\n"
            ));
        }
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/>{bullet_levels}</w:abstractNum>
  <w:abstractNum w:abstractNumId="1"><w:multiLevelType w:val="hybridMultilevel"/>{decimal_levels}</w:abstractNum>
{num_xml}</w:numbering>
"#
    )
}

/// 脚注本体（区切り線の定義2件＋本文の脚注）を書き出す
fn footnotes_xml(notes: &[String]) -> String {
    let mut body = String::new();
    for (i, runs) in notes.iter().enumerate() {
        let id = i + 1;
        body.push_str(&format!(
            "  <w:footnote w:id=\"{id}\"><w:p><w:r><w:rPr><w:vertAlign w:val=\"superscript\"/><w:sz w:val=\"18\"/></w:rPr><w:footnoteRef/></w:r><w:r><w:rPr><w:sz w:val=\"18\"/></w:rPr><w:t xml:space=\"preserve\"> </w:t></w:r>{runs}</w:p></w:footnote>\n"
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnote w:type="separator" w:id="-1"><w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:r><w:separator/></w:r></w:p></w:footnote>
  <w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>
{body}</w:footnotes>
"#
    )
}

const SETTINGS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnotePr><w:footnote w:id="-1"/><w:footnote w:id="0"/></w:footnotePr>
</w:settings>
"#;

fn build_docx_zip(
    document_xml: &str,
    styles_xml: &str,
    collected: &DocxImageCollector,
    header_footer: &HeaderFooter,
) -> Result<Vec<u8>, String> {
    let images = &collected.images;
    let has_footnotes = !collected.footnotes.is_empty();
    let buf = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let options: zip::write::FileOptions<'_, '_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut parts: Vec<(String, Vec<u8>)> = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types_xml(images, has_footnotes, header_footer).into_bytes(),
        ),
        ("_rels/.rels".to_string(), ROOT_RELS_XML.as_bytes().to_vec()),
        (
            "word/document.xml".to_string(),
            document_xml.as_bytes().to_vec(),
        ),
        (
            "word/styles.xml".to_string(),
            styles_xml.as_bytes().to_vec(),
        ),
        (
            "word/numbering.xml".to_string(),
            numbering_xml(&collected.nums).into_bytes(),
        ),
        (
            "word/_rels/document.xml.rels".to_string(),
            document_rels_xml(images, &collected.links, has_footnotes, header_footer).into_bytes(),
        ),
    ];
    if let Some(header) = &header_footer.header {
        parts.push(("word/header1.xml".to_string(), header.as_bytes().to_vec()));
    }
    if let Some(footer) = &header_footer.footer {
        parts.push(("word/footer1.xml".to_string(), footer.as_bytes().to_vec()));
    }
    if has_footnotes {
        parts.push((
            "word/footnotes.xml".to_string(),
            footnotes_xml(&collected.footnotes).into_bytes(),
        ));
        // 脚注の中の数式・画像も参照できるよう、本文と同じ画像の関連付けを持たせる
        parts.push((
            "word/_rels/footnotes.xml.rels".to_string(),
            wrap_relationships(
                image_relationships(images)
                    .into_iter()
                    .chain(link_relationships(&collected.links))
                    .collect(),
            )
            .into_bytes(),
        ));
        parts.push((
            "word/settings.xml".to_string(),
            SETTINGS_XML.as_bytes().to_vec(),
        ));
    }

    for (name, bytes) in &parts {
        zip.start_file(name.as_str(), options)
            .map_err(|e| e.to_string())?;
        zip.write_all(bytes).map_err(|e| e.to_string())?;
    }

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
        let bytes =
            generate_docx_bytes_with_base(doc, None).expect("docx generation should succeed");
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
        doc.settings.font_choice = crate::model::FontChoice::Custom(r#"Evil"&<font>"#.to_string());
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
            assert!(
                names.contains(&format!("word/media/image{i}.png")),
                "{names:?}"
            );
        }
        let document = read_docx_part(&doc, "word/document.xml");
        assert!(document.contains(r#"descr="$x^2$""#));
        assert!(
            document.contains(r#"<w:position w:val="-"#),
            "inline math is lowered to the baseline"
        );
        assert!(document.contains(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:drawing>"#));
        for id in 1..=3 {
            assert_eq!(
                document.matches(&format!(r#"<wp:docPr id="{id}""#)).count(),
                1
            );
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
        let bytes =
            generate_docx_bytes_with_base(&doc, None).expect("docx generation should succeed");
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
        // 番号は文字ではなくWordの段落番号で振り、開始番号は numbering.xml の startOverride で指定する
        assert!(!out.contains(">5. <"));
        assert_eq!(out.matches("<w:numId w:val=\"1\"/>").count(), 2);
        let numbering = numbering_xml(&images.nums);
        assert!(numbering.contains(
            "<w:num w:numId=\"1\"><w:abstractNumId w:val=\"1\"/><w:lvlOverride w:ilvl=\"0\"><w:startOverride w:val=\"5\"/>"
        ));
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
            ```typst\n#line(length: 50%)\n```\n\n\
            - [ ] 未完了\n- [x] 完了\n\n\
            1. 手順\n\n   ```\n   cmd\n   ```\n   - 子\n2. 次\n\n\
            > 外\n>\n> > 内\n\n\
            脚注つき[^a]の本文 [背景]{bg: #ffeeaa}\n\n\
            [^a]: 脚注の**中身** $x^2$ [参照](https://example.com/?a=1&b=\"2\")\n";
        let doc = parse_document(src);
        let bytes =
            generate_docx_bytes_with_base(&doc, None).expect("docx generation should succeed");

        let cursor = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).unwrap();

        for part in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/styles.xml",
            "word/numbering.xml",
            "word/footnotes.xml",
            "word/settings.xml",
            "word/_rels/document.xml.rels",
            "word/_rels/footnotes.xml.rels",
            "word/header1.xml",
            "word/footer1.xml",
        ] {
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
        let dir =
            std::env::temp_dir().join(format!("typer_docx_image_test_{}", std::process::id()));
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

/// Milestone 13（Wordの本物のリスト・脚注・引用のインデント）のテスト
#[cfg(test)]
mod structure_tests {
    use super::*;
    use crate::parser::parse_document;

    fn part(src: &str, name: &str) -> Option<String> {
        let doc = parse_document(src);
        let bytes =
            generate_docx_bytes_with_base(&doc, None).expect("docx generation should succeed");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut file = archive.by_name(name).ok()?;
        let mut s = String::new();
        std::io::Read::read_to_string(&mut file, &mut s).unwrap();
        Some(s)
    }

    fn document(src: &str) -> String {
        part(src, "word/document.xml").unwrap()
    }

    #[test]
    fn bullet_list_uses_word_numbering_not_text_bullets() {
        let out = document("- a\n- b\n");
        assert!(!out.contains("• "));
        assert_eq!(
            out.matches("<w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr>")
                .count(),
            2
        );
        let numbering = part("- a\n", "word/numbering.xml").unwrap();
        assert!(numbering.contains("<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>"));
        assert!(numbering.contains("<w:numFmt w:val=\"bullet\"/>"));
    }

    #[test]
    fn nested_lists_use_deeper_levels_and_separate_numbering() {
        let out = document("1. 一\n   1. 子\n2. 二\n   1. 子\n");
        // 外側は numId 1 のレベル0、入れ子はそれぞれ別の numId（番号が1から振り直される）のレベル1
        assert_eq!(
            out.matches("<w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/>")
                .count(),
            2
        );
        assert!(out.contains("<w:ilvl w:val=\"1\"/><w:numId w:val=\"2\"/>"));
        assert!(out.contains("<w:ilvl w:val=\"1\"/><w:numId w:val=\"3\"/>"));
        assert!(out.contains("<w:ind w:left=\"720\" w:hanging=\"360\"/>"));
        let numbering = part("1. 一\n   1. 子\n", "word/numbering.xml").unwrap();
        assert!(numbering.contains("<w:lvlOverride w:ilvl=\"1\"><w:startOverride w:val=\"1\"/>"));
    }

    #[test]
    fn loose_list_and_item_children_are_indented_under_item() {
        let out = document("- a\n\n  続き\n\n  ```\n  cmd\n  ```\n");
        assert!(out.contains("<w:p><w:pPr><w:ind w:left=\"360\"/></w:pPr><w:r><w:rPr></w:rPr><w:t xml:space=\"preserve\">続き</w:t>"), "{out}");
        assert!(out.contains("<w:shd w:val=\"clear\" w:fill=\"F8F9FA\"/><w:ind w:left=\"360\"/>"));
    }

    #[test]
    fn task_items_get_box_characters_without_numbering() {
        let out = document("- [ ] 未\n- [x] 済\n");
        assert!(out.contains("☐ </w:t>"));
        assert!(out.contains("☑ </w:t>"));
        assert!(!out.contains("<w:numPr>"));
    }

    #[test]
    fn quote_paragraphs_are_indented_and_nested_quotes_more() {
        let out = document("> 外\n>\n> > 内\n");
        assert!(
            out.contains(
                "<w:ind w:left=\"720\"/></w:pPr><w:r><w:rPr></w:rPr><w:t xml:space=\"preserve\">外"
            ),
            "{out}"
        );
        assert!(out.contains(
            "<w:ind w:left=\"1440\"/></w:pPr><w:r><w:rPr></w:rPr><w:t xml:space=\"preserve\">内"
        ));
    }

    #[test]
    fn list_inside_quote_is_indented_past_the_quote() {
        let out = document("> - a\n");
        assert!(
            out.contains("<w:ind w:left=\"1080\" w:hanging=\"360\"/>"),
            "{out}"
        );
    }

    #[test]
    fn table_inside_quote_gets_table_indent() {
        let out = document("> | A |\n> |---|\n> | 1 |\n");
        assert!(
            out.contains("<w:tblInd w:w=\"720\" w:type=\"dxa\"/><w:tblBorders>"),
            "{out}"
        );
    }

    #[test]
    fn footnotes_become_real_word_footnotes() {
        let src = "本文[^a]と[^b]\n\n[^a]: 注A\n\n[^b]: **注B**\n";
        let out = document(src);
        assert!(out.contains("<w:footnoteReference w:id=\"1\"/>"));
        assert!(out.contains("<w:footnoteReference w:id=\"2\"/>"));
        let notes = part(src, "word/footnotes.xml").unwrap();
        assert!(notes.contains("<w:footnote w:type=\"separator\" w:id=\"-1\">"));
        assert!(notes.contains("<w:footnote w:id=\"1\">"));
        assert!(notes.contains(">注A</w:t>"));
        assert!(
            notes.contains(
                "<w:b/><w:sz w:val=\"18\"/></w:rPr><w:t xml:space=\"preserve\">注B</w:t>"
            )
        );
        let rels = part(src, "word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("Target=\"footnotes.xml\""));
        assert!(rels.contains("Target=\"settings.xml\""));
        let types = part(src, "[Content_Types].xml").unwrap();
        assert!(types.contains("/word/footnotes.xml"));
    }

    #[test]
    fn no_footnote_parts_without_footnotes() {
        assert!(part("本文\n", "word/footnotes.xml").is_none());
        assert!(part("本文\n", "word/settings.xml").is_none());
        let rels = part("本文\n", "word/_rels/document.xml.rels").unwrap();
        assert!(!rels.contains("footnotes.xml"));
        assert!(rels.contains("Target=\"numbering.xml\""));
    }

    #[test]
    fn repeated_footnote_reuses_the_first_number() {
        let src = "一[^a]、二[^b]、再び[^a]\n\n[^a]: 注A\n\n[^b]: 注B\n";
        let out = document(src);
        assert_eq!(out.matches("<w:footnoteReference").count(), 2);
        assert!(
            out.contains("<w:vertAlign w:val=\"superscript\"/></w:rPr><w:t>1</w:t>"),
            "{out}"
        );
        let notes = part(src, "word/footnotes.xml").unwrap();
        assert_eq!(notes.matches(">注A</w:t>").count(), 1);
    }

    #[test]
    fn math_inside_footnote_is_referenced_from_footnote_rels() {
        let src = "本文[^a]\n\n[^a]: $x^2$\n";
        let notes = part(src, "word/footnotes.xml").unwrap();
        let Some(pos) = notes.find("r:embed=\"") else {
            // 数式を組めない環境では等幅の文字列に落ちる
            assert!(notes.contains("$x^2$"));
            return;
        };
        let id: String = notes[pos + 9..].chars().take_while(|c| *c != '"').collect();
        let rels = part(src, "word/_rels/footnotes.xml.rels").unwrap();
        assert!(rels.contains(&format!("Id=\"{id}\"")), "{rels}");
    }
}

#[cfg(test)]
mod link_and_background_tests {
    use super::*;
    use crate::parser::parse_document;

    fn part(src: &str, name: &str) -> String {
        let doc = parse_document(src);
        let bytes = generate_docx_bytes_with_base(&doc, None).expect("docx generation");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut file = archive.by_name(name).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut file, &mut s).unwrap();
        s
    }

    #[test]
    fn external_links_become_clickable_hyperlinks() {
        let src = "[公式](https://example.com/a?x=1&y=2) と [もう一度](https://example.com/a?x=1&y=2) と [別](mailto:a@example.com)\n";
        let doc_xml = part(src, "word/document.xml");
        let rels = part(src, "word/_rels/document.xml.rels");
        // 同じリンク先は1つの関連付けを使い回す
        assert_eq!(doc_xml.matches("r:id=\"rIdLink1\"").count(), 2, "{doc_xml}");
        assert!(doc_xml.contains("r:id=\"rIdLink2\""));
        assert!(rels.contains(
            r#"Id="rIdLink1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/a?x=1&amp;y=2" TargetMode="External""#
        ), "{rels}");
        assert!(rels.contains(r#"Target="mailto:a@example.com""#));
        // 見た目（青・下線）はリンクの中のランに付く
        assert!(doc_xml.contains("<w:hyperlink"));
        assert!(doc_xml.contains("<w:color w:val=\"1A73E8\"/>"));
    }

    #[test]
    fn heading_anchor_links_stay_styled_text() {
        let doc_xml = part("[概要へ](#概要)\n", "word/document.xml");
        assert!(!doc_xml.contains("<w:hyperlink"));
        assert!(doc_xml.contains("概要へ"));
        assert!(!part("[概要へ](#概要)\n", "word/_rels/document.xml.rels").contains("hyperlink"));
    }

    #[test]
    fn inline_background_becomes_run_shading() {
        let doc_xml = part("本文の[強調]{bg: #fff2cc}部分\n", "word/document.xml");
        assert!(
            doc_xml.contains("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FFF2CC\"/></w:rPr><w:t xml:space=\"preserve\">強調"),
            "{doc_xml}"
        );
    }

    #[test]
    fn whole_cell_background_fills_the_cell() {
        let doc_xml = part(
            "| A | B |\n| - | - |\n| [塗り]{bg: #fff2cc, red} | 普通 |\n",
            "word/document.xml",
        );
        assert!(
            doc_xml.contains("<w:tcPr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FFF2CC\"/>"),
            "{doc_xml}"
        );
        // 文字の網かけとしては付けない（色指定は残る）
        let cell_run = doc_xml.split("塗り").next().unwrap();
        let last_rpr = cell_run.rsplit("<w:rPr>").next().unwrap();
        assert!(!last_rpr.contains("w:shd"), "{last_rpr}");
        assert!(
            last_rpr.contains("<w:color w:val=\"C5221F\"/>"),
            "{last_rpr}"
        );
    }

    #[test]
    fn run_properties_follow_the_schema_order() {
        let doc_xml = part(
            "***[全部]{color: red, size: 14pt, underline, bg: #eeeeee}*** ~~`x`~~\n",
            "word/document.xml",
        );
        let rpr = doc_xml
            .split("<w:rPr>")
            .find(|s| s.contains("全部"))
            .unwrap()
            .split("</w:rPr>")
            .next()
            .unwrap();
        let order = ["<w:b/>", "<w:i/>", "<w:color", "<w:sz", "<w:u ", "<w:shd"];
        let positions: Vec<usize> = order.iter().map(|t| rpr.find(t).unwrap()).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{rpr}");
        let code = doc_xml
            .split("<w:rPr>")
            .find(|s| s.contains(">x<"))
            .unwrap();
        assert!(
            code.find("<w:rFonts").unwrap() < code.find("<w:strike/>").unwrap(),
            "{code}"
        );
    }
}

#[cfg(test)]
mod word_check_export {
    /// `docs/word-check/sample.md` をWord・PDF・HTMLに書き出す（実機確認用）。
    /// 普段のテストでは動かさない：
    /// `$env:TYPER_EXPORT_DIR = "出力先"; cargo test word_check_export -- --ignored`
    #[test]
    #[ignore]
    fn export_word_check_sample() {
        let out_dir = std::path::PathBuf::from(
            std::env::var("TYPER_EXPORT_DIR").expect("set TYPER_EXPORT_DIR"),
        );
        std::fs::create_dir_all(&out_dir).unwrap();
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/word-check");
        let src = std::fs::read_to_string(base.join("sample.md")).unwrap();
        let doc = crate::parser::parse_document(&src);
        assert!(crate::lint::lint_markdown(&src).is_empty());

        let docx = super::generate_docx_bytes_with_base(&doc, Some(&base)).unwrap();
        std::fs::write(out_dir.join("sample.docx"), docx).unwrap();
        let html = crate::html_export::generate_html_with_base(&doc, Some(&base));
        std::fs::write(out_dir.join("sample.html"), html).unwrap();
        let code = crate::typst_gen::generate_typst_source_with_base(&doc, Some(&base));
        let pdf = crate::typst_engine::TypstEngine::new()
            .export_pdf(&code)
            .unwrap();
        std::fs::write(out_dir.join("sample.pdf"), pdf).unwrap();
    }
}

#[cfg(test)]
mod paragraph_and_page_tests {
    use super::*;
    use crate::parser::parse_document;

    fn part(src: &str, name: &str) -> Option<String> {
        let doc = parse_document(src);
        let bytes = generate_docx_bytes_with_base(&doc, None).expect("docx generation");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut file = archive.by_name(name).ok()?;
        let mut s = String::new();
        std::io::Read::read_to_string(&mut file, &mut s).unwrap();
        Some(s)
    }

    #[test]
    fn paragraph_alignment_color_and_size_are_applied() {
        let doc = part(
            "{align: right}\n日付\n\n{align: center, size: 16pt, color: blue}\nタイトル\n",
            "word/document.xml",
        )
        .unwrap();
        assert!(
            doc.contains("<w:p><w:pPr><w:jc w:val=\"right\"/></w:pPr>"),
            "{doc}"
        );
        let title = doc.split("<w:p>").find(|p| p.contains("タイトル")).unwrap();
        assert!(title.contains("<w:jc w:val=\"center\"/>"), "{title}");
        assert!(title.contains("<w:color w:val=\"1A73E8\"/>"), "{title}");
        assert!(title.contains("<w:sz w:val=\"32\"/>"), "{title}");
    }

    #[test]
    fn heading_properties_follow_schema_order_and_keep_with_next() {
        let doc = part(
            "{align: center, color: red}\n## 見出し\n",
            "word/document.xml",
        )
        .unwrap();
        assert!(
            doc.contains("<w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/><w:jc w:val=\"center\"/><w:outlineLvl w:val=\"1\"/></w:pPr>"),
            "{doc}"
        );
        assert!(
            doc.contains("<w:b/><w:color w:val=\"C5221F\"/><w:sz w:val=\"28\"/>"),
            "{doc}"
        );
    }

    #[test]
    fn page_margins_have_required_header_footer_and_gutter() {
        let doc = part("本文\n", "word/document.xml").unwrap();
        assert!(
            doc.contains("w:left=\"1134\" w:header=\"567\" w:footer=\"567\" w:gutter=\"0\"/>"),
            "{doc}"
        );
    }

    #[test]
    fn page_number_only_footer_is_centered_by_default() {
        let footer = part("本文\n", "word/footer1.xml").unwrap();
        assert!(footer.contains("<w:jc w:val=\"center\"/>"));
        assert!(footer.contains("w:instr=\" PAGE \"") && footer.contains("w:instr=\" NUMPAGES \""));
        assert!(part("本文\n", "word/header1.xml").is_none());
        let rels = part("本文\n", "word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("Target=\"footer1.xml\"") && !rels.contains("header1.xml"));
        let types = part("本文\n", "[Content_Types].xml").unwrap();
        assert!(types.contains("/word/footer1.xml") && !types.contains("/word/header1.xml"));
    }

    #[test]
    fn header_and_footer_text_with_page_number() {
        let src =
            "---\nheader: {{会社名}} 資料\nfooter: 社外秘 & 限定\n会社名: サンプル\n---\n本文\n";
        let header = part(src, "word/header1.xml").unwrap();
        assert!(header.contains("<w:jc w:val=\"right\"/>"));
        assert!(header.contains(">サンプル 資料<"), "{header}");
        let footer = part(src, "word/footer1.xml").unwrap();
        // 文字列は左、ページ番号は右端のタブ位置（A4縦・余白20mm → 本文幅 11906 − 2×1134 = 9638）
        assert!(
            footer.contains("<w:tab w:val=\"right\" w:pos=\"9638\"/>"),
            "{footer}"
        );
        assert!(footer.contains(">社外秘 &amp; 限定<"), "{footer}");
        assert!(footer.find("限定").unwrap() < footer.find("PAGE").unwrap());
        let doc = part(src, "word/document.xml").unwrap();
        assert!(doc.contains("<w:headerReference w:type=\"default\" r:id=\"rIdHeader1\"/>"));
        assert!(doc.contains("<w:footerReference w:type=\"default\" r:id=\"rIdFooter1\"/>"));
    }

    #[test]
    fn no_footer_part_when_page_numbers_are_off_and_no_footer_text() {
        let src = "---\npage-number: off\n---\n本文\n";
        assert!(part(src, "word/footer1.xml").is_none());
        assert!(
            !part(src, "word/document.xml")
                .unwrap()
                .contains("footerReference")
        );
        let with_text = part(
            "---\npage-number: off\nfooter: 社外秘\n---\n本文\n",
            "word/footer1.xml",
        )
        .unwrap();
        assert!(with_text.contains("<w:jc w:val=\"center\"/>") && !with_text.contains("PAGE"));
    }
}
