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

/// What the editor's own layout looked like last frame, so a cursor move can tell whether it
/// is already in sight.
struct ViewGeom {
    text_top: f32,
    text_left: f32,
    clip: Rect,
    row_h: f32,
}

/// What one frame of the editor produced.
pub struct EditorOutput {
    /// The buffer changed: the modal layer's edits do not go through the `TextEdit`.
    pub changed: bool,
    /// A command the command line asked the window for (`:w`, `:q`, `:e`).
    pub request: Option<vim::Request>,
}

pub struct Editor {
    pub text: String,
    /// A line to put the cursor on, set by a click on a page or a diagnostic.
    goto: Option<usize>,
    /// The line the cursor was on last frame.
    pub cursor_line: usize,
    /// The syntax-coloured runs of the current text.
    runs: Arc<Vec<(Range<usize>, Color32)>>,
    /// `(hash, len, dark)` of the text the runs were computed for.
    coloured: Option<(u64, usize, bool)>,
    /// The editor's monospace size.
    pub size: f32,
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
            cursor_line: 0,
            runs: Arc::new(Vec::new()),
            coloured: None,
            size: 12.5,
            vim: Vim::new(),
            vim_bindings: true,
            vim_active: false,
            focus_on_show: true,
            view: None,
        }
    }

    /// Ask for the cursor to be put on `line` (0-based) the next time the editor is shown.
    pub fn go_to_line(&mut self, line: usize) {
        self.goto = Some(line);
    }

    pub fn line_count(&self) -> usize {
        self.text.lines().count().max(1)
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
        // The row height of a laid-out line is what the gutter must line up with; asking the
        // font for it directly can differ by a rounding step.
        let probe = ui.fonts_mut(|fonts| fonts.layout_no_wrap("M".to_owned(), mono.clone(), t.text));
        let row_h = probe.rows.first().map(|row| row.row.height()).unwrap_or(1.0).max(1.0);
        let char_w = ui.fonts_mut(|f| f.glyph_width(&mono, 'M')).max(1.0);
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

        let lines = self.line_count();
        let digits = lines.to_string().len().max(2);
        let gutter = digits as f32 * char_w + 26.0;

        let mut scroll_to = None;
        if let Some(line) = self.goto.take() {
            if self.vim_bindings {
                self.vim.set_line(&self.text, line);
            } else {
                let byte = self.line_start(line);
                let index = self.text[..byte].chars().count();
                let mut state = TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(index))));
                state.store(ui.ctx(), id);
                ui.memory_mut(|memory| memory.request_focus(id));
            }
            scroll_to = Some(line);
        }
        // The modal layer's cursor is the truth; make the `TextEdit` show it.
        if self.vim_bindings {
            let (primary, secondary) = self.vim.caret(&self.text);
            let mut range = CCursorRange::one(CCursor::new(byte_to_char(&self.text, primary)));
            range.primary = CCursor::new(byte_to_char(&self.text, primary));
            range.secondary = CCursor::new(byte_to_char(&self.text, secondary));
            let mut state = TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            state.cursor.set_char_range(Some(range));
            state.store(ui.ctx(), id);
            self.cursor_line = line_of_byte(&self.text, primary);
            if self.vim.moved {
                scroll_to = Some(self.cursor_line);
            }
        }

        let runs = self.runs.clone();
        let base = t.text;
        let layout_font = mono.clone();
        let mut layouter = move |ui: &Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
            let text = buffer.as_str();
            let mut job = LayoutJob::default();
            job.wrap.max_width = wrap_width;
            let mut at = 0usize;
            for (range, color) in runs.iter() {
                if range.start >= text.len() {
                    break;
                }
                if range.start > at {
                    append(&mut job, &layout_font, text, at, range.start, base);
                }
                let end = range.end.min(text.len());
                if range.start < end {
                    append(&mut job, &layout_font, text, range.start, end, *color);
                }
                at = end;
            }
            if at < text.len() {
                append(&mut job, &layout_font, text, at, text.len(), base);
            }
            ui.fonts_mut(|fonts| fonts.layout_job(job))
        };

        let prompt: Option<(char, String)> = if self.vim_bindings { self.vim.prompt().map(|(sigil, line)| (sigil, line.to_string())) } else { None };
        let reserved = if prompt.is_some() { PROMPT_HEIGHT } else { 0.0 };
        let mut scroll = egui::ScrollArea::both().auto_shrink([false, false]).id_salt(("source-scroll", id));
        if let Some(line) = scroll_to {
            let column = if self.vim_bindings { char_column(&self.text, line, self.vim.cursor()) } else { 0 };
            let (vertical, horizontal) = self.scroll_offsets(line, column, row_h, char_w, gutter);
            if let Some(vertical) = vertical {
                scroll = scroll.vertical_scroll_offset(vertical);
            }
            if let Some(horizontal) = horizontal {
                scroll = scroll.horizontal_scroll_offset(horizontal);
            }
        }

        let output = scroll.show(ui, |ui| {
            let clip = ui.clip_rect();
            let longest = self.text.lines().map(|line| line.chars().count()).max().unwrap_or(0) as f32;
            let content = vec2((gutter + longest * char_w + 48.0).max(clip.width()), (lines as f32 * row_h + 28.0 + reserved).max(clip.height()));
            let (rect, _) = ui.allocate_exact_size(content, egui::Sense::hover());
            let text_rect = Rect::from_min_size(rect.min + vec2(gutter, 14.0), vec2(content.x - gutter, content.y - 14.0));

            // The current line, under the text: the TextEdit's own frame is off, so this is
            // visible behind the glyphs.
            let line_top = text_rect.top() + self.cursor_line as f32 * row_h;
            let row = Rect::from_min_max(pos2(text_rect.left(), line_top), pos2(clip.right(), line_top + row_h));
            if row.intersects(clip) {
                ui.painter().rect_filled(row, 0.0, if t.dark() { Color32::from_white_alpha(16) } else { Color32::from_black_alpha(14) });
                let bar = Rect::from_min_max(pos2(text_rect.left() - 4.0, line_top), pos2(text_rect.left() - 1.0, line_top + row_h));
                ui.painter().rect_filled(bar, 1.0, t.accent.gamma_multiply(0.55));
            }

            let response = ui.put(
                text_rect,
                TextEdit::multiline(&mut self.text)
                    .id(id)
                    .font(mono.clone())
                    .text_color(t.text)
                    .frame(egui::Frame::NONE)
                    .margin(vec2(0.0, 0.0))
                    .desired_width(text_rect.width())
                    .lock_focus(true)
                    .layouter(&mut layouter),
            );

            // The gutter is painted over the viewport's left edge, so the numbers stay put
            // while the text scrolls sideways.
            let gutter_rect = Rect::from_min_max(pos2(clip.left(), clip.top()), pos2(clip.left() + gutter, clip.bottom()));
            ui.painter().rect_filled(gutter_rect, 0.0, t.panel);
            ui.painter().vline(gutter_rect.right() - 0.5, gutter_rect.y_range(), Stroke::new(1.0, t.divider));
            let number_font = FontId::monospace(self.size - 1.5);
            let first = (((clip.top() - text_rect.top()) / row_h).floor() as isize).max(0) as usize;
            let last = (((clip.bottom() - text_rect.top()) / row_h).ceil() as isize).max(0) as usize + 1;
            for line in first..last.min(lines) {
                let y = text_rect.top() + line as f32 * row_h + row_h * 0.5;
                let color = if line == self.cursor_line { t.accent_text } else { t.text_faint.gamma_multiply(0.85) };
                ui.painter().text(pos2(gutter_rect.right() - 8.0, y), Align2::RIGHT_CENTER, (line + 1).to_string(), number_font.clone(), color);
            }
            self.view = Some(ViewGeom { text_top: text_rect.top(), text_left: text_rect.left(), clip, row_h });
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
            self.cursor_line = line_of_byte(&self.text, self.vim.cursor());
        } else if let Some(state) = TextEdit::load_state(ui.ctx(), id) {
            if let Some(range) = state.cursor.char_range() {
                let index = range.primary.index.0.min(self.text.chars().count());
                let byte = self.text.char_indices().nth(index).map_or(self.text.len(), |(byte, _)| byte);
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

    /// Where the view must scroll to so the cursor is in sight. `None` when the line (or the
    /// character) is already visible, so a one-line motion does not move the view.
    fn scroll_offsets(&self, line: usize, column: usize, row_h: f32, char_w: f32, gutter: f32) -> (Option<f32>, Option<f32>) {
        let Some(view) = &self.view else {
            return (Some((line as f32 * row_h - row_h * 8.0).max(0.0)), None);
        };
        let y = view.text_top + line as f32 * view.row_h;
        let vertical_visible = y >= view.clip.top() && y + view.row_h <= view.clip.bottom();
        let vertical = (!vertical_visible).then(|| (line as f32 * row_h - view.clip.height() * 0.35).max(0.0));
        let x = view.text_left + column as f32 * char_w;
        let band = view.clip.width() - gutter;
        let horizontal_visible = x >= view.clip.left() + gutter + 4.0 && x <= view.clip.right() - 24.0;
        let horizontal = (!horizontal_visible).then(|| (column as f32 * char_w - band * 0.25).max(0.0));
        (vertical, horizontal)
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

fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices().nth(index).map_or(text.len(), |(byte, _)| byte)
}

fn line_of_byte(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].matches('\n').count()
}

/// The cursor's column in characters, when it sits on `line`.
fn char_column(text: &str, line: usize, byte: usize) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(offset) => start += offset + 1,
            None => return 0,
        }
    }
    text[start..byte.min(text.len())].chars().count()
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
