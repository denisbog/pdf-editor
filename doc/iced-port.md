# Task: move the window layer from egui to iced

Status: **planned, not started.** This is the work plan for a later session. The app stays on
egui/eframe until an iced front-end reaches parity; nothing here is a rewrite in place.

## Why consider it at all

The engine (Typst), the page view's data (rasters at a scale) and the vim state machine do not
care about the UI toolkit. What the toolkit decides is text editing and chrome:

| Question | egui 0.36 (today) | iced 0.14 |
| --- | --- | --- |
| Source text shaping | harfrust since 0.35, so kerning, ligatures, Arabic joining and Indic conjuncts shape; **no bidi** (UAX #9). An RTL source line mixed with Latin/code renders in logical order. Upstream PR #8577 is in review. | cosmic-text: bidi, complex scripts, emoji. |
| Modal editing | `TextEdit` consumes keys; a modal layer must take the event queue (that is what `src/vim.rs` does) and fight caret-range churn. | `CodeEditor::key_binding` receives every `KeyPress` and returns a `Binding`; `Binding::Custom(msg)` / `None` gate the editor. `Action`s are byte offsets. |
| Redraw model | immediate mode: the whole window is re-tessellated on every frame; frames are requested explicitly. Two flicker bugs came from that (see README notes). | retained widgets, `RedrawRequest::{NextFrame, At, Wait}`; still whole-window repaints, no damage regions. |
| Accessibility | accesskit, shipped (`eframe` feature `accesskit`). | no accesskit (upstream issue #552 still open in late 2026). |
| Chrome | `theme.rs` / `widgets.rs` / `icons.rs` are a port of PdfCraft's egui theme; panels, rails, palette, dialogs all work. | would be rewritten: `Theme`/`Style` + custom widgets. |

**Verdict (recorded 2026-11):** the gain that matters is bidi/complex-script source editing and a
first-class modal hook; the losses are the inherited chrome, accessibility, and ~2.5–3k lines of
working UI. Not worth switching today. Revisit when one of these becomes true:

1. People write Typst source in RTL or complex scripts and egui still has no bidi
   (check `emilk/egui` PR #8577 first — if it lands, this reason is gone).
2. Vim fidelity needs an editor whose key handling is a public API rather than an event queue, and
   the egui workarounds (see "Lessons") become a maintenance problem.
3. Someone else should own the code editor and `typst-iced-editor` has matured past 0.2.0.

If "large documents need partial repaint" is the reason: **neither** framework does damage
rectangles, so that is a custom-renderer question, not an iced question.

## Inventory: what moves, what is rewritten

Sizes as of `0615e72`: 5,929 lines of Rust.

| Today | Lines | Portability |
| --- | --- | --- |
| `src/engine/` (`world`, `compile`, `mod`: Typst world, compile/export thread, raster pool, click-to-source index) | 702 | **Moves unchanged.** No egui types except `egui::Context` for `request_repaint` (becomes `window::RedrawRequest`/a callback). Consider an `Arc<dyn Fn()>` "wake" instead. |
| `src/vim.rs` (modes, motions, operators, text objects, registers, undo, `/` search, `:` line) | 1,835 | **Logic moves; the mutation layer is replaced.** It speaks `(text: &str, cursor: byte)` and mutates a `String`. On iced those become reads from `Content` and `Action`s. |
| `src/editor.rs` (wrapping geometry, gutter, bands, marks, prompt bar, caret plumbing) | 723 | **Replaced by `typst-iced-editor`** (see below). The *rules* to re-implement/verify are listed under "Lessons". |
| `src/app.rs` (chrome, tabs, page view, panels, palette, dialogs, session) | 2,096 | **Rewritten** as an iced application (Elm `Message/update/view`). |
| `src/theme.rs`, `widgets.rs`, `icons.rs` (PdfCraft tokens, chrome widgets, Lucide icons) | 437 | **Rewritten** as iced styles and widgets; the *tokens* (colours, radii, spacings) carry over verbatim. |
| `src/main.rs` (`--compile`, `--check`, `--pages`) | 136 | **Moves**, minus the `eframe::run_native` arm. |

## Target crates (verified available)

- `iced 0.14.0`, `iced_widget 0.14.2` — renderers: `wgpu` (default) or `tiny-skia` for software.
- `typst-iced-editor 0.2.0` (MIT, `rust-version 1.92`, depends only on `iced_core`): a Typst code
  editor widget — incremental `typst_syntax` parsing, syntax highlighting, **soft wrap**, gutter,
  current line, selection/caret, mouse/keyboard, clipboard, undo/redo, folds, diagnostics
  squiggles, and a `key_binding` hook. Requires `iced` feature `svg` (otherwise a cryptic
  `E0277` about `iced_wgpu::Renderer`). Must resolve to the same `typst-* 0.15.x` versions as our
  engine.
- Everything else stays: `typst`, `typst-layout`, `typst-render`, `typst-pdf`, `typst-kit`, `rfd`,
  `serde`/`serde_json`, `time`, the Lucide/Inter/JetBrains crates.

## Proposed shape

Make the front-end additive instead of swapping in place:

```
pdf-editor/            (workspace)
├── crates/core/       engine + vim state machine + document model (no UI types)
├── crates/egui-app/   today's binary, on the shared core   (kept working)
└── crates/iced-app/   the new binary, on the same core     (built to parity)
```

**Step 0 (worth doing even if the port never happens):** extract `engine/` and `vim.rs` into
`crates/core`, with a `Source` abstraction the front-ends implement:

```rust
pub trait Source {
    fn text(&self) -> &str;
    fn cursor(&self) -> usize;          // byte offset
    fn selection(&self) -> Option<Range<usize>>;
    fn apply(&mut self, edit: Edit);    // Replace/Insert/MoveTo/SelectTo/Scroll/Undo…
}
```

The egui front-end implements it over `TextEdit` state + our galley plumbing; the iced front-end
implements it over `typst_iced_editor::Content`. `vim.rs` then compiles against the trait and is
shared byte-for-byte, which is the single largest piece of shared behaviour (1.8k lines).

## The vim adapter on `typst-iced-editor`

`key_binding(|press: KeyPress| -> Option<Binding<Message>>)` is a real modal gate: return
`Binding::Custom(Message::Vim(press))` for Normal/Visual/Command modes (the editor inserts
nothing), and fall back to `Binding::from_key_press(press)` in Insert mode, with an override for
Escape. Our engine already speaks byte offsets, and the widget's actions take byte offsets:

| Vim | Action |
| --- | --- |
| motions, `gg`, `G`, page click, diagnostics | `Action::MoveTo(byte)` |
| `v`/`V` selection growth | `Action::SelectTo(byte)`, `SelectLine(byte)`, `SelectWord(byte)` |
| `d`/`c`/`y` + motion, `dd`, `ciw`, `p` | `Action::Replace { range, text }`, `ApplyEdits(Vec<(Range, String)>)` |
| `x`, `X` | `Action::Delete`, `Backspace` |
| `J`, `>>`, `<<`, `gJ` | `JoinLines`, `Indent`, `Unindent`, `MoveLineUp/Down` |
| `u`, `Ctrl+R` | `Action::Undo`, `Redo` (the editor's history) |
| `Ctrl+D/U` | `Action::Scroll { y }` |

Also available on `Content`: `text()`, `buffer()` (with the live `typst_syntax` tree,
`line_text`, `byte_to_line`, `clamp`), `cursor()`, `selection()`, `perform(Action)`,
`set_diagnostics` (replaces our `marks_for` + squiggle painting), `set_search_matches` /
`reveal_search_match` (replaces our `/` search and lets the widget highlight matches),
`set_auto_pairs(false)` (wanted for vim typing), folds.

**Known gaps to solve in the port:**

- **Undo granularity**: the editor's history is one entry per `Replace`/`ApplyEdits`, so an insert
  session is several undos where Vim expects one. Options: accept it, or send a small PR upstream
  for explicit undo groups.
- **`Escape`** is bound to `Binding::Unfocus` by default; override it to leave Insert mode without
  losing focus.
- **Auto-pairs** must be off (and auto-indent decided) for Vim-like typing.
- **IME**: iced 0.14 has preedit support; verify pre-edit events do not cause a redraw loop the
  way the empty KWin pre-edits did under egui (see "Lessons" 3).

## Page view on iced

The viewer is the app's identity; it is also the part with no ready-made widget.

- A custom `Widget` (or a `canvas::Program`) painting the pasteboard, per-page shadow, borders and
  the page image. Rasters arrive as RGBA bytes from the existing raster pool and become an
  `image::Handle::from_rgba(w, h, bytes)`; keep one handle per `(page, scale)` and replace it when
  a new generation lands. A handle is immutable, so a refresh means a new handle (same allocation
  cost as today's `ctx.load_texture`).
- Zoom/scroll anchoring, the two-page spread, fit-to-width and the "typeset when typing pauses"
  debounce all move over as logic; only the geometry calls change.
- Click-to-source: hit-test the page rect → page-local points → the existing
  `engine::line_at` index → `Action::MoveTo(byte)` on the editor's `Content` (this becomes exact,
  and `move_to(line, column)` finally collects its own column).
- Diagnostics: feed the compile's `Diagnostic`s (line/column/range) into
  `Content::set_diagnostics`, which draws the squiggles; the gutter numbers and line wash are the
  widget's own.

## Chrome port

| Today (egui) | On iced |
| --- | --- |
| `Panel::top` tab strip / mode bar (custom painted) | `container` rows with custom `Button` styles, or one custom widget for the strip |
| `Panel::left/right` rails, custom split with a grip gap | `row!` + custom widgets; resizable split via `pane_grid` or a hand-rolled grip (keep the 6 px grip gap idea: it is what makes the drag reach the handle) |
| Problems / Document / Log panels | `container` + `scrollable`; hover rows are a custom widget (reserve the background shape before the text, as now) |
| Command palette (floating `Area`) | `float!` / `stack!` overlay, or an `overlay`-based custom widget |
| `rfd` dialogs | unchanged |
| eframe storage (`app.ron`) | own JSON store (`dirs`/`directories` or a fixed path), same `Settings` struct |
| Light/dark | iced `Theme` + our token struct mapped to `Style` |
| Lucide icons | `svg` feature + `iced::widget::svg` (already a dependency via `lucide-static-svg`) |
| Raster textures | `image::Handle::from_rgba` |

## Work breakdown

Estimates are for one focused developer; each phase ends with a runnable app.

| Phase | Work | Size |
| --- | --- | --- |
| 0 | Extract `crates/core` (engine + `vim.rs` + `Source` trait); egui app keeps working on it | 1–2 days |
| 1 | Spike `crates/iced-app`: `typst-iced-editor` pane, compile thread, one page image, click-to-source. No chrome. **This is the go/no-go gate.** | 1–2 days |
| 2 | Page view: pasteboard, spread, zoom/scroll anchor, fit-to-width, raster handles | 2–3 days |
| 3 | Chrome: tab strip, mode bar, rails, split with grip, panels, palette, toasts, dialogs | 3–5 days |
| 4 | Settings/session persistence, export, open/new/save, shortcuts | 1 day |
| 5 | Vim adapter on `key_binding`/`Action`; parity checklist below | 2–3 days |
| 6 | Polishing: theme parity, icons, accessibility notes, screenshots, README | 1 day |

Total ≈ 10–15 days to parity, versus "keep improving the egui app" at ~0 incremental risk.

## Spike acceptance (phase 1 gate)

Run in the headless KWin session (`desktop-app-linux` skill), with `doc/demo.typ`:

1. Typing 200 characters at speed keeps the caret with the text and never shows an un-coloured
   frame (the two egui flicker bugs are the regression to watch for).
2. A long line soft-wraps; the gutter number sits on the first visual row; the current-line band
   covers all rows.
3. Clicking a word on the rendered page puts the caret on that line **and** column.
4. Errors underline the offending span (squiggle + gutter colour) — `Content::set_diagnostics`.
5. An Arabic/Hebrew line in the source renders in visual order (this is the whole reason to
   consider iced; if it does not, the port has no justification).
6. Idle CPU under 5% with a document open; no continuous redraw.

## Lessons that must survive the port

Written down because they were expensive to find:

1. **Never hand the widget a layout for a different text.** The caret and the text must come from
   the same layout in the same frame, or the editor flashes plain text on every keystroke.
2. **Do not rewrite the caret range every frame.** egui normalises it (`prefer_next_row`), so a
   per-frame write looks like a selection change and the widget re-scrolls each frame.
3. **Filter empty IME pre-edits.** KWin sends `Preedit("")` every frame while a text field has
   focus; egui repaints on any event, so this alone pins the app at 60 fps and ~25% CPU.
4. **Gutter numbers per logical line, on its first wrapped row**; bands and squiggles per visual
   row; a span that wraps is two segments.
5. **Scroll from one place only**: the widget scrolls for typing; the app scrolls for jumps.
6. **The split needs a grip gap** between the two panes, or the scroll areas swallow the drag.
7. Diagnostics are byte ranges into the text that was compiled; drop the marks on the first
   keystroke and rebuild them with the next compile.

## What stays true either way

- The engine, the CLI helpers (`--compile`, `--check`, `--pages`) and the export path are
  UI-agnostic and must stay so.
- One compile feeds both the page view and the PDF (no second renderer).
- The README's "Notes and limitations" carries the environment quirks (fonts, IME, raster caps).
