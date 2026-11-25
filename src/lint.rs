//! 原稿の書き間違いの検出（ロードマップ 7-1）
//!
//! 独自拡張の属性記法（`{align: right}` / `[文字]{red}` / `![図](a.png){width: 50%}` など）は、
//! 読み取れない指定があっても組版を止めず、黙って無視するか本文の文字として表示する。
//! 書いた本人は「効いていない」ことに気づきにくいため、ここで原稿を1行ずつ調べ、
//! 原稿の行番号付きの警告として返す。組版そのものには一切影響しない。

use crate::model::{Block, CellMerge, CellSlot, resolve_table_layout};
use crate::parser::{
    clean_frontmatter_value, is_columns_end_directive, is_standalone_block_attr_line,
    normalize_color, parse_columns_start_directive, parse_settings, parse_space_directive,
};

/// 原稿の書き間違い1件分
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxWarning {
    /// 原稿上の行番号（1始まり、フロントマターを含む）
    pub line: usize,
    pub message: String,
}

/// 段落・見出し・表などの直前に書く属性行で使えるキー
const BLOCK_KEYS: &str = "align, line-height, size, color, block-width, indent, indent-left, indent-right, widths, header-bg, row-padding, box";
/// `[文字]{...}` で使える指定
const INLINE_KEYS: &str = "色名・#RRGGBB, color, size, bg, underline";
/// `![図](パス){...}` で使えるキー
const IMAGE_KEYS: &str = "width, float, dx, dy";

/// 原稿全体を調べ、見つかった書き間違いを行番号順に返す
pub fn lint_markdown(text: &str) -> Vec<SyntaxWarning> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut warnings = Vec::new();

    let body_start = lint_frontmatter(&lines, &mut warnings);

    let settings = parse_settings(text);
    let mut defined_vars: Vec<String> = settings.custom_vars.keys().cloned().collect();
    if settings.title.is_some() {
        defined_vars.push("title".to_string());
    }

    let mut in_fenced_code = false;
    for (idx, raw_line) in lines.iter().enumerate().skip(body_start) {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fenced_code = !in_fenced_code;
            continue;
        }
        if in_fenced_code {
            continue;
        }
        let mut warn = |message: String| {
            warnings.push(SyntaxWarning {
                line: line_no,
                message,
            })
        };

        if trimmed.starts_with(":::") {
            if !is_known_directive(trimmed) {
                warn(format!(
                    "「{trimmed}」は使えない指示です（使えるもの: :::pagebreak / :::toc / :::columns 2 / :::columns-end / :::space 1em）"
                ));
            }
            continue;
        }

        if let Some(message) = check_block_attr_line(trimmed) {
            warn(message);
            continue;
        }
        if is_standalone_block_attr_line(trimmed) {
            for message in check_block_attrs(&trimmed[1..trimmed.len() - 1]) {
                warn(message);
            }
            continue;
        }

        let without_code = strip_inline_code(raw_line);
        for message in check_inline_specs(&without_code) {
            warn(message);
        }
        for message in check_variables(&without_code, &defined_vars) {
            warn(message);
        }
    }

    check_table_merges(&crate::parser::parse_document(text).blocks, &mut warnings);
    warnings.sort_by_key(|w| w.line);
    warnings
}

/// 表のセル結合の `<` / `^` のうち、結合先がなく空のセルになるものを調べる
fn check_table_merges(blocks: &[Block], warnings: &mut Vec<SyntaxWarning>) {
    for block in blocks {
        match block {
            Block::Table {
                headers,
                rows,
                attrs,
                merges,
                ..
            } => {
                let Some(table_line) = attrs.source_line else {
                    continue;
                };
                let col_count = headers
                    .len()
                    .max(rows.first().map(|r| r.len()).unwrap_or(1))
                    .max(1);
                let (header, body) =
                    resolve_table_layout(merges, !headers.is_empty(), rows.len(), col_count);
                let header_row = std::iter::once((table_line, &header, &merges.header));
                // 本文の行は、見出し行と区切り行（`| --- |`）の次から1行ずつ並ぶ
                let body_rows = body
                    .iter()
                    .zip(&merges.rows)
                    .enumerate()
                    .map(|(i, (slots, m))| (table_line + 2 + i, slots, m));
                for (line, slots, row_merges) in header_row.chain(body_rows) {
                    for (c, slot) in slots.iter().enumerate() {
                        if !matches!(slot, CellSlot::Origin { orphan: true, .. }) {
                            continue;
                        }
                        let message = match row_merges.get(c) {
                            Some(CellMerge::Left) => format!(
                                "表の{}列目の「<」は結合できないため、空のセルになります（左端の列では使えません。左のセルが上と結合している場合も、結合した範囲が長方形にならないため使えません）",
                                c + 1
                            ),
                            _ => format!(
                                "表の{}列目の「^」は結合できないため、空のセルになります（見出し行の直後の行では使えません。上のセルが横に結合している場合は、この行の同じ範囲もすべて「^」か「<」にしてください）",
                                c + 1
                            ),
                        };
                        warnings.push(SyntaxWarning { line, message });
                    }
                }
            }
            Block::BlockQuote { blocks, .. } | Block::Columns { blocks, .. } => {
                check_table_merges(blocks, warnings)
            }
            Block::List { items, .. } => {
                for item in items {
                    check_table_merges(&item.children, warnings);
                }
            }
            _ => {}
        }
    }
}

/// フロントマター（先頭の `---` 〜 `---`）の値を調べ、本文が始まる行の添字（0始まり）を返す
fn lint_frontmatter(lines: &[&str], warnings: &mut Vec<SyntaxWarning>) -> usize {
    if lines.first().map(|l| l.trim()) != Some("---") {
        return 0;
    }
    let Some(end) = lines
        .iter()
        .skip(1)
        .position(|l| l.trim() == "---")
        .map(|p| p + 1)
    else {
        return 0;
    };
    for (idx, line) in lines.iter().enumerate().take(end).skip(1) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut warn = |message: String| {
            warnings.push(SyntaxWarning {
                line: idx + 1,
                message,
            })
        };
        let Some((key, val)) = line.split_once(':') else {
            warn(format!(
                "「{line}」は「キー: 値」の形になっていないため無視されます"
            ));
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = clean_frontmatter_value(val);
        if val.is_empty() {
            continue;
        }
        match key.as_str() {
            "paper" => {
                if crate::model::paper_size(val).is_none() {
                    let names: Vec<&str> =
                        crate::model::PAPER_SIZES.iter().map(|p| p.label).collect();
                    warn(format!(
                        "paper の「{val}」は使えない用紙サイズのため A4 になります（{}）",
                        names.join(" / ")
                    ));
                }
            }
            "orientation" => {
                let v = val.to_ascii_lowercase();
                if !matches!(
                    v.as_str(),
                    "portrait" | "landscape" | "縦" | "縦向き" | "横" | "横向き"
                ) {
                    warn(format!(
                        "orientation の「{val}」は読み取れないため縦向きになります（portrait / landscape）"
                    ));
                }
            }
            "line-height" | "lineheight" => {
                if val.parse::<f32>().is_err() {
                    warn(format!(
                        "line-height の「{val}」は数値ではないため無視されます（例: 1.5）"
                    ));
                }
            }
            "margin" | "font-size" | "fontsize" | "size" => {
                if !is_valid_length(val) {
                    warn(length_message(&key, val));
                }
            }
            "columns" | "column" => {
                if !matches!(val.parse::<u8>(), Ok(1..=4)) {
                    warn(format!("columns の「{val}」は無視されます（1〜4の数）"));
                }
            }
            "accent-color" | "accent" => {
                if normalize_color(val).is_none() {
                    warn(color_message(&key, val));
                }
            }
            _ => {}
        }
    }
    end + 1
}

fn is_known_directive(trimmed: &str) -> bool {
    let low = trimmed.to_ascii_lowercase();
    matches!(
        low.as_str(),
        ":::pagebreak" | "::: pagebreak" | ":::toc" | "::: toc"
    ) || parse_columns_start_directive(trimmed).is_some()
        || is_columns_end_directive(trimmed)
        || parse_space_directive(trimmed).is_some()
}

/// `{` で始まる単独行のうち、属性行として読み取られないものを調べる。
/// 問題があればそのメッセージ、属性行として正しく読まれる（または属性行ではない）なら `None`
fn check_block_attr_line(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with('{') || trimmed.starts_with("{{") {
        return None;
    }
    let low = trimmed.to_ascii_lowercase();
    if matches!(low.as_str(), "{pagebreak}" | "{toc}") {
        return None;
    }
    let closed = trimmed.ends_with('}');
    let inner = if closed {
        &trimmed[1..trimmed.len() - 1]
    } else {
        &trimmed[1..]
    };
    // `{color:red|重要}` のような文中装飾の書き方は、行全体でも本文として扱われるので対象外
    if inner.contains('|') {
        return None;
    }
    let first = inner.split(',').next().unwrap_or("");
    let key = first
        .split_once(':')
        .map(|(k, _)| k)
        .unwrap_or(first)
        .trim();
    // 属性キーらしくない（日本語の文章を `{}` で囲んだだけ等）ものは、本文として正しい
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
        return None;
    }
    if !closed {
        if is_block_key(key) {
            return Some(format!(
                "属性行の閉じ括弧「}}」がありません。この行は本文として表示されます（「{trimmed}}}」のように閉じてください）"
            ));
        }
        return None;
    }
    if is_standalone_block_attr_line(trimmed) {
        return None;
    }
    if is_block_key(key) {
        // 既知のキーなのに属性行として読まれない（途中に `{` `}` がある等）
        return Some(format!(
            "「{trimmed}」は属性行として読み取れないため、本文として表示されます"
        ));
    }
    Some(format!(
        "「{key}」は使えない属性です。この行は本文として表示されます（使えるもの: {BLOCK_KEYS}）"
    ))
}

fn is_block_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
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
            | "row-padding"
            | "block-width"
            | "indent"
            | "indent-left"
            | "indent-right"
            | "box"
    )
}

/// 属性行の中身（`{` `}` の内側）を調べる。読み取りの規則は `parser::parse_block_attributes_into` と同じ
fn check_block_attrs(inner: &str) -> Vec<String> {
    let mut messages = Vec::new();
    let lower = inner.trim().to_ascii_lowercase();
    if let Some(rest) = lower
        .strip_prefix("widths:")
        .or_else(|| lower.strip_prefix("width:"))
    {
        for item in rest.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !is_valid_column_width(item) {
                messages.push(format!(
                    "列幅「{item}」は読み取れないため auto になります（例: 30%, 2fr, 40mm, auto）"
                ));
            }
        }
        return messages;
    }

    for part in inner.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((k, v)) = part.split_once(':') else {
            if !part.eq_ignore_ascii_case("box") {
                messages.push(format!(
                    "「{part}」は使えない属性です（使えるもの: {BLOCK_KEYS}）"
                ));
            }
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim();
        match key.as_str() {
            "align" => {
                if !matches!(
                    val.to_ascii_lowercase().as_str(),
                    "left" | "center" | "right" | "左" | "中央" | "右"
                ) {
                    messages.push(format!(
                        "align の「{val}」は読み取れないため無視されます（left / center / right）"
                    ));
                }
            }
            "line-height" | "lineheight" => {
                if val.parse::<f32>().is_err() {
                    messages.push(format!(
                        "line-height の「{val}」は数値ではないため無視されます（例: 1.2）"
                    ));
                }
            }
            "size" | "font-size" | "row-padding" | "block-width" | "indent" | "indent-left"
            | "indent-right" => {
                if !is_valid_length(val) {
                    messages.push(length_message(&key, val));
                }
            }
            "color" | "c" | "header-bg" => {
                if normalize_color(val).is_none() {
                    messages.push(color_message(&key, val));
                }
            }
            _ => messages.push(format!(
                "「{key}」は使えない属性のため無視されます（使えるもの: {BLOCK_KEYS}）"
            )),
        }
    }
    messages
}

/// 行の中の `[文字]{...}` と `![図](パス){...}` の指定を調べる
fn check_inline_specs(line: &str) -> Vec<String> {
    let mut messages = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = line[search_from..].find('{') {
        let open = search_from + rel;
        search_from = open + 1;
        let before = &line[..open];
        let kind = if before.ends_with(']') {
            SpecKind::Inline
        } else if before.ends_with(')') && before.contains("![") {
            SpecKind::Image
        } else {
            continue;
        };
        // `{{変数}}` は別に調べる
        if line[open..].starts_with("{{") {
            continue;
        }
        let Some(close_rel) = line[open..].find('}') else {
            messages.push(match kind {
                SpecKind::Inline => {
                    "文字装飾の閉じ括弧「}」がありません。装飾は効かず、そのまま文字として表示されます"
                        .to_string()
                }
                SpecKind::Image => "画像の指定の閉じ括弧「}」がありません".to_string(),
            });
            break;
        };
        let spec = &line[open + 1..open + close_rel];
        search_from = open + close_rel + 1;
        match kind {
            SpecKind::Inline => messages.extend(check_inline_style(spec)),
            SpecKind::Image => messages.extend(check_image_attrs(spec)),
        }
    }
    messages
}

#[derive(Clone, Copy)]
enum SpecKind {
    Inline,
    Image,
}

/// `[文字]{...}` の中身を調べる。読み取りの規則は `parser::parse_inline_style` と同じ
fn check_inline_style(spec: &str) -> Vec<String> {
    let mut messages = Vec::new();
    let mut any_valid = false;
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let ok = match part.split_once(':') {
            Some((k, v)) => {
                let val = v.trim();
                match k.trim().to_ascii_lowercase().as_str() {
                    "color" | "c" | "bg" | "highlight" => {
                        let valid = normalize_color(val).is_some();
                        if !valid {
                            messages.push(color_message(k.trim(), val));
                        }
                        valid
                    }
                    "size" | "font-size" => {
                        let valid = is_valid_length(val);
                        if !valid {
                            messages.push(length_message(k.trim(), val));
                        }
                        valid
                    }
                    other => {
                        messages.push(format!(
                            "文字装飾の「{other}」は使えない指定です（使えるもの: {INLINE_KEYS}）"
                        ));
                        false
                    }
                }
            }
            None => {
                let low = part.to_ascii_lowercase();
                let valid = low == "underline"
                    || low == "u"
                    || part == "下線"
                    || normalize_color(part).is_some()
                    || (part.ends_with("pt") && is_valid_length(part));
                if !valid {
                    messages.push(format!(
                        "文字装飾の「{part}」は読み取れません（使えるもの: {INLINE_KEYS}）"
                    ));
                }
                valid
            }
        };
        any_valid |= ok;
    }
    if !any_valid && messages.is_empty() {
        messages.push("文字装飾の指定が空です".to_string());
    }
    if !any_valid {
        if let Some(first) = messages.first_mut() {
            first.push_str("。装飾は効かず、[ ] と { } がそのまま表示されます");
        }
    }
    messages
}

/// `![図](パス){...}` の中身を調べる。読み取りの規則は `parser::try_parse_image_attrs` と同じ
fn check_image_attrs(spec: &str) -> Vec<String> {
    let mut messages = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once(':') {
            Some((k, v)) => {
                let key = k.trim().to_ascii_lowercase();
                let val = v.trim();
                match key.as_str() {
                    "width" | "w" | "dx" | "dy" => {
                        let valid = if key == "dx" || key == "dy" {
                            is_valid_offset(val)
                        } else {
                            is_valid_length(val)
                        };
                        if !valid {
                            messages.push(length_message(&key, val));
                        }
                    }
                    "float" => {
                        if !matches!(val.to_ascii_lowercase().as_str(), "true" | "false") {
                            messages
                                .push(format!("float の「{val}」は読み取れません（true / false）"));
                        }
                    }
                    _ => messages.push(format!(
                        "画像の「{key}」は使えない指定です（使えるもの: {IMAGE_KEYS}）"
                    )),
                }
            }
            None => {
                if !part.eq_ignore_ascii_case("float") {
                    messages.push(format!(
                        "画像の「{part}」は読み取れません（使えるもの: {IMAGE_KEYS}）"
                    ));
                }
            }
        }
    }
    messages
}

/// `{{キー}}` のうち、フロントマターで定義されていないものを調べる
fn check_variables(line: &str, defined: &[String]) -> Vec<String> {
    let mut messages = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start + 2..].find("}}") else {
            break;
        };
        let key = rest[start + 2..start + 2 + end].trim();
        if !key.is_empty() && !defined.iter().any(|d| d == key) {
            messages.push(format!(
                "変数「{key}」はフロントマターで定義されていないため、{{{{{key}}}}} のまま表示されます（フロントマターに「{key}: 値」を追加してください）"
            ));
        }
        rest = &rest[start + 2 + end + 2..];
    }
    messages
}

/// 行の中のインラインコード（`` `...` ``）を取り除く（中の記法は文字として書かれたもの）
fn strip_inline_code(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    for ch in line.chars() {
        if ch == '`' {
            in_code = !in_code;
            continue;
        }
        if !in_code {
            out.push(ch);
        }
    }
    out
}

/// `typst_gen::sanitize_length` と同じ規則：数値＋単位（pt / mm / cm / in / em / %）で 0 より大きく 1000 以下
fn is_valid_length(raw: &str) -> bool {
    let trimmed = raw.trim();
    ["pt", "mm", "cm", "in", "em", "%"].iter().any(|unit| {
        trimmed
            .strip_suffix(unit)
            .and_then(|n| n.trim().parse::<f32>().ok())
            .is_some_and(|v| v > 0.0 && v <= 1000.0)
    })
}

/// 画像のずらし量（`dx` / `dy`）は負の値や 0 も使える
fn is_valid_offset(raw: &str) -> bool {
    let trimmed = raw.trim();
    ["pt", "mm", "cm", "in", "em", "%"].iter().any(|unit| {
        trimmed
            .strip_suffix(unit)
            .and_then(|n| n.trim().parse::<f32>().ok())
            .is_some_and(|v| v.abs() <= 1000.0)
    })
}

/// `typst_gen::sanitize_column_width` と同じ規則
fn is_valid_column_width(raw: &str) -> bool {
    let item = raw.trim();
    if item == "auto" {
        return true;
    }
    for unit in ["fr", "%"] {
        if let Some(n) = item.strip_suffix(unit) {
            return n.trim().parse::<f32>().is_ok_and(|v| v > 0.0);
        }
    }
    is_valid_length(item)
}

fn length_message(key: &str, val: &str) -> String {
    let hint = if val.trim().parse::<f32>().is_ok() {
        "単位がありません。"
    } else {
        ""
    };
    format!("{key} の「{val}」は読み取れないため無視されます（{hint}例: 12pt, 20mm, 50%）")
}

fn color_message(key: &str, val: &str) -> String {
    format!(
        "{key} の「{val}」は色として読み取れないため無視されます（red / blue / green / orange / purple / gray / black、または #RRGGBB）"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(src: &str) -> Vec<usize> {
        lint_markdown(src).into_iter().map(|w| w.line).collect()
    }

    fn messages_of(src: &str) -> String {
        lint_markdown(src)
            .into_iter()
            .map(|w| format!("{}: {}", w.line, w.message))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn valid_documents_produce_no_warnings() {
        let src = "---\ntitle: 報告\npaper: A4\norientation: landscape\nmargin: 20mm\nfont-size: 10.5pt\nline-height: 1.5\naccent-color: \"#1a73e8\"\n会社名: サンプル\n---\n\n# {{会社名}} {{title}}\n\n{align: right, size: 9pt}\n日付\n\n{widths: 20%, 2fr, 30mm, auto}\n{header-bg: #fdf0e6}\n{row-padding: 12pt}\n| a | b | c | d |\n| - | - | - | - |\n| [赤]{red} | [大]{color: blue, size: 12pt} | [背景]{bg: #fff2cc} | [下線]{underline} |\n\n{box}\n> 注意\n\n![図](a.png){width: 50%, float: true, dx: -5mm, dy: 0mm}\n\n:::pagebreak\n:::toc\n:::columns 2\n:::columns-end\n:::space 1em\n{pagebreak}\n\n`[x]{nope}` と {{未定義}} はコード内なので対象外ではない\n";
        // 最後の行の {{未定義}} だけが警告になる（インラインコードの中の記法は対象外）
        let warnings = lint_markdown(src);
        assert_eq!(warnings.len(), 1, "{}", messages_of(src));
        assert!(warnings[0].message.contains("未定義"));
    }

    #[test]
    fn unknown_block_attribute_key_is_reported_with_its_line() {
        let src = "# 見出し\n\n{colr: red}\n本文\n";
        assert_eq!(lines_of(src), vec![3]);
        assert!(messages_of(src).contains("「colr」は使えない属性"));
    }

    #[test]
    fn bad_values_in_a_recognized_attribute_line_are_reported() {
        let src = "{align: middle, size: 12, color: redd, line-height: wide, foo: 1}\n本文\n";
        let msg = messages_of(src);
        assert!(msg.contains("align の「middle」"), "{msg}");
        assert!(
            msg.contains("size の「12」") && msg.contains("単位がありません"),
            "{msg}"
        );
        assert!(msg.contains("color の「redd」"), "{msg}");
        assert!(msg.contains("line-height の「wide」"), "{msg}");
        assert!(msg.contains("「foo」は使えない属性"), "{msg}");
        assert!(lines_of(src).iter().all(|&l| l == 1));
    }

    #[test]
    fn bad_column_widths_are_reported() {
        let msg = messages_of("{widths: 20%, abc, 30}\n| a | b | c |\n| - | - | - |\n");
        assert!(
            msg.contains("列幅「abc」") && msg.contains("列幅「30」"),
            "{msg}"
        );
    }

    #[test]
    fn unclosed_attribute_line_is_reported() {
        let msg = messages_of("{align: right\n本文\n");
        assert!(msg.contains("閉じ括弧"), "{msg}");
    }

    #[test]
    fn braces_around_ordinary_text_are_not_reported() {
        assert!(lint_markdown("{注意事項}\n\n{ 補足 }\n\n{a: b}c\n").is_empty());
    }

    #[test]
    fn inline_style_mistakes_are_reported() {
        let msg = messages_of("これは[重要]{rde}です。[大きく]{size: 12}。[枠]{border: 1pt}\n");
        assert!(msg.contains("「rde」は読み取れません"), "{msg}");
        assert!(msg.contains("そのまま表示されます"), "{msg}");
        assert!(msg.contains("size の「12」"), "{msg}");
        assert!(msg.contains("「border」は使えない指定"), "{msg}");
    }

    #[test]
    fn image_attribute_mistakes_are_reported() {
        let msg = messages_of("![図](a.png){widht: 50%}\n\n![図](b.png){width: 50}\n");
        assert!(msg.contains("画像の「widht」"), "{msg}");
        assert!(msg.contains("width の「50」"), "{msg}");
        assert_eq!(
            lines_of("![図](a.png){widht: 50%}\n\n![図](b.png){width: 50}\n"),
            vec![1, 3]
        );
    }

    #[test]
    fn unknown_directive_is_reported() {
        let msg = messages_of("本文\n:::pagebrek\n");
        assert!(msg.starts_with("2: 「:::pagebrek」は使えない指示"), "{msg}");
    }

    #[test]
    fn undefined_variable_is_reported_but_defined_ones_are_not() {
        let src = "---\n会社名: サンプル\n---\n{{会社名}} {{部署}}\n";
        let warnings = lint_markdown(src);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].line, 4);
        assert!(warnings[0].message.contains("「部署」"));
    }

    #[test]
    fn fenced_code_is_skipped() {
        assert!(lint_markdown("```\n{colr: red}\n[a]{zzz}\n:::nope\n```\n").is_empty());
        assert!(lint_markdown("~~~typst\n#let x = {colr: 1}\n~~~\n").is_empty());
    }

    #[test]
    fn frontmatter_value_mistakes_are_reported_on_their_lines() {
        let src = "---\norientation: sideways\nmargin: 20\nline-height: big\naccent-color: navy\nこれは値なし\n---\n本文\n";
        assert_eq!(lines_of(src), vec![2, 3, 4, 5, 6]);
    }

    #[test]
    fn crlf_and_bom_keep_line_numbers() {
        let src = "\u{feff}---\r\ntitle: t\r\n---\r\n\r\n{colr: red}\r\n本文\r\n";
        assert_eq!(lines_of(src), vec![5]);
    }
}

#[cfg(test)]
mod builtin_document_tests {
    use super::lint_markdown;

    /// アプリに同梱している原稿（サンプル・テンプレート・説明書の例）で誤検出しないこと
    #[test]
    fn bundled_documents_have_no_warnings() {
        let mut docs: Vec<(String, String)> = vec![(
            "sample".to_string(),
            crate::sample::SAMPLE_DOCUMENT.to_string(),
        )];
        for (name, body) in crate::templates::TEMPLATES {
            docs.push((name.to_string(), body.to_string()));
        }
        for (name, text) in docs {
            let warnings = lint_markdown(&text);
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
        }
    }

    /// 説明書（実践ガイド・マニュアル・README）の ```markdown のコード例でも誤検出しないこと
    #[test]
    fn documentation_examples_have_no_warnings() {
        for guide in [
            include_str!("../docs/PRACTICAL_GUIDE.md"),
            include_str!("../docs/MANUAL.md"),
            include_str!("../README.md"),
        ] {
            check_examples(guide);
        }
    }

    fn check_examples(guide: &str) {
        let mut failures = Vec::new();
        let mut in_example = false;
        let mut example = String::new();
        for line in guide.lines() {
            if !in_example && (line.starts_with("```markdown") || line.starts_with("````markdown"))
            {
                in_example = true;
                example.clear();
                continue;
            }
            if in_example && (line == "```" || line == "````") {
                in_example = false;
                // 説明のためにわざと誤りを書いた例は除く
                if !example.contains("誤り") && !example.contains("間違い") {
                    for w in lint_markdown(&example) {
                        failures.push(format!("{}: {}\n---\n{}", w.line, w.message, example));
                    }
                }
                continue;
            }
            if in_example {
                example.push_str(line);
                example.push('\n');
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }
}
