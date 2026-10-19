//! 独自マークアップ拡張対応のMarkdownパーサー
//!
//! 標準Markdown (`pulldown-cmark`) をベースにしつつ、以下の拡張構文を解析して
//! 内部文書モデル (`crate::model::Document`) を生成する。
//!
//! 1. 先頭YAML風フロントマター (`---` 〜 `---`)
//! 2. ブロック直前属性行 (`{widths: 20%, 50%, 30%}`, `{align: right}`, `{line-height: 1.2}` 等)
//! 3. 改ページ指定 (`:::pagebreak` または `{pagebreak}`)
//! 4. インライン装飾 (`[重要]{red}`, `[テキスト]{color: red, size: 12pt}`, `{color:red|重要}`)

use crate::model::{
    Alignment, Block, BlockAttributes, Document, DocumentSettings, FontChoice, Inline, InlineStyle,
    Orientation,
};
use pulldown_cmark::{Alignment as CmarkAlign, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::fmt::Write;

const ATTR_MARKER_PREFIX: &str = "@@TYPER_ATTR:";
const ATTR_MARKER_SUFFIX: &str = "@@";
const PAGEBREAK_MARKER: &str = "@@TYPER_PAGEBREAK@@";
const TOC_MARKER: &str = "@@TYPER_TOC@@";
const SPACE_MARKER_PREFIX: &str = "@@TYPER_SPACE:";
const SPACE_MARKER_SUFFIX: &str = "@@";
const COLUMNS_START_MARKER_PREFIX: &str = "@@TYPER_COLUMNS_START:";
const COLUMNS_START_MARKER_SUFFIX: &str = "@@";
const COLUMNS_END_MARKER: &str = "@@TYPER_COLUMNS_END@@";

/// 原稿テキスト全体を解析して内部文書モデル (`Document`) を返す
pub fn parse_document(input: &str) -> Document {
    let (settings, body) = extract_frontmatter(input);
    let preprocessed = preprocess_block_directives(body);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(&preprocessed, options);

    let mut blocks = Vec::new();
    let mut pending_attrs = BlockAttributes::default();

    // 状態管理用スタック・バッファ
    let mut current_inlines: Vec<Inline> = Vec::new();
    let mut inline_stack: Vec<(InlineContainer, Vec<Inline>)> = Vec::new();

    let mut in_heading: Option<u8> = None;
    let mut in_paragraph = false;
    let mut in_blockquote = false;
    let mut blockquote_blocks: Vec<Block> = Vec::new();
    let mut blockquote_attrs = BlockAttributes::default();

    let mut in_list: Option<(bool, u64, BlockAttributes)> = None;
    let mut list_items: Vec<Vec<Inline>> = Vec::new();

    let mut in_code_block: Option<(Option<String>, BlockAttributes)> = None;
    let mut code_buffer = String::new();

    let mut in_table: Option<TableState> = None;

    let mut in_columns: Option<(u8, Vec<Block>)> = None;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    in_heading = Some(heading_level_to_u8(level));
                    current_inlines.clear();
                }
                Tag::Paragraph => {
                    in_paragraph = true;
                    current_inlines.clear();
                }
                Tag::BlockQuote(_) => {
                    in_blockquote = true;
                    blockquote_blocks.clear();
                    blockquote_attrs = std::mem::take(&mut pending_attrs);
                }
                Tag::List(start_num) => {
                    let ordered = start_num.is_some();
                    let start = start_num.unwrap_or(1);
                    let attrs = std::mem::take(&mut pending_attrs);
                    in_list = Some((ordered, start, attrs));
                    list_items.clear();
                }
                Tag::Item => {
                    current_inlines.clear();
                }
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(l) if !l.is_empty() => {
                            Some(l.to_string())
                        }
                        _ => None,
                    };
                    let attrs = std::mem::take(&mut pending_attrs);
                    in_code_block = Some((lang, attrs));
                    code_buffer.clear();
                }
                Tag::Table(alignments) => {
                    let mapped_aligns = alignments
                        .into_iter()
                        .map(|a| match a {
                            CmarkAlign::None => None,
                            CmarkAlign::Left => Some(Alignment::Left),
                            CmarkAlign::Center => Some(Alignment::Center),
                            CmarkAlign::Right => Some(Alignment::Right),
                        })
                        .collect();
                    let attrs = std::mem::take(&mut pending_attrs);
                    in_table = Some(TableState {
                        alignments: mapped_aligns,
                        headers: Vec::new(),
                        rows: Vec::new(),
                        current_row: Vec::new(),
                        in_header: false,
                        attrs,
                    });
                }
                Tag::TableHead => {
                    if let Some(t) = &mut in_table {
                        t.in_header = true;
                        t.current_row.clear();
                    }
                }
                Tag::TableRow => {
                    if let Some(t) = &mut in_table {
                        t.current_row.clear();
                    }
                }
                Tag::TableCell => {
                    current_inlines.clear();
                }
                Tag::Strong => {
                    let prev = std::mem::take(&mut current_inlines);
                    inline_stack.push((InlineContainer::Strong, prev));
                }
                Tag::Emphasis => {
                    let prev = std::mem::take(&mut current_inlines);
                    inline_stack.push((InlineContainer::Emphasis, prev));
                }
                Tag::Strikethrough => {
                    let prev = std::mem::take(&mut current_inlines);
                    inline_stack.push((InlineContainer::Strikethrough, prev));
                }
                Tag::Link { dest_url, .. } => {
                    let prev = std::mem::take(&mut current_inlines);
                    inline_stack.push((InlineContainer::Link(dest_url.to_string()), prev));
                }
                Tag::Image { dest_url, .. } => {
                    let prev = std::mem::take(&mut current_inlines);
                    inline_stack.push((InlineContainer::Image(dest_url.to_string()), prev));
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    if let Some(level) = in_heading.take() {
                        let content = expand_custom_inlines(std::mem::take(&mut current_inlines));
                        let attrs = std::mem::take(&mut pending_attrs);
                        let block = Block::Heading {
                            level,
                            content,
                            attrs,
                        };
                        push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, &mut in_columns, block);
                    }
                }
                TagEnd::Paragraph => {
                    in_paragraph = false;
                    let raw_inlines = std::mem::take(&mut current_inlines);
                    // マーカー段落かどうか判定
                    if let Some(single_text) = as_single_plain_text(&raw_inlines) {
                        let trimmed = single_text.trim();
                        if trimmed == PAGEBREAK_MARKER {
                            pending_attrs = BlockAttributes::default();
                            push_block(
                                &mut blocks,
                                &mut blockquote_blocks,
                                in_blockquote,
                                &mut in_columns,
                                Block::PageBreak,
                            );
                            continue;
                        }
                        if trimmed == TOC_MARKER {
                            pending_attrs = BlockAttributes::default();
                            push_block(
                                &mut blocks,
                                &mut blockquote_blocks,
                                in_blockquote,
                                &mut in_columns,
                                Block::TableOfContents,
                            );
                            continue;
                        }
                        if let Some(attr_str) = trimmed
                            .strip_prefix(ATTR_MARKER_PREFIX)
                            .and_then(|s| s.strip_suffix(ATTR_MARKER_SUFFIX))
                        {
                            parse_block_attributes_into(attr_str, &mut pending_attrs);
                            continue;
                        }
                        if let Some(height) = trimmed
                            .strip_prefix(SPACE_MARKER_PREFIX)
                            .and_then(|s| s.strip_suffix(SPACE_MARKER_SUFFIX))
                        {
                            push_block(
                                &mut blocks,
                                &mut blockquote_blocks,
                                in_blockquote,
                                &mut in_columns,
                                Block::Spacer {
                                    height: height.to_string(),
                                },
                            );
                            continue;
                        }
                        if let Some(count_str) = trimmed
                            .strip_prefix(COLUMNS_START_MARKER_PREFIX)
                            .and_then(|s| s.strip_suffix(COLUMNS_START_MARKER_SUFFIX))
                        {
                            let count = count_str.parse::<u8>().unwrap_or(2).clamp(2, 4);
                            in_columns = Some((count, Vec::new()));
                            continue;
                        }
                        if trimmed == COLUMNS_END_MARKER {
                            if let Some((count, col_blocks)) = in_columns.take() {
                                push_block(
                                    &mut blocks,
                                    &mut blockquote_blocks,
                                    in_blockquote,
                                    &mut in_columns,
                                    Block::Columns {
                                        count,
                                        blocks: col_blocks,
                                    },
                                );
                            }
                            continue;
                        }
                    }

                    let content = expand_custom_inlines(raw_inlines);
                    let attrs = std::mem::take(&mut pending_attrs);
                    let block = Block::Paragraph { content, attrs };
                    push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, &mut in_columns, block);
                }
                TagEnd::BlockQuote(_) => {
                    in_blockquote = false;
                    let bq_blocks = std::mem::take(&mut blockquote_blocks);
                    let attrs = std::mem::take(&mut blockquote_attrs);
                    blocks.push(Block::BlockQuote {
                        blocks: bq_blocks,
                        attrs,
                    });
                }
                TagEnd::Item => {
                    let item_inlines = expand_custom_inlines(std::mem::take(&mut current_inlines));
                    list_items.push(item_inlines);
                }
                TagEnd::List(_) => {
                    if let Some((ordered, start, attrs)) = in_list.take() {
                        let items = std::mem::take(&mut list_items);
                        let block = Block::List {
                            ordered,
                            start,
                            items,
                            attrs,
                        };
                        push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, &mut in_columns, block);
                    }
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, attrs)) = in_code_block.take() {
                        let code = std::mem::take(&mut code_buffer);
                        let block = if lang.as_deref() == Some("typst") {
                            Block::RawTypst { code, attrs }
                        } else {
                            Block::CodeBlock { lang, code, attrs }
                        };
                        push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, &mut in_columns, block);
                    }
                }
                TagEnd::TableCell => {
                    if let Some(t) = &mut in_table {
                        let cell_inlines =
                            expand_custom_inlines(std::mem::take(&mut current_inlines));
                        t.current_row.push(cell_inlines);
                    }
                }
                TagEnd::TableHead => {
                    if let Some(t) = &mut in_table {
                        t.headers = std::mem::take(&mut t.current_row);
                        t.in_header = false;
                    }
                }
                TagEnd::TableRow => {
                    if let Some(t) = &mut in_table {
                        let row = std::mem::take(&mut t.current_row);
                        if !row.is_empty() {
                            t.rows.push(row);
                        }
                    }
                }
                TagEnd::Table => {
                    if let Some(t) = in_table.take() {
                        let block = Block::Table {
                            headers: t.headers,
                            alignments: t.alignments,
                            rows: t.rows,
                            attrs: t.attrs,
                        };
                        push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, &mut in_columns, block);
                    }
                }
                TagEnd::Strong
                | TagEnd::Emphasis
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image => {
                    if let Some((container, mut parent_inlines)) = inline_stack.pop() {
                        let inner = expand_custom_inlines(std::mem::take(&mut current_inlines));
                        let wrapped = match container {
                            InlineContainer::Strong => Inline::Strong(inner),
                            InlineContainer::Emphasis => Inline::Emphasis(inner),
                            InlineContainer::Strikethrough => Inline::Strikethrough(inner),
                            InlineContainer::Link(url) => Inline::Link { text: inner, url },
                            InlineContainer::Image(url) => {
                                let alt = inlines_to_plain_text(&inner);
                                Inline::Image {
                                    alt,
                                    url,
                                    width: None,
                                }
                            }
                        };
                        parent_inlines.push(wrapped);
                        current_inlines = parent_inlines;
                    }
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block.is_some() {
                    code_buffer.push_str(&text);
                } else {
                    current_inlines.push(Inline::Text(text.to_string()));
                }
            }
            Event::Code(code) => {
                current_inlines.push(Inline::Code(code.to_string()));
            }
            Event::SoftBreak => {
                if in_paragraph || in_heading.is_some() {
                    current_inlines.push(Inline::SoftBreak);
                }
            }
            Event::HardBreak => {
                current_inlines.push(Inline::HardBreak);
            }
            Event::InlineHtml(html) => {
                // 表のセルなど1行でしか書けない箇所向けに、`<br>` をハードブレイクとして扱う
                // （物理行をまたげる `\` 改行の代替として使える）
                if is_br_tag(&html) {
                    current_inlines.push(Inline::HardBreak);
                }
            }
            Event::Rule => {
                pending_attrs = BlockAttributes::default();
                push_block(
                    &mut blocks,
                    &mut blockquote_blocks,
                    in_blockquote,
                    &mut in_columns,
                    Block::HorizontalRule,
                );
            }
            _ => {}
        }
    }

    Document { settings, blocks }
}

/// GUIの設定バーで変更された `DocumentSettings` を原稿先頭のフロントマターに反映した文字列を返す
pub fn update_document_settings_in_markdown(input: &str, settings: &DocumentSettings) -> String {
    let (_, body) = extract_frontmatter(input);
    let orient = match settings.orientation {
        Orientation::Portrait => "portrait",
        Orientation::Landscape => "landscape",
    };
    let font = match settings.font_choice {
        FontChoice::Gothic => "gothic",
        FontChoice::Mincho => "mincho",
    };
    let paper = settings.paper.to_ascii_uppercase();
    let trimmed_body = body.trim_start_matches(['\r', '\n']);

    let mut frontmatter = format!(
        "---\npaper: {}\norientation: {}\nmargin: {}\nfont-size: {}\nline-height: {:.1}\nfont: {}\naccent-color: {}\n",
        paper,
        orient,
        settings.margin,
        settings.font_size,
        settings.line_height,
        font,
        settings.accent_color
    );
    if let Some(header) = &settings.header_text {
        let _ = writeln!(frontmatter, "header: {header}");
    }
    if let Some(footer) = &settings.footer_text {
        let _ = writeln!(frontmatter, "footer: {footer}");
    }
    if !settings.show_page_number {
        frontmatter.push_str("page-number: off\n");
    }
    if settings.columns > 1 {
        let _ = writeln!(frontmatter, "columns: {}", settings.columns);
    }
    if let Some(watermark) = &settings.watermark {
        let _ = writeln!(frontmatter, "watermark: {watermark}");
    }
    frontmatter.push_str("---\n\n");
    frontmatter.push_str(trimmed_body);
    frontmatter
}

pub(crate) fn inlines_to_plain_text(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) | Inline::Code(t) => s.push_str(t),
            Inline::Strong(c)
            | Inline::Emphasis(c)
            | Inline::Strikethrough(c)
            | Inline::Styled { content: c, .. }
            | Inline::Link { text: c, .. } => s.push_str(&inlines_to_plain_text(c)),
            Inline::Image { alt, .. } => s.push_str(alt),
            Inline::SoftBreak | Inline::HardBreak => s.push(' '),
        }
    }
    s
}

enum InlineContainer {
    Strong,
    Emphasis,
    Strikethrough,
    Link(String),
    Image(String),
}

struct TableState {
    alignments: Vec<Option<Alignment>>,
    headers: Vec<Vec<Inline>>,
    rows: Vec<Vec<Vec<Inline>>>,
    current_row: Vec<Vec<Inline>>,
    in_header: bool,
    attrs: BlockAttributes,
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// `<br>` / `<br/>` / `<br />` （大文字小文字を問わない）かどうかを判定する
fn is_br_tag(html: &str) -> bool {
    matches!(
        html.trim().to_ascii_lowercase().as_str(),
        "<br>" | "<br/>" | "<br />"
    )
}

fn push_block(
    blocks: &mut Vec<Block>,
    blockquote_blocks: &mut Vec<Block>,
    in_blockquote: bool,
    in_columns: &mut Option<(u8, Vec<Block>)>,
    block: Block,
) {
    if let Some((_, col_blocks)) = in_columns {
        col_blocks.push(block);
    } else if in_blockquote {
        blockquote_blocks.push(block);
    } else {
        blocks.push(block);
    }
}

fn as_single_plain_text(inlines: &[Inline]) -> Option<&str> {
    if inlines.len() == 1 {
        if let Inline::Text(s) = &inlines[0] {
            return Some(s.as_str());
        }
    }
    None
}

/// 先頭の `---` 〜 `---` フロントマターを抽出し、`DocumentSettings` と残りの本文を返す
fn extract_frontmatter(input: &str) -> (DocumentSettings, &str) {
    let mut settings = DocumentSettings::default();
    let trimmed = input.trim_start_matches('\u{feff}');

    if !trimmed.starts_with("---") {
        return (settings, input);
    }

    let mut lines = trimmed.lines();
    let first = lines.next().unwrap_or("");
    if first.trim() != "---" {
        return (settings, input);
    }

    let mut byte_offset = first.len();
    if trimmed[byte_offset..].starts_with("\r\n") {
        byte_offset += 2;
    } else if trimmed[byte_offset..].starts_with('\n') {
        byte_offset += 1;
    }

    let mut found_end = false;
    let mut fm_lines = Vec::new();

    for line in lines {
        let line_len = line.len();
        byte_offset += line_len;
        if trimmed[byte_offset..].starts_with("\r\n") {
            byte_offset += 2;
        } else if trimmed[byte_offset..].starts_with('\n') {
            byte_offset += 1;
        }

        if line.trim() == "---" {
            found_end = true;
            break;
        }
        fm_lines.push(line);
    }

    if !found_end {
        return (settings, input);
    }

    for line in fm_lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            match key.as_str() {
                "paper" => {
                    if !val.is_empty() {
                        settings.paper = val.to_ascii_lowercase();
                    }
                }
                "orientation" => {
                    let v_low = val.to_ascii_lowercase();
                    if v_low == "landscape" || val == "横" || val == "横向き" {
                        settings.orientation = Orientation::Landscape;
                    } else {
                        settings.orientation = Orientation::Portrait;
                    }
                }
                "margin" => {
                    if !val.is_empty() {
                        settings.margin = val.to_string();
                    }
                }
                "font-size" | "fontsize" | "size" => {
                    if !val.is_empty() {
                        settings.font_size = val.to_string();
                    }
                }
                "line-height" | "lineheight" => {
                    if let Ok(lh) = val.parse::<f32>() {
                        settings.line_height = lh.clamp(0.8, 4.0);
                    }
                }
                "font" | "font-family" => {
                    let v_low = val.to_ascii_lowercase();
                    if v_low.contains("mincho") || val.contains("明朝") {
                        settings.font_choice = FontChoice::Mincho;
                    } else {
                        settings.font_choice = FontChoice::Gothic;
                    }
                }
                "accent-color" | "accent" => {
                    if let Some(c) = normalize_color(val) {
                        settings.accent_color = c;
                    }
                }
                "header" => {
                    if !val.is_empty() {
                        settings.header_text = Some(val.to_string());
                    }
                }
                "footer" => {
                    if !val.is_empty() {
                        settings.footer_text = Some(val.to_string());
                    }
                }
                "page-number" | "page-numbering" | "numbering" => {
                    let v_low = val.to_ascii_lowercase();
                    settings.show_page_number =
                        !matches!(v_low.as_str(), "off" | "false" | "no" | "none" | "0");
                }
                "columns" | "column" => {
                    if let Ok(n) = val.parse::<u8>() {
                        settings.columns = n.clamp(1, 4);
                    }
                }
                "watermark" => {
                    let v_low = val.to_ascii_lowercase();
                    if val.is_empty() || matches!(v_low.as_str(), "off" | "false" | "no" | "none")
                    {
                        settings.watermark = None;
                    } else {
                        settings.watermark = Some(val.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    (settings, &trimmed[byte_offset..])
}

/// 単独行のブロック属性 `{widths: ...}`, `{align: ...}` や `:::pagebreak` を
/// 前後に空行を持つ独立マーカーに変換し、直後の表や段落と混ざらないようにする
fn preprocess_block_directives(body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 128);
    let mut in_fenced_code = false;

    for line in body.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fenced_code = !in_fenced_code;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_fenced_code {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if trimmed.eq_ignore_ascii_case(":::pagebreak")
            || trimmed.eq_ignore_ascii_case("{pagebreak}")
            || trimmed.eq_ignore_ascii_case("::: pagebreak")
        {
            out.push('\n');
            out.push_str(PAGEBREAK_MARKER);
            out.push_str("\n\n");
            continue;
        }

        if trimmed.eq_ignore_ascii_case(":::toc")
            || trimmed.eq_ignore_ascii_case("{toc}")
            || trimmed.eq_ignore_ascii_case("::: toc")
        {
            out.push('\n');
            out.push_str(TOC_MARKER);
            out.push_str("\n\n");
            continue;
        }

        if let Some(count) = parse_columns_start_directive(trimmed) {
            out.push('\n');
            out.push_str(COLUMNS_START_MARKER_PREFIX);
            out.push_str(&count.to_string());
            out.push_str(COLUMNS_START_MARKER_SUFFIX);
            out.push_str("\n\n");
            continue;
        }

        if is_columns_end_directive(trimmed) {
            out.push('\n');
            out.push_str(COLUMNS_END_MARKER);
            out.push_str("\n\n");
            continue;
        }

        if let Some(height) = parse_space_directive(trimmed) {
            out.push('\n');
            out.push_str(SPACE_MARKER_PREFIX);
            out.push_str(&height);
            out.push_str(SPACE_MARKER_SUFFIX);
            out.push_str("\n\n");
            continue;
        }

        if is_standalone_block_attr_line(trimmed) {
            let inner = &trimmed[1..trimmed.len() - 1];
            out.push('\n');
            out.push_str(ATTR_MARKER_PREFIX);
            out.push_str(inner.trim());
            out.push_str(ATTR_MARKER_SUFFIX);
            out.push_str("\n\n");
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

/// `:::columns 2` のような範囲指定段組み開始行を解析し、段数を返す（値省略時は既定で2段）
fn parse_columns_start_directive(trimmed: &str) -> Option<u8> {
    let rest = trimmed
        .strip_prefix(":::columns")
        .or_else(|| trimmed.strip_prefix("::: columns"))?;
    // `:::columns-end` 等の終了行は別処理のため、直後に "-" が続く場合はここでは扱わない
    if rest.starts_with('-') {
        return None;
    }
    let val = rest.trim();
    if val.is_empty() {
        Some(2)
    } else {
        val.parse::<u8>().ok().map(|n| n.clamp(2, 4))
    }
}

/// `:::columns-end` / `:::end-columns` のような範囲指定段組みの終了行かどうかを判定する
fn is_columns_end_directive(trimmed: &str) -> bool {
    let normalized = trimmed.to_ascii_lowercase().replace(' ', "");
    matches!(
        normalized.as_str(),
        ":::columns-end" | ":::end-columns" | ":::columnsend" | ":::endcolumns"
    )
}

/// `:::space 1em` のような明示的な垂直空白指定行を解析し、高さの文字列を返す
/// （値省略時は既定で "1em"）
fn parse_space_directive(trimmed: &str) -> Option<String> {
    let rest = trimmed
        .strip_prefix(":::space")
        .or_else(|| trimmed.strip_prefix("::: space"))?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let val = rest.trim();
    if val.is_empty() {
        Some("1em".to_string())
    } else {
        Some(val.to_string())
    }
}

/// 単独行が `{widths: ...}` や `{align: right}` のようなブロック属性指定行かどうかを判定する
fn is_standalone_block_attr_line(trimmed: &str) -> bool {
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') || trimmed.len() < 4 {
        return false;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    // インライン構文 `{color:red|重要}` のようにパイプを含むものや、途中に `}` があるものは除外
    if inner.contains('|') || inner.contains('{') || inner.contains('}') {
        return false;
    }
    // 単独のフラグ属性（コロンを持たないキーワード、例: `{box}`）
    if inner.trim().eq_ignore_ascii_case("box") {
        return true;
    }

    // 先頭のキーを取り出してブロック属性キーワードかどうか確認
    let first_pair = inner.split(',').next().unwrap_or("");
    if let Some((k, _)) = first_pair.split_once(':') {
        let key = k.trim().to_ascii_lowercase();
        matches!(
            key.as_str(),
            "widths"
                | "width"
                | "align"
                | "line-height"
                | "lineheight"
                | "size"
                | "font-size"
                | "color"
                | "c"
                | "header-bg"
                | "block-width"
                | "indent"
                | "indent-left"
                | "indent-right"
        )
    } else {
        false
    }
}

/// `widths: 20%, 50%, 30%` や `align: right, line-height: 1.2` を解析して `BlockAttributes` に格納する
fn parse_block_attributes_into(attr_str: &str, attrs: &mut BlockAttributes) {
    // widths はカンマ区切りのリストを持つため、キー単位で切り出す
    // 例: "widths: 20%, 50%, 30%" または "align: right, line-height: 1.2, size: 12pt"
    let lower = attr_str.to_ascii_lowercase();
    if let Some(rest) = lower
        .strip_prefix("widths:")
        .or_else(|| lower.strip_prefix("width:"))
    {
        let items: Vec<String> = rest
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !items.is_empty() {
            attrs.widths = Some(items);
        }
        return;
    }

    for part in attr_str.split(',') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            match key.as_str() {
                "align" => {
                    let v_low = val.to_ascii_lowercase();
                    attrs.align = match v_low.as_str() {
                        "center" | "中央" => Some(Alignment::Center),
                        "right" | "右" => Some(Alignment::Right),
                        "left" | "左" => Some(Alignment::Left),
                        _ => None,
                    };
                }
                "line-height" | "lineheight" => {
                    if let Ok(lh) = val.parse::<f32>() {
                        attrs.line_height = Some(lh.clamp(0.8, 4.0));
                    }
                }
                "size" | "font-size" => {
                    if !val.is_empty() {
                        attrs.font_size = Some(val.to_string());
                    }
                }
                "color" | "c" => {
                    if let Some(c) = normalize_color(val) {
                        attrs.color = Some(c);
                    }
                }
                "header-bg" => {
                    if let Some(c) = normalize_color(val) {
                        attrs.header_bg = Some(c);
                    }
                }
                "block-width" => {
                    if !val.is_empty() {
                        attrs.block_width = Some(val.to_string());
                    }
                }
                "indent" => {
                    if !val.is_empty() {
                        attrs.indent_left = Some(val.to_string());
                    }
                }
                "indent-left" => {
                    if !val.is_empty() {
                        attrs.indent_left = Some(val.to_string());
                    }
                }
                "indent-right" => {
                    if !val.is_empty() {
                        attrs.indent_right = Some(val.to_string());
                    }
                }
                _ => {}
            }
        } else if part.eq_ignore_ascii_case("box") {
            attrs.boxed = true;
        }
    }
}

/// 原稿テキストから見出し一覧を抽出する（アウトラインパネル用）。
/// フェンスコードブロック内の `#` はコメント扱いで見出しとして拾わない。
/// 戻り値は `(見出しレベル, 見出しテキスト, 行頭の文字インデックス)` のリスト。
pub fn extract_headings_outline(text: &str) -> Vec<(u8, String, usize)> {
    let mut result = Vec::new();
    let mut in_fenced_code = false;
    let mut char_offset = 0usize;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let trimmed_start = trimmed.trim_start();

        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            in_fenced_code = !in_fenced_code;
        } else if !in_fenced_code && trimmed_start.starts_with('#') {
            let level = trimmed_start.chars().take_while(|&c| c == '#').count();
            if level >= 1
                && level <= 6
                && trimmed_start[level..].starts_with(|c: char| c == ' ' || c == '\t')
            {
                let title = trimmed_start[level..].trim().to_string();
                if !title.is_empty() {
                    result.push((level as u8, title, char_offset));
                }
            }
        }

        char_offset += line.chars().count();
    }

    result
}

/// インライン列の中の `Inline::Text` に含まれる独自インライン構文
/// 1) `[テキスト]{red}` / `[テキスト]{color: red, size: 12pt}`
/// 2) `{color:red|テキスト}` / `{red:テキスト}`
/// を展開する
fn expand_custom_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
    // pulldown-cmark は `[重要]{red}` のような連続したテキストを `[重要]` と `{red}` のように
    // 複数の `Event::Text` へ分割することがあるため、独自記法の境界をまたいで見失わないよう、
    // 隣接する `Inline::Text` をいったん1つの文字列へ結合してから走査する
    let mut out = Vec::with_capacity(inlines.len());
    let mut text_buf = String::new();

    for inline in inlines {
        match inline {
            Inline::Text(s) => {
                text_buf.push_str(&s);
            }
            other => {
                if !text_buf.is_empty() {
                    parse_inline_custom_spans(&text_buf, &mut out);
                    text_buf.clear();
                }
                out.push(other);
            }
        }
    }
    if !text_buf.is_empty() {
        parse_inline_custom_spans(&text_buf, &mut out);
    }
    attach_image_size_attrs(out)
}

/// `![alt](url){width: 50%}` のように画像直後に置かれたサイズ指定を画像ノードへ取り込む。
/// この時点では画像とサイズ指定のテキストは別々の `Inline` として隣接しているため、
/// 直後のテキストが `{width: ...}` / `{w: ...}` で始まっていれば取り込んで消費する。
fn attach_image_size_attrs(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::with_capacity(inlines.len());
    let mut iter = inlines.into_iter().peekable();

    while let Some(inline) = iter.next() {
        let Inline::Image { alt, url, width } = inline else {
            out.push(inline);
            continue;
        };

        let mut new_width = width;
        let mut remainder = None;
        if let Some(Inline::Text(next_text)) = iter.peek() {
            if let Some((w, consumed)) = try_parse_image_size_attr(next_text) {
                new_width = Some(w);
                let rest = next_text[consumed..].to_string();
                remainder = Some(rest);
            }
        }
        if remainder.is_some() {
            iter.next(); // サイズ指定を含んでいたテキストノードを消費する
        }

        out.push(Inline::Image {
            alt,
            url,
            width: new_width,
        });
        if let Some(rest) = remainder {
            if !rest.is_empty() {
                out.push(Inline::Text(rest));
            }
        }
    }

    out
}

/// 先頭が `{width: ...}` または `{w: ...}` であれば、指定値と消費したバイト数を返す
fn try_parse_image_size_attr(text: &str) -> Option<(String, usize)> {
    if !text.starts_with('{') {
        return None;
    }
    let close = text.find('}')?;
    let inner = &text[1..close];
    let low = inner.to_ascii_lowercase();
    let rest = low
        .strip_prefix("width:")
        .or_else(|| low.strip_prefix("w:"))?;
    let val = rest.trim();
    if val.is_empty() {
        return None;
    }
    Some((val.to_string(), close + 1))
}

fn parse_inline_custom_spans(text: &str, out: &mut Vec<Inline>) {
    let mut rest = text;

    while !rest.is_empty() {
        // 次の `[` または `{` を探す
        let next_special = rest.find(['[', '{']);
        let Some(pos) = next_special else {
            push_plain_text(out, rest);
            break;
        };

        if pos > 0 {
            push_plain_text(out, &rest[..pos]);
            rest = &rest[pos..];
        }

        if rest.starts_with('[') {
            // `[テキスト]{属性}` パターンの試行
            if let Some((span_text, attr_spec, consumed)) = try_parse_bracket_span(rest) {
                if let Some(style) = parse_inline_style(attr_spec) {
                    out.push(Inline::Styled {
                        content: vec![Inline::Text(span_text.to_string())],
                        style,
                    });
                    rest = &rest[consumed..];
                    continue;
                }
            }
            // マッチしなければ先頭の1文字を通常テキストとして進める
            let ch_len = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            push_plain_text(out, &rest[..ch_len]);
            rest = &rest[ch_len..];
        } else if rest.starts_with('{') {
            // `{color:red|テキスト}` パターンの試行
            if let Some((style, span_text, consumed)) = try_parse_brace_inline(rest) {
                out.push(Inline::Styled {
                    content: vec![Inline::Text(span_text.to_string())],
                    style,
                });
                rest = &rest[consumed..];
                continue;
            }
            let ch_len = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            push_plain_text(out, &rest[..ch_len]);
            rest = &rest[ch_len..];
        }
    }
}

fn push_plain_text(out: &mut Vec<Inline>, s: &str) {
    if s.is_empty() {
        return;
    }
    if let Some(Inline::Text(prev)) = out.last_mut() {
        prev.push_str(s);
    } else {
        out.push(Inline::Text(s.to_string()));
    }
}

/// `[テキスト]{属性}` の形式を切り出す
fn try_parse_bracket_span(s: &str) -> Option<(&str, &str, usize)> {
    let close_bracket = s.find("]{")?;
    let span_text = &s[1..close_bracket];
    if span_text.is_empty() || span_text.contains('[') || span_text.contains(']') {
        return None;
    }
    let after_brace_start = close_bracket + 2;
    let rel_close_brace = s[after_brace_start..].find('}')?;
    let attr_spec = &s[after_brace_start..after_brace_start + rel_close_brace];
    let total_consumed = after_brace_start + rel_close_brace + 1;
    Some((span_text, attr_spec, total_consumed))
}

/// `{color:red|重要事項}` の形式を切り出す
fn try_parse_brace_inline(s: &str) -> Option<(InlineStyle, &str, usize)> {
    let close_brace = s.find('}')?;
    let inner = &s[1..close_brace];
    let (spec, span_text) = inner.split_once('|')?;
    if span_text.is_empty() {
        return None;
    }
    let style = parse_inline_style(spec)?;
    Some((style, span_text, close_brace + 1))
}

/// `red`, `blue`, `color: red, size: 12pt` などを `InlineStyle` に変換する
fn parse_inline_style(spec: &str) -> Option<InlineStyle> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    let mut style = InlineStyle::default();

    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            match key.as_str() {
                "color" | "c" => {
                    if let Some(c) = normalize_color(val) {
                        style.color = Some(c);
                    }
                }
                "size" | "font-size" => {
                    if !val.is_empty() {
                        style.size = Some(val.to_string());
                    }
                }
                "bg" | "highlight" => {
                    if let Some(c) = normalize_color(val) {
                        style.bg = Some(c);
                    }
                }
                _ => {}
            }
        } else {
            // ショートハンド: `{red}` や `{14pt}` / `{underline}`
            let part_low = part.to_ascii_lowercase();
            if part_low == "underline" || part_low == "u" || part == "下線" {
                style.underline = true;
            } else if let Some(c) = normalize_color(part) {
                style.color = Some(c);
            } else if part.ends_with("pt") {
                style.size = Some(part.to_string());
            }
        }
    }

    if style.color.is_some() || style.size.is_some() || style.bg.is_some() || style.underline {
        Some(style)
    } else {
        None
    }
}

fn normalize_color(raw: &str) -> Option<String> {
    let low = raw.trim().to_ascii_lowercase();
    match low.as_str() {
        "red" | "赤" => Some("#c5221f".to_string()),
        "blue" | "青" => Some("#1a73e8".to_string()),
        "green" | "緑" => Some("#188038".to_string()),
        "orange" | "橙" => Some("#e37400".to_string()),
        "purple" | "紫" => Some("#8430ce".to_string()),
        "gray" | "grey" | "灰" => Some("#5f6368".to_string()),
        "black" | "黒" => Some("#202124".to_string()),
        s if s.starts_with('#') && (s.len() == 4 || s.len() == 7) => Some(s.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_columns_directive_wraps_enclosed_blocks_only() {
        let src = "本文A\n\n:::columns 3\n## 見出し\n本文B\n:::columns-end\n\n本文C\n";
        let doc = parse_document(src);
        assert_eq!(doc.blocks.len(), 3);
        assert!(matches!(&doc.blocks[0], Block::Paragraph { .. }));
        match &doc.blocks[1] {
            Block::Columns { count, blocks } => {
                assert_eq!(*count, 3);
                assert_eq!(blocks.len(), 2);
                assert!(matches!(&blocks[0], Block::Heading { level: 2, .. }));
                assert!(matches!(&blocks[1], Block::Paragraph { .. }));
            }
            other => panic!("Expected Columns block, got {other:?}"),
        }
        assert!(matches!(&doc.blocks[2], Block::Paragraph { .. }));
    }

    #[test]
    fn test_columns_directive_default_count_and_clamp() {
        let default_doc = parse_document(":::columns\n本文\n:::columns-end\n");
        match &default_doc.blocks[0] {
            Block::Columns { count, .. } => assert_eq!(*count, 2),
            other => panic!("Expected Columns block, got {other:?}"),
        }

        let clamped_doc = parse_document(":::columns 9\n本文\n:::columns-end\n");
        match &clamped_doc.blocks[0] {
            Block::Columns { count, .. } => assert_eq!(*count, 4),
            other => panic!("Expected Columns block, got {other:?}"),
        }
    }

    #[test]
    fn test_frontmatter_and_table_widths() {
        let src = r#"---
paper: A4
margin: 15mm
line-height: 1.4
---

{widths: 20%, 80%}
| 項目 | 内容 |
| :--- | :--- |
| [重要]{red} | テスト内容 |
"#;
        let doc = parse_document(src);
        assert_eq!(doc.settings.margin, "15mm");
        assert!((doc.settings.line_height - 1.4).abs() < 0.001);
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            Block::Table { attrs, rows, .. } => {
                assert_eq!(
                    attrs.widths,
                    Some(vec!["20%".to_string(), "80%".to_string()])
                );
                assert_eq!(rows.len(), 1);
            }
            _ => panic!("Expected Table block"),
        }
    }

    #[test]
    fn test_typst_fenced_code_block_becomes_raw_typst_block() {
        let doc = parse_document("```typst\n#line(length: 50%)\n```\n");
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            Block::RawTypst { code, .. } => {
                assert!(code.contains("#line(length: 50%)"));
            }
            other => panic!("Expected RawTypst block, got {other:?}"),
        }
    }

    #[test]
    fn test_frontmatter_accent_color_roundtrip() {
        let src = "---\naccent-color: #e37400\n---\n\n本文\n";
        let doc = parse_document(src);
        assert_eq!(doc.settings.accent_color, "#e37400");

        let regenerated = update_document_settings_in_markdown(src, &doc.settings);
        assert!(regenerated.contains("accent-color: #e37400"));
    }

    #[test]
    fn test_frontmatter_header_and_footer_roundtrip() {
        let src = "---\nheader: 社内限定資料\nfooter: 社外秘\n---\n\n本文\n";
        let doc = parse_document(src);
        assert_eq!(doc.settings.header_text.as_deref(), Some("社内限定資料"));
        assert_eq!(doc.settings.footer_text.as_deref(), Some("社外秘"));

        let regenerated = update_document_settings_in_markdown(src, &doc.settings);
        assert!(regenerated.contains("header: 社内限定資料"));
        assert!(regenerated.contains("footer: 社外秘"));

        // 設定変更の度に再生成されても、未指定ならheader/footer行自体が出ないことを確認する
        let doc_without = parse_document("本文のみ\n");
        let regenerated_without =
            update_document_settings_in_markdown("本文のみ\n", &doc_without.settings);
        assert!(!regenerated_without.contains("header:"));
        assert!(!regenerated_without.contains("footer:"));
    }

    #[test]
    fn test_page_number_defaults_to_shown() {
        let doc = parse_document("本文\n");
        assert!(doc.settings.show_page_number);
    }

    #[test]
    fn test_page_number_off_variants_all_recognized() {
        for variant in ["off", "false", "no", "none", "0", "OFF"] {
            let src = format!("---\npage-number: {variant}\n---\n\n本文\n");
            let doc = parse_document(&src);
            assert!(
                !doc.settings.show_page_number,
                "page-number: {variant} should disable page numbers"
            );
        }
    }

    #[test]
    fn test_page_number_off_roundtrip_through_regeneration() {
        let src = "---\npage-number: off\n---\n\n本文\n";
        let doc = parse_document(src);
        assert!(!doc.settings.show_page_number);

        let regenerated = update_document_settings_in_markdown(src, &doc.settings);
        assert!(regenerated.contains("page-number: off"));

        // 既定（表示する）の場合はpage-number行自体が出ない
        let doc_on = parse_document("本文のみ\n");
        let regenerated_on = update_document_settings_in_markdown("本文のみ\n", &doc_on.settings);
        assert!(!regenerated_on.contains("page-number"));
    }

    #[test]
    fn test_block_color_and_header_bg_attributes_parsed() {
        let src = "{color: red}\n段落本文\n\n{widths: 50%, 50%}\n{header-bg: #fdf0e6}\n| A | B |\n| :--- | :--- |\n| 1 | 2 |\n";
        let doc = parse_document(src);
        match &doc.blocks[0] {
            Block::Paragraph { attrs, .. } => {
                assert_eq!(attrs.color.as_deref(), Some("#c5221f"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
        match &doc.blocks[1] {
            Block::Table { attrs, .. } => {
                assert_eq!(attrs.header_bg.as_deref(), Some("#fdf0e6"));
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn test_block_width_attribute_parsed_and_not_confused_with_table_width() {
        let doc = parse_document("{align: center, block-width: 60%}\n段落本文\n");
        match &doc.blocks[0] {
            Block::Paragraph { attrs, .. } => {
                assert_eq!(attrs.block_width.as_deref(), Some("60%"));
                assert_eq!(attrs.align, Some(Alignment::Center));
                assert!(attrs.widths.is_none());
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn test_br_tag_in_table_cell_becomes_hard_break() {
        // 表のセルは1物理行でしか書けないため、`\` 改行の代わりに `<br>` を使えるようにする
        let doc = parse_document("| A |\n| :--- |\n| 1行目<br>2行目 |\n");
        match &doc.blocks[0] {
            Block::Table { rows, .. } => {
                assert_eq!(
                    rows[0][0],
                    vec![
                        Inline::Text("1行目".to_string()),
                        Inline::HardBreak,
                        Inline::Text("2行目".to_string()),
                    ]
                );
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn test_br_tag_variants_all_recognized() {
        for tag in ["<br>", "<br/>", "<br />", "<BR>", "<Br/>"] {
            let src = format!("1行目{tag}2行目\n");
            let doc = parse_document(&src);
            match &doc.blocks[0] {
                Block::Paragraph { content, .. } => {
                    assert!(
                        content.contains(&Inline::HardBreak),
                        "{tag} should become a hard break: {content:?}"
                    );
                }
                other => panic!("expected paragraph, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_toc_directive_variants_parsed_as_table_of_contents_block() {
        for directive in [":::toc", "{toc}", "::: toc", ":::TOC"] {
            let src = format!("# 見出し\n\n{directive}\n\n## 次の見出し\n");
            let doc = parse_document(&src);
            assert!(
                doc.blocks
                    .iter()
                    .any(|b| matches!(b, Block::TableOfContents)),
                "{directive} should produce a TableOfContents block: {:?}",
                doc.blocks
            );
        }
    }

    #[test]
    fn test_underline_shorthand_and_strikethrough() {
        let doc = parse_document("[下線テキスト]{underline}、~~取り消し線~~\n");
        match &doc.blocks[0] {
            Block::Paragraph { content, .. } => {
                let has_underline = content
                    .iter()
                    .any(|i| matches!(i, Inline::Styled { style, .. } if style.underline));
                let has_strike = content
                    .iter()
                    .any(|i| matches!(i, Inline::Strikethrough(_)));
                assert!(
                    has_underline,
                    "underline span should be parsed: {content:?}"
                );
                assert!(has_strike, "strikethrough should be parsed: {content:?}");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn test_image_size_attribute_attached_and_remainder_kept() {
        let doc = parse_document("![図1](sample.png){width: 50%} 補足文\n");
        match &doc.blocks[0] {
            Block::Paragraph { content, .. } => {
                assert_eq!(content.len(), 2);
                match &content[0] {
                    Inline::Image { width, .. } => {
                        assert_eq!(width.as_deref(), Some("50%"));
                    }
                    other => panic!("expected image, got {other:?}"),
                }
                match &content[1] {
                    Inline::Text(t) => assert_eq!(t, " 補足文"),
                    other => panic!("expected trailing text, got {other:?}"),
                }
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn test_image_without_size_attribute_has_no_width() {
        let doc = parse_document("![図1](sample.png)\n");
        match &doc.blocks[0] {
            Block::Paragraph { content, .. } => match &content[0] {
                Inline::Image { width, .. } => assert_eq!(*width, None),
                other => panic!("expected image, got {other:?}"),
            },
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn test_space_directive_with_and_without_value() {
        let doc = parse_document(":::space 2em\n本文\n:::space\n");
        match &doc.blocks[0] {
            Block::Spacer { height } => assert_eq!(height, "2em"),
            other => panic!("expected spacer, got {other:?}"),
        }
        match &doc.blocks[2] {
            Block::Spacer { height } => assert_eq!(height, "1em"),
            other => panic!("expected spacer, got {other:?}"),
        }
    }

    #[test]
    fn test_boxed_blockquote_attribute() {
        let doc = parse_document("{box}\n> 囲み枠の引用\n");
        match &doc.blocks[0] {
            Block::BlockQuote { attrs, .. } => assert!(attrs.boxed),
            other => panic!("expected blockquote, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_headings_outline_basic() {
        let text =
            "# 見出し1\n本文\n## 見出し2\n```\n# コード内は見出しではない\n```\n### 見出し3\n";
        let headings = extract_headings_outline(text);
        assert_eq!(
            headings,
            vec![
                (1, "見出し1".to_string(), 0),
                (2, "見出し2".to_string(), 10),
                (3, "見出し3".to_string(), 41),
            ]
        );
    }

    #[test]
    fn test_other_fenced_code_block_stays_regular_code_block() {
        let doc = parse_document("```rust\nfn main() {}\n```\n");
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            Block::CodeBlock { lang, code, .. } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(code.contains("fn main()"));
            }
            other => panic!("Expected CodeBlock, got {other:?}"),
        }
    }
}
