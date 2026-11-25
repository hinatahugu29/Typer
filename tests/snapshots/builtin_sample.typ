#set page(
  paper: "a4",
  flipped: false,
  margin: 20mm,
  numbering: "1 / 1",
)
#set text(
  font: ("BIZ UDPGothic", "BIZ UDGothic", "Yu Gothic", "Meiryo", "MS Gothic"),
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

