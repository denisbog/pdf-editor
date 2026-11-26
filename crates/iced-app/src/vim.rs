//! The vim adapter on `typst_iced_editor`.
//!
//! The modal machine itself is `pdf_editor_core::vim` — the very code the egui build runs.
//! What is toolkit-specific is how a command reaches the text: the core layer edits a plain
//! `String`, and this module turns the difference between the old and the new text into the
//! editor's own `Action`s. That is also what keeps the editor's syntax tree, selection,
//! scroll and undo story intact: every vim command is a real edit from its point of view.
//!
//! Key routing is the other half: in Normal, Visual and Command mode `key_binding` gives every
//! press to [`Message::Vim`](crate::app::Message), so the widget inserts nothing; Insert mode
//! falls back to the widget's own bindings, with Escape diverted back to the modal layer.

use pdf_editor_core::vim::{self, Key, Mode, Vim};
use typst_iced_editor::{Action, Content, KeyPress};

/// One key press, translated for the modal layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Press {
    pub key: Key,
    pub shift: bool,
    pub ctrl: bool,
    /// The character this press types, when it types one (the command line's input).
    pub text: Option<String>,
}

impl Press {
    /// Translate an editor key press. `None` when the key is not one the modal layer names.
    pub fn new(press: &KeyPress) -> Option<Self> {
        let ctrl = press.modifiers.control() || press.modifiers.command();
        let shift = press.modifiers.shift();
        let text = press.text.as_ref().map(|text| text.to_string());
        let (key, shifted) = match key_of(press) {
            Some(found) => found,
            None => {
                let typed = text.as_deref().and_then(|text| text.chars().next())?;
                let (key, shifted) = vim::key_for_char(typed)?;
                (key, shifted)
            }
        };
        Some(Self { key, shift: shift || shifted, ctrl, text })
    }
}

fn key_of(press: &KeyPress) -> Option<(Key, bool)> {
    use iced::keyboard::key::Named;
    use iced::keyboard::Key as IcedKey;

    let named = match press.modified_key.as_ref() {
        IcedKey::Named(named) => named,
        IcedKey::Character(value) => {
            // The character is the shifted result; `key_for_char` recovers the key and Shift.
            let ch = value.chars().next()?;
            return vim::key_for_char(ch);
        }
        _ => return None,
    };
    Some(match named {
        Named::ArrowLeft => (Key::ArrowLeft, false),
        Named::ArrowRight => (Key::ArrowRight, false),
        Named::ArrowUp => (Key::ArrowUp, false),
        Named::ArrowDown => (Key::ArrowDown, false),
        Named::Home => (Key::Home, false),
        Named::End => (Key::End, false),
        Named::PageUp => (Key::PageUp, false),
        Named::PageDown => (Key::PageDown, false),
        Named::Escape => (Key::Escape, false),
        Named::Enter => (Key::Enter, false),
        Named::Backspace => (Key::Backspace, false),
        Named::Space => (Key::Space, false),
        _ => return None,
    })
}

/// The modal layer's binding state for one document.
pub struct VimEditor {
    pub vim: Vim,
    /// The editor's text as of the last sync, so a command's mutation can be diffed.
    mirror: String,
}

impl VimEditor {
    pub fn new(text: &str) -> Self {
        Self { vim: Vim::new(), mirror: text.to_string() }
    }

    /// Whether the modal layer owns the keyboard right now.
    pub fn captures(&self) -> bool {
        self.vim.mode != Mode::Insert || self.vim.prompt().is_some()
    }

    /// Take the editor's current text and caret, so the layer starts from the truth (a click,
    /// a typing session, a programmatic jump).
    pub fn sync(&mut self, content: &Content) {
        let text = content.text();
        let selection = content.selection();
        let cursor = content.cursor();
        let caret = {
            let buffer = content.buffer();
            buffer.byte_of(cursor)
        };
        let anchor = if selection.is_empty() { caret } else if selection.start == caret { selection.end } else { selection.start };
        self.mirror = text.clone();
        self.vim.sync(&text, caret, anchor);
    }

    /// Put the caret on a byte offset the app picked (a click on a page, a diagnostic).
    pub fn move_to(&mut self, content: &mut Content, byte: usize) {
        content.perform(Action::MoveTo(byte));
        self.vim.set_cursor(&content.text(), byte);
        self.mirror = content.text();
    }

    /// Feed one press to the modal layer and translate the result into editor actions.
    ///
    /// Returns the actions to perform, in order.
    pub fn press(&mut self, press: &Press) -> Vec<Action> {
        let mut text = self.mirror.clone();
        let mut actions = Vec::new();
        if self.vim.prompt().is_some() {
            match press.key {
                Key::Escape => {}
                Key::Enter | Key::Backspace => {}
                _ => {
                    if let Some(typed) = &press.text {
                        self.vim.type_text(typed);
                    }
                }
            }
        }
        let consumed = self.vim.press(&mut text, press.key, press.shift, press.ctrl);
        if !consumed {
            return actions;
        }
        if text != self.mirror {
            if let Some((range, replacement)) = diff(&self.mirror, &text) {
                actions.push(Action::Replace { range, text: replacement });
            }
            self.mirror = text.clone();
        }
        // The caret and selection always come from the modal layer: it is the authority on
        // where they should be, and the editor only moved its own copy because of an edit.
        if let Some(range) = self.vim.selection(&text) {
            // `SelectTo` extends from the current head, so the head goes to the start first.
            actions.push(Action::MoveTo(range.start));
            actions.push(Action::SelectTo(range.end));
        } else if actions.is_empty() || self.vim.moved {
            actions.push(Action::MoveTo(self.vim.caret(&text).0));
        }
        actions
    }

    /// Mirror the editor back into the modal layer after the editor itself did something
    /// (Insert-mode typing, a mouse click, an undo from the editor's history).
    pub fn after_editor(&mut self, content: &Content) {
        self.sync(content);
    }

    /// The mode badge text (`NORMAL`, `INSERT 2d`, …).
    pub fn status(&self) -> String {
        self.vim.status()
    }

    pub fn prompt(&self) -> Option<(char, String)> {
        self.vim.prompt().map(|(sigil, line)| (sigil, line.to_string()))
    }

    pub fn message(&self) -> &str {
        &self.vim.message
    }

    /// Retire a note the last command left, once the write it asked for has run.
    pub fn clear_message(&mut self) {
        self.vim.clear_message();
    }

    pub fn take_request(&mut self) -> Option<vim::Request> {
        self.vim.request.take()
    }
}

/// The single replacement that turns `before` into `after`, when there is one.
///
/// Every vim command is a contiguous edit of the buffer (delete, paste, join, indent, toggle
/// case, undo), which is what makes one `Action::Replace` enough. If the texts differ in more
/// than one region the whole buffer is replaced, which is still correct.
pub fn diff(before: &str, after: &str) -> Option<(std::ops::Range<usize>, String)> {
    if before == after {
        return None;
    }
    let before_bytes = before.as_bytes();
    let after_bytes = after.as_bytes();
    let mut prefix = 0;
    while prefix < before_bytes.len() && prefix < after_bytes.len() && before_bytes[prefix] == after_bytes[prefix] {
        prefix += 1;
    }
    while prefix > 0 && (!before.is_char_boundary(prefix) || !after.is_char_boundary(prefix)) {
        prefix -= 1;
    }
    let mut suffix = 0;
    while suffix < before_bytes.len() - prefix && suffix < after_bytes.len() - prefix && before_bytes[before_bytes.len() - 1 - suffix] == after_bytes[after_bytes.len() - 1 - suffix] {
        suffix += 1;
    }
    while suffix > 0 && (!before.is_char_boundary(before.len() - suffix) || !after.is_char_boundary(after.len() - suffix)) {
        suffix -= 1;
    }
    let range = prefix..before.len() - suffix;
    let text = after[prefix..after.len() - suffix].to_string();
    Some((range, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_is_the_single_region_that_changed() {
        assert_eq!(diff("one two", "one three"), Some((5..7, "hree".to_string())));
        assert_eq!(diff("abc", "abc"), None);
        // An insertion at the end is an empty range at the end.
        assert_eq!(diff("abc", "abcd"), Some((3..3, "d".to_string())));
        // Multibyte text stays on character boundaries.
        let (range, text) = diff("héllo", "hxllo").unwrap();
        assert_eq!((range, text.as_str()), (1..3, "x"));
    }

    #[test]
    fn a_normal_mode_edit_becomes_one_replace_action() {
        let mut content = Content::with_text("one\ntwo\n");
        content.perform(Action::MoveTo(4));
        let mut editor = VimEditor::new(&content.text());
        editor.sync(&content);
        // `dd` deletes the second line.
        let mut actions = Vec::new();
        for key in [Key::D, Key::D] {
            actions = editor.press(&Press { key, shift: false, ctrl: false, text: None });
            for action in actions.drain(..) {
                content.perform(action);
            }
        }
        assert_eq!(content.text(), "one\n");
    }

    #[test]
    fn visual_mode_leaves_a_selection_in_the_editor() {
        let mut content = Content::with_text("abcdef\n");
        let mut editor = VimEditor::new(&content.text());
        editor.sync(&content);
        for key in [Key::V, Key::L, Key::L] {
            for action in editor.press(&Press { key, shift: false, ctrl: false, text: None }) {
                content.perform(action);
            }
        }
        assert_eq!(content.selection(), 0..3);
    }

    #[test]
    fn a_typed_character_on_the_command_line_is_recorded() {
        let mut content = Content::with_text("= Title\n");
        let mut editor = VimEditor::new(&content.text());
        editor.sync(&content);
        editor.press(&Press { key: Key::Colon, shift: false, ctrl: false, text: Some(":".to_string()) });
        editor.press(&Press { key: Key::W, shift: false, ctrl: false, text: Some("w".to_string()) });
        assert_eq!(editor.prompt(), Some((':'.to_string().chars().next().unwrap(), "w".to_string())));
        editor.press(&Press { key: Key::Enter, shift: false, ctrl: false, text: None });
        assert_eq!(editor.take_request(), Some(vim::Request::Save));
    }
}
