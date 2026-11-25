//! テキスト操作ユーティリティ・シンタックスハイライト
//!
//! `ui.rs` から分離した、`TyperApp` の状態を持たない純粋なヘルパー関数群。
//! 文字/バイトインデックス変換、HEXカラー検証、独自拡張構文のシンタックスハイライトを担う。

use eframe::egui::{self, Color32, FontFamily, TextFormat, text::LayoutJob};

/// `#rgb` または `#rrggbb` 形式のHEXカラーコードとして妥当かを検証する
pub(crate) fn is_valid_hex_color(s: &str) -> bool {
    s.starts_with('#')
        && (s.len() == 4 || s.len() == 7)
        && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// 文字インデックス (`chars()` の順番) をUTF-8バイトオフセットに変換する
pub(crate) fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte_i, _)| byte_i)
        .unwrap_or(s.len())
}

/// UTF-8バイトオフセットを文字インデックスに変換する
pub(crate) fn byte_to_char_idx(s: &str, byte_idx: usize) -> usize {
    let clamped = byte_idx.min(s.len());
    s[..clamped].chars().count()
}

/// 文字インデックスから 1始まりの `(行番号, 列番号)` を計算する
/// 1始まりの行番号から、その行の先頭と末尾（改行を含まない）の文字インデックスを返す。
/// 行が存在しなければ `None`
pub(crate) fn line_char_range(s: &str, line: usize) -> Option<(usize, usize)> {
    if line == 0 {
        return None;
    }
    let mut start = 0usize;
    for (i, text) in s.split('\n').enumerate() {
        let len = text.trim_end_matches('\r').chars().count();
        if i + 1 == line {
            return Some((start, start + len));
        }
        start += text.chars().count() + 1;
    }
    None
}

pub(crate) fn char_idx_to_line_col(s: &str, char_idx: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in s.chars().enumerate() {
        if i >= char_idx {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// シンタックスハイライトの配色セット（ライト/ダークモードで切り替える）
struct HighlightPalette {
    default_color: Color32,
    heading_color: Color32,
    attr_color: Color32,
    attr_bg: Color32,
    pagebreak_color: Color32,
    pagebreak_bg: Color32,
    frontmatter_color: Color32,
    table_pipe_color: Color32,
}

impl HighlightPalette {
    fn for_theme(dark_mode: bool) -> Self {
        if dark_mode {
            Self {
                default_color: Color32::from_rgb(222, 224, 227),
                heading_color: Color32::from_rgb(130, 177, 255),
                attr_color: Color32::from_rgb(216, 160, 255),
                attr_bg: Color32::from_rgb(58, 46, 71),
                pagebreak_color: Color32::from_rgb(255, 171, 92),
                pagebreak_bg: Color32::from_rgb(66, 50, 26),
                frontmatter_color: Color32::from_rgb(129, 201, 149),
                table_pipe_color: Color32::from_rgb(130, 177, 255),
            }
        } else {
            Self {
                default_color: Color32::from_rgb(32, 33, 36),
                heading_color: Color32::from_rgb(23, 78, 166),
                attr_color: Color32::from_rgb(147, 52, 230),
                attr_bg: Color32::from_rgb(248, 242, 254),
                pagebreak_color: Color32::from_rgb(227, 116, 0),
                pagebreak_bg: Color32::from_rgb(254, 247, 224),
                frontmatter_color: Color32::from_rgb(13, 101, 45),
                table_pipe_color: Color32::from_rgb(26, 115, 232),
            }
        }
    }
}

/// 標準Markdownおよび独自拡張構文（`{widths: ...}`, `[...]{red}`, `:::pagebreak` 等）の
/// シンタックスハイライトを行う（`dark_mode` に応じて配色を切り替える）
pub(crate) fn highlight_custom_markdown(text: &str, dark_mode: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font_id = egui::FontId::new(14.5, FontFamily::Monospace);
    let palette = HighlightPalette::for_theme(dark_mode);

    let default_fmt = TextFormat {
        font_id: font_id.clone(),
        color: palette.default_color,
        ..Default::default()
    };
    let heading_fmt = TextFormat {
        font_id: egui::FontId::new(15.0, FontFamily::Monospace),
        color: palette.heading_color,
        ..Default::default()
    };
    let attr_fmt = TextFormat {
        font_id: font_id.clone(),
        color: palette.attr_color,
        background: palette.attr_bg,
        ..Default::default()
    };
    let pagebreak_fmt = TextFormat {
        font_id: font_id.clone(),
        color: palette.pagebreak_color,
        background: palette.pagebreak_bg,
        ..Default::default()
    };
    let frontmatter_fmt = TextFormat {
        font_id: font_id.clone(),
        color: palette.frontmatter_color,
        ..Default::default()
    };
    let table_pipe_fmt = TextFormat {
        font_id: font_id.clone(),
        color: palette.table_pipe_color,
        ..Default::default()
    };

    let mut in_frontmatter = false;
    for (idx, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        if idx == 0 && trimmed == "---" {
            in_frontmatter = true;
            job.append(line, 0.0, frontmatter_fmt.clone());
            continue;
        }
        if in_frontmatter {
            job.append(line, 0.0, frontmatter_fmt.clone());
            if trimmed == "---" {
                in_frontmatter = false;
            }
            continue;
        }

        if trimmed.starts_with('#') {
            job.append(line, 0.0, heading_fmt.clone());
        } else if trimmed.eq_ignore_ascii_case(":::pagebreak")
            || trimmed.eq_ignore_ascii_case("{pagebreak}")
        {
            job.append(line, 0.0, pagebreak_fmt.clone());
        } else if trimmed.starts_with('{') && trimmed.ends_with('}') && !trimmed.contains('|') {
            job.append(line, 0.0, attr_fmt.clone());
        } else if trimmed.starts_with('|') {
            append_line_with_inline_highlights(
                &mut job,
                line,
                &default_fmt,
                &attr_fmt,
                Some(&table_pipe_fmt),
            );
        } else {
            append_line_with_inline_highlights(&mut job, line, &default_fmt, &attr_fmt, None);
        }
    }

    if text.is_empty() {
        job.append("", 0.0, default_fmt);
    }

    job
}

fn append_line_with_inline_highlights(
    job: &mut LayoutJob,
    line: &str,
    default_fmt: &TextFormat,
    attr_fmt: &TextFormat,
    pipe_fmt: Option<&TextFormat>,
) {
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(pf) = pipe_fmt {
            if rest.starts_with('|') {
                job.append("|", 0.0, pf.clone());
                rest = &rest[1..];
                continue;
            }
        }
        if rest.starts_with('[') {
            if let Some(close_bracket) = rest.find("]{") {
                let after = close_bracket + 2;
                if let Some(close_brace) = rest[after..].find('}') {
                    let total = after + close_brace + 1;
                    job.append(&rest[..total], 0.0, attr_fmt.clone());
                    rest = &rest[total..];
                    continue;
                }
            }
        }
        if rest.starts_with('{') {
            if let Some(close_brace) = rest.find('}') {
                let inner = &rest[1..close_brace];
                if inner.contains(':') {
                    let total = close_brace + 1;
                    job.append(&rest[..total], 0.0, attr_fmt.clone());
                    rest = &rest[total..];
                    continue;
                }
            }
        }

        let first_char_len = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        let next_pos = if pipe_fmt.is_some() {
            rest[first_char_len..]
                .find(['|', '[', '{'])
                .map(|p| p + first_char_len)
                .unwrap_or(rest.len())
        } else {
            rest[first_char_len..]
                .find(['[', '{'])
                .map(|p| p + first_char_len)
                .unwrap_or(rest.len())
        };

        job.append(&rest[..next_pos], 0.0, default_fmt.clone());
        rest = &rest[next_pos..];
    }
}

/// Excelなどの表計算ソフトからコピーしたセル（タブ区切りのテキスト）を、Markdownの表に変換する。
///
/// - 1行目を見出し行にする（列数は最も多い行に合わせ、足りないセルは空にする）
/// - セル内の改行（Excelは `"..."` で囲んで書き出す）は `<br>` に、`|` は `\|` にする
/// - `<` `^` だけのセルは結合の指定と区別するため `\<` `\^` にする
///
/// タブを1つも含まない（表のデータではない）ときは `None`
pub(crate) fn tsv_to_markdown_table(text: &str) -> Option<String> {
    let rows = parse_tsv(text);
    if !rows.iter().any(|r| r.len() > 1) {
        return None;
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(1);
    let format_row = |row: &[String]| {
        let mut line = String::from("|");
        for i in 0..cols {
            let cell = row.get(i).map(String::as_str).unwrap_or("");
            line.push(' ');
            line.push_str(&escape_table_cell(cell));
            line.push_str(" |");
        }
        line
    };
    let mut out = String::new();
    out.push_str(&format_row(&rows[0]));
    out.push('\n');
    out.push('|');
    for _ in 0..cols {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in &rows[1..] {
        out.push_str(&format_row(row));
        out.push('\n');
    }
    Some(out)
}

fn escape_table_cell(cell: &str) -> String {
    let trimmed = cell.trim();
    if trimmed == "<" || trimmed == "^" {
        return format!("\\{trimmed}");
    }
    trimmed
        .replace('|', "\\|")
        .replace("\r\n", "\n")
        .replace('\n', "<br>")
}

/// タブ区切りのテキストを行・セルに分ける。`"` で始まるセルは、次の `"` までを
/// （タブ・改行・`""` で表した `"` を含めて）1つのセルとして読む
fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let text = text.replace("\r\n", "\n");
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    let mut at_cell_start = true;
    while let Some(ch) = chars.next() {
        match ch {
            '"' if at_cell_start => {
                at_cell_start = false;
                while let Some(c) = chars.next() {
                    if c == '"' {
                        if chars.peek() == Some(&'"') {
                            chars.next();
                            cell.push('"');
                        } else {
                            break;
                        }
                    } else {
                        cell.push(c);
                    }
                }
            }
            '\t' => {
                row.push(std::mem::take(&mut cell));
                at_cell_start = true;
            }
            '\n' => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
                at_cell_start = true;
            }
            c => {
                at_cell_start = false;
                cell.push(c);
            }
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    // 末尾の空行（Excelはコピーの最後に改行を付ける）は捨てる
    while rows.last().is_some_and(|r| r.iter().all(|c| c.is_empty())) {
        rows.pop();
    }
    rows
}

#[cfg(test)]
mod tsv_tests {
    use super::tsv_to_markdown_table;

    #[test]
    fn excel_cells_become_a_markdown_table() {
        let tsv = "項目\t担当\t期限\r\n資料作成\t山田\t11/30\r\n確認\t佐藤\t12/5\r\n";
        assert_eq!(
            tsv_to_markdown_table(tsv).unwrap(),
            "| 項目 | 担当 | 期限 |\n| --- | --- | --- |\n| 資料作成 | 山田 | 11/30 |\n| 確認 | 佐藤 | 12/5 |\n"
        );
    }

    #[test]
    fn quoted_cells_with_newlines_quotes_and_pipes() {
        let tsv = "a\tb\n\"1行目\n2行目\"\t\"言った\"\"はい\"\"\"\nx|y\t<\n";
        assert_eq!(
            tsv_to_markdown_table(tsv).unwrap(),
            "| a | b |\n| --- | --- |\n| 1行目<br>2行目 | 言った\"はい\" |\n| x\\|y | \\< |\n"
        );
    }

    #[test]
    fn ragged_rows_are_padded_and_empty_cells_kept() {
        let tsv = "a\tb\tc\n1\n\t\t3\n";
        assert_eq!(
            tsv_to_markdown_table(tsv).unwrap(),
            "| a | b | c |\n| --- | --- | --- |\n| 1 |  |  |\n|  |  | 3 |\n"
        );
    }

    #[test]
    fn text_without_tabs_is_not_a_table() {
        assert_eq!(tsv_to_markdown_table("ただの文章\n2行目\n"), None);
        assert_eq!(tsv_to_markdown_table(""), None);
    }

    #[test]
    fn converted_table_parses_as_a_table() {
        let md = tsv_to_markdown_table("a\tb\n1\t2\n").unwrap();
        let doc = crate::parser::parse_document(&md);
        assert!(matches!(doc.blocks[0], crate::model::Block::Table { .. }));
    }
}
