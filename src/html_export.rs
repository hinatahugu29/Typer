//! 内部文書モデル (`Document`) から連続表示用単一HTMLファイルへの変換器
//!
//! ページ境界（改ページ指定等）に依存しない、ブラウザ上で読みやすいHTML+CSSを生成する。

use crate::model::{Alignment, Block, BlockAttributes, Document, FontChoice, Inline};
use std::fmt::Write;

pub fn generate_html(doc: &Document) -> String {
    let mut body = String::with_capacity(4096);

    for block in &doc.blocks {
        render_html_block(&mut body, block);
    }

    let s = &doc.settings;
    let font_family = match s.font_choice {
        FontChoice::Gothic => {
            r#""BIZ UDPGothic", "Yu Gothic UI", "Yu Gothic", "Meiryo", sans-serif"#
        }
        FontChoice::Mincho => r#""BIZ UDPMincho", "Yu Mincho", "MS PMincho", serif"#,
    };

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
    --accent-color: #1a73e8;
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
    max-width: 840px;
    margin: 0 auto;
    background: #ffffff;
    padding: 2.5rem 3rem;
    border-radius: 8px;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.08);
  }}
  h1 {{
    font-size: 1.5em;
    color: #174ea6;
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
  hr {{
    border: none;
    border-top: 1px solid var(--border-color);
    margin: 1.5em 0;
  }}
</style>
</head>
<body>
<div class="container">
{body}</div>
</body>
</html>
"#,
        font_family = font_family,
        font_size = html_escape(&s.font_size),
        line_height = s.line_height,
        body = body
    )
}

fn render_html_block(out: &mut String, block: &Block) {
    match block {
        Block::Heading {
            level,
            content,
            attrs,
        } => {
            let lvl = (*level).clamp(1, 6);
            let style = block_style_attr(attrs);
            let _ = write!(out, "<h{lvl}{style}>");
            render_html_inlines(out, content);
            let _ = writeln!(out, "</h{lvl}>");
        }
        Block::Paragraph { content, attrs } => {
            let style = block_style_attr(attrs);
            let _ = write!(out, "<p{style}>");
            render_html_inlines(out, content);
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
                render_html_inlines(out, item);
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
                out.push_str("  <thead>\n    <tr>\n");
                for (i, cell) in headers.iter().enumerate() {
                    let align_style = cell_align_attr(alignments.get(i).copied().flatten());
                    let _ = write!(out, "      <th{align_style}>");
                    render_html_inlines(out, cell);
                    out.push_str("</th>\n");
                }
                out.push_str("    </tr>\n  </thead>\n");
            }
            out.push_str("  <tbody>\n");
            for row in rows {
                out.push_str("    <tr>\n");
                for (i, cell) in row.iter().enumerate() {
                    let align_style = cell_align_attr(alignments.get(i).copied().flatten());
                    let _ = write!(out, "      <td{align_style}>");
                    render_html_inlines(out, cell);
                    out.push_str("</td>\n");
                }
                out.push_str("    </tr>\n");
            }
            out.push_str("  </tbody>\n</table>\n");
        }
        Block::BlockQuote { blocks, attrs } => {
            let style = block_style_attr(attrs);
            let _ = writeln!(out, "<blockquote{style}>");
            for b in blocks {
                render_html_block(out, b);
            }
            out.push_str("</blockquote>\n");
        }
        Block::CodeBlock { lang: _, code, attrs } => {
            let style = block_style_attr(attrs);
            let _ = writeln!(out, "<pre{style}><code>{}</code></pre>", html_escape(code));
        }
        Block::HorizontalRule => {
            out.push_str("<hr>\n");
        }
        Block::PageBreak => {
            // 要件6.2: PDF専用の改ページ指定はHTML出力時に原則無視する
        }
    }
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

fn render_html_inlines(out: &mut String, inlines: &[Inline]) {
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push_str(&html_escape(t)),
            Inline::Strong(c) => {
                out.push_str("<strong>");
                render_html_inlines(out, c);
                out.push_str("</strong>");
            }
            Inline::Emphasis(c) => {
                out.push_str("<em>");
                render_html_inlines(out, c);
                out.push_str("</em>");
            }
            Inline::Strikethrough(c) => {
                out.push_str("<del>");
                render_html_inlines(out, c);
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
                if css.is_empty() {
                    render_html_inlines(out, content);
                } else {
                    let _ = write!(out, "<span style=\"{}\">", css.join("; "));
                    render_html_inlines(out, content);
                    out.push_str("</span>");
                }
            }
            Inline::Link { text, url } => {
                let _ = write!(out, "<a href=\"{}\">", html_escape(url));
                render_html_inlines(out, text);
                out.push_str("</a>");
            }
            Inline::SoftBreak => out.push('\n'),
            Inline::HardBreak => out.push_str("<br>\n"),
        }
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
