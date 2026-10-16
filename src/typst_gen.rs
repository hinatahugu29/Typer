//! 内部文書モデル (`Document`) から Typst マークアップ文字列への変換器
//!
//! 日本語の社内報告書・業務文書に適した組版ルール（フォント、余白、見出し直後の改ページ抑制、
//! 表の列幅・ヘッダ背景色・セル折り返しなど）を適用したTypstコードを生成する。

use crate::model::{
    Alignment, Block, BlockAttributes, Document, FontChoice, Inline, Orientation,
};
use std::fmt::Write;

pub fn generate_typst_source(doc: &Document) -> String {
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

    let font_list = match s.font_choice {
        FontChoice::Gothic => {
            r#"("BIZ UDPGothic", "BIZ UDGothic", "Yu Gothic", "Meiryo", "MS Gothic")"#
        }
        FontChoice::Mincho => {
            r#"("BIZ UDPMincho", "BIZ UDMincho", "Yu Mincho", "MS Mincho", "Yu Gothic")"#
        }
    };

    // ページ・基本組版設定プリアンブル
    let _ = writeln!(
        out,
        r##"#set page(
  paper: "{paper}",
  flipped: {flipped},
  margin: {margin},
  numbering: "1 / 1",
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
  stroke: (bottom: 1.2pt + rgb("#1a73e8")),
  inset: (bottom: 5pt),
  text(size: 1.45em, weight: "bold", fill: rgb("#174ea6"), it.body)
)
#show heading.where(level: 2): it => block(
  width: 100%,
  above: 1.35em,
  below: 0.7em,
  stroke: (bottom: 0.7pt + rgb("#dadce0"), left: 3.5pt + rgb("#1a73e8")),
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
        render_block(&mut out, block);
    }

    out
}

fn render_block(out: &mut String, block: &Block) {
    match block {
        Block::Heading {
            level,
            content,
            attrs,
        } => {
            let prefix = "=".repeat((*level as usize).clamp(1, 6));
            let mut inner = String::new();
            let _ = write!(inner, "{prefix} ");
            render_inlines(&mut inner, content);
            wrap_with_block_attrs(out, &inner, attrs);
            out.push_str("\n\n");
        }
        Block::Paragraph { content, attrs } => {
            let mut inner = String::new();
            render_inlines(&mut inner, content);
            wrap_with_block_attrs(out, &inner, attrs);
            out.push_str("\n\n");
        }
        Block::List {
            ordered,
            start: _,
            items,
            attrs,
        } => {
            let mut inner = String::new();
            for item in items {
                if *ordered {
                    inner.push_str("+ ");
                } else {
                    inner.push_str("- ");
                }
                render_inlines(&mut inner, item);
                inner.push('\n');
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
            render_table(out, headers, alignments, rows, attrs);
            out.push_str("\n\n");
        }
        Block::BlockQuote {
            blocks: sub_blocks,
            attrs,
        } => {
            let mut inner = String::new();
            for b in sub_blocks {
                render_block(&mut inner, b);
            }
            let quote_code = format!(
                "#block(width: 100%, fill: rgb(\"#f8f9fa\"), stroke: (left: 3pt + rgb(\"#80868b\")), inset: (x: 10pt, y: 8pt))[{}]",
                inner.trim()
            );
            wrap_with_block_attrs(out, &quote_code, attrs);
            out.push_str("\n\n");
        }
        Block::CodeBlock { lang: _, code, attrs } => {
            let escaped = code.replace("```", "` ` `");
            let raw_block = format!(
                "#block(width: 100%, fill: rgb(\"#f1f3f4\"), inset: 8pt, radius: 3pt)[\n```{}\n```]",
                escaped
            );
            wrap_with_block_attrs(out, &raw_block, attrs);
            out.push_str("\n\n");
        }
        Block::HorizontalRule => {
            out.push_str("#line(length: 100%, stroke: 0.6pt + rgb(\"#bdc1c6\"))\n\n");
        }
        Block::PageBreak => {
            out.push_str("#pagebreak(weak: true)\n\n");
        }
    }
}

fn render_table(
    out: &mut String,
    headers: &[Vec<Inline>],
    alignments: &[Option<Alignment>],
    rows: &[Vec<Vec<Inline>>],
    attrs: &BlockAttributes,
) {
    let col_count = headers
        .len()
        .max(rows.first().map(|r| r.len()).unwrap_or(1))
        .max(1);

    // 列幅指定の組み立て
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

    // 列ごとの文字揃え指定
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

    if !headers.is_empty() {
        table_code.push_str("  table.header(");
        for cell in headers.iter().take(col_count) {
            table_code.push_str("[*");
            render_inlines(&mut table_code, cell);
            table_code.push_str("*], ");
        }
        for _ in headers.len()..col_count {
            table_code.push_str("[], ");
        }
        table_code.push_str("),\n");
    }

    for row in rows {
        table_code.push_str("  ");
        for cell in row.iter().take(col_count) {
            table_code.push('[');
            render_inlines(&mut table_code, cell);
            table_code.push_str("], ");
        }
        for _ in row.len()..col_count {
            table_code.push_str("[], ");
        }
        table_code.push('\n');
    }

    table_code.push(')');
    wrap_with_block_attrs(out, &table_code, attrs);
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

fn render_inlines(out: &mut String, inlines: &[Inline]) {
    for inline in inlines {
        match inline {
            Inline::Text(t) => {
                escape_typst_text(out, t);
            }
            Inline::Strong(children) => {
                out.push_str("#strong[");
                render_inlines(out, children);
                out.push(']');
            }
            Inline::Emphasis(children) => {
                out.push_str("#emph[");
                render_inlines(out, children);
                out.push(']');
            }
            Inline::Strikethrough(children) => {
                out.push_str("#strike[");
                render_inlines(out, children);
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
                render_inlines(&mut inner, content);

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
                render_inlines(out, text);
                out.push(']');
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
    // パーセント指定はTypstの比率指定 (fr) に変換すると余白を除いた表幅100%に対して綺麗に配分される
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
