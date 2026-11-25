//! The source editor: the Typst text, its syntax colours (from Typst's own parser), line
//! numbers, and the "go to line" that a click on a rendered page or a diagnostic triggers.

use std::ops::Range;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Rect, Stroke, TextEdit, Ui, pos2, vec2};
use typst::syntax::{SyntaxKind, SyntaxNode};

use crate::theme::Tokens;

/// How long after the last keystroke the document is compiled again.
pub const DEBOUNCE: f64 = 0.22;

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
}

impl Editor {
    pub fn new(text: String) -> Self {
        Self { text, goto: None, cursor_line: 0, runs: Arc::new(Vec::new()), coloured: None, size: 12.5 }
    }

    /// Ask for the cursor to be put on `line` (0-based) the next time the editor is shown.
    pub fn go_to_line(&mut self, line: usize) {
        self.goto = Some(line);
    }

    pub fn line_count(&self) -> usize {
        self.text.lines().count().max(1)
    }

    /// Show the editor and return its response. `changed` reports an edit of the text.
    pub fn show(&mut self, ui: &mut Ui, t: &Tokens) -> egui::Response {
        self.recolour(t);
        let mono = FontId::monospace(self.size);
        let row_h = ui.fonts_mut(|f| f.row_height(&mono)).max(1.0);
        let char_w = ui.fonts_mut(|f| f.glyph_width(&mono, 'M')).max(1.0);
        let lines = self.line_count();
        let digits = lines.to_string().len().max(2);
        let gutter = digits as f32 * char_w + 26.0;
        let id = ui.make_persistent_id("typst-source");

        // A jump puts the caret on the line and asks the scroll view to bring it into sight.
        let mut scroll_to = None;
        if let Some(line) = self.goto.take() {
            let byte = self.line_start(line);
            let char_index = self.text[..byte].chars().count();
            let mut state = TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            state.cursor.set_char_range(Some(egui::text_selection::CCursorRange::one(egui::text::CCursor::new(char_index))));
            state.store(ui.ctx(), id);
            ui.memory_mut(|memory| memory.request_focus(id));
            scroll_to = Some(line);
            self.cursor_line = line;
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

        let mut scroll = egui::ScrollArea::both().auto_shrink([false, false]).id_salt(("source-scroll", id));
        if let Some(line) = scroll_to {
            scroll = scroll.vertical_scroll_offset((line as f32 * row_h - 140.0).max(0.0));
        }
        let output = scroll.show(ui, |ui| {
            let clip = ui.clip_rect();
            let longest = self.text.lines().map(|line| line.chars().count()).max().unwrap_or(0) as f32;
            let content = vec2((gutter + longest * char_w + 48.0).max(clip.width()), (lines as f32 * row_h + 28.0).max(clip.height()));
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

            // Where the caret is now, for the line highlight and for "line n of m" in the bar.
            if let Some(state) = TextEdit::load_state(ui.ctx(), id) {
                if let Some(range) = state.cursor.char_range() {
                    let index = range.primary.index.0.min(self.text.chars().count());
                    let byte = self.text.char_indices().nth(index).map_or(self.text.len(), |(byte, _)| byte);
                    self.cursor_line = self.text[..byte].matches('\n').count();
                }
            }

            // The gutter is painted over the viewport's left edge, so the numbers stay put
            // while the text scrolls sideways.
            let gutter_rect = Rect::from_min_max(pos2(clip.left(), clip.top()), pos2(clip.left() + gutter, clip.bottom()));
            ui.painter().rect_filled(gutter_rect, 0.0, if matches!(t.kind, crate::theme::ThemeKind::Dark) { t.panel } else { t.panel });
            ui.painter().vline(gutter_rect.right() - 0.5, gutter_rect.y_range(), Stroke::new(1.0, t.divider));
            let number_font = FontId::monospace(self.size - 1.5);
            let first = (((clip.top() - text_rect.top()) / row_h).floor() as isize).max(0) as usize;
            let last = (((clip.bottom() - text_rect.top()) / row_h).ceil() as isize).max(0) as usize + 1;
            for line in first..last.min(lines) {
                let y = text_rect.top() + line as f32 * row_h + row_h * 0.5;
                let color = if line == self.cursor_line { t.accent_text } else { t.text_faint.gamma_multiply(0.85) };
                ui.painter().text(pos2(gutter_rect.right() - 8.0, y), egui::Align2::RIGHT_CENTER, (line + 1).to_string(), number_font.clone(), color);
            }
            response
        });
        output.inner
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
        // Any byte the tree does not cover (it should cover everything) keeps the plain colour.
        self.runs = Arc::new(runs);
        self.coloured = Some((hash, self.text.len(), t.dark()));
    }
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
                } else if matches!(kind, SyntaxKind::Text) {
                    self.text
                } else if kind == SyntaxKind::FuncCall || kind == SyntaxKind::Args || kind == SyntaxKind::Named {
                    self.text
                } else {
                    self.text
                }
            }
        }
    }
}

