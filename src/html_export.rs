//! 内部文書モデル (`Document`) から連続表示用単一HTMLファイルへの変換器
//!
//! ページ境界（改ページ指定等）に依存しない、ブラウザ上で読みやすいHTML+CSSを生成する。
//! ローカル画像が存在する場合はBase64データURIとしてHTML内に直接埋め込み、単一ファイルで完結させる。

use crate::model::{Alignment, Block, BlockAttributes, Document, FontChoice, Inline, InlineStyle};
use crate::typst_gen::resolve_local_image_path;
use std::fmt::Write;
use std::path::Path;

#[cfg(test)]
pub fn generate_html(doc: &Document) -> String {
    generate_html_with_base(doc, None)
}

pub fn generate_html_with_base(doc: &Document, base_dir: Option<&Path>) -> String {
    let mut body = String::with_capacity(4096);

    let toc_entries = collect_toc_entries(&doc.blocks);
    let mut heading_counter = 0usize;
    for block in &doc.blocks {
        render_html_block(
            &mut body,
            block,
            base_dir,
            &toc_entries,
            &mut heading_counter,
        );
    }

    let s = &doc.settings;
    let font_family = match s.font_choice {
        FontChoice::Gothic => {
            r#""BIZ UDPGothic", "Yu Gothic UI", "Yu Gothic", "Meiryo", sans-serif"#
        }
        FontChoice::Mincho => r#""BIZ UDPMincho", "Yu Mincho", "MS PMincho", serif"#,
    };
    let accent_color = sanitize_color_hex(&s.accent_color);
    let columns_css = if s.columns > 1 {
        format!(
            "column-count: {}; column-gap: 2.5rem;",
            s.columns.clamp(1, 4)
        )
    } else {
        String::new()
    };
    let watermark_html = render_watermark(s.watermark.as_deref());

    format!(
        r#"<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>文書出力</title>
<style>
  :root {{
    --text-color: #202124;
    --accent-color: {accent_color};
    --border-color: #dadce0;
    --table-border: #9aa0a6;
    --header-bg: #f1f3f4;
  }}
  body {{
    font-family: {font_family};
    font-size: {font_size};
    line-height: {line_height};
    color: var(--text-color);
    background-color: #f8f9fa;
    margin: 0;
    padding: 2rem 1rem;
  }}
  .container {{
    position: relative;
    max-width: 840px;
    margin: 0 auto;
    background: #ffffff;
    padding: 2.5rem 3rem;
    border-radius: 8px;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.08);
    {columns_css}
  }}
  .doc-watermark {{
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%) rotate(-30deg);
    font-size: 5rem;
    font-weight: bold;
    color: rgba(154, 160, 166, 0.3);
    white-space: nowrap;
    pointer-events: none;
    z-index: 0;
  }}
  h1 {{
    font-size: 1.5em;
    color: var(--accent-color);
    border-bottom: 2px solid var(--accent-color);
    padding-bottom: 0.3em;
    margin-top: 1.5em;
    margin-bottom: 0.8em;
  }}
  h2 {{
    font-size: 1.25em;
    border-left: 4px solid var(--accent-color);
    border-bottom: 1px solid var(--border-color);
    padding: 0.15em 0 0.25em 0.5em;
    margin-top: 1.4em;
    margin-bottom: 0.7em;
  }}
  h3 {{
    font-size: 1.1em;
    color: #3c4043;
    margin-top: 1.2em;
    margin-bottom: 0.5em;
  }}
  p {{
    margin: 0.75em 0;
  }}
  table {{
    width: 100%;
    border-collapse: collapse;
    margin: 1em 0;
    table-layout: fixed;
  }}
  th, td {{
    border: 1px solid var(--table-border);
    padding: 0.5rem 0.65rem;
    vertical-align: middle;
    word-wrap: break-word;
  }}
  th {{
    background-color: var(--header-bg);
    font-weight: bold;
  }}
  blockquote {{
    margin: 1em 0;
    padding: 0.6rem 1rem;
    background: #f8f9fa;
    border-left: 4px solid #80868b;
  }}
  blockquote.boxed-quote {{
    background: #ffffff;
    border: 1px solid #9aa0a6;
    border-radius: 4px;
    border-left: 1px solid #9aa0a6;
    padding: 0.8rem 1rem;
  }}
  pre {{
    background: #f1f3f4;
    padding: 0.8rem 1rem;
    border-radius: 4px;
    overflow-x: auto;
  }}
  code {{
    background: #f1f3f4;
    padding: 0.1rem 0.3rem;
    border-radius: 3px;
    font-family: Consolas, monospace;
  }}
  figure.doc-image {{
    margin: 1.2em 0;
    text-align: center;
  }}
  figure.doc-image img {{
    max-width: 85%;
    height: auto;
    border-radius: 4px;
    border: 1px solid var(--border-color);
  }}
  figure.doc-image figcaption {{
    font-size: 0.9em;
    color: #5f6368;
    margin-top: 0.4em;
  }}
  hr {{
    border: none;
    border-top: 1px solid var(--border-color);
    margin: 1.5em 0;
  }}
  .raw-typst-note {{
    margin: 1em 0;
    border: 1px dashed #9aa0a6;
    border-radius: 4px;
    padding: 0.6rem 1rem;
    background: #f8f9fa;
  }}
  .raw-typst-note .raw-typst-label {{
    margin: 0 0 0.4em 0;
    font-size: 0.85em;
    color: #5f6368;
  }}
  .raw-typst-note pre {{
    margin: 0;
  }}
  .toc {{
    margin: 1em 0 1.5em 0;
    padding: 0.8rem 1.2rem;
    border: 1px solid var(--border-color);
    border-radius: 4px;
    background: #f8f9fa;
  }}
  .toc .toc-title {{
    margin: 0 0 0.5em 0;
    font-weight: bold;
    color: var(--accent-color);
  }}
  .toc ul {{
    list-style: none;
    margin: 0;
    padding: 0;
  }}
  .toc li {{
    margin: 0.25em 0;
  }}
  .toc li a {{
    text-decoration: none;
    color: var(--text-color);
  }}
  .toc li a:hover {{
    text-decoration: underline;
  }}
  .toc .toc-level-2 {{ padding-left: 1.2em; }}
  .toc .toc-level-3 {{ padding-left: 2.4em; }}
  .toc .toc-level-4 {{ padding-left: 3.6em; }}
  .toc .toc-level-5 {{ padding-left: 4.8em; }}
  .toc .toc-level-6 {{ padding-left: 6em; }}
  .doc-header, .doc-footer {{
    font-size: 0.8em;
    color: #5f6368;
    padding: 0.4em 0;
  }}
  .doc-header {{
    text-align: right;
    border-bottom: 1px solid var(--border-color);
    margin-bottom: 1em;
  }}
  .doc-footer {{
    text-align: center;
    border-top: 1px solid var(--border-color);
    margin-top: 1em;
  }}
</style>
</head>
<body>
<div class="container">
{watermark}{header}{body}{footer}</div>
</body>
</html>
"#,
        font_family = font_family,
        font_size = html_escape(&s.font_size),
        line_height = s.line_height,
        header = render_doc_header_footer("doc-header", s.header_text.as_deref()),
        body = body,
        footer = render_doc_header_footer("doc-footer", s.footer_text.as_deref()),
        watermark = watermark_html,
        columns_css = columns_css,
    )
}

/// 文書共通の透かし文字を本文の背面に表示する（1文書につき1箇所、CSSで回転・半透明表示）
fn render_watermark(text: Option<&str>) -> String {
    match text {
        Some(t) if !t.trim().is_empty() => {
            format!("<div class=\"doc-watermark\">{}</div>\n", html_escape(t.trim()))
        }
        _ => String::new(),
    }
}

/// 文書共通のヘッダー・フッター文字列をHTMLの一箇所に描画する（HTMLはページ概念を持たないため、
/// PDFのような各ページ反復ではなく、文書全体の先頭・末尾に1回だけ表示する）
fn render_doc_header_footer(class_name: &str, text: Option<&str>) -> String {
    match text {
        Some(t) if !t.is_empty() => {
            format!("<div class=\"{class_name}\">{}</div>\n", html_escape(t))
        }
        _ => String::new(),
    }
}

fn render_html_block(
    out: &mut String,
    block: &Block,
    base_dir: Option<&Path>,
    toc_entries: &[TocEntry],
    heading_counter: &mut usize,
) {
    match block {
        Block::Heading {
            level,
            content,
            attrs,
        } => {
            let lvl = (*level).clamp(1, 6);
            let style = block_style_attr(attrs);
            let id = toc_entries
                .get(*heading_counter)
                .map(|e| e.id.as_str())
                .unwrap_or_default();
            *heading_counter += 1;
            let _ = write!(out, "<h{lvl} id=\"{id}\"{style}>");
            render_html_inlines(out, content, base_dir);
            let _ = writeln!(out, "</h{lvl}>");
        }
        Block::Paragraph { content, attrs } => {
            let style = block_style_attr(attrs);
            let _ = write!(out, "<p{style}>");
            render_html_inlines(out, content, base_dir);
            let _ = writeln!(out, "</p>");
        }
        Block::List {
            ordered,
            start,
            items,
            attrs,
        } => {
            let style = block_style_attr(attrs);
            if *ordered {
                let _ = writeln!(out, "<ol start=\"{start}\"{style}>");
            } else {
                let _ = writeln!(out, "<ul{style}>");
            }
            for item in items {
                out.push_str("  <li>");
                render_html_inlines(out, item, base_dir);
                out.push_str("</li>\n");
            }
            if *ordered {
                out.push_str("</ol>\n");
            } else {
                out.push_str("</ul>\n");
            }
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
            out.push_str("<table>\n");
            if let Some(widths) = &attrs.widths {
                out.push_str("  <colgroup>\n");
                for i in 0..col_count {
                    if let Some(w) = widths.get(i) {
                        let w_trim = w.trim();
                        if w_trim != "auto" {
                            let _ = writeln!(
                                out,
                                "    <col style=\"width: {};\">",
                                html_escape(w_trim)
                            );
                            continue;
                        }
                    }
                    out.push_str("    <col>\n");
                }
                out.push_str("  </colgroup>\n");
            }
            if !headers.is_empty() {
                let header_bg_style = attrs
                    .header_bg
                    .as_ref()
                    .map(|c| format!(" style=\"background-color: {}\"", sanitize_color_hex(c)))
                    .unwrap_or_default();
                let _ = write!(out, "  <thead>\n    <tr{header_bg_style}>\n");
                for (i, cell) in headers.iter().enumerate() {
                    let align_style = cell_align_attr(alignments.get(i).copied().flatten());
                    let _ = write!(out, "      <th{align_style}>");
                    render_html_inlines(out, cell, base_dir);
                    out.push_str("</th>\n");
                }
                out.push_str("    </tr>\n  </thead>\n");
            }
            out.push_str("  <tbody>\n");
            for row in rows {
                out.push_str("    <tr>\n");
                for (i, cell) in row.iter().enumerate() {
                    let align = alignments.get(i).copied().flatten();
                    render_html_table_cell(out, cell, align, base_dir);
                }
                out.push_str("    </tr>\n");
            }
            out.push_str("  </tbody>\n</table>\n");
        }
        Block::BlockQuote { blocks, attrs } => {
            let style = block_style_attr(attrs);
            let class_attr = if attrs.boxed {
                " class=\"boxed-quote\""
            } else {
                ""
            };
            let _ = writeln!(out, "<blockquote{class_attr}{style}>");
            for b in blocks {
                render_html_block(out, b, base_dir, toc_entries, heading_counter);
            }
            out.push_str("</blockquote>\n");
        }
        Block::CodeBlock {
            lang: _,
            code,
            attrs,
        } => {
            let style = block_style_attr(attrs);
            let _ = writeln!(out, "<pre{style}><code>{}</code></pre>", html_escape(code));
        }
        Block::RawTypst { code, attrs: _ } => {
            // HTML出力ではTypstコードを評価できないため、注記付きでソースをそのまま表示する
            let _ = writeln!(
                out,
                "<div class=\"raw-typst-note\"><p class=\"raw-typst-label\">⚠ Typstコード（HTML出力では実行されません。PDF出力時のみ反映されます）</p><pre><code>{}</code></pre></div>",
                html_escape(code)
            );
        }
        Block::Spacer { height } => {
            let _ = writeln!(out, "<div style=\"height: {}\"></div>", html_escape(height));
        }
        Block::HorizontalRule => {
            out.push_str("<hr>\n");
        }
        Block::PageBreak => {
            // 要件6.2: PDF専用の改ページ指定はHTML出力時に原則無視する
        }
        Block::TableOfContents => {
            render_toc(out, toc_entries);
        }
        Block::Columns { count, blocks } => {
            let safe_count = (*count).clamp(2, 4);
            let _ = writeln!(
                out,
                "<div style=\"column-count: {safe_count}; column-gap: 2rem;\">"
            );
            for b in blocks {
                render_html_block(out, b, base_dir, toc_entries, heading_counter);
            }
            out.push_str("</div>\n");
        }
    }
}

/// 目次に収録する見出しの情報（レベル・表示テキスト・対応する見出し要素のid）
struct TocEntry {
    level: u8,
    text: String,
    id: String,
}

/// 文書内の見出し（引用ブロック内も含む）を出現順に収集し、各見出しへ付与するidを採番する
fn collect_toc_entries(blocks: &[Block]) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    collect_toc_entries_into(blocks, &mut entries);
    entries
}

fn collect_toc_entries_into(blocks: &[Block], out: &mut Vec<TocEntry>) {
    for block in blocks {
        match block {
            Block::Heading { level, content, .. } => {
                let id = format!("toc-heading-{}", out.len());
                out.push(TocEntry {
                    level: *level,
                    text: crate::parser::inlines_to_plain_text(content),
                    id,
                });
            }
            Block::BlockQuote { blocks, .. } => collect_toc_entries_into(blocks, out),
            Block::Columns { blocks, .. } => collect_toc_entries_into(blocks, out),
            _ => {}
        }
    }
}

/// `:::toc` 挿入位置に、見出し一覧への入れ子リンクリストとして目次を描画する
fn render_toc(out: &mut String, toc_entries: &[TocEntry]) {
    if toc_entries.is_empty() {
        return;
    }
    out.push_str("<nav class=\"toc\">\n  <p class=\"toc-title\">目次</p>\n  <ul>\n");
    for entry in toc_entries {
        let indent_class = format!(" class=\"toc-level-{}\"", entry.level.clamp(1, 6));
        let _ = writeln!(
            out,
            "    <li{indent_class}><a href=\"#{}\">{}</a></li>",
            entry.id,
            html_escape(&entry.text)
        );
    }
    out.push_str("  </ul>\n</nav>\n");
}

fn block_style_attr(attrs: &BlockAttributes) -> String {
    let mut rules = Vec::new();
    if let Some(align) = attrs.align {
        let a = match align {
            Alignment::Left => "left",
            Alignment::Center => "center",
            Alignment::Right => "right",
        };
        rules.push(format!("text-align: {a}"));
    }
    if let Some(lh) = attrs.line_height {
        rules.push(format!("line-height: {lh}"));
    }
    if let Some(size) = &attrs.font_size {
        rules.push(format!("font-size: {}", html_escape(size)));
    }
    if let Some(color) = &attrs.color {
        rules.push(format!("color: {}", sanitize_color_hex(color)));
    }
    if let Some(block_width) = &attrs.block_width {
        rules.push(format!("width: {}", html_escape(block_width)));
        // ブロック自体の水平位置をTypst側の `#align` と揃える
        // （text-align は内側のテキスト寄せのみを担当するため）
        match attrs.align {
            Some(Alignment::Center) => {
                rules.push("margin-left: auto; margin-right: auto".to_string())
            }
            Some(Alignment::Right) => rules.push("margin-left: auto".to_string()),
            _ => {}
        }
    }
    if rules.is_empty() {
        String::new()
    } else {
        format!(" style=\"{}\"", rules.join("; "))
    }
}

fn cell_align_attr(align: Option<Alignment>) -> &'static str {
    match align {
        Some(Alignment::Left) => " style=\"text-align: left\"",
        Some(Alignment::Center) => " style=\"text-align: center\"",
        Some(Alignment::Right) => " style=\"text-align: right\"",
        None => "",
    }
}

fn cell_align_css(align: Option<Alignment>) -> Option<&'static str> {
    match align {
        Some(Alignment::Left) => Some("text-align: left"),
        Some(Alignment::Center) => Some("text-align: center"),
        Some(Alignment::Right) => Some("text-align: right"),
        None => None,
    }
}

/// セル内容が丸ごと `{bg: ...}` で装飾されている場合は、`<td>` 自体の背景色として扱い
/// セル単位の完全な塗りつぶしを行う。それ以外は従来通り内容をそのまま描画する。
fn render_html_table_cell(
    out: &mut String,
    cell: &[Inline],
    align: Option<Alignment>,
    base_dir: Option<&Path>,
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
            let mut css = format!("background-color: {}", sanitize_color_hex(bg));
            if let Some(align_css) = cell_align_css(align) {
                css.push_str("; ");
                css.push_str(align_css);
            }
            let _ = write!(out, "      <td style=\"{css}\">");
            render_html_inlines(out, &cell_content, base_dir);
            out.push_str("</td>\n");
            return;
        }
    }
    let align_style = cell_align_attr(align);
    let _ = write!(out, "      <td{align_style}>");
    render_html_inlines(out, cell, base_dir);
    out.push_str("</td>\n");
}

fn render_html_inlines(out: &mut String, inlines: &[Inline], base_dir: Option<&Path>) {
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push_str(&html_escape(t)),
            Inline::Strong(c) => {
                out.push_str("<strong>");
                render_html_inlines(out, c, base_dir);
                out.push_str("</strong>");
            }
            Inline::Emphasis(c) => {
                out.push_str("<em>");
                render_html_inlines(out, c, base_dir);
                out.push_str("</em>");
            }
            Inline::Strikethrough(c) => {
                out.push_str("<del>");
                render_html_inlines(out, c, base_dir);
                out.push_str("</del>");
            }
            Inline::Code(code) => {
                let _ = write!(out, "<code>{}</code>", html_escape(code));
            }
            Inline::Styled { content, style } => {
                let mut css = Vec::new();
                if let Some(c) = &style.color {
                    css.push(format!("color: {}", html_escape(c)));
                }
                if let Some(s) = &style.size {
                    css.push(format!("font-size: {}", html_escape(s)));
                }
                if let Some(bg) = &style.bg {
                    css.push(format!("background-color: {}", html_escape(bg)));
                }
                if style.underline {
                    css.push("text-decoration: underline".to_string());
                }
                if css.is_empty() {
                    render_html_inlines(out, content, base_dir);
                } else {
                    let _ = write!(out, "<span style=\"{}\">", css.join("; "));
                    render_html_inlines(out, content, base_dir);
                    out.push_str("</span>");
                }
            }
            Inline::Link { text, url } => {
                let _ = write!(out, "<a href=\"{}\">", html_escape(url));
                render_html_inlines(out, text, base_dir);
                out.push_str("</a>");
            }
            Inline::Image { alt, url, width } => {
                let width_style = width
                    .as_ref()
                    .map(|w| format!(" style=\"width: {}; max-width: 100%;\"", html_escape(w)))
                    .unwrap_or_default();
                let src_attr = if let Some(resolved) = resolve_local_image_path(url, base_dir) {
                    if let Ok(bytes) = std::fs::read(&resolved) {
                        let mime = match resolved
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .as_str()
                        {
                            "jpg" | "jpeg" => "image/jpeg",
                            "gif" => "image/gif",
                            "svg" => "image/svg+xml",
                            "webp" => "image/webp",
                            _ => "image/png",
                        };
                        format!("data:{mime};base64,{}", encode_base64(&bytes))
                    } else {
                        html_escape(url)
                    }
                } else {
                    html_escape(url)
                };

                if alt.trim().is_empty() {
                    let _ = write!(
                        out,
                        "<figure class=\"doc-image\"><img src=\"{src_attr}\" alt=\"\"{width_style}></figure>"
                    );
                } else {
                    let escaped_alt = html_escape(alt.trim());
                    let _ = write!(
                        out,
                        "<figure class=\"doc-image\"><img src=\"{src_attr}\" alt=\"{escaped_alt}\"{width_style}><figcaption>{escaped_alt}</figcaption></figure>"
                    );
                }
            }
            Inline::SoftBreak => out.push('\n'),
            Inline::HardBreak => out.push_str("<br>\n"),
        }
    }
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// `#xxxxxx` / `#xxx` 形式のみ許可し、不正な値は既定のアクセントカラーにフォールバックする
fn sanitize_color_hex(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('#')
        && (trimmed.len() == 4 || trimmed.len() == 7)
        && trimmed[1..].chars().all(|c| c.is_ascii_hexdigit())
    {
        trimmed.to_string()
    } else {
        "#1a73e8".to_string()
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_document;

    #[test]
    fn test_html_escape_escapes_all_special_chars() {
        assert_eq!(html_escape(r#"<a>&"'"#), "&lt;a&gt;&amp;&quot;&#39;");
        assert_eq!(html_escape("普通の日本語"), "普通の日本語");
    }

    #[test]
    fn test_encode_base64_matches_known_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn test_generate_html_escapes_text_and_wraps_heading() {
        let doc = parse_document("# <タイトル>\n\n本文&説明\n");
        let html = generate_html(&doc);
        assert!(html.contains("<h1 id=\"toc-heading-0\">&lt;タイトル&gt;</h1>"));
        assert!(html.contains("<p>本文&amp;説明</p>"));
    }

    #[test]
    fn test_generate_html_table_with_widths_emits_colgroup() {
        let doc =
            parse_document("{widths: 30%, 70%}\n| 項目 | 内容 |\n| :--- | :--- |\n| A | B |\n");
        let html = generate_html(&doc);
        assert!(html.contains("<colgroup>"));
        assert!(html.contains("width: 30%;"));
        assert!(html.contains("項目</th>"));
    }

    #[test]
    fn test_generate_html_missing_image_falls_back_to_escaped_url() {
        let doc = parse_document("![代替テキスト](does_not_exist.png)\n");
        let html = generate_html(&doc);
        assert!(html.contains("<img src=\"does_not_exist.png\""));
        assert!(html.contains("代替テキスト"));
    }

    #[test]
    fn test_generate_html_styled_inline_emits_inline_style() {
        let doc = parse_document("[重要]{red}\n");
        let html = generate_html(&doc);
        assert!(html.contains("style=\"color: #c5221f\""));
        assert!(html.contains("重要"));
    }

    #[test]
    fn test_accent_color_applied_to_css_variable() {
        let src = "---\naccent-color: #8430ce\n---\n\n本文\n";
        let doc = parse_document(src);
        let html = generate_html(&doc);
        assert!(html.contains("--accent-color: #8430ce;"));
    }

    #[test]
    fn test_block_color_attribute_emits_css_color() {
        let doc = parse_document("{color: red}\n段落\n");
        let html = generate_html(&doc);
        assert!(html.contains("color: #c5221f"));
    }

    #[test]
    fn test_block_width_attribute_emits_css_width_and_centers() {
        let doc = parse_document("{align: center, block-width: 60%}\n段落\n");
        let html = generate_html(&doc);
        assert!(html.contains("width: 60%"));
        assert!(html.contains("margin-left: auto; margin-right: auto"));
    }

    #[test]
    fn test_block_width_without_center_align_has_no_auto_margin() {
        let doc = parse_document("{block-width: 60%}\n段落\n");
        let html = generate_html(&doc);
        assert!(html.contains("width: 60%"));
        assert!(!html.contains("margin-left"));
    }

    #[test]
    fn test_table_header_bg_attribute_emits_inline_style() {
        let doc = parse_document("{header-bg: #fdf0e6}\n| A |\n| :--- |\n| 1 |\n");
        let html = generate_html(&doc);
        assert!(html.contains("<tr style=\"background-color: #fdf0e6\">"));
    }

    #[test]
    fn test_table_cell_whole_bg_emits_td_background_color() {
        // セル内容が丸ごと {bg:...} の場合は <span> ハイライトではなく
        // <td> 自体の background-color として塗りつぶされる
        let doc =
            parse_document("| A | B |\n| :--- | :--- |\n| [全体塗り]{bg: #fff2cc} | 普通セル |\n");
        let html = generate_html(&doc);
        assert!(
            html.contains(
                "<td style=\"background-color: #fff2cc; text-align: left\">全体塗り</td>"
            )
        );
    }

    #[test]
    fn test_toc_directive_renders_linked_heading_list() {
        let doc = parse_document("# 第一章\n\n:::toc\n\n## 第一節\n\n本文です。\n");
        let html = generate_html(&doc);
        // 見出し自体にidが付与され、目次はそのidへのアンカーリンクになっている
        assert!(html.contains("<h1 id=\"toc-heading-0\">第一章</h1>"));
        assert!(html.contains("<h2 id=\"toc-heading-1\">第一節</h2>"));
        assert!(html.contains("<a href=\"#toc-heading-0\">第一章</a>"));
        assert!(html.contains("<a href=\"#toc-heading-1\">第一節</a>"));
        assert!(html.contains("class=\"toc-level-2\""));
    }

    #[test]
    fn test_toc_directive_with_no_headings_renders_nothing() {
        let doc = parse_document(":::toc\n\n本文のみの文書です。\n");
        let html = generate_html(&doc);
        assert!(!html.contains("class=\"toc\""));
    }

    #[test]
    fn test_header_and_footer_text_render_once_at_document_ends() {
        let doc = parse_document("---\nheader: 社内限定資料\nfooter: 社外秘\n---\n\n本文です。\n");
        let html = generate_html(&doc);
        assert!(html.contains("<div class=\"doc-header\">社内限定資料</div>"));
        assert!(html.contains("<div class=\"doc-footer\">社外秘</div>"));
    }

    #[test]
    fn test_no_header_footer_divs_when_unset() {
        let doc = parse_document("本文のみ\n");
        let html = generate_html(&doc);
        assert!(!html.contains("<div class=\"doc-header\">"));
        assert!(!html.contains("<div class=\"doc-footer\">"));
    }

    #[test]
    fn test_underline_style_emits_text_decoration() {
        let doc = parse_document("[下線]{underline}\n");
        let html = generate_html(&doc);
        assert!(html.contains("text-decoration: underline"));
    }

    #[test]
    fn test_image_width_attribute_emits_inline_style() {
        let doc = parse_document("![図](does_not_exist.png){width: 50%}\n");
        let html = generate_html(&doc);
        assert!(html.contains("style=\"width: 50%; max-width: 100%;\""));
    }

    #[test]
    fn test_spacer_block_emits_height_div() {
        let doc = parse_document(":::space 2em\n");
        let html = generate_html(&doc);
        assert!(html.contains("<div style=\"height: 2em\"></div>"));
    }

    #[test]
    fn test_boxed_blockquote_emits_boxed_class() {
        let doc = parse_document("{box}\n> 引用\n");
        let html = generate_html(&doc);
        assert!(html.contains("<blockquote class=\"boxed-quote\">"));
    }

    #[test]
    fn test_generate_html_raw_typst_block_shows_escaped_source_with_note() {
        let doc = parse_document("```typst\n#line(length: 50%)\n```\n");
        let html = generate_html(&doc);
        assert!(html.contains("raw-typst-note"));
        assert!(html.contains("HTML出力では実行されません"));
        assert!(html.contains("#line(length: 50%)"));
    }
}
