//! 内部文書モデル（AST）
//!
//! 標準Markdownの構造と、将来・現在の独自拡張属性（表の列幅、行間、文字配置、文字色、ページ設定など）を
//! 保持する中間表現。パーサー層と出力層（Typst / HTML）を疎結合に保つ役割を担う。

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub settings: DocumentSettings,
    pub blocks: Vec<Block>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            settings: DocumentSettings::default(),
            blocks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontChoice {
    Gothic,
    Mincho,
    /// ユーザーが指定したフォントファミリー名をそのまま使用する（Windowsにインストール済みの任意フォント）
    Custom(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentSettings {
    /// 文書タイトル（HTMLのタブ名・PDFの文書タイトルに使う）。未指定時は最初の見出しやファイル名で補う
    pub title: Option<String>,
    /// 用紙サイズ（初期仕様は "a4"）
    pub paper: String,
    /// 用紙の向き（縦 / 横）
    pub orientation: Orientation,
    /// ページ余白（例: "20mm", "25mm"）
    pub margin: String,
    /// 基本文字サイズ（例: "10.5pt", "11pt"）
    pub font_size: String,
    /// 基本行間（倍率表記: 1.5 = 行送り調整）
    pub line_height: f32,
    /// 基本フォント種別（ゴシック / 明朝）
    pub font_choice: FontChoice,
    /// 欧文（英数字）だけに使うフォント。未指定なら和文フォントの欧文グリフを使う
    pub latin_font: Option<String>,
    /// 文書全体のアクセントカラー（見出し罫線・表ヘッダ・リンク等に反映。例: "#1a73e8"）
    pub accent_color: String,
    /// 全ページ共通のヘッダー文字列（社名・資料名など。未指定時は非表示）
    pub header_text: Option<String>,
    /// 全ページ共通のフッター文字列（「社外秘」等の表記。未指定時は非表示）
    pub footer_text: Option<String>,
    /// ページ番号を表示するか（既定: 表示する）
    pub show_page_number: bool,
    /// 本文の段組み数（既定: 1 = 段組みなし。2〜4段をサポート）
    pub columns: u8,
    /// 透かし文字（例: "社外秘", "DRAFT"）。未指定時は非表示
    pub watermark: Option<String>,
    /// フロントマターで定義された独自変数（本文中の `{{key}}` を値に置換するために使用）
    pub custom_vars: std::collections::BTreeMap<String, String>,
}

impl Default for DocumentSettings {
    fn default() -> Self {
        Self {
            title: None,
            paper: "a4".to_string(),
            orientation: Orientation::Portrait,
            margin: "20mm".to_string(),
            font_size: "10.5pt".to_string(),
            line_height: 1.5,
            font_choice: FontChoice::Gothic,
            latin_font: None,
            accent_color: "#1a73e8".to_string(),
            header_text: None,
            footer_text: None,
            show_page_number: true,
            columns: 1,
            watermark: None,
            custom_vars: std::collections::BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
}

/// ブロック要素（段落、表、見出しなど）に付与できる拡張属性の「器」
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockAttributes {
    /// 文字配置（left / center / right）
    pub align: Option<Alignment>,
    /// 段落単位の行間（例: 1.2, 1.8）
    pub line_height: Option<f32>,
    /// 表の列幅リスト（例: ["20%", "50%", "30%"] や ["30mm", "1fr", "auto"]）
    pub widths: Option<Vec<String>>,
    /// ブロック単位の文字サイズ（例: "12pt", "9pt"）
    pub font_size: Option<String>,
    /// ブロック単位の文字色（見出し・段落の個別色上書きに使用。例: "red", "#188038"）
    pub color: Option<String>,
    /// 表のヘッダー行の背景色上書き（表ブロックのみ使用。例: "#fdf0e6"）
    pub header_bg: Option<String>,
    /// 表のセル内上下余白の上書き（表ブロックのみ使用。行の高さに見えるゆとりを調整する。例: "12pt", "4mm"）
    pub row_padding: Option<String>,
    /// 引用ブロックを左バー付きの塗りつぶしではなく、全周を罫線で囲んだボックス表示にする
    pub boxed: bool,
    /// ブロック単位の表示幅（表以外の見出し・段落・引用などに使用。例: "60%", "100mm"）
    pub block_width: Option<String>,
    /// ブロック単位の左インデント（段下げ。例: "20mm", "2em"）
    pub indent_left: Option<String>,
    /// ブロック単位の右インデント（例: "20mm", "2em"）
    pub indent_right: Option<String>,
    /// このブロックが始まる原稿上の行番号（1始まり、フロントマターを含む原稿全体での行）。
    /// 書式ではなく、組版エラーを原稿の行へ対応づけるための位置情報（`is_empty` の判定には含めない）
    pub source_line: Option<usize>,
}

impl BlockAttributes {
    pub fn is_empty(&self) -> bool {
        self.align.is_none()
            && self.line_height.is_none()
            && self.widths.is_none()
            && self.font_size.is_none()
            && self.color.is_none()
            && self.header_bg.is_none()
            && self.row_padding.is_none()
            && !self.boxed
            && self.block_width.is_none()
            && self.indent_left.is_none()
            && self.indent_right.is_none()
    }
}

/// リストの1項目。項目の本文（最初の段落）に加えて、その下に書かれた
/// 入れ子リスト・続きの段落・コードブロック・引用などを `children` に保持する
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListItem {
    pub content: Vec<Inline>,
    pub children: Vec<Block>,
    /// チェックリストの項目（`- [ ]` / `- [x]`）なら `Some(チェック済みか)`
    pub checked: Option<bool>,
    /// この項目が始まる原稿上の行番号（1始まり。`BlockAttributes::source_line` と同じ）
    pub source_line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
        attrs: BlockAttributes,
    },
    Paragraph {
        content: Vec<Inline>,
        attrs: BlockAttributes,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
        attrs: BlockAttributes,
    },
    Table {
        headers: Vec<Vec<Inline>>,
        alignments: Vec<Option<Alignment>>,
        rows: Vec<Vec<Vec<Inline>>>,
        attrs: BlockAttributes,
        /// セル結合の指定（見出し行・各行のセルごと。`<` / `^` だけを書いたセル）
        merges: TableMerges,
    },
    BlockQuote {
        blocks: Vec<Block>,
        attrs: BlockAttributes,
    },
    CodeBlock {
        lang: Option<String>,
        code: String,
        attrs: BlockAttributes,
    },
    /// ` ```typst ` フェンスコードブロックによる上級者向けの生Typstコード直接挿入。
    /// 内容はエスケープせずそのままTypstソースへ埋め込まれる（HTML出力では評価不能のため注記表示のみ）。
    RawTypst {
        code: String,
        attrs: BlockAttributes,
    },
    /// `:::space 1em` 等で指定する明示的な垂直方向の空白ブロック
    Spacer {
        height: String,
    },
    HorizontalRule,
    PageBreak,
    /// `:::toc` で挿入する目次。見出し一覧から自動生成される（PDF出力はページ番号付き）
    TableOfContents,
    /// `:::columns N` 〜 `:::columns-end` で囲んだ範囲だけを段組み表示するブロック
    Columns {
        count: u8,
        blocks: Vec<Block>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InlineStyle {
    /// 文字色（例: "red", "blue", "#d93025"）
    pub color: Option<String>,
    /// 文字サイズ（例: "12pt", "80%"）
    pub size: Option<String>,
    /// 背景ハイライト色
    pub bg: Option<String>,
    /// 下線を表示するか
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Code(String),
    Styled {
        content: Vec<Inline>,
        style: InlineStyle,
    },
    Link {
        text: Vec<Inline>,
        url: String,
    },
    Image {
        alt: String,
        url: String,
        /// 画像の表示幅（例: "50%", "80mm"）。未指定時は既定の80%幅で表示
        width: Option<String>,
        /// true の場合、本文の流れから外して現在位置に浮かせて重ね配置する（透過画像などの絶対配置用途）
        float: bool,
        /// float時の水平方向オフセット（例: "10mm"）。基準は現在のカーソル（段落）位置
        offset_x: Option<String>,
        /// float時の垂直方向オフセット（例: "-5mm"）。基準は現在のカーソル（段落）位置
        offset_y: Option<String>,
    },
    /// `$...$`（行内）/ `$$...$$`（独立行）の数式。中身はLaTeX記法のまま保持する
    Math {
        tex: String,
        display: bool,
    },
    /// 脚注（`本文[^1]` ＋ `[^1]: 注記`）。定義の中身を参照位置に持たせる。
    /// `id` は文書中で最初に参照された順の通し番号（0始まり）。同じ脚注を2回目以降に
    /// 参照した箇所は `repeat: true` になり、各出力形式は新しい注を作らず同じ番号を指す
    Footnote {
        id: usize,
        content: Vec<Inline>,
        repeat: bool,
    },
    /// 解析途中だけ使う、定義との対応付け前の脚注参照（`[^ラベル]`）。
    /// `parse_document` の最後に `Footnote` か元の文字列に置き換わるため、出力層には届かない
    FootnoteRef(String),
    SoftBreak,
    HardBreak,
}

/// 表のセルに書かれた結合の指定
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellMerge {
    /// 結合しない（ふつうのセル）
    #[default]
    None,
    /// `<` だけを書いたセル：左のセルと結合する
    Left,
    /// `^` だけを書いたセル：上のセルと結合する
    Up,
}

/// 表全体のセル結合の指定。添字は `headers` / `rows` と同じ（足りない分は結合なし）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableMerges {
    pub header: Vec<CellMerge>,
    pub rows: Vec<Vec<CellMerge>>,
}

/// 結合を解決した後の、表の1マスの描き方
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellSlot {
    /// セルを描く位置。`colspan` × `rowspan` マスを占める。
    /// `orphan` は結合の指定（`<` / `^`）なのに結合先がなかったセルで、中身は空にする
    Origin {
        colspan: usize,
        rowspan: usize,
        orphan: bool,
    },
    /// 左や上のセルに結合されて、描かないマス
    Covered,
}

/// 表のセル結合を解決し、見出し行と各行の1マスごとの描き方を返す（どちらも `col_count` 列）。
///
/// - `<` は左隣と結合する。左端の列に書いた `<` は結合先がない
/// - `^` は上のセルと結合する。見出し行と本文の行をまたいでは結合しない
/// - 結合した範囲は必ず長方形になる。2行目以降に広げられるのは、その行の該当する全マスが
///   `^`（左端の列）または `^` / `<`（それ以外の列）のときだけ
pub fn resolve_table_layout(
    merges: &TableMerges,
    has_header: bool,
    row_count: usize,
    col_count: usize,
) -> (Vec<CellSlot>, Vec<Vec<CellSlot>>) {
    // 見出し行を0行目として、本文の行と同じ格子で解決する
    let header_rows = usize::from(has_header);
    let total = header_rows + row_count;
    let merge_at = |r: usize, c: usize| -> CellMerge {
        let row = if r < header_rows {
            Some(&merges.header)
        } else {
            merges.rows.get(r - header_rows)
        };
        row.and_then(|m| m.get(c)).copied().unwrap_or_default()
    };
    let same_section = |a: usize, b: usize| (a < header_rows) == (b < header_rows);

    let mut grid: Vec<Vec<Option<CellSlot>>> = vec![vec![None; col_count]; total];
    for r in 0..total {
        for c in 0..col_count {
            if grid[r][c].is_some() {
                continue;
            }
            let mut colspan = 1;
            while c + colspan < col_count
                && merge_at(r, c + colspan) == CellMerge::Left
                && grid[r][c + colspan].is_none()
            {
                colspan += 1;
            }
            let mut rowspan = 1;
            while r + rowspan < total && same_section(r, r + rowspan) {
                let next = r + rowspan;
                let first_up = merge_at(next, c) == CellMerge::Up;
                let rest_merged = (c + 1..c + colspan)
                    .all(|cc| matches!(merge_at(next, cc), CellMerge::Up | CellMerge::Left));
                let free = (c..c + colspan).all(|cc| grid[next][cc].is_none());
                if !(first_up && rest_merged && free) {
                    break;
                }
                rowspan += 1;
            }
            for row in grid.iter_mut().skip(r).take(rowspan) {
                for slot in row.iter_mut().skip(c).take(colspan) {
                    *slot = Some(CellSlot::Covered);
                }
            }
            grid[r][c] = Some(CellSlot::Origin {
                colspan,
                rowspan,
                orphan: merge_at(r, c) != CellMerge::None,
            });
        }
    }
    let mut rows: Vec<Vec<CellSlot>> = grid
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|s| s.unwrap_or(CellSlot::Covered))
                .collect()
        })
        .collect();
    let header = if has_header {
        rows.remove(0)
    } else {
        Vec::new()
    };
    (header, rows)
}

#[cfg(test)]
mod table_layout_tests {
    use super::*;
    use CellMerge::{Left as L, None as N, Up as U};

    fn origin(colspan: usize, rowspan: usize) -> CellSlot {
        CellSlot::Origin {
            colspan,
            rowspan,
            orphan: false,
        }
    }
    const C: CellSlot = CellSlot::Covered;

    #[test]
    fn colspan_rowspan_and_block_merges() {
        let merges = TableMerges {
            header: vec![N, L, N],
            rows: vec![vec![N, L, N], vec![U, L, N], vec![U, U, N]],
        };
        let (header, rows) = resolve_table_layout(&merges, true, 3, 3);
        assert_eq!(header, vec![origin(2, 1), C, origin(1, 1)]);
        // 1行目の先頭セルが 2列×3行 を占め、3列目はそれぞれ独立
        assert_eq!(rows[0], vec![origin(2, 3), C, origin(1, 1)]);
        assert_eq!(rows[1], vec![C, C, origin(1, 1)]);
        assert_eq!(rows[2], vec![C, C, origin(1, 1)]);
    }

    #[test]
    fn two_by_two_block() {
        let merges = TableMerges {
            header: vec![],
            rows: vec![vec![N, L], vec![U, U]],
        };
        let (_, rows) = resolve_table_layout(&merges, true, 2, 2);
        assert_eq!(rows[0], vec![origin(2, 2), C]);
        assert_eq!(rows[1], vec![C, C]);
    }

    #[test]
    fn markers_without_a_target_are_orphans() {
        let orphan = CellSlot::Origin {
            colspan: 1,
            rowspan: 1,
            orphan: true,
        };
        // 左端の `<`、本文1行目の `^`（見出しとはまたがない）
        let merges = TableMerges {
            header: vec![N, N],
            rows: vec![vec![U, N], vec![L, N]],
        };
        let (_, rows) = resolve_table_layout(&merges, true, 2, 2);
        assert_eq!(rows[0][0], orphan);
        assert_eq!(rows[1][0], orphan);
        // 長方形にならない結合：上の行が2列結合でも、下の行が `^` と普通のセルなら広げない
        let merges = TableMerges {
            header: vec![],
            rows: vec![vec![N, L], vec![U, N]],
        };
        let (_, rows) = resolve_table_layout(&merges, false, 2, 2);
        assert_eq!(rows[0], vec![origin(2, 1), C]);
        assert_eq!(rows[1], vec![orphan, origin(1, 1)]);
    }

    #[test]
    fn short_rows_and_missing_merges_are_plain_cells() {
        let (header, rows) = resolve_table_layout(&TableMerges::default(), true, 1, 3);
        assert_eq!(header, vec![origin(1, 1); 3]);
        assert_eq!(rows[0], vec![origin(1, 1); 3]);
    }
}
