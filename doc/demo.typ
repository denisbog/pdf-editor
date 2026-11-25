// A small document to try the editor on: everything on the page is set by Typst from this
// file. Click any word on the right and the caret lands on the line that set it.

#set page(paper: "a4", margin: (x: 2.2cm, y: 2.4cm))
#set text(font: "Libertinus Serif", size: 10.5pt)
#set par(justify: true, leading: 0.7em)
#set heading(numbering: "1.1")
#show link: underline

#align(center)[
  #text(19pt, weight: "bold")[Notes on a Page Editor]
  #v(4pt)
  #text(10pt, fill: luma(110))[Written, typeset and exported in one window]
]

#v(6pt)
#block(
  inset: (x: 12pt, y: 10pt),
  fill: luma(246),
  radius: 4pt,
  width: 100%,
)[
  *Abstract.* The page view is not a picture of the document; it *is* the document.
  Typst lays the file out into pages, and those pages are what you read, click and
  export. There is no separate compile step and no second renderer.
]

= Why a page editor

Writing tools usually split the work in two: a text buffer, and a preview that is
either a second implementation of the layout engine or a photograph of an older run
of it. Here the two are the same object — the pages come from the same compile that
the PDF export uses, and a click on a page maps back to the line that set the glyphs
under the pointer.

== Typography in the page view

Paragraphs are justified and hyphenated as Typst would set them for print:

#block(inset: (left: 14pt), stroke: (left: 2pt + luma(200)))[
  Body text, headings, tables and mathematics are all laid out by the same engine, so
  the page you edit against is the page you get.
]

The typographic detail is decided in the source:

- `#set par(justify: true)` for justified measures,
- `#set text(font: "Libertinus Serif")` for the face,
- `#set heading(numbering: "1.1")` for the numbering above.

= Mathematics and tables <maths>

The quadratic formula and a summary of the sizes the editor reports:

$ x = (-b plus.minus sqrt(b^2 - 4 a c)) / (2 a) $

#figure(
  table(
    columns: (auto, auto, auto),
    align: (left, right, right),
    stroke: none,
    table.hline(stroke: 0.8pt),
    table.header([*Unit*], [*Points*], [*Millimetres*]),
    table.hline(stroke: 0.4pt),
    [inch], [72], [25.4],
    [point], [1], [0.353],
    [pica], [12], [4.233],
    table.hline(stroke: 0.8pt),
  ),
  caption: [Common typographic units, as Typst knows them.],
)

= Cross references and footnotes

A labelled section can be referenced from anywhere: see @maths, which is numbered
automatically, and the note below#footnote[Footnotes are laid out by the same engine,
at the bottom of the page they belong to.] falls on this page.

#heading(numbering: none, outlined: false)[A short code sample]

```typ
#let emphasise(body) = text(weight: "semibold", fill: rgb("#1b63e0"), body)
```

#pagebreak()

= The second page

Pages are laid out as a stack, and a spread appears when the window is wide enough
for two of them. Zooming keeps the point under the pointer still; *Fit to width* and
the page box in the rail move the view without moving your place in the text.
