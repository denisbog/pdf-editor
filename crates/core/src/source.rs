//! The buffer abstraction the vim state machine edits.
//!
//! Vim speaks `(text: &str, cursor: byte offset)`; a front-end's editor widget speaks its own
//! thing (`TextEdit` + galley plumbing under egui, `typst_iced_editor::Content` under iced).
//! [`Source`] is the seam between them: the vim layer asks a source for its text, cursor and
//! selection, and hands back an [`Edit`] describing the change. A front-end translates that
//! edit into whatever its widget understands.

use std::ops::Range;

/// One change to a source: an edit of its text, a caret movement, or a scroll request.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Replace a byte range with `text` (an insert has an empty range).
    ///
    /// This is the only text-changing edit: everything vim does — delete, change, paste,
    /// join, indent, toggle case, undo — is expressed as one or more of these, which is
    /// exactly the shape `typst_iced_editor::Action::Replace` and egui's `TextEdit` both take.
    Replace { range: Range<usize>, text: String },
    /// Put the caret at a byte offset, collapsing any selection.
    MoveTo(usize),
    /// Extend the selection to a byte offset.
    SelectTo(usize),
    /// Scroll the view by this many vertical pixels.
    Scroll { y: f32 },
    /// Undo one step in the widget's own history (used in Insert mode, where typing does not
    /// pass through vim).
    Undo,
    /// Redo one step in the widget's own history.
    Redo,
}

/// A text buffer a front-end lets vim drive.
pub trait Source {
    /// The whole text.
    fn text(&self) -> &str;
    /// The caret, as a byte offset.
    fn cursor(&self) -> usize;
    /// The selection, in byte offsets, when there is one.
    fn selection(&self) -> Option<Range<usize>>;
    /// Apply one edit.
    fn apply(&mut self, edit: Edit);
}

/// A plain in-memory source: text, a caret and an anchor.
///
/// The iced front-end keeps the authority in the editor widget's [`Content`] and mirrors it
/// into a `Buffer` for each vim key press (see `crates/iced-app/src/vim.rs`); the egui
/// front-end uses `TextEdit` state instead but implements the same trait over it.
#[derive(Clone, Debug, Default)]
pub struct Buffer {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    /// Accumulated vertical scroll requests, drained by the front-end.
    pub scroll: f32,
}

impl Buffer {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), cursor: 0, anchor: None, scroll: 0.0 }
    }

    /// Move the caret and collapse the selection without going through [`Source::apply`].
    pub fn set_cursor(&mut self, byte: usize) {
        self.cursor = clamp(&self.text, byte);
        self.anchor = None;
    }

    /// Replace the whole text, keeping the caret inside it (a compile or a file reload).
    pub fn reset(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = clamp(&self.text, self.cursor);
        if self.anchor.is_some_and(|anchor| anchor > self.text.len()) {
            self.anchor = None;
        }
    }

    pub fn take_scroll(&mut self) -> f32 {
        std::mem::take(&mut self.scroll)
    }
}

impl Source for Buffer {
    fn text(&self) -> &str {
        &self.text
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| if anchor < self.cursor { anchor..self.cursor } else { self.cursor..anchor })
    }

    fn apply(&mut self, edit: Edit) {
        match edit {
            Edit::Replace { range, text } => {
                let start = clamp(&self.text, range.start);
                let end = clamp(&self.text, range.end.max(start));
                self.text.replace_range(start..end, &text);
                self.cursor = start + text.len();
                self.anchor = None;
            }
            Edit::MoveTo(byte) => self.set_cursor(byte),
            Edit::SelectTo(byte) => {
                // Extending a selection starts one at the current caret, as a shift-motion does.
                if self.anchor.is_none() {
                    self.anchor = Some(self.cursor);
                }
                self.cursor = clamp(&self.text, byte);
            }
            Edit::Scroll { y } => self.scroll += y,
            Edit::Undo | Edit::Redo => {}
        }
        self.cursor = clamp(&self.text, self.cursor);
    }
}

fn clamp(text: &str, mut byte: usize) -> usize {
    byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_moves_the_caret_to_the_end_of_the_insertion() {
        let mut buffer = Buffer::new("hello");
        buffer.apply(Edit::Replace { range: 0..5, text: "bye".to_string() });
        assert_eq!(buffer.text(), "bye");
        assert_eq!(buffer.cursor(), 3);
        assert_eq!(buffer.selection(), None);
    }

    #[test]
    fn a_move_clamps_to_a_character_boundary() {
        let mut buffer = Buffer::new("héllo");
        buffer.apply(Edit::MoveTo(2));
        assert_eq!(buffer.cursor(), 1, "the middle of é is not a boundary");
    }

    #[test]
    fn a_selection_keeps_both_ends() {
        let mut buffer = Buffer::new("abcdef");
        buffer.apply(Edit::SelectTo(4));
        buffer.apply(Edit::MoveTo(0));
        buffer.apply(Edit::SelectTo(4));
        assert_eq!(buffer.selection(), Some(0..4));
    }
}
