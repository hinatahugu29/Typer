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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontChoice {
    Gothic,
    Mincho,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentSettings {
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
    /// 文書全体のアクセントカラー（見出し罫線・表ヘッダ・リンク等に反映。例: "#1a73e8"）
    pub accent_color: String,
}

impl Default for DocumentSettings {
    fn default() -> Self {
        Self {
            paper: "a4".to_string(),
            orientation: Orientation::Portrait,
            margin: "20mm".to_string(),
            font_size: "10.5pt".to_string(),
            line_height: 1.5,
            font_choice: FontChoice::Gothic,
            accent_color: "#1a73e8".to_string(),
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
    /// 引用ブロックを左バー付きの塗りつぶしではなく、全周を罫線で囲んだボックス表示にする
    pub boxed: bool,
    /// ブロック単位の表示幅（表以外の見出し・段落・引用などに使用。例: "60%", "100mm"）
    pub block_width: Option<String>,
}

impl BlockAttributes {
    pub fn is_empty(&self) -> bool {
        self.align.is_none()
            && self.line_height.is_none()
            && self.widths.is_none()
            && self.font_size.is_none()
            && self.color.is_none()
            && self.header_bg.is_none()
            && !self.boxed
            && self.block_width.is_none()
    }
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
        items: Vec<Vec<Inline>>,
        attrs: BlockAttributes,
    },
    Table {
        headers: Vec<Vec<Inline>>,
        alignments: Vec<Option<Alignment>>,
        rows: Vec<Vec<Vec<Inline>>>,
        attrs: BlockAttributes,
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
    RawTypst { code: String, attrs: BlockAttributes },
    /// `:::space 1em` 等で指定する明示的な垂直方向の空白ブロック
    Spacer { height: String },
    HorizontalRule,
    PageBreak,
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
    },
    SoftBreak,
    HardBreak,
}
