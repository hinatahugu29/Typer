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

const ATTR_MARKER_PREFIX: &str = "@@TYPER_ATTR:";
const ATTR_MARKER_SUFFIX: &str = "@@";
const PAGEBREAK_MARKER: &str = "@@TYPER_PAGEBREAK@@";

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
                        push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, block);
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
                                Block::PageBreak,
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
                    }

                    let content = expand_custom_inlines(raw_inlines);
                    let attrs = std::mem::take(&mut pending_attrs);
                    let block = Block::Paragraph { content, attrs };
                    push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, block);
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
                        push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, block);
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
                        push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, block);
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
                        push_block(&mut blocks, &mut blockquote_blocks, in_blockquote, block);
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
                                Inline::Image { alt, url }
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
            Event::Rule => {
                pending_attrs = BlockAttributes::default();
                push_block(
                    &mut blocks,
                    &mut blockquote_blocks,
                    in_blockquote,
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

    format!(
        "---\npaper: {}\norientation: {}\nmargin: {}\nfont-size: {}\nline-height: {:.1}\nfont: {}\n---\n\n{}",
        paper, orient, settings.margin, settings.font_size, settings.line_height, font, trimmed_body
    )
}

fn inlines_to_plain_text(inlines: &[Inline]) -> String {
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

fn push_block(
    blocks: &mut Vec<Block>,
    blockquote_blocks: &mut Vec<Block>,
    in_blockquote: bool,
    block: Block,
) {
    if in_blockquote {
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
                _ => {}
            }
        }
    }
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
    out
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
            // ショートハンド: `{red}` や `{14pt}`
            if let Some(c) = normalize_color(part) {
                style.color = Some(c);
            } else if part.ends_with("pt") {
                style.size = Some(part.to_string());
            }
        }
    }

    if style.color.is_some() || style.size.is_some() || style.bg.is_some() {
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
