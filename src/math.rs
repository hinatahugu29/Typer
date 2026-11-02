//! `$...$` に書かれたLaTeX記法の数式を、Typstの数式記法へ変換する
//!
//! 計算プリントや簡単な技術文書で使う範囲（四則演算・分数・平方根・べき乗・添字・
//! ギリシャ文字・関係演算子など）に絞った軽量な変換器。対応表にないコマンドは
//! `"\foo"` のような文字列として出力する。Typst側で未定義の識別子を出すと文書全体の
//! コンパイルが失敗し、プレビューが丸ごと表示されなくなるため、未知の入力は必ず
//! 文字列か安全な記号に落とす。

/// 引数なしで記号・演算子に置き換わるコマンド
const SYMBOLS: &[(&str, &str)] = &[
    // 四則演算・符号
    ("times", "times"),
    ("div", "div"),
    ("cdot", "dot.op"),
    ("pm", "plus.minus"),
    ("mp", "minus.plus"),
    ("ast", "ast"),
    ("star", "star"),
    ("circ", "circle.small"),
    ("degree", "degree"),
    // 関係
    ("le", "<="),
    ("leq", "<="),
    ("ge", ">="),
    ("geq", ">="),
    ("ne", "!="),
    ("neq", "!="),
    ("approx", "approx"),
    ("equiv", "equiv"),
    ("sim", "tilde.op"),
    ("propto", "prop"),
    ("ll", "<<"),
    ("gg", ">>"),
    // 矢印・論理
    ("to", "->"),
    ("rightarrow", "->"),
    ("leftarrow", "<-"),
    ("Rightarrow", "=>"),
    ("Leftarrow", "arrow.l.double"),
    ("Leftrightarrow", "<=>"),
    ("leftrightarrow", "<->"),
    ("implies", "=>"),
    ("iff", "<=>"),
    ("therefore", "therefore"),
    ("because", "because"),
    ("forall", "forall"),
    ("exists", "exists"),
    ("neg", "not"),
    ("land", "and"),
    ("lor", "or"),
    // 集合
    ("in", "in"),
    ("notin", "in.not"),
    ("subset", "subset"),
    ("subseteq", "subset.eq"),
    ("supset", "supset"),
    ("cup", "union"),
    ("cap", "inter"),
    ("emptyset", "emptyset"),
    ("varnothing", "emptyset"),
    // 解析・その他
    ("infty", "infinity"),
    ("partial", "partial"),
    ("nabla", "nabla"),
    ("sum", "sum"),
    ("prod", "product"),
    ("int", "integral"),
    ("oint", "integral.cont"),
    ("angle", "angle"),
    ("perp", "perp"),
    ("parallel", "parallel"),
    ("triangle", "triangle.stroked.t"),
    ("ldots", "dots.h"),
    ("dots", "dots.h"),
    ("cdots", "dots.h.c"),
    ("vdots", "dots.v"),
    ("prime", "prime"),
    // 関数名（立体で表示される）
    ("sin", "sin"),
    ("cos", "cos"),
    ("tan", "tan"),
    ("log", "log"),
    ("ln", "ln"),
    ("exp", "exp"),
    ("lim", "lim"),
    ("max", "max"),
    ("min", "min"),
    ("det", "det"),
    ("gcd", "gcd"),
    // 空白
    ("quad", "quad"),
    ("qquad", "wide"),
    (",", "thin"),
    (";", "med"),
    (":", "med"),
    (" ", "space"),
    ("!", ""),
    // 記号そのもの
    ("%", "%"),
    ("$", "\\$"),
    ("&", "\\&"),
    ("#", "\\#"),
    ("_", "\\_"),
    ("{", "brace.l"),
    ("}", "brace.r"),
    ("|", "bar.v.double"),
    ("\\", "\\"),
    ("langle", "chevron.l"),
    ("rangle", "chevron.r"),
];

/// ギリシャ文字（LaTeXとTypstで名前が同じ）
const GREEK: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
    "lambda", "mu", "nu", "xi", "pi", "rho", "sigma", "tau", "upsilon", "phi", "chi", "psi",
    "omega", "Gamma", "Delta", "Theta", "Lambda", "Xi", "Pi", "Sigma", "Upsilon", "Phi", "Psi",
    "Omega",
];

/// 引数1つを取り、Typstの関数呼び出しになるコマンド
const UNARY: &[(&str, &str)] = &[
    ("sqrt", "sqrt"),
    ("mathbf", "bold"),
    ("boldsymbol", "bold"),
    ("mathit", "italic"),
    ("mathrm", "upright"),
    ("overline", "overline"),
    ("underline", "underline"),
    ("bar", "overline"),
    ("hat", "hat"),
    ("widehat", "hat"),
    ("tilde", "tilde"),
    ("widetilde", "tilde"),
    ("vec", "arrow"),
    ("dot", "dot"),
    ("ddot", "dot.double"),
    ("abs", "abs"),
];

/// 引数2つを取るコマンド
const BINARY: &[(&str, &str)] = &[
    ("frac", "frac"),
    ("dfrac", "frac"),
    ("tfrac", "frac"),
    ("binom", "binom"),
];

/// 後ろの区切り記号だけを残して読み捨てるサイズ指定コマンド
const SIZING: &[&str] = &[
    "left", "right", "big", "Big", "bigg", "Bigg", "bigl", "bigr", "Bigl", "Bigr", "displaystyle",
    "textstyle",
];

pub fn latex_to_typst(tex: &str) -> String {
    let mut conv = Converter {
        chars: tex.chars().collect(),
        pos: 0,
        call_depth: 0,
    };
    conv.sequence(None).trim().to_string()
}

struct Converter {
    chars: Vec<char>,
    pos: usize,
    /// `frac(...)` などTypst関数の引数の中にいる深さ。引数内では `,` `;` が区切りになるため文字列にする
    call_depth: usize,
}

impl Converter {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    /// `stop`（`}` や `]`）またはテキスト末尾まで読み、要素を空白区切りで並べる
    fn sequence(&mut self, stop: Option<char>) -> String {
        let mut out = String::new();
        loop {
            self.skip_spaces();
            let Some(c) = self.peek() else { break };
            if Some(c) == stop {
                self.pos += 1;
                break;
            }
            if c == '^' || c == '_' {
                self.pos += 1;
                let arg = self.argument();
                if !arg.is_empty() {
                    // 添字は直前の要素にくっつける（空白を挟まない）
                    out.push(c);
                    out.push('(');
                    out.push_str(&arg);
                    out.push(')');
                }
                continue;
            }
            let atom = self.atom(stop);
            if atom.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&atom);
        }
        out
    }

    /// `^` `_` やコマンドの引数。`{...}` ならその中身、そうでなければ次の1要素
    fn argument(&mut self) -> String {
        self.skip_spaces();
        match self.peek() {
            Some('{') => {
                self.pos += 1;
                self.sequence(Some('}'))
            }
            Some(_) => {
                // `x^23` はLaTeXでは `x^{2}3` の意味なので、数字も1文字だけ取る
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                    return self.chars[self.pos - 1].to_string();
                }
                self.atom(None)
            }
            None => String::new(),
        }
    }

    /// 関数呼び出しの引数として埋め込む形（空なら `""`）
    fn call_argument(&mut self) -> String {
        self.call_depth += 1;
        let arg = self.argument();
        self.call_depth -= 1;
        if arg.trim().is_empty() {
            "\"\"".to_string()
        } else {
            arg
        }
    }

    fn atom(&mut self, stop: Option<char>) -> String {
        let Some(c) = self.peek() else {
            return String::new();
        };
        self.pos += 1;
        match c {
            '\\' => self.command(),
            '{' => self.sequence(Some('}')),
            // 対応の取れない閉じ括弧は読み捨てる
            '}' if stop != Some('}') => String::new(),
            '0'..='9' | '.' => {
                let mut s = c.to_string();
                while let Some(d) = self.peek().filter(|d| d.is_ascii_digit() || *d == '.') {
                    s.push(d);
                    self.pos += 1;
                }
                s
            }
            // Typstでは連続した英字が1つの識別子として解釈されるため、1文字ずつ区切る
            'a'..='z' | 'A'..='Z' => c.to_string(),
            '+' | '-' | '=' | '<' | '>' | '(' | ')' | '[' | ']' | '|' | '!' | '\'' | '*' | ':'
            | '?' | '&' => c.to_string(),
            // Typstの数式では `/` が分数、関数引数の中の `,` `;` が区切りになるため文字として出す
            '/' => "\\/".to_string(),
            ',' | ';' if self.call_depth > 0 => quote(&c.to_string()),
            ',' | ';' => c.to_string(),
            '~' => "space".to_string(),
            '"' => "\\\"".to_string(),
            '#' | '$' | '@' | '`' => format!("\\{c}"),
            other if other.is_alphanumeric() => quote(&other.to_string()),
            other if other.is_ascii() => quote(&other.to_string()),
            other => other.to_string(),
        }
    }

    fn command(&mut self) -> String {
        let Some(first) = self.peek() else {
            return String::new();
        };
        let name: String = if first.is_ascii_alphabetic() {
            let start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                self.pos += 1;
            }
            self.chars[start..self.pos].iter().collect()
        } else {
            self.pos += 1;
            first.to_string()
        };

        if let Some((_, typst)) = SYMBOLS.iter().find(|(latex, _)| *latex == name) {
            return (*typst).to_string();
        }
        if GREEK.contains(&name.as_str()) {
            return name;
        }
        if SIZING.contains(&name.as_str()) {
            self.skip_spaces();
            // `\left.` の `.` は「区切り記号なし」の意味
            if self.peek() == Some('.') {
                self.pos += 1;
            }
            return String::new();
        }
        if name == "sqrt" {
            self.skip_spaces();
            if self.peek() == Some('[') {
                self.pos += 1;
                self.call_depth += 1;
                let index = self.sequence(Some(']'));
                self.call_depth -= 1;
                let radicand = self.call_argument();
                let index = if index.trim().is_empty() { "\"\"".to_string() } else { index };
                return format!("root({index}, {radicand})");
            }
        }
        if let Some((_, typst)) = UNARY.iter().find(|(latex, _)| *latex == name) {
            let arg = self.call_argument();
            return format!("{typst}({arg})");
        }
        if let Some((_, typst)) = BINARY.iter().find(|(latex, _)| *latex == name) {
            let a = self.call_argument();
            let b = self.call_argument();
            return format!("{typst}({a}, {b})");
        }
        if matches!(name.as_str(), "text" | "textrm" | "mbox" | "mathrm" | "operatorname") {
            let raw = self.raw_group();
            return if name == "operatorname" {
                format!("op({})", quote(&raw))
            } else {
                quote(&raw)
            };
        }
        quote(&format!("\\{name}"))
    }

    /// `\text{...}` の中身を変換せずにそのまま取り出す
    fn raw_group(&mut self) -> String {
        self.skip_spaces();
        if self.peek() != Some('{') {
            return String::new();
        }
        self.pos += 1;
        let mut depth = 1;
        let mut raw = String::new();
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            raw.push(c);
        }
        raw
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 対応表に載せている全コマンドの代表的な書き方（Typstで実際にコンパイルできるかの総当たり確認用）
#[cfg(test)]
pub(crate) fn all_supported_samples() -> Vec<String> {
    let mut samples = Vec::new();
    for (latex, _) in SYMBOLS {
        samples.push(format!("x \\{latex} y"));
    }
    for name in GREEK {
        samples.push(format!("\\{name}"));
    }
    for (latex, _) in UNARY {
        samples.push(format!("\\{latex}{{x}}"));
    }
    for (latex, _) in BINARY {
        samples.push(format!("\\{latex}{{a}}{{b}}"));
    }
    for name in SIZING {
        samples.push(format!("\\{name}( x \\{name})"));
    }
    samples.extend(
        [
            r"\sqrt[3]{8}",
            r"\sqrt[a,b;c]{x,y} \frac{1,2;3}{4} \max(a, b) \{1, 2\}",
            r"\text{円} \mbox{cm} \operatorname{f}(x)",
            r"\foo \frac{}{} x^ ",
            r#"a/b, c; "q" # @ ` ~ é あ × ÷"#,
            r"\left. x \right|",
            r"}{",
        ]
        .map(String::from),
    );
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arithmetic_print_style() {
        assert_eq!(latex_to_typst(r"4 \times 6 + 18 ="), "4 times 6 + 18 =");
        assert_eq!(latex_to_typst(r"12 \div 3"), "12 div 3");
    }

    #[test]
    fn test_fraction_sqrt_and_scripts() {
        assert_eq!(latex_to_typst(r"\frac{1}{2}"), "frac(1, 2)");
        assert_eq!(latex_to_typst(r"\sqrt{2}"), "sqrt(2)");
        assert_eq!(latex_to_typst(r"\sqrt[3]{8}"), "root(3, 8)");
        assert_eq!(latex_to_typst("x^2 + y_1"), "x^(2) + y_(1)");
        assert_eq!(latex_to_typst("x^{2n}"), "x^(2 n)");
        assert_eq!(latex_to_typst("a^23"), "a^(2) 3");
    }

    #[test]
    fn test_letters_are_separated_so_they_are_not_read_as_identifiers() {
        assert_eq!(latex_to_typst("abc"), "a b c");
    }

    #[test]
    fn test_slash_and_comma_are_literal() {
        assert_eq!(latex_to_typst("a/b"), "a \\/ b");
        assert_eq!(latex_to_typst(r"\frac{1,5}{2}"), "frac(1 \",\" 5, 2)");
        // 関数引数の外では普通のカンマとして出す（文字列にすると前後に余計な空白が入る）
        assert_eq!(latex_to_typst(r"\alpha, \beta"), "alpha , beta");
    }

    #[test]
    fn test_unknown_commands_become_strings() {
        assert_eq!(latex_to_typst(r"\foo"), r#""\\foo""#);
    }

    #[test]
    fn test_text_command_keeps_contents_verbatim() {
        assert_eq!(latex_to_typst(r"\text{円}"), "\"円\"");
        assert_eq!(latex_to_typst(r"x \text{cm}"), "x \"cm\"");
    }

    #[test]
    fn test_left_right_keep_delimiters_only() {
        assert_eq!(latex_to_typst(r"\left( x \right)"), "( x )");
    }
}
