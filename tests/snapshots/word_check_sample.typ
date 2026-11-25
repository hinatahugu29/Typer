#set document(title: "Word出力 確認用原稿")
#set page(
  paper: "a4",
  flipped: false,
  margin: 20mm,
  numbering: "1 / 1",
  header: align(right)[#text(size: 8pt, fill: rgb("#5f6368"))[株式会社サンプル 確認用]],
  footer: context [#grid(
    columns: (1fr, 1fr),
    align(left)[#text(size: 8pt, fill: rgb("#5f6368"))[社外秘]],
    align(right)[#text(size: 8pt, fill: rgb("#5f6368"))[#counter(page).display() / #counter(page).final().at(0)]],
  )],
)
#set text(
  font: ((name: "Times New Roman", covers: "latin-in-cjk"), "BIZ UDPMincho", "BIZ UDMincho", "Yu Mincho", "MS Mincho", "Yu Gothic"),
  size: 10.5pt,
  lang: "ja",
  region: "JP",
)
#set par(
  leading: 0.85em,
  spacing: 1.15em,
  justify: true,
)
#show heading: set block(above: 1.4em, below: 0.75em, sticky: true)
#show outline: it => {
  state("typer-in-outline", false).update(true)
  it
  state("typer-in-outline", false).update(false)
}
#show heading.where(level: 1): it => block(
  width: 100%,
  above: 1.5em,
  below: 0.9em,
  stroke: (bottom: 1.2pt + rgb("#1a73e8")),
  inset: (bottom: 5pt),
  text(size: 1.45em, weight: "bold", fill: rgb("#1a73e8"), it.body)
)
#show heading.where(level: 2): it => block(
  width: 100%,
  above: 1.35em,
  below: 0.7em,
  stroke: (bottom: 0.7pt + rgb("#dadce0"), left: 3.5pt + rgb("#1a73e8")),
  inset: (left: 7pt, bottom: 4pt, top: 2pt),
  text(size: 1.2em, weight: "bold", fill: rgb("#202124"), it.body)
)
#show heading.where(level: 3): it => block(
  above: 1.15em,
  below: 0.55em,
  text(size: 1.08em, weight: "bold", fill: rgb("#3c4043"), it.body)
)
#set table(
  stroke: 0.5pt + rgb("#9aa0a6"),
  inset: (x: 7pt, y: 6pt),
  fill: (_, y) => if y == 0 { rgb("#f1f3f4") } else { none },
)

= Word出力 確認用原稿

この原稿は、Word（.docx）出力をMicrosoft Wordで開いて確認するためのものです。確認項目は同じフォルダの #link("README.md")[README.md] を参照してください。英数字 ABC abc 123 は欧文フォント（Times New Roman）、和文は明朝体になります。

#outline(depth: 3)

== 1. 文字の装飾

#strong[太字]、#emph[斜体]、#strike[取り消し線]、#underline[下線]、#text(fill: rgb("#c5221f"))[赤文字]、#text(size: 14pt)[大きな文字]、#highlight(fill: rgb("#fff2cc"))[背景色付き]、#highlight(fill: rgb("#e8f0fe"))[#text(fill: rgb("#1a73e8"), size: 12pt)[#underline[全部まとめて]]]、#box(fill: rgb("#f1f3f4"), inset: (x: 3pt, y: 1pt), outset: (y: 2pt), radius: 2pt, raw("インラインコード"))。

リンク：#link("https://typst.app/")[Typstの公式サイト] と #link("mailto:info@example.com")[メール]。文書内の見出しへのリンク：#link("#3-表")[表の章へ]（Wordでは見た目のみ）。

#align(right)[2026年11月25日 \ 
情報システム部]

#align(center)[#text(size: 16pt)[#strong[中央寄せのタイトル]]]

#pad(left: 20mm, right: 0pt)[この段落は左に20mm字下げされます。]

== 2. リスト

#list(
  [箇条書き1

#list(
  [入れ子の項目

#list(
  [さらに入れ子]
)]
)], 
  [箇条書き2]
)

#enum(start: 3, 
  [3から始まる番号], 
  [次の番号

#enum(start: 1, 
  [入れ子は1から振り直し], 
  [二番目]
)]
)

#list(marker: [], body-indent: 0pt, 
  [#box(width: 0.85em, height: 0.85em, stroke: 0.6pt + rgb("#5f6368"), radius: 1.5pt, baseline: 0.12em) 未完了のタスク], 
  [#box(width: 0.85em, height: 0.85em, stroke: 0.6pt + rgb("#5f6368"), radius: 1.5pt, baseline: 0.12em)[#place(line(start: (0.17em, 0.45em), end: (0.36em, 0.64em), stroke: 0.9pt))#place(line(start: (0.36em, 0.64em), end: (0.7em, 0.2em), stroke: 0.9pt))] 完了したタスク]
)

== 3. 表

#table(
  columns: (25fr, 45fr, 30fr),
  align: (left + horizon, left + horizon, center + horizon),
  fill: (_, y) => if y == 0 { rgb("#fdf0e6") } else { none },
  table.header([*項目*], [*内容*], [*備考*], ),
  [発生日時], [2026年10月14日 10:15], [#text(fill: rgb("#c5221f"))[監視]にて検知], 
  [対応], [セル内の \ 
改行], table.cell(fill: rgb("#fff2cc"))[セル全体を塗る], 
  [数式], [$frac(1, 2) + frac(1, 3) = frac(5, 6)$], [右の列は中央寄せ], 
)

セルの結合（#box(fill: rgb("#f1f3f4"), inset: (x: 3pt, y: 1pt), outset: (y: 2pt), radius: 2pt, raw("<")) は左と、#box(fill: rgb("#f1f3f4"), inset: (x: 3pt, y: 1pt), outset: (y: 2pt), radius: 2pt, raw("^")) は上と結合）：

#table(
  columns: (20fr, 40fr, 30mm),
  align: (left + horizon, center + horizon, center + horizon),
  table.header([*区分*], table.cell(colspan: 2)[*承認*], ),
  table.cell(rowspan: 2)[担当], [山田], table.cell(rowspan: 2)[印], 
  [佐藤], 
  [備考], table.cell(colspan: 2)[特になし], 
)

== 4. 引用・コード・脚注

#block(width: 100%, fill: rgb("#f8f9fa"), stroke: (left: 3pt + rgb("#80868b")), inset: (x: 10pt, y: 8pt))[通常の引用です。

#block(width: 100%, fill: rgb("#f8f9fa"), stroke: (left: 3pt + rgb("#80868b")), inset: (x: 10pt, y: 8pt))[入れ子の引用です。]]

#block(width: 100%, fill: white, stroke: 0.8pt + rgb("#9aa0a6"), radius: 4pt, inset: (x: 12pt, y: 10pt))[囲み枠の引用です。]

#block(width: 100%, fill: rgb("#f1f3f4"), inset: 8pt, radius: 3pt)[
```
コードブロック
  インデントを保つ
```]

売上は前年比120%でした#context if not state("typer-in-outline", false).get() [#footnote[速報値です。詳細は#link("https://example.com/report")[こちら]。]#context state("typer-fn-0", 0).update(counter(footnote).get().first())]。同じ注をもう一度参照します#context if not state("typer-in-outline", false).get() { super(str(state("typer-fn-0", 0).get())) }。

== 5. 数式と画像

文中の数式 $x^(2) + y^(2) = r^(2)$ は本文と高さが揃います。

$ x = frac(- b plus.minus sqrt(b^(2) - 4 a c), 2 a) $

#figure(image("/__local_img__/{<ROOT>/docs/word-check/../../assets/icon.png}.png", width: 30%, fit: "contain"), caption: [アプリのアイコン])

#pagebreak(weak: true)

== 6. 改ページ後のページ

この見出しは新しいページから始まります。株式会社サンプル が差し込まれていること。

