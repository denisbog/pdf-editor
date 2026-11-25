# PDF Editor

A native editor for [Typst](https://github.com/typst/typst) whose page view *is* the typeset
document: the source is on the left, the pages Typst lays out are on the right, and every
keystroke is typeset again. A click on a word in the page puts the caret on the line that set
it, and **Export PDF** writes exactly the pages on screen.

```
┌ tab strip ── ⌂ · document tabs · + New ──────────────────── theme · help · info ┐
├ mode bar ── Menu │ Source · Split · Preview ── compile status · palette · zoom  │
│ ┌──┐┌─ source ─────────────────┐┌── page view (typst) ──┐┌─ panel ────────────┐ │
│ │  ││ 1  #set page(...)        ││                        ││ Problems / Document│ │
│ │▐▌││ 2  #set text(...)        ││   ┌──────────────┐     ││      / Log         │ │
│ │  ││ 3                        ││   │  page 1      │     ││                    │ │
│ │  ││ 4  = Heading             ││   └──────────────┘     ││                    │ │
│ └──┘└──────────────────────────┘└────────────────────────┘└────────────────────┘ │
└───────────────────── pasteboard (gray), pages with a shadow ────────────────────┘
```

The chrome — tab strip, mode bar, tool rails, panels, pasteboard, colour palette, Inter
typeface and Lucide icons — is [`../pdf-translate`](../pdf-translate)'s, which is PdfCraft's
(`crates/ui-egui`), so the three apps read as one family. What changes is the engine: pages
come from the Typst compiler instead of from a PDF file.

## Before building this: does such an app already exist?

Checked while starting this project (November 2025). **Typst editors with a live page preview
exist and are good** — the official web app (typst.app), the desktop editors *Typstify*,
*DBV Typst Editor*, *LumiType*, *Edist* (macOS), *typster*, *Typsastra*, the Tauri-based
*Typst Editor*, and the VS Code extension with *Tinymist*. **A PDF editor built on Typst does
not exist**, because Typst cannot read a PDF: it is a typesetting engine, not a PDF editor.
Any "edit the PDF with Typst" tool therefore has to *reconstruct* Typst source from a PDF,
which is a different (research) problem.

So this app is the first kind: a Typst editor whose page view is the real typeset output,
built on top of the `pdf-translate` shell. If what you want is instead "open a PDF, get an
editable Typst document", that is a converter, and it belongs in front of this editor — the
page view, click-to-source and export here are what it would target.

The only Rust integration that renders Typst into egui before this one is
[`typst-egui`](https://github.com/mattfbacon/typst-egui), which describes itself as "a very
restricted proof-of-concept". This app does not use it: it drives `typst` directly, so pages
are compiled once and rasterised at whatever zoom the view needs.

## What it does

* **The page view is the compile.** One compile produces the pages you read *and* the PDF you
  export. There is no separate preview renderer, no screenshot of an older run.
* **Live typesetting.** Edits are compiled after a 220 ms pause (Ctrl+R compiles at once), on
  a background thread — typing never waits for layout. When the source does not compile, the
  last good pages stay on screen and a banner says why.
* **Click a page, land in the source.** Every text run in the layout carries the span it was
  set from, so a click maps page point → glyph → source line. The caret goes there, the line
  is marked in the gutter, and the view switches out of *Preview* so you can see it. Hovering
  a word marks the run a click would jump to.
* **Vim motions in the source pane.** Normal, Insert and Visual modes with counts, operators,
  text objects, `u`/`Ctrl+R` and a command line — see below.
* **Syntax colours from Typst's own parser.** The editor does not pattern-match markup: it
  parses the buffer with `typst::syntax::parse` and colours the leaves (keywords, strings,
  numbers, math, labels, markers), so the highlighting cannot drift from the language.
* **Problems with places.** Compile errors and warnings are listed with file:line:column and
  hints; a click puts the caret on the line. Panels for the document's page sizes and stats,
  and a compile/export log.
* **Tabs, sessions and PDF export.** Each document opens in its own tab (`Ctrl+Tab` switches,
  the `x` closes, unsaved changes are confirmed before closing); the tabs and the page each
  was on are restored at the next start. **Export PDF** (`Ctrl+E`) writes the compiled
  document; **Save** (`Ctrl+S`) writes the source.
* **The page view behaves like a reader.** Zoom with `Ctrl` + wheel around the pointer, the
  standard zoom steps, *Actual size*, *Fit to width*, panning, a two-page spread when the band
  is wide enough, the page box and page buttons in the rail, `Page Down`/`Up`, `Home`/`End`.
* **A document to try it on.** [`doc/demo.typ`](doc/demo.typ) exercises headings, an abstract
  block, math, a table with a caption, a cross-reference, a footnote and a code block.

![PDF Editor: source on the left, the typeset pages on the right](doc/screenshot.png)

## Vim motions

The source pane is modal, like Vim. It starts in **Normal** mode: press `i`, `a`, `o` … to type
and `Esc` to come back. The mode and any half-typed command are shown next to the file name
(`NORMAL`, `INSERT`, `VISUAL`, `VISUAL LINE`, `COMMAND`, plus things like `2d` or `ciw`).

| | |
| --- | --- |
| Modes | `i I a A o O` insert; `v` character-wise and `V` line-wise Visual; `:` command line; `Esc` (or `Ctrl+[`) back to Normal |
| Motions | `h j k l`, `w W b B e E`, `0 ^ $`, `gg`/`G`/`{count}G`, `{`/`}` paragraphs, `+`/`-`/`_`, the column motion, `f t F T` with `;` `,` |
| Counts | before a motion, an operator or an insert command (`3j`, `2dd`, `5x`, `2p`, `3G`) |
| Operators | `d c y > <`, doubled for whole lines (`dd cc yy >> <<`), with motions (`dw`, `d$`, `dfx`, `d}`) and text objects (`iw aw iW aW`, `i" a"`, `i' a'`, `` i` a` ``, `i( a(`, `i[ a[`, `i{ a{`) |
| Shortcuts | `x X s S D C Y r ~ J`; `p`/`P` with the unnamed register |
| Visual | `d c y x s ~ > < J p` on the selection, `o` to swap ends, `v`/`V` to toggle |
| Search | `/pattern` then `n` / `N` (wraps around) |
| Command line | `:w` write, `:q` / `:q!` close the tab, `:e <file>` open, `:comp` compile, `:<n>` go to line |
| Undo | `u` and `Ctrl+R`; one insert session (or one change + its typing) is one step |

Deliberately not implemented, and warned about on the status line when pressed: marks
(`m`, `'`), macros (`q`), block Visual (`Ctrl+V`), `.` repeat, named registers, `zz`/`zt`/`zb`
and folding. `Ctrl+D`/`Ctrl+U`/`Ctrl+F`/`Ctrl+B` move the cursor by lines (the view follows)
rather than scrolling under a still cursor.

The bindings can be switched off from **Menu ▸ Vim bindings** or the command palette
(*Vim bindings: on/off*); the choice is remembered. With them off the pane is an ordinary text
editor and the badge disappears.

## Requirements

* Rust with a toolchain at least `1.92` (the `typst` crate's `rust-version`). This machine runs
  a nightly `1.102`.
* Fonts: Typst's embedded faces (Libertinus Serif, New Computer Modern, DejaVu Sans Mono) come
  with the build; everything installed on the machine is scanned at startup as well, so
  `#set text(font: "…")` can name any installed family. The first scan takes a moment — the
  mode bar says *loading fonts…* until it is done.
* No network at runtime. `#import "@preview/…"` resolves only if the package is already in
  Typst's package cache; there is no downloader here (see *Notes and limitations*).

## Build & run

```bash
cd ~/llm/pdf-editor
cargo run --release -- doc/demo.typ
```

A `.typ` file named on the command line opens in a tab; without an argument the documents of
the last session are reopened (new, never-saved documents are not restored). **Open…**
(`Ctrl+O`) picks another; **New** (`Ctrl+N`) starts a document from a small template.

### Debug helpers (no UI)

```bash
# compile a file and write the PDF (defaults to the file's own name)
pdf-editor --compile doc/demo.typ /tmp/demo.pdf

# compile and list errors and warnings with line/column
pdf-editor --check doc/demo.typ

# list pages, their size, and the click-to-source runs indexed for each
pdf-editor --pages doc/demo.typ
```

### Test hooks

Native file dialogs cannot be driven headlessly, so a save or export can be pointed at a
folder instead:

```bash
PDF_EDITOR_SAVE_DIR=/tmp/out PDF_EDITOR_EXPORT_DIR=/tmp/out pdf-editor doc/demo.typ
```

Then `Ctrl+S` writes `/tmp/out/demo.typ` and `Ctrl+E` writes `/tmp/out/demo.pdf` without
opening a dialog.

## Chrome and shortcuts

The tab strip holds one tab per document and the **New** action; the mode bar has the
**Menu**, the view tabs (*Source · Split · Preview*), the compile status (click it for
*Problems*), the command palette and the zoom control. The left rail has save, export,
compile, pan and the zoom tools, with the page box, page count and page buttons at the
bottom. The right rail toggles the panels and holds the theme and help buttons.

| Keys | |
| --- | --- |
| `Ctrl+N` / `Ctrl+O` | new document / open |
| `Ctrl+S` / `Ctrl+Shift+S` | save / save as |
| `Ctrl+E` | export PDF |
| `Ctrl+R`, `Ctrl+Enter` | compile now |
| `Ctrl+W` | close the tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | next / previous tab |
| `Ctrl+K` | command palette |
| `Ctrl++` / `Ctrl+−` / `Ctrl+0` / `Ctrl+2` | zoom in / out / 100% / fit to width |
| `Ctrl` + wheel | zoom around the pointer |
| click a page | go to the line that set the text |
| `Page Down` / `Page Up`, `Home` / `End` | next / previous, first / last page (when no text field has the keyboard) |

The page-keys give way to the source editor while the pointer is over it, so `Page Down`
scrolls text there and turns pages everywhere else.

## How it is built

| file | role |
| --- | --- |
| `src/main.rs` | entry point and the debug CLI |
| `src/app.rs` | the window: chrome, document tabs, page stack, panels, click-to-source, session, export |
| `src/editor.rs` | the source editor: buffer, Typst syntax colours, gutter, current line, go-to-line, the command line |
| `src/vim.rs` | the modal layer: modes, motions, operators, text objects, registers, undo, search |
| `src/engine/world.rs` | the `typst::World`: the buffer as main source, includes from the document's folder, fonts, date |
| `src/engine/compile.rs` | the compile thread (compile, diagnostics, text index for click-to-source, PDF export) |
| `src/engine/mod.rs` | the raster pool that turns compiled pages into textures at the requested zoom |
| `src/theme.rs`, `src/icons.rs`, `src/widgets.rs` | PdfCraft's design tokens, Lucide icons and chrome widgets |

**Compiling.** `typst::compile::<PagedDocument>` runs on a dedicated thread that owns the
`PagedDocument` and the last word: when a newer buffer is queued while a compile runs, the
buffers in between are dropped. The result is an `Arc<PagedDocument>` plus, per page, its size
in points and a list of text runs with the source line each came from. Errors keep the old
pages; warnings are shown but do not stop the render.

**Click-to-source.** The compile thread walks each page's frame items (groups included, with
their transform applied), and for every glyph asks the world for the byte range of its span.
Glyphs from the same line that sit on the same row are merged into one rectangle; the index is
`(rect in points, line)`. A click divides by the zoom, asks for the nearest rect within 6 pt,
and moves the caret. Text from an included file has no line in the buffer, so clicking it does
nothing (the page still shows it).

**Rasterising.** A pool of threads takes `(page, scale, tag)` requests from the UI each frame,
in order of urgency (the page in view first), renders the page with `typst-render` at the
requested scale (capped at 3× device pixels per point and 24 Mpx per page), and uploads the
result as an egui texture. A request whose page left the screen is replaced, not queued.

**Export.** The same `PagedDocument` is handed to the compile thread, which writes it with
`typst-pdf` — so the PDF cannot differ from what was on screen.

## Notes and limitations

* **A Typst engine, not a PDF importer.** Opening a PDF and editing it in place is not
  something Typst can do; this app edits `.typ` files. A PDF → Typst converter in front of it
  is a separate project.
* **No package downloader.** `@preview/…` imports resolve from Typst's local package cache
  (`~/.cache/typst/packages`) and fail otherwise. Everything else — `#include`, `#import`
  of local files, `#read`, images — resolves relative to the document's folder, as
  `typst compile` does.
* **One file per tab is the main source.** An included file can be read and typeset, but
  editing it means opening it in its own tab (and its edits are not noticed by the including
  document until it is compiled again).
* **No soft wrap in the source.** Long lines scroll horizontally, so a line number always
  means one screen row. `Tab` indents inside the source instead of moving focus.
* **Raster scale is capped.** At very high zoom the page is stretched from a 3× raster
  instead of being re-rendered per tile, which is cheaper and blurrier than PdfCraft's tiling.
  Pages are also rendered whole, so a poster-sized page at high zoom costs a large texture.
* **Vim is a subset.** See the table above: no marks, macros, block Visual, `.` repeat or named
  registers. Undo snapshots copy the whole buffer, so a very large document pays for `u`; the
  history keeps the last 400 changes.
* **Text index is per line, not per word.** A click anywhere in a run of glyphs set from one
  source line lands at the start of that line; the caret is not placed at the character.
* **The session lives in eframe's storage** (`~/.local/share/pdfeditor/app.ron`), together
  with the zoom and the theme, and is written every ten seconds and at exit.
* **New documents live in the working directory.** `Ctrl+N` names the buffer
  `untitled.typ` (or `untitled-2.typ`, …) next to the process; until it is saved it is not
  restored in the next session.
