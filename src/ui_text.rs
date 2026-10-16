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

/// 標準Markdownおよび独自拡張構文（`{widths: ...}`, `[...]{red}`, `:::pagebreak` 等）の
/// シンタックスハイライトを行う
pub(crate) fn highlight_custom_markdown(text: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font_id = egui::FontId::new(14.5, FontFamily::Monospace);

    let default_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(32, 33, 36),
        ..Default::default()
    };
    let heading_fmt = TextFormat {
        font_id: egui::FontId::new(15.0, FontFamily::Monospace),
        color: Color32::from_rgb(23, 78, 166),
        ..Default::default()
    };
    let attr_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(147, 52, 230),
        background: Color32::from_rgb(248, 242, 254),
        ..Default::default()
    };
    let pagebreak_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(227, 116, 0),
        background: Color32::from_rgb(254, 247, 224),
        ..Default::default()
    };
    let frontmatter_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(13, 101, 45),
        ..Default::default()
    };
    let table_pipe_fmt = TextFormat {
        font_id: font_id.clone(),
        color: Color32::from_rgb(26, 115, 232),
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
