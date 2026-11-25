# Task: move the window layer from egui to iced

Status: **implemented** (November 2026). The plan below is kept as written, with the result
recorded in ["What landed"](#what-landed) and in the amended phases and acceptance runs. The
egui binary still exists and still works: the port was additive, as planned.

Two binaries now share one core:

```
pdf-editor/            (workspace)
├── crates/core/       engine + vim + document model (no UI types)
├── crates/egui-app/   pdf-editor       — the original shell
└── crates/iced-app/   pdf-editor-iced  — the new shell
```

## What landed

| Plan | What was built |
| --- | --- |
| Step 0: `crates/core` with `engine/` and `vim.rs`, plus a `Source` trait | Done. `crates/core` holds `engine/` (compile thread, raster pool, glyph index), `vim.rs`, `source.rs` (`Source`, `Edit`, `Buffer`) and `document.rs` (settings/session, theme kinds, diagnostic marks, paths). |
| `egui::Context` in the engine → wake callback | Done: `engine::Waker` (an `Arc<dyn Fn()>`); egui passes `request_repaint`, iced passes nothing and polls, the CLI passes `Waker::none()`. |
| vim speaks the same code in both front-ends | Done. `vim.rs` lost its `egui::Key`/`Ui` coupling: it has a toolkit-neutral `Key` (with `key_for_char` as the inverse of `char_of` for front-ends that only see text) and public `press`/`type_text`. Both shells feed it; the egui event-queue filter is now `crates/egui-app/src/vim_input.rs`. |
| vim mutates a `String`; the iced editor takes `Action`s | Done differently, and cheaper: the iced adapter keeps the editor's text as a mirror and turns each command's mutation into the single contiguous difference (`Action::Replace`) plus `MoveTo`/`SelectTo`. Every vim command is a contiguous edit, so one `Replace` is enough; `crates/iced-app/src/vim.rs` tests the diff and the `dd`/Visual cases. |
| Page view: raster handles, spread, zoom anchor, fit-to-width | Done with plain widgets: `scrollable` + rows of `image::Handle` pages with container shadows; `pages.rs` carries the egui geometry (spread, gaps, margins) as `f32`s. Scroll tracking comes from `Scrollable::on_scroll` and jumps from `operation::scroll_to`. |
| Click-to-source becomes exact | Done: the compile indexes every glyph's source byte (`Glyph`), so `engine::position_at` returns `(line, column)`; a click on "Notes" lands on `11:31` rather than the start of line 11. |
| Diagnostics through `Content::set_diagnostics` | Done: `document::diagnostic_ranges` hands byte ranges (and messages) to the editor; the squiggles are the widget's, the line wash and gutter colours are ours. |
| Chrome on iced widgets | Done: one `tab strip` and one `mode bar` built from `row`/`button`/`container`, both rails, `Problems`/`Document`/`Log` panels, palette/toasts/shortcuts/unsaved-changes overlays via `stack`, `svg` icons from the same Lucide set. |
| Resizable splits | Done by hand rather than with `pane_grid`: a 6 px grip (the plan's "grip gap") starts a drag, and global cursor events apply the delta to the split ratio or the panel width — the same pixels the egui build drags. |
| Settings/session | One JSON file for both shells (`~/.config/pdf-editor/settings.json`, `PDF_EDITOR_SETTINGS` overrides): zoom, theme, view, split and side width, vim on/off, session tabs. eframe's storage is no longer used. |
| Undo granularity (the known gap) | Not needed: the shared vim machine keeps its own snapshots, so one insert session is one `u`, in both shells. The editor's own history still records the `Replace`s for Insert-mode `Ctrl+Z`. |
| Redraw model | Waker is idle; the iced heartbeat (`iced::time::every`) only exists while work is in flight or a toast is fading, so an idle window asks for no frames. |

A later pass rebuilt the window on the `rust-iced-desktop-ui` design system: one token module
(colours, chrome metrics, the Inter face registered as "Inter Variable"), one `widgets.rs`
vocabulary for every button/tab/row/panel/overlay, a 40 px panel shell with a close control,
scrim-backed palette/shortcuts/dialog layers that close on click-away, `Esc` priority and
shortcut gating while an overlay is open, tooltips on icon-only controls, `on_press_maybe`
disabled states, indicator dots on closed panels that have content, and one 7 px scrollbar
everywhere. The source/pages split is driven by one `split_widths()` (source `Fixed`, pages
`Fill`, clamped minimum) so the grip drags at the ratio the page geometry uses. The skill grew
two pitfalls (a divider that cannot move because both panes are `Fill`; a fixed SVG squeezed
inside `center_y` next to a fixed-width scrollable) and a `drag.py` helper for press→move→release
tests. `scripts/ui-lint.sh` from that skill now reports no structural hits for
`crates/iced-app/src`.

The raster queue is declarative (every visible, unsatisfied page, in-flight ones included with
their tag), so replacing the pool's queue cannot drop a request that is still being rendered —
that bug made the page view keep its old pixels after an edit. The same fix went into the egui
build, which had the same "only new requests" pattern.

Not carried over: accessibility (iced has no accesskit), the debug CLI (it stays in `pdf-editor`),
and the egui-specific IME and caret workarounds (the iced editor owns pre-edit and the caret; it
was checked by typing, see the acceptance run). The source pane takes the keyboard on the first
click rather than on launch: `CodeEditor` exposes no widget id, and iced's `operation::focus`
needs one, so there is no autofocus hook to call.

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

| Phase | Work | Size | Status |
| --- | --- | --- | --- |
| 0 | Extract `crates/core` (engine + `vim.rs` + `Source` trait); egui app keeps working on it | 1–2 days | done |
| 1 | Spike `crates/iced-app`: `typst-iced-editor` pane, compile thread, one page image, click-to-source. No chrome. **This is the go/no-go gate.** | 1–2 days | done (gate passed) |
| 2 | Page view: pasteboard, spread, zoom/scroll anchor, fit-to-width, raster handles | 2–3 days | done |
| 3 | Chrome: tab strip, mode bar, rails, split with grip, panels, palette, toasts, dialogs | 3–5 days | done |
| 4 | Settings/session persistence, export, open/new/save, shortcuts | 1 day | done |
| 5 | Vim adapter on `key_binding`/`Action`; parity checklist below | 2–3 days | done |
| 6 | Polishing: theme parity, icons, accessibility notes, screenshots, README | 1 day | done, minus accessibility (not available on iced) |

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

**Result (run in the headless KWin session):**

| # | Observed |
| --- | --- |
| 1 | Typing in Insert mode keeps the caret with the text; the source is coloured on every frame (the editor re-shapes from its own buffer, so there is no stale-layout frame). |
| 2 | Long comment lines and the `#set …` lines soft-wrap; the gutter numbers one logical line on its first row; the current-line band covers the wrapped rows. |
| 3 | Clicking "Notes on a Page Editor" on the page moved the caret to `11:31` — the line *and* the column of the word under the pointer. Clicking `= Why a page editor` moved it to `28:3`. |
| 4 | An accidental edit inside `leading:` produced a squiggle under the broken span and an error in the mode bar after the 0.6 s debounce; `Content::set_diagnostics` was fed from the compile's byte ranges. |
| 5 | An Arabic/Hebrew test document renders in visual, shaped order in the iced pane; the same file in the egui pane draws boxes in logical order (`doc/bidi-*.png`). |
| 6 | `top` over 9 s of idleness: 0.3–1.0% CPU, no frames requested (the heartbeat subscription is absent when nothing is in flight). |

Jank observed on the way, all in the shell rather than the engine: the editor's `line_height`
takes an iced `Pixels` value (`Pixels(17.5)`), not a bare `f32`, which is a *relative* factor —
passing 17.5 turned one row into 218 px and made the current-line wash cover the pane; and the
page click must not divide the mouse area's local point by the zoom twice (the widget's `on_move`
reports page-local pixels; the shell converts them once).

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
