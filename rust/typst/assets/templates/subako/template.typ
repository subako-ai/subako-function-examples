// The Subako document template, after the subako.ai site: its ink-on-paper
// palette, Playfair Display headings over Inter text, and the Subako logo in
// the header. Use it with `#show: subako.with(title: "...")`.

#let ink = rgb("#16221F")
#let strong-ink = rgb("#2A3835")
#let body-ink = rgb("#41504D")
#let muted = rgb("#606B69")
#let paper = rgb("#FFFFF6")
#let sunken = rgb("#E1E1D8")
#let line-soft = rgb("#CDCDCB")
#let line-strong = rgb("#818987")

// Noto Sans JP covers what the Latin-only faces lack, Japanese above all.
#let serif = ("Playfair Display", "Noto Sans JP")
#let sans = ("Inter", "Noto Sans JP")
#let mono = ("IBM Plex Mono", "Noto Sans JP")

#let subako(title: none, body) = {
  set document(title: title) if title != none

  set page(
    paper: "a4",
    fill: paper,
    margin: (x: 22mm, top: 30mm, bottom: 25mm),
    header: context {
      set text(font: sans, size: 8pt, fill: muted)
      grid(
        columns: (auto, 1fr),
        align: (left + horizon, right + horizon),
        image("logo.svg", height: 7mm),
        if document.title != none { document.title },
      )
      v(-1mm)
      line(length: 100%, stroke: 0.5pt + line-soft)
    },
    footer: context {
      set text(font: sans, size: 8pt, fill: muted)
      line(length: 100%, stroke: 0.5pt + line-soft)
      v(-1mm)
      grid(
        columns: (1fr, auto),
        [subako.ai],
        counter(page).display("1 / 1", both: true),
      )
    },
  )

  set text(font: sans, size: 10pt, fill: body-ink)
  set par(leading: 0.75em, spacing: 1.2em)

  show heading: set text(font: serif, weight: "regular", fill: ink)
  show heading: set block(above: 1.8em, below: 0.9em)
  show heading.where(level: 1): set text(size: 22pt)
  show heading.where(level: 2): set text(size: 16pt)
  show heading.where(level: 3): set text(size: 12.5pt)
  show heading.where(level: 4): set text(font: sans, size: 10pt, weight: "bold")

  show strong: set text(fill: strong-ink)
  show link: set text(fill: ink)
  show link: underline.with(stroke: 0.5pt + line-strong, offset: 2pt)

  show raw: set text(font: mono, size: 8.5pt)
  show raw.where(block: false): box.with(
    fill: sunken,
    inset: (x: 3pt),
    outset: (y: 3pt),
    radius: 2pt,
  )
  show raw.where(block: true): block.with(
    width: 100%,
    fill: sunken,
    inset: 10pt,
    radius: 3pt,
  )

  show quote.where(block: true): block.with(
    stroke: (left: 2pt + line-strong),
    inset: (left: 12pt, y: 4pt),
  )
  show quote: set text(fill: muted)

  set table(
    stroke: (_, y) => (
      top: if y == 0 { 0.75pt + line-strong },
      bottom: 0.5pt + line-soft,
    ),
    fill: (_, y) => if y == 0 { sunken },
    inset: (x: 8pt, y: 6pt),
  )
  show table.cell.where(y: 0): set text(weight: "bold", fill: ink)

  set list(marker: text(fill: muted)[•])
  set line(stroke: 0.5pt + line-soft)

  if title != none {
    block(below: 2em, text(font: serif, size: 28pt, fill: ink, title))
  }

  body
}
