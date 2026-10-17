//! 内部文書モデル (`Document`) から Typst マークアップ文字列への変換器
//!
//! 日本語の社内報告書・業務文書に適した組版ルール（フォント、余白、見出し直後の改ページ抑制、
//! 表の列幅・ヘッダ背景色・セル折り返し、ローカル画像の埋め込みなど）を適用したTypstコードを生成する。

use crate::model::{
    Alignment, Block, BlockAttributes, Document, FontChoice, Inline, InlineStyle, Orientation,
};
use std::fmt::Write;
use std::path::{Path, PathBuf};

pub const LOCAL_IMAGE_VPATH_PREFIX: &str = "__local_img__/";

#[cfg(test)]
pub fn generate_typst_source(doc: &Document) -> String {
    generate_typst_source_with_base(doc, None)
}

pub fn generate_typst_source_with_base(doc: &Document, base_dir: Option<&Path>) -> String {
    let mut out = String::with_capacity(4096);

    let s = &doc.settings;
    let flipped = match s.orientation {
        Orientation::Portrait => "false",
        Orientation::Landscape => "true",
    };
    let paper = sanitize_ident(&s.paper, "a4");
    let margin = sanitize_length(&s.margin, "20mm");
    let font_size = sanitize_length(&s.font_size, "10.5pt");
    let leading_em = ((s.line_height - 0.65).clamp(0.25, 3.0) * 100.0).round() / 100.0;
    let accent = sanitize_color_hex(&s.accent_color);

    let font_list = match s.font_choice {
        FontChoice::Gothic => {
            r#"("BIZ UDPGothic", "BIZ UDGothic", "Yu Gothic", "Meiryo", "MS Gothic")"#
        }
        FontChoice::Mincho => {
            r#"("BIZ UDPMincho", "BIZ UDMincho", "Yu Mincho", "MS Mincho", "Yu Gothic")"#
        }
    };

    let header_footer_params = page_header_footer_params(
        s.header_text.as_deref(),
        s.footer_text.as_deref(),
        s.show_page_number,
    );
    let numbering_param = if s.show_page_number {
        "\"1 / 1\""
    } else {
        "none"
    };

    // ページ・基本組版設定プリアンブル
    let _ = writeln!(
        out,
        r##"#set page(
  paper: "{paper}",
  flipped: {flipped},
  margin: {margin},
  numbering: {numbering_param},{header_footer_params}
)
#set text(
  font: {font_list},
  size: {font_size},
  lang: "ja",
  region: "JP",
)
#set par(
  leading: {leading_em}em,
  spacing: 1.15em,
  justify: true,
)
#show heading: set block(above: 1.4em, below: 0.75em, sticky: true)
#show heading.where(level: 1): it => block(
  width: 100%,
  above: 1.5em,
  below: 0.9em,
  stroke: (bottom: 1.2pt + rgb("{accent}")),
  inset: (bottom: 5pt),
  text(size: 1.45em, weight: "bold", fill: rgb("{accent}"), it.body)
)
#show heading.where(level: 2): it => block(
  width: 100%,
  above: 1.35em,
  below: 0.7em,
  stroke: (bottom: 0.7pt + rgb("#dadce0"), left: 3.5pt + rgb("{accent}")),
  inset: (left: 7pt, bottom: 4pt, top: 2pt),
  text(size: 1.2em, weight: "bold", fill: rgb("#202124"), it.body)
)
#show heading.where(level: 3): it => block(
  above: 1.15em,
  below: 0.55em,
  text(size: 1.08em, weight: "bold", fill: rgb("#3c4043"), it.body)
)
#set table(
  stroke: 0.5pt + rgb("#9aa0a6"),
  inset: (x: 7pt, y: 6pt),
  fill: (_, y) => if y == 0 {{ rgb("#f1f3f4") }} else {{ none }},
)
"##
    );

    for block in &doc.blocks {
        render_block(&mut out, block, base_dir);
    }

    out
}

fn render_block(out: &mut String, block: &Block, base_dir: Option<&Path>) {
    match block {
        Block::Heading {
            level,
            content,
            attrs,
        } => {
            let prefix = "=".repeat((*level as usize).clamp(1, 6));
            let mut inner = String::new();
            let _ = write!(inner, "{prefix} ");
            render_inlines(&mut inner, content, base_dir);
            wrap_with_block_attrs(out, &inner, attrs);
            out.push_str("\n\n");
        }
        Block::Paragraph { content, attrs } => {
            let mut inner = String::new();
            render_inlines(&mut inner, content, base_dir);
            wrap_with_block_attrs(out, &inner, attrs);
            out.push_str("\n\n");
        }
        Block::List {
            ordered,
            start,
            items,
            attrs,
        } => {
            let mut inner = String::new();
            if *ordered && *start != 1 {
                // 既定（1始まり）以外の番号付きリストは、`+` 記法では開始番号を指定できないため
                // `#enum(start: ...)` 関数形式で明示的に組み立てる
                let _ = write!(inner, "#enum(start: {start})[\n");
                for item in items {
                    inner.push_str("  [");
                    render_inlines(&mut inner, item, base_dir);
                    inner.push_str("]\n");
                }
                inner.push(']');
            } else {
                for item in items {
                    if *ordered {
                        inner.push_str("+ ");
                    } else {
                        inner.push_str("- ");
                    }
                    render_inlines(&mut inner, item, base_dir);
                    inner.push('\n');
                }
            }
            wrap_with_block_attrs(out, inner.trim_end(), attrs);
            out.push_str("\n\n");
        }
        Block::Table {
            headers,
            alignments,
            rows,
            attrs,
        } => {
            render_table(out, headers, alignments, rows, attrs, base_dir);
            out.push_str("\n\n");
        }
        Block::BlockQuote {
            blocks: sub_blocks,
            attrs,
        } => {
            let mut inner = String::new();
            for b in sub_blocks {
                render_block(&mut inner, b, base_dir);
            }
            let quote_code = if attrs.boxed {
                format!(
                    "#block(width: 100%, fill: white, stroke: 0.8pt + rgb(\"#9aa0a6\"), radius: 4pt, inset: (x: 12pt, y: 10pt))[{}]",
                    inner.trim()
                )
            } else {
                format!(
                    "#block(width: 100%, fill: rgb(\"#f8f9fa\"), stroke: (left: 3pt + rgb(\"#80868b\")), inset: (x: 10pt, y: 8pt))[{}]",
                    inner.trim()
                )
            };
            wrap_with_block_attrs(out, &quote_code, attrs);
            out.push_str("\n\n");
        }
        Block::CodeBlock {
            lang: _,
            code,
            attrs,
        } => {
            let escaped = code.replace("```", "` ` `");
            let raw_block = format!(
                "#block(width: 100%, fill: rgb(\"#f1f3f4\"), inset: 8pt, radius: 3pt)[\n```{}\n```]",
                escaped
            );
            wrap_with_block_attrs(out, &raw_block, attrs);
            out.push_str("\n\n");
        }
        Block::RawTypst { code, attrs } => {
            // 上級者向け: エスケープせずTypstソースへそのまま埋め込む
            wrap_with_block_attrs(out, code, attrs);
            out.push_str("\n\n");
        }
        Block::Spacer { height } => {
            let safe_height = sanitize_length(height, "1em");
            let _ = writeln!(out, "#v({safe_height})\n");
        }
        Block::HorizontalRule => {
            out.push_str("#line(length: 100%, stroke: 0.6pt + rgb(\"#bdc1c6\"))\n\n");
        }
        Block::PageBreak => {
            out.push_str("#pagebreak(weak: true)\n\n");
        }
        Block::TableOfContents => {
            out.push_str("#outline(depth: 3)\n\n");
        }
    }
}

fn render_table(
    out: &mut String,
    headers: &[Vec<Inline>],
    alignments: &[Option<Alignment>],
    rows: &[Vec<Vec<Inline>>],
    attrs: &BlockAttributes,
    base_dir: Option<&Path>,
) {
    let col_count = headers
        .len()
        .max(rows.first().map(|r| r.len()).unwrap_or(1))
        .max(1);

    let columns_spec = if let Some(widths) = &attrs.widths {
        let mut specs = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let raw = widths.get(i).map(|s| s.as_str()).unwrap_or("auto");
            specs.push(sanitize_column_width(raw));
        }
        format!("({})", specs.join(", "))
    } else {
        format!("{}", col_count)
    };

    let align_spec = if alignments.iter().any(|a| a.is_some()) {
        let mut aligns = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let a = alignments.get(i).copied().flatten();
            let s = match a {
                Some(Alignment::Left) => "left + horizon",
                Some(Alignment::Center) => "center + horizon",
                Some(Alignment::Right) => "right + horizon",
                None => "left + horizon",
            };
            aligns.push(s);
        }
        format!("({}),", aligns.join(", "))
    } else {
        "left + horizon,".to_string()
    };

    let mut table_code = String::new();
    let _ = writeln!(
        table_code,
        "#table(\n  columns: {columns_spec},\n  align: {align_spec}"
    );

    if let Some(header_bg) = &attrs.header_bg {
        let safe_bg = sanitize_color_hex(header_bg);
        let _ = writeln!(
            table_code,
            "  fill: (_, y) => if y == 0 {{ rgb(\"{safe_bg}\") }} else {{ none }},"
        );
    }

    if !headers.is_empty() {
        table_code.push_str("  table.header(");
        for cell in headers.iter().take(col_count) {
            render_table_cell(&mut table_code, cell, base_dir, Some("*"), Some("*"));
            table_code.push_str(", ");
        }
        for _ in headers.len()..col_count {
            table_code.push_str("[], ");
        }
        table_code.push_str("),\n");
    }

    for row in rows {
        table_code.push_str("  ");
        for cell in row.iter().take(col_count) {
            render_table_cell(&mut table_code, cell, base_dir, None, None);
            table_code.push_str(", ");
        }
        for _ in row.len()..col_count {
            table_code.push_str("[], ");
        }
        table_code.push('\n');
    }

    table_code.push(')');
    wrap_with_block_attrs(out, &table_code, attrs);
}

/// セル内容が丸ごと `{bg: ...}` で装飾されている場合、そのセル全体の背景色として扱い、
/// `table.cell(fill: ...)` でセル単位の完全な塗りつぶしを行う。
/// それ以外の通常セルは従来通り `[内容]` のコンテンツブロックとして描画する。
fn render_table_cell(
    out: &mut String,
    cell: &[Inline],
    base_dir: Option<&Path>,
    content_prefix: Option<&str>,
    content_suffix: Option<&str>,
) {
    if let [Inline::Styled { content, style }] = cell {
        if let Some(bg) = &style.bg {
            let mut remaining_style = style.clone();
            remaining_style.bg = None;
            let cell_content = if remaining_style == InlineStyle::default() {
                content.clone()
            } else {
                vec![Inline::Styled {
                    content: content.clone(),
                    style: remaining_style,
                }]
            };
            let safe_bg = sanitize_color_hex(bg);
            let _ = write!(out, "table.cell(fill: rgb(\"{safe_bg}\"))[");
            out.push_str(content_prefix.unwrap_or(""));
            render_inlines(out, &cell_content, base_dir);
            out.push_str(content_suffix.unwrap_or(""));
            out.push(']');
            return;
        }
    }
    out.push('[');
    out.push_str(content_prefix.unwrap_or(""));
    render_inlines(out, cell, base_dir);
    out.push_str(content_suffix.unwrap_or(""));
    out.push(']');
}

fn wrap_with_block_attrs(out: &mut String, content: &str, attrs: &BlockAttributes) {
    if attrs.is_empty() {
        out.push_str(content);
        return;
    }

    let mut current = content.to_string();

    if let Some(size) = &attrs.font_size {
        let safe_size = sanitize_length(size, "10.5pt");
        current = format!("#text(size: {safe_size})[{current}]");
    }

    if let Some(lh) = attrs.line_height {
        let leading_em = ((lh - 0.65).clamp(0.25, 3.0) * 100.0).round() / 100.0;
        current = format!("#{{\n  set par(leading: {leading_em}em)\n  [{current}]\n}}");
    }

    if let Some(color) = &attrs.color {
        let safe_color = sanitize_color_hex(color);
        current = format!("#text(fill: rgb(\"{safe_color}\"))[{current}]");
    }

    if let Some(block_width) = &attrs.block_width {
        let safe_width = sanitize_length(block_width, "100%");
        current = format!("#block(width: {safe_width})[{current}]");
    }

    if let Some(align) = attrs.align {
        let align_kw = match align {
            Alignment::Left => "left",
            Alignment::Center => "center",
            Alignment::Right => "right",
        };
        current = format!("#align({align_kw})[{current}]");
    }

    out.push_str(&current);
}

fn render_inlines(out: &mut String, inlines: &[Inline], base_dir: Option<&Path>) {
    for inline in inlines {
        match inline {
            Inline::Text(t) => {
                escape_typst_text(out, t);
            }
            Inline::Strong(children) => {
                out.push_str("#strong[");
                render_inlines(out, children, base_dir);
                out.push(']');
            }
            Inline::Emphasis(children) => {
                out.push_str("#emph[");
                render_inlines(out, children, base_dir);
                out.push(']');
            }
            Inline::Strikethrough(children) => {
                out.push_str("#strike[");
                render_inlines(out, children, base_dir);
                out.push(']');
            }
            Inline::Code(code) => {
                let escaped = code.replace('\\', "\\\\").replace('"', "\\\"");
                let _ = write!(
                    out,
                    "#box(fill: rgb(\"#f1f3f4\"), inset: (x: 3pt, y: 1pt), outset: (y: 2pt), radius: 2pt, raw(\"{escaped}\"))"
                );
            }
            Inline::Styled { content, style } => {
                let mut inner = String::new();
                render_inlines(&mut inner, content, base_dir);

                if style.underline {
                    inner = format!("#underline[{inner}]");
                }

                let mut params = Vec::new();
                if let Some(color) = &style.color {
                    params.push(format!("fill: rgb(\"{}\")", sanitize_color_hex(color)));
                }
                if let Some(size) = &style.size {
                    params.push(format!("size: {}", sanitize_length(size, "10.5pt")));
                }

                if !params.is_empty() {
                    inner = format!("#text({})[{}]", params.join(", "), inner);
                }
                if let Some(bg) = &style.bg {
                    inner = format!(
                        "#highlight(fill: rgb(\"{}\"))[{}]",
                        sanitize_color_hex(bg),
                        inner
                    );
                }
                out.push_str(&inner);
            }
            Inline::Link { text, url } => {
                let safe_url = url.replace('\\', "\\\\").replace('"', "\\\"");
                out.push_str(&format!("#link(\"{safe_url}\")["));
                render_inlines(out, text, base_dir);
                out.push(']');
            }
            Inline::Image { alt, url, width } => {
                render_image_inline(out, alt, url, width.as_deref(), base_dir);
            }
            Inline::SoftBreak => {
                out.push(' ');
            }
            Inline::HardBreak => {
                out.push_str(" \\ \n");
            }
        }
    }
}

fn render_image_inline(
    out: &mut String,
    alt: &str,
    url: &str,
    width: Option<&str>,
    base_dir: Option<&Path>,
) {
    if let Some(resolved) = resolve_local_image_path(url, base_dir) {
        let vpath = encode_local_image_vpath(&resolved);
        let safe_width = sanitize_length(width.unwrap_or("80%"), "80%");
        if alt.trim().is_empty() {
            let _ = write!(
                out,
                "#image(\"/{vpath}\", width: {safe_width}, fit: \"contain\")"
            );
        } else {
            let mut escaped_alt = String::new();
            escape_typst_text(&mut escaped_alt, alt.trim());
            let _ = write!(
                out,
                "#figure(image(\"/{vpath}\", width: {safe_width}, fit: \"contain\"), caption: [{escaped_alt}])"
            );
        }
    } else {
        // 画像パスをまだ入力途中・または該当ファイルが存在しない場合でもエラー停止せずプレースホルダ枠を表示する
        let mut escaped_url = String::new();
        escape_typst_text(&mut escaped_url, url);
        let mut escaped_alt = String::new();
        escape_typst_text(&mut escaped_alt, alt);
        let label = if escaped_alt.is_empty() {
            escaped_url
        } else {
            format!("{escaped_alt} ({escaped_url})")
        };
        let _ = write!(
            out,
            "#block(width: 100%, fill: rgb(\"#f8f9fa\"), stroke: (paint: rgb(\"#9aa0a6\"), thickness: 0.7pt, dash: \"dashed\"), inset: 10pt, radius: 4pt)[#text(fill: rgb(\"#5f6368\"))[🖼 画像未検出: {label}]]"
        );
    }
}

pub fn resolve_local_image_path(url: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return None;
    }

    let candidate = Path::new(trimmed);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else if let Some(base) = base_dir {
        base.join(candidate)
    } else {
        std::env::current_dir().ok()?.join(candidate)
    };

    if resolved.is_file() {
        let ext = resolved
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp"
        ) {
            return Some(resolved);
        }
    }
    None
}

/// Windowsのドライブレター (`C:\...`) や日本語パスを含むファイルパスを
/// Typstの仮想パスとして安全に扱える16進数エンコード文字列に変換する
pub fn encode_local_image_vpath(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    let path_str = path.to_string_lossy();
    let mut hex = String::with_capacity(path_str.len() * 2);
    for b in path_str.as_bytes() {
        let _ = write!(hex, "{b:02x}");
    }
    format!("{LOCAL_IMAGE_VPATH_PREFIX}{hex}.{ext}")
}

pub fn decode_local_image_vpath(vpath_without_slash: &str) -> Option<PathBuf> {
    let rest = vpath_without_slash.strip_prefix(LOCAL_IMAGE_VPATH_PREFIX)?;
    let (hex_part, _ext) = rest.rsplit_once('.').unwrap_or((rest, ""));
    if hex_part.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex_part.len() / 2);
    for i in (0..hex_part.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex_part[i..i + 2], 16).ok()?;
        bytes.push(byte);
    }
    let path_str = String::from_utf8(bytes).ok()?;
    Some(PathBuf::from(path_str))
}

/// `#set page(...)` に渡す `header:` / `footer:` 引数部分を組み立てる。
/// フッター文字列を指定した場合も、既存のページ番号表示（左下実装の `numbering` と同等の
/// "N / 総ページ数" 表示）を失わないよう、フッター文字列とページ番号を左右に並べて表示する。
fn page_header_footer_params(
    header_text: Option<&str>,
    footer_text: Option<&str>,
    show_page_number: bool,
) -> String {
    let mut params = String::new();

    if let Some(header) = header_text {
        let mut safe = String::new();
        escape_typst_text(&mut safe, header);
        let _ = write!(
            params,
            "\n  header: align(right)[#text(size: 8pt, fill: rgb(\"#5f6368\"))[{safe}]],"
        );
    }

    if let Some(footer) = footer_text {
        let mut safe = String::new();
        escape_typst_text(&mut safe, footer);
        if show_page_number {
            let _ = write!(
                params,
                "\n  footer: context [#grid(\n    columns: (1fr, 1fr),\n    align(left)[#text(size: 8pt, fill: rgb(\"#5f6368\"))[{safe}]],\n    align(right)[#text(size: 8pt, fill: rgb(\"#5f6368\"))[#counter(page).display() / #counter(page).final().at(0)]],\n  )],"
            );
        } else {
            // ページ番号を非表示にしている場合は、フッター文字列のみを中央に表示する
            let _ = write!(
                params,
                "\n  footer: align(center)[#text(size: 8pt, fill: rgb(\"#5f6368\"))[{safe}]],"
            );
        }
    }

    params
}

fn escape_typst_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '\\' | '#' | '[' | ']' | '*' | '_' | '`' | '$' | '<' | '>' | '@' | '~' | '=' | '+'
            | '-' | '/' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
}

fn sanitize_ident(raw: &str, default: &str) -> String {
    let s: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if s.is_empty() {
        default.to_string()
    } else {
        s.to_ascii_lowercase()
    }
}

fn sanitize_length(raw: &str, default: &str) -> String {
    let trimmed = raw.trim();
    let valid_units = ["pt", "mm", "cm", "in", "em", "%"];
    for unit in valid_units {
        if let Some(num_part) = trimmed.strip_suffix(unit) {
            if let Ok(v) = num_part.trim().parse::<f32>() {
                if v > 0.0 && v <= 1000.0 {
                    return format!("{v}{unit}");
                }
            }
        }
    }
    default.to_string()
}

fn sanitize_column_width(raw: &str) -> String {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed == "auto" {
        return "auto".to_string();
    }
    if let Some(num_part) = trimmed.strip_suffix("fr") {
        if let Ok(v) = num_part.trim().parse::<f32>() {
            if v > 0.0 {
                return format!("{v}fr");
            }
        }
    }
    if let Some(num_part) = trimmed.strip_suffix('%') {
        if let Ok(v) = num_part.trim().parse::<f32>() {
            if v > 0.0 {
                return format!("{v}fr");
            }
        }
    }
    sanitize_length(&trimmed, "auto")
}

fn sanitize_color_hex(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('#')
        && (trimmed.len() == 4 || trimmed.len() == 7)
        && trimmed[1..].chars().all(|c| c.is_ascii_hexdigit())
    {
        trimmed.to_string()
    } else {
        "#c5221f".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_document;

    #[test]
    fn test_escape_typst_text_escapes_special_chars() {
        let mut out = String::new();
        escape_typst_text(&mut out, "#[a*b]_c_");
        assert_eq!(out, "\\#\\[a\\*b\\]\\_c\\_");
    }

    #[test]
    fn test_sanitize_length_accepts_valid_and_falls_back_on_invalid() {
        assert_eq!(sanitize_length("12pt", "10.5pt"), "12pt");
        assert_eq!(sanitize_length("  20mm ", "20mm"), "20mm");
        assert_eq!(sanitize_length("not-a-length", "10.5pt"), "10.5pt");
        assert_eq!(sanitize_length("-5pt", "10.5pt"), "10.5pt");
        assert_eq!(sanitize_length("9999pt", "10.5pt"), "10.5pt");
    }

    #[test]
    fn test_sanitize_column_width_variants() {
        assert_eq!(sanitize_column_width("auto"), "auto");
        assert_eq!(sanitize_column_width("2fr"), "2fr");
        assert_eq!(sanitize_column_width("30%"), "30fr");
        assert_eq!(sanitize_column_width("garbage"), "auto");
    }

    #[test]
    fn test_sanitize_color_hex_accepts_hex_and_falls_back() {
        assert_eq!(sanitize_color_hex("#1a73e8"), "#1a73e8");
        assert_eq!(sanitize_color_hex("#fff"), "#fff");
        assert_eq!(sanitize_color_hex("not-a-color"), "#c5221f");
    }

    #[test]
    fn test_local_image_vpath_roundtrip() {
        let path = Path::new(r"C:\報告書\図1.png");
        let vpath = encode_local_image_vpath(path);
        assert!(vpath.starts_with(LOCAL_IMAGE_VPATH_PREFIX));
        assert!(vpath.ends_with(".png"));
        let decoded = decode_local_image_vpath(&vpath).expect("should decode");
        assert_eq!(decoded, path);
    }

    #[test]
    fn test_resolve_local_image_path_rejects_remote_urls() {
        assert_eq!(
            resolve_local_image_path("https://example.com/a.png", None),
            None
        );
        assert_eq!(
            resolve_local_image_path("http://example.com/a.png", None),
            None
        );
        assert_eq!(resolve_local_image_path("", None), None);
    }

    #[test]
    fn test_generate_typst_source_missing_image_renders_placeholder() {
        let doc = parse_document("![caption](does_not_exist.png)\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("画像未検出"));
    }

    #[test]
    fn test_generate_typst_source_escapes_inline_text() {
        let doc = parse_document("価格は#100[税込]*です*\n");
        let code = generate_typst_source(&doc);
        assert!(!code.contains("価格は#100"));
    }

    #[test]
    fn test_accent_color_applied_to_heading_show_rules() {
        let src = "---\naccent-color: #e37400\n---\n\n# 見出し\n";
        let doc = parse_document(src);
        let code = generate_typst_source(&doc);
        assert!(code.contains("rgb(\"#e37400\")"));
        assert!(!code.contains("#1a73e8"));
    }

    #[test]
    fn test_block_color_attribute_wraps_text_fill() {
        let doc = parse_document("{color: red}\n段落\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("#text(fill: rgb(\"#c5221f\"))"));
    }

    #[test]
    fn test_block_width_attribute_wraps_block_and_align() {
        let doc = parse_document("{align: center, block-width: 60%}\n段落\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("#block(width: 60%)"));
        assert!(code.contains("#align(center)"));
    }

    #[test]
    fn test_table_header_bg_attribute_overrides_fill() {
        let doc = parse_document("{header-bg: #fdf0e6}\n| A |\n| :--- |\n| 1 |\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("fill: (_, y) => if y == 0 { rgb(\"#fdf0e6\") } else { none }"));
    }

    #[test]
    fn test_table_cell_whole_bg_renders_as_table_cell_fill_not_highlight() {
        // セル内容が丸ごと {bg:...} の場合は #highlight() ではなく
        // table.cell(fill: ...) によるセル単位の完全な塗りつぶしになる
        let doc =
            parse_document("| A | B |\n| :--- | :--- |\n| [全体塗り]{bg: #fff2cc} | 普通セル |\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("table.cell(fill: rgb(\"#fff2cc\"))[全体塗り]"));
        assert!(!code.contains("#highlight"));
    }

    #[test]
    fn test_toc_directive_generates_typst_outline_call() {
        let doc = parse_document("# 見出し1\n\n:::toc\n\n## 見出し2\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("#outline(depth: 3)"));
    }

    #[test]
    fn test_page_number_off_generates_none_numbering() {
        let doc = parse_document("---\npage-number: off\n---\n\n本文\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("numbering: none,"));
        assert!(!code.contains("numbering: \"1 / 1\""));
    }

    #[test]
    fn test_page_number_default_generates_1_of_1_numbering() {
        let doc = parse_document("本文\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("numbering: \"1 / 1\","));
    }

    #[test]
    fn test_page_number_off_with_footer_omits_page_counter() {
        let doc = parse_document("---\nfooter: 社外秘\npage-number: off\n---\n\n本文\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("footer: align(center)"));
        assert!(!code.contains("counter(page)"));
    }

    #[test]
    fn test_underline_style_wraps_with_underline_call() {
        let doc = parse_document("[下線]{underline}\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("#underline["));
    }

    #[test]
    fn test_image_width_attribute_overrides_default() {
        let dir = std::env::temp_dir().join(format!(
            "typer_test_img_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("sample.png");
        std::fs::write(&img_path, b"not a real png, just needs to exist").unwrap();

        let doc = parse_document("![図](sample.png){width: 50%}\n");
        let code = generate_typst_source_with_base(&doc, Some(&dir));
        assert!(code.contains("width: 50%"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_image_without_width_uses_default_80_percent() {
        let dir = std::env::temp_dir().join(format!(
            "typer_test_img2_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("sample.png");
        std::fs::write(&img_path, b"not a real png, just needs to exist").unwrap();

        let doc = parse_document("![図](sample.png)\n");
        let code = generate_typst_source_with_base(&doc, Some(&dir));
        assert!(code.contains("width: 80%"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_spacer_block_emits_v_call() {
        let doc = parse_document(":::space 2em\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("#v(2em)"));
    }

    #[test]
    fn test_boxed_blockquote_uses_full_border_style() {
        let doc = parse_document("{box}\n> 引用\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("stroke: 0.8pt + rgb(\"#9aa0a6\"), radius: 4pt"));
    }

    #[test]
    fn test_typst_fenced_code_block_passes_through_unescaped() {
        let doc = parse_document("```typst\n#line(length: 50%, stroke: 1pt)\n```\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("#line(length: 50%, stroke: 1pt)"));
    }

    #[test]
    fn test_ordered_list_default_start_uses_plain_enum_shorthand() {
        let doc = parse_document("1. 一番目\n2. 二番目\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("+ 一番目"));
        assert!(code.contains("+ 二番目"));
        assert!(!code.contains("#enum(start:"));
    }

    #[test]
    fn test_ordered_list_custom_start_uses_enum_function_form() {
        let doc = parse_document("5. 五番目\n6. 六番目\n");
        let code = generate_typst_source(&doc);
        assert!(code.contains("#enum(start: 5)"));
        assert!(code.contains("[五番目]"));
        assert!(code.contains("[六番目]"));
    }
}
