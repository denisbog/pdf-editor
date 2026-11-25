//! The source editor: the Typst text, its syntax colours (from Typst's own parser), line
//! numbers, Vim motions, and the "go to line" a click on a rendered page triggers.

use std::ops::Range;
use std::sync::Arc;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::text_selection::CCursorRange;
use egui::{Align2, Color32, FontId, Rect, Stroke, TextEdit, Ui, pos2, vec2};
use typst::syntax::{SyntaxKind, SyntaxNode};

use crate::theme::Tokens;
use crate::vim::{self, Vim};

/// How long after the last keystroke the document is typeset again. The page view is meant to
/// settle once you stop typing, not to follow every word.
pub const DEBOUNCE: f64 = 0.6;
/// Height of the command line (`:w`, `/pattern`) drawn at the bottom of the pane.
const PROMPT_HEIGHT: f32 = 26.0;
/// Room above the first line and below the last, so the caret never sits on the edge.
const PADDING_TOP: f32 = 10.0;
const PADDING_BOTTOM: f32 = 24.0;

/// Where each logical line's rows are in the wrapped galley: its first row's top, the height of
/// all its rows together, and the height of the first row (for centring its number).
struct LineGeometry {
    entries: Vec<(usize, f32, f32, f32)>,
}

impl LineGeometry {
    fn new(galley: &egui::Galley) -> Self {
        let mut lines: Vec<(usize, f32, f32)> = Vec::new(); // (line, top, first row height)
        let mut line = 0usize;
        for placed in &galley.rows {
            if lines.last().map(|entry| entry.0) != Some(line) {
                lines.push((line, placed.pos.y, placed.row.size.y));
            }
            if placed.ends_with_newline {
                line += 1;
            }
        }
        let total = galley.size().y;
        let mut entries: Vec<(usize, f32, f32, f32)> = Vec::with_capacity(lines.len());
        for (index, (number, top, first_row)) in lines.iter().copied().enumerate() {
            let bottom = lines.get(index + 1).map_or(total, |next| next.1);
            entries.push((number, top, (bottom - top).max(first_row), first_row));
        }
        Self { entries }
    }

    fn entries(&self) -> impl Iterator<Item = (usize, f32, f32, f32)> + '_ {
        self.entries.iter().copied()
    }

    fn line(&self, line: usize) -> Option<(f32, f32, f32)> {
        self.entries.iter().find(|entry| entry.0 == line).map(|entry| (entry.1, entry.2, entry.3))
    }
}

/// What the editor's own layout looked like last frame, so a cursor move can tell whether it
/// is already in sight.
struct ViewGeom {
    /// The content's top edge, in the scroll view's coordinates.
    content_top: f32,
    clip: Rect,
}

/// What one frame of the editor produced.
pub struct EditorOutput {
    /// The buffer changed: the modal layer's edits do not go through the `TextEdit`.
    pub changed: bool,
    /// A command the command line asked the window for (`:w`, `:q`, `:e`).
    pub request: Option<vim::Request>,
}

/// A diagnostic's place in the buffer: a line, and the run of characters on it to underline.
#[derive(Clone, Debug)]
pub struct Mark {
    /// 0-based line.
    pub line: usize,
    /// 0-based column in characters from the line's start.
    pub column: usize,
    /// How many characters the diagnostic covers (at least one).
    pub len: usize,
    pub error: bool,
}

pub struct Editor {
    pub text: String,
    /// A line (and column) to put the cursor on, set by a click on a page or a problem.
    goto: Option<(usize, usize)>,
    /// Places the last compile complained about, underlined in the text and marked in the gutter.
    marks: Vec<Mark>,
    /// The line the cursor was on last frame.
    pub cursor_line: usize,
    /// The cursor as a byte offset, for the column in the status line.
    cursor_byte: usize,
    /// The syntax-coloured runs of the current text.
    runs: Arc<Vec<(Range<usize>, Color32)>>,
    /// `(hash, len, dark)` of the text the runs were computed for.
    coloured: Option<(u64, usize, bool)>,
    /// The editor's monospace size.
    pub size: f32,
    /// The height of one row of the laid-out text (for small scroll adjustments).
    row_h: f32,
    /// Modal editing for the source pane.
    pub vim: Vim,
    pub vim_bindings: bool,
    /// Whether the source pane holds the keyboard. egui drops focus on an unmodified Escape,
    /// which Vim needs for leaving Insert mode, so the pane remembers it and takes focus back.
    vim_active: bool,
    /// Take the keyboard as soon as the pane is first shown, so typing works without a click.
    focus_on_show: bool,
    view: Option<ViewGeom>,
}

impl Editor {
    pub fn new(text: String) -> Self {
        Self {
            text,
            goto: None,
            marks: Vec::new(),
            cursor_line: 0,
            cursor_byte: 0,
            runs: Arc::new(Vec::new()),
            coloured: None,
            size: 12.5,
            row_h: 17.0,
            vim: Vim::new(),
            vim_bindings: true,
            vim_active: false,
            focus_on_show: true,
            view: None,
        }
    }

    /// Ask for the cursor to be put on `line` (0-based) the next time the editor is shown.
    pub fn go_to_line(&mut self, line: usize) {
        self.goto = Some((line, 0));
    }

    /// Put the cursor on a line and column (0-based, characters), e.g. a diagnostic's place.
    pub fn go_to_position(&mut self, line: usize, column: usize) {
        self.goto = Some((line, column));
    }

    /// The places the last compile complained about; replaced on every compile.
    pub fn set_marks(&mut self, marks: Vec<Mark>) {
        self.marks = marks;
    }

    /// Forget the marks: the text moved on, so their offsets no longer hold.
    pub fn clear_marks(&mut self) {
        self.marks.clear();
    }

    /// Byte offset of a line/column pair.
    fn byte_at(&self, line: usize, column: usize) -> usize {
        let start = self.line_start(line);
        let end = self.text[start..].find('\n').map_or(self.text.len(), |offset| start + offset);
        let mut byte = start;
        for _ in 0..column {
            if byte >= end {
                break;
            }
            byte += self.text[byte..].chars().next().map_or(0, char::len_utf8);
        }
        byte
    }

    pub fn line_count(&self) -> usize {
        self.text.lines().count().max(1)
    }

    /// The cursor's column, 0-based, in characters from the line's start.
    pub fn cursor_column(&self) -> usize {
        let start = self.line_start(self.cursor_line);
        self.text[start.min(self.text.len())..self.cursor_byte.min(self.text.len())].chars().count()
    }

    /// The mode indicator: `NORMAL`, `INSERT`, … and any operator being typed.
    pub fn mode_label(&self) -> String {
        if self.vim_bindings {
            self.vim.status()
        } else {
            "EDIT".to_string()
        }
    }

    /// Show the editor.
    pub fn show(&mut self, ui: &mut Ui, t: &Tokens) -> EditorOutput {
        let mono = FontId::monospace(self.size);
        let char_w = ui.fonts_mut(|fonts| fonts.glyph_width(&mono, '0')).max(1.0);
        let id = ui.make_persistent_id("typst-source");
        if self.focus_on_show {
            self.focus_on_show = false;
            ui.memory_mut(|memory| memory.request_focus(id));
            self.vim_active = true;
        }
        let focused = ui.memory(|memory| memory.has_focus(id));
        let elsewhere = ui.memory(|memory| memory.focused()).is_some_and(|other| other != id);
        if focused {
            self.vim_active = true;
        } else if elsewhere {
            // The palette, the page box or a dialog has the keyboard.
            self.vim_active = false;
        }
        let owns = self.vim_bindings && self.vim_active;

        // The modal layer gets the keys first: in Normal mode it takes all of them, in Insert
        // mode only Escape and the undo keys.
        let mut request = None;
        if owns {
            self.vim.handle(ui, &mut self.text, true);
            request = self.vim.request.take();
            if !focused {
                ui.memory_mut(|memory| memory.request_focus(id));
            }
        }
        self.recolour(t);

        let digits = self.line_count().to_string().len().max(2);
        let gutter = digits as f32 * char_w + 26.0;

        // A jump sets the caret; remember where it went so the view can follow it below.
        let mut scroll_to = None;
        if let Some((line, column)) = self.goto.take() {
            let byte = self.byte_at(line, column);
            if self.vim_bindings {
                self.vim.set_cursor(&self.text, byte);
            } else {
                let index = self.text[..byte].chars().count();
                let mut state = TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(index))));
                state.store(ui.ctx(), id);
                ui.memory_mut(|memory| memory.request_focus(id));
            }
            scroll_to = Some(self.char_index(line, column));
        }
        // The modal layer's cursor is the truth; make the `TextEdit` show it. Only when it has
        // actually moved: egui rewrites the range it is given (it sets `prefer_next_row`), and
        // storing our own copy every frame would look like a selection change on every frame -
        // which makes the widget re-scroll to the caret and repaint the window at full rate.
        if self.vim_bindings {
            let (primary, secondary) = self.vim.caret(&self.text);
            let (p, s) = (byte_to_char(&self.text, primary), byte_to_char(&self.text, secondary));
            let mut state = TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            let current = state.cursor.char_range();
            let same = current.is_some_and(|range| range.primary.index.0 == p && range.secondary.index.0 == s);
            if !same {
                let mut range = CCursorRange::one(CCursor::new(p));
                range.primary = CCursor::new(p);
                range.secondary = CCursor::new(s);
                state.cursor.set_char_range(Some(range));
                state.store(ui.ctx(), id);
            }
            self.cursor_byte = primary;
            self.cursor_line = line_of_byte(&self.text, primary);
            if self.vim.moved {
                scroll_to = Some(byte_to_char(&self.text, primary));
            }
        }

        // The text is laid out here rather than inside the `TextEdit`: the gutter, the current
        // line and the diagnostics need the same wrapped geometry the text is drawn with, in the
        // same frame. The `TextEdit` is handed this galley back, so it costs one layout.
        let wrap_width = (ui.available_width() - gutter - 18.0).max(120.0);
        let galley = self.layout(ui, &mono, wrap_width, t);
        let geometry = LineGeometry::new(&galley);
        self.row_h = galley.rows.first().map_or(self.size * 1.4, |row| row.row.size.y).max(1.0);

        let prompt: Option<(char, String)> = if self.vim_bindings { self.vim.prompt().map(|(sigil, line)| (sigil, line.to_string())) } else { None };
        let reserved = if prompt.is_some() { PROMPT_HEIGHT } else { 0.0 };
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("source-scroll", id));
        if let Some(index) = scroll_to {
            if let Some(offset) = self.scroll_offset_for(&galley, index) {
                scroll = scroll.vertical_scroll_offset(offset);
            }
        }

        let laid = galley.clone();
        let expected = self.text.len();
        let base = t.text;
        let mut layouter = move |ui: &Ui, buffer: &dyn egui::TextBuffer, wrap: f32| {
            if buffer.as_str().len() == expected {
                return laid.clone();
            }
            // Should not happen (the buffer is the text we just laid out), but never hand the
            // widget a galley that belongs to other text.
            let mut job = LayoutJob::default();
            job.wrap.max_width = wrap;
            append(&mut job, &FontId::monospace(12.0), buffer.as_str(), 0, buffer.as_str().len(), base);
            ui.fonts_mut(|fonts| fonts.layout_job(job))
        };

        let output = scroll.show(ui, |ui| {
            let clip = ui.clip_rect();
            let content = vec2(clip.width().max(1.0), (PADDING_TOP + galley.size().y + PADDING_BOTTOM + reserved).max(clip.height()));
            let (rect, _) = ui.allocate_exact_size(content, egui::Sense::hover());
            let text_origin = rect.min + vec2(gutter, PADDING_TOP);
            let text_rect = Rect::from_min_size(text_origin, vec2(wrap_width, galley.size().y));
            let painter = ui.painter().clone();

            // The current line, under the text: the TextEdit's own frame is off, so this shows
            // through behind the glyphs. A wrapped line is one band over all of its rows.
            if let Some((top, height, _)) = geometry.line(self.cursor_line) {
                let band = Rect::from_min_max(pos2(text_origin.x, text_origin.y + top), pos2(clip.right(), text_origin.y + top + height));
                if band.intersects(clip) {
                    painter.rect_filled(band, 0.0, if t.dark() { Color32::from_white_alpha(16) } else { Color32::from_black_alpha(14) });
                    let bar = Rect::from_min_max(pos2(text_origin.x - 4.0, band.top()), pos2(text_origin.x - 1.0, band.bottom()));
                    painter.rect_filled(bar, 1.0, t.accent.gamma_multiply(0.55));
                }
            }

            // Diagnostics: the line is washed faintly, the offending span more strongly, so the
            // eye finds the line first and then the column.
            let (error_color, warning_color) = diagnostic_colors(t);
            for mark in &self.marks {
                let Some((top, height, _)) = geometry.line(mark.line) else { continue };
                let color = if mark.error { error_color } else { warning_color };
                let line_band = Rect::from_min_max(pos2(text_origin.x, text_origin.y + top), pos2(clip.right(), text_origin.y + top + height));
                if line_band.intersects(clip) {
                    painter.rect_filled(line_band, 0.0, color.gamma_multiply(if t.dark() { 0.07 } else { 0.045 }));
                }
                for (x0, x1, row_top, row_h) in self.mark_segments(&galley, mark) {
                    let band = Rect::from_min_max(
                        pos2(text_origin.x + x0, text_origin.y + row_top + 1.0),
                        pos2(text_origin.x + x1, text_origin.y + row_top + row_h - 2.0),
                    );
                    if band.intersects(clip) {
                        painter.rect_filled(band, 2.0, color.gamma_multiply(if t.dark() { 0.16 } else { 0.10 }));
                    }
                }
            }

            let response = ui.put(
                text_rect,
                TextEdit::multiline(&mut self.text)
                    .id(id)
                    .font(mono.clone())
                    .text_color(t.text)
                    .frame(egui::Frame::NONE)
                    .margin(vec2(0.0, 0.0))
                    .desired_width(wrap_width)
                    .lock_focus(true)
                    .layouter(&mut layouter),
            );

            for mark in &self.marks {
                let color = if mark.error { error_color } else { warning_color };
                for (x0, x1, row_top, row_h) in self.mark_segments(&galley, mark) {
                    let y = text_origin.y + row_top + row_h - 1.5;
                    let x0 = (text_origin.x + x0).max(clip.left());
                    let x1 = (text_origin.x + x1).min(clip.right());
                    if x1 > x0 && y > clip.top() && y < clip.bottom() + row_h {
                        squiggle(&painter, x0, x1, y, color);
                    }
                }
            }

            // The gutter is painted over the viewport's left edge, so the numbers stay put while
            // the text scrolls. Wrapped rows carry their line's number only on the first row.
            let gutter_rect = Rect::from_min_max(pos2(clip.left(), clip.top()), pos2(clip.left() + gutter, clip.bottom()));
            painter.rect_filled(gutter_rect, 0.0, t.panel);
            painter.vline(gutter_rect.right() - 0.5, gutter_rect.y_range(), Stroke::new(1.0, t.divider));
            let number_font = FontId::monospace(self.size - 1.5);
            for (line, top, height, first_row_height) in geometry.entries() {
                let y = text_origin.y + top + first_row_height * 0.5;
                if y < clip.top() - self.row_h || y > clip.bottom() + self.row_h || y + height < clip.top() {
                    continue;
                }
                let mark = self.marks.iter().find(|mark| mark.line == line);
                let color = match mark {
                    Some(mark) if mark.error => error_color,
                    Some(_) => warning_color,
                    None if line == self.cursor_line => t.accent_text,
                    None => t.text_faint.gamma_multiply(0.85),
                };
                painter.text(pos2(gutter_rect.right() - 8.0, y), Align2::RIGHT_CENTER, (line + 1).to_string(), number_font.clone(), color);
                if mark.is_some() {
                    let bar = Rect::from_min_max(pos2(gutter_rect.left() + 2.0, y - first_row_height * 0.35), pos2(gutter_rect.left() + 4.5, y + first_row_height * 0.35));
                    painter.rect_filled(bar, 1.0, color);
                }
            }
            self.view = Some(ViewGeom { content_top: rect.top(), clip });
            response
        });

        // Pull the caret back out: in Insert mode the TextEdit moved it, and the mouse can
        // click or select anywhere.
        if self.vim_bindings {
            if let Some(state) = TextEdit::load_state(ui.ctx(), id) {
                if let Some(range) = state.cursor.char_range() {
                    let primary = char_to_byte(&self.text, range.primary.index.0);
                    let secondary = char_to_byte(&self.text, range.secondary.index.0);
                    self.vim.sync(&self.text, primary, secondary);
                }
            }
            self.cursor_byte = self.vim.cursor();
            self.cursor_line = line_of_byte(&self.text, self.cursor_byte);
        } else if let Some(state) = TextEdit::load_state(ui.ctx(), id) {
            if let Some(range) = state.cursor.char_range() {
                let index = range.primary.index.0.min(self.text.chars().count());
                let byte = self.text.char_indices().nth(index).map_or(self.text.len(), |(byte, _)| byte);
                self.cursor_byte = byte;
                self.cursor_line = self.text[..byte].matches('\n').count();
            }
        }

        if let Some((sigil, line)) = &prompt {
            self.prompt_bar(ui, t, &mono, *sigil, line);
        }

        // Edits the modal layer made do not go through the `TextEdit`, so its `changed` flag
        // has to be joined with ours.
        EditorOutput { changed: output.inner.changed() || (self.vim_bindings && self.vim.changed), request }
    }

    /// Lay the buffer out with its syntax colours, wrapped at `wrap_width`.
    fn layout(&self, ui: &Ui, mono: &FontId, wrap_width: f32, t: &Tokens) -> std::sync::Arc<egui::Galley> {
        let mut job = LayoutJob::default();
        job.wrap.max_width = wrap_width;
        let mut at = 0usize;
        for (range, color) in self.runs.iter() {
            if range.start >= self.text.len() {
                break;
            }
            if range.start > at {
                append(&mut job, mono, &self.text, at, range.start, t.text);
            }
            let end = range.end.min(self.text.len());
            if range.start < end {
                append(&mut job, mono, &self.text, range.start, end, *color);
            }
            at = end;
        }
        if at < self.text.len() {
            append(&mut job, mono, &self.text, at, self.text.len(), t.text);
        }
        if self.text.is_empty() {
            // An empty buffer still needs a row to put the caret on.
            job.append(" ", 0.0, egui::TextFormat { font_id: mono.clone(), color: Color32::TRANSPARENT, ..Default::default() });
        }
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    }

    /// The rectangles a diagnostic covers, in text-local coordinates: one per visual row, since
    /// a wrapped span starts on one row and ends on another.
    fn mark_segments(&self, galley: &egui::Galley, mark: &Mark) -> Vec<(f32, f32, f32, f32)> {
        let start = self.char_index(mark.line, mark.column);
        let end = self.char_index(mark.line, mark.column + mark.len.max(1));
        let a = galley.pos_from_cursor(CCursor::new(start));
        let b = galley.pos_from_cursor(CCursor::new(end));
        let right = galley.size().x;
        if (a.top() - b.top()).abs() < 1.0 {
            return vec![(a.left(), b.left().min(right), a.top(), a.height())];
        }
        // Wrapped: the first row to its end, the last row from its start; a span of several rows
        // is rare enough that the rows in between only carry the line wash.
        vec![(a.left(), right, a.top(), a.height()), (0.0, b.left(), b.top(), b.height())]
    }

    /// The scroll offset that brings the caret at `index` into sight. `None` when it already is,
    /// so a one-line motion does not move the view.
    fn scroll_offset_for(&self, galley: &egui::Galley, index: usize) -> Option<f32> {
        let caret = galley.pos_from_cursor(CCursor::new(index));
        let Some(view) = &self.view else {
            return Some((PADDING_TOP + caret.top() - self.row_h * 8.0).max(0.0));
        };
        let y = view.content_top + PADDING_TOP + caret.top();
        let visible = y >= view.clip.top() && y + caret.height() <= view.clip.bottom();
        (!visible).then(|| (PADDING_TOP + caret.top() - view.clip.height() * 0.35).max(0.0))
    }

    /// The character index of a line/column pair.
    fn char_index(&self, line: usize, column: usize) -> usize {
        let byte = self.byte_at(line, column);
        self.text[..byte].chars().count()
    }

    fn prompt_bar(&self, ui: &Ui, t: &Tokens, mono: &FontId, sigil: char, line: &str) {
        let area = ui.max_rect();
        let bar = Rect::from_min_max(pos2(area.left(), area.bottom() - PROMPT_HEIGHT), area.right_bottom());
        ui.painter().rect_filled(bar, 0.0, t.field);
        ui.painter().hline(bar.x_range(), bar.top(), Stroke::new(1.0, t.divider));
        let text = format!("{sigil}{line}");
        let left = bar.left_center() + vec2(10.0, 0.0);
        let galley = ui.painter().layout_no_wrap(text, mono.clone(), t.text);
        let size = galley.size();
        ui.painter().galley(left - vec2(0.0, size.y * 0.5), galley, t.text);
        let caret = Rect::from_min_size(pos2(left.x + size.x + 1.0, left.y - size.y * 0.5 + 1.0), vec2(1.5, size.y - 2.0));
        ui.painter().rect_filled(caret, 0.0, t.text);
    }

    /// Byte offset of the start of `line` (0-based), clamped to the text's end.
    fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        self.text.match_indices('\n').nth(line - 1).map_or(self.text.len(), |(index, _)| index + 1)
    }

    fn recolour(&mut self, t: &Tokens) {
        let hash = fnv(self.text.as_bytes());
        if self.coloured == Some((hash, self.text.len(), t.dark())) {
            return;
        }
        let palette = Palette::for_tokens(t);
        let root = typst::syntax::parse(&self.text);
        let mut runs = Vec::new();
        collect(&root, 0, &palette, &mut runs);
        self.runs = Arc::new(runs);
        self.coloured = Some((hash, self.text.len(), t.dark()));
    }
}

/// The colours a diagnostic's mark is drawn in for this theme.
fn diagnostic_colors(t: &Tokens) -> (Color32, Color32) {
    if t.dark() {
        (Color32::from_rgb(0xF0, 0x7A, 0x7A), Color32::from_rgb(0xE0, 0xA9, 0x6C))
    } else {
        (Color32::from_rgb(255, 0, 0), Color32::from_rgb(0, 0, 255))
    }
}

/// A wavy underline, the way an editor says "this bit is wrong".
fn squiggle(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    const STEP: f32 = 3.0;
    const AMPLITUDE: f32 = 1.4;
    let mut points = Vec::with_capacity(((x1 - x0) / STEP).max(1.0) as usize + 2);
    let mut x = x0;
    let mut up = true;
    while x < x1 {
        points.push(pos2(x, if up { y - AMPLITUDE } else { y + AMPLITUDE }));
        up = !up;
        x += STEP;
    }
    points.push(pos2(x1, y));
    painter.add(egui::Shape::line(points, Stroke::new(1.1, color)));
}

fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices().nth(index).map_or(text.len(), |(byte, _)| byte)
}

fn line_of_byte(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].matches('\n').count()
}

fn append(job: &mut LayoutJob, mono: &FontId, text: &str, start: usize, end: usize, color: Color32) {
    if start >= end || end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return;
    }
    job.append(&text[start..end], 0.0, TextFormat { font_id: mono.clone(), color, ..Default::default() });
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Walk the syntax tree in order, recording the colour of every leaf.
fn collect(node: &SyntaxNode, offset: usize, palette: &Palette, runs: &mut Vec<(Range<usize>, Color32)>) {
    let mut children = node.children().peekable();
    if children.peek().is_none() {
        let color = palette.of(node.kind());
        let len = node.leaf_text().len();
        if len > 0 {
            runs.push((offset..offset + len, color));
        }
        return;
    }
    let mut at = offset;
    for child in children {
        collect(child, at, palette, runs);
        at += child.len();
    }
}

struct Palette {
    text: Color32,
    comment: Color32,
    keyword: Color32,
    string: Color32,
    number: Color32,
    marker: Color32,
    label: Color32,
    math: Color32,
    error: Color32,
}

impl Palette {
    fn for_tokens(t: &Tokens) -> Self {
        if t.dark() {
            Self {
                text: t.text,
                comment: Color32::from_rgb(0x7A, 0x86, 0x94),
                keyword: Color32::from_rgb(0xC6, 0x92, 0xE8),
                string: Color32::from_rgb(0x9E, 0xD3, 0x9A),
                number: Color32::from_rgb(0xE0, 0xA9, 0x6C),
                marker: Color32::from_rgb(0x7E, 0x8C, 0x9E),
                label: Color32::from_rgb(0xE5, 0xC0, 0x7B),
                math: Color32::from_rgb(0x8F, 0xCB, 0xE8),
                error: Color32::from_rgb(0xF0, 0x7A, 0x7A),
            }
        } else {
            Self {
                text: t.text,
                comment: Color32::from_rgb(0x86, 0x8E, 0x96),
                keyword: Color32::from_rgb(0x8E, 0x3F, 0xA8),
                string: Color32::from_rgb(0x2E, 0x7D, 0x32),
                number: Color32::from_rgb(0xB0, 0x66, 0x00),
                marker: Color32::from_rgb(0x9A, 0xA1, 0xAB),
                label: Color32::from_rgb(0x8A, 0x5A, 0x00),
                math: Color32::from_rgb(0x0F, 0x6E, 0x8C),
                error: Color32::from_rgb(0xC0, 0x39, 0x2B),
            }
        }
    }

    fn of(&self, kind: SyntaxKind) -> Color32 {
        if kind.is_keyword() {
            return self.keyword;
        }
        if kind.is_trivia() {
            return self.comment;
        }
        match kind {
            SyntaxKind::Error => self.error,
            SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::Shebang => self.comment,
            SyntaxKind::Str | SyntaxKind::Raw | SyntaxKind::RawDelim | SyntaxKind::RawLang => self.string,
            SyntaxKind::Escape | SyntaxKind::Shorthand | SyntaxKind::SmartQuote | SyntaxKind::MathShorthand => self.number,
            SyntaxKind::Label | SyntaxKind::Ref | SyntaxKind::RefMarker | SyntaxKind::Link => self.label,
            SyntaxKind::HeadingMarker
            | SyntaxKind::ListMarker
            | SyntaxKind::EnumMarker
            | SyntaxKind::TermMarker
            | SyntaxKind::Star
            | SyntaxKind::Underscore
            | SyntaxKind::Dollar
            | SyntaxKind::Hash
            | SyntaxKind::Eq
            | SyntaxKind::Colon
            | SyntaxKind::Comma
            | SyntaxKind::Semicolon => self.marker,
            SyntaxKind::Math | SyntaxKind::MathText | SyntaxKind::MathIdent | SyntaxKind::MathFieldAccess | SyntaxKind::MathAlignPoint => self.math,
            SyntaxKind::Int | SyntaxKind::Float | SyntaxKind::Numeric | SyntaxKind::Ident => self.text,
            kind => {
                let name = kind.name();
                if name.ends_with("Paren") || name.ends_with("Brace") || name.ends_with("Bracket") {
                    self.marker
                } else {
                    self.text
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_line_maps_to_its_first_row_and_takes_its_rows_together() {
        let ctx = egui::Context::default();
        // egui only has fonts after a pass has run.
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        let mut job = LayoutJob::default();
        job.wrap.max_width = 60.0;
        job.append(
            "aaaa bbbb cccc dddd\nee",
            0.0,
            TextFormat { font_id: egui::FontId::monospace(12.0), color: egui::Color32::WHITE, ..Default::default() },
        );
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        let geometry = LineGeometry::new(&galley);
        let entries: Vec<(usize, f32, f32, f32)> = geometry.entries().collect();
        assert_eq!(entries.len(), 2, "two logical lines");
        assert_eq!(entries[0].0, 0);
        assert_eq!(entries[1].0, 1);
        // The first line wrapped, so its height is more than one row's, and the second line
        // starts below it.
        assert!(entries[0].2 > entries[0].3 + 1.0, "line 0 spans several rows");
        assert!(entries[1].1 > entries[0].1 + entries[0].3, "line 1 starts below line 0's first row");
    }

}
