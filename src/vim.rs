//! Vim motions for the source pane.
//!
//! The editor is an egui `TextEdit`, so this module is the modal layer in front of it: while the
//! editor has focus it takes the keyboard, keeps its own cursor, and only lets keys through to
//! the `TextEdit` in Insert mode. Every command edits the buffer directly and records an undo
//! snapshot, so `u` and `Ctrl+R` behave the same in both modes.
//!
//! Implemented: Normal, Insert, Visual (char and line) and Command-line modes; counts;
//! operators `d c y > <` with motions and text objects, `dd cc yy`, `D C Y`, `x X s S r ~ J`;
//! `i I a A o O`; motions `h j k l w W b B e E 0 ^ $ gg G { } + - _ | f t F T ; ,`;
//! text objects `iw aw iW aW` plus quotes and brackets; the unnamed register and `p P`;
//! `u` / `Ctrl+R` / `.`-less undo; `/` search with `n N`; and a command line with
//! `:w :wq :q :q! :e <file> :<line>`.
//!
//! Deliberately absent (see the README): marks, macros, block visual (`Ctrl+V`), `.` repeat,
//! named registers, `zz`/`zt`/`zb`, and folding.

use egui::Key;
use std::ops::Range;

/// Which mode the editor is in. Operator-pending stays Normal, as in Vim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual { line: bool },
    Command,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual { line: false } => "VISUAL",
            Mode::Visual { line: true } => "VISUAL LINE",
            Mode::Command => "COMMAND",
        }
    }
}

/// Something the window has to do on the command line's behalf.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Save,
    Close,
    Open(String),
    Compile,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Dedent,
    ToggleCase,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FindKind {
    Forward,
    Backward,
    TillForward,
    TillBackward,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PromptKind {
    Command,
    Search,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Pending {
    None,
    /// An operator waiting for a motion or a text object.
    Operator { op: Op, count: u32 },
    /// `i` or `a` after an operator, waiting for the object.
    Object { op: Op, count: u32, around: bool },
    /// `f t F T` waiting for the character to find.
    Find { kind: FindKind, count: u32, op: Option<Op> },
    /// `r` waiting for the replacement character.
    Replace { count: u32 },
    /// `g` waiting for its second key (`gg`).
    G { count: u32 },
}

/// A place a motion reaches. `inclusive` motions include the character at `target` when an
/// operator is applied to them (Vim's `e`, `f`, `$`); `linewise` ones cover whole lines.
#[derive(Clone, Copy, Debug)]
struct Motion {
    target: usize,
    linewise: bool,
    inclusive: bool,
}

struct Snapshot {
    text: String,
    cursor: usize,
}

#[derive(Default)]
struct Register {
    text: String,
    linewise: bool,
}

pub struct Vim {
    pub mode: Mode,
    /// Byte offset. In Normal and Visual mode it is the character under the cursor; in Insert
    /// mode it is the caret (the insertion point).
    cursor: usize,
    /// The other end of the Visual selection (a byte offset).
    anchor: Option<usize>,
    count: u32,
    pending: Pending,
    register: Register,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The column `j` and `k` try to keep, in characters.
    desired_column: Option<usize>,
    last_find: Option<(FindKind, char)>,
    search: Option<String>,
    prompt: Option<(PromptKind, String)>,
    pub request: Option<Request>,
    /// The buffer changed this frame (the window recompiles).
    pub changed: bool,
    /// The cursor moved this frame (the view scrolls it into sight).
    pub moved: bool,
    /// A short note for the status line ("3 lines yanked", "pattern not found").
    pub message: String,
}

impl Vim {
    pub fn new() -> Self {
        Self {
            mode: Mode::Normal,
            cursor: 0,
            anchor: None,
            count: 0,
            pending: Pending::None,
            register: Register::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            desired_column: None,
            last_find: None,
            search: None,
            prompt: None,
            request: None,
            changed: false,
            moved: false,
            message: String::new(),
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Put the cursor somewhere (a click on a page, a diagnostic, a new file).
    pub fn set_cursor(&mut self, text: &str, cursor: usize) {
        self.cursor = floor_boundary(text, cursor.min(text.len()));
        self.anchor = None;
        self.desired_column = None;
        self.pending = Pending::None;
        self.count = 0;
        self.moved = true;
        self.normalize(text);
    }

    /// Adopt the caret the `TextEdit` ended up with (mouse clicks, typing in Insert mode).
    /// The two ends are byte offsets; in Normal mode only the primary one matters.
    pub fn sync(&mut self, text: &str, primary: usize, secondary: usize) {
        let primary = floor_boundary(text, primary.min(text.len()));
        let secondary = floor_boundary(text, secondary.min(text.len()));
        let moved = primary != self.cursor;
        self.cursor = primary;
        if self.mode == Mode::Insert {
            return;
        }
        if matches!(self.mode, Mode::Visual { .. }) {
            self.anchor = Some(secondary);
            if moved {
                self.normalize(text);
            }
        } else if self.anchor.is_some() || moved {
            // A click (or a lost selection) collapses the selection in Normal mode.
            self.anchor = None;
            self.normalize(text);
        }
    }

    /// The caret range the `TextEdit` should show, in byte offsets.
    pub fn caret(&self, text: &str) -> (usize, usize) {
        match self.selection(text) {
            Some(range) => {
                let (low, high) = (range.start, range.end);
                if self.anchor.is_some_and(|anchor| anchor <= self.cursor) {
                    (low, high.saturating_sub(1).max(low))
                } else {
                    (high.saturating_sub(1).max(low), low)
                }
            }
            None => (self.cursor, self.cursor),
        }
    }

    /// The mode indicator, with any count or operator being typed.
    pub fn status(&self) -> String {
        let mut status = self.mode.label().to_string();
        if self.mode == Mode::Insert || self.mode == Mode::Command {
            return status;
        }
        let mut prefix = String::new();
        if self.count > 0 {
            prefix.push_str(&self.count.to_string());
        }
        match self.pending {
            Pending::None => {}
            Pending::Operator { op, .. } => prefix.push_str(op_key(op)),
            Pending::Object { op, around, .. } => {
                prefix.push_str(op_key(op));
                prefix.push(if around { 'a' } else { 'i' });
            }
            Pending::Find { kind, op, .. } => {
                if let Some(op) = op {
                    prefix.push_str(op_key(op));
                }
                prefix.push_str(find_key(kind));
            }
            Pending::Replace { .. } => prefix.push('r'),
            Pending::G { .. } => prefix.push('g'),
        }
        if !prefix.is_empty() {
            status = format!("{status}   {prefix}");
        }
        status
    }

    /// The command line being typed, for the little bar under the editor.
    pub fn prompt(&self) -> Option<(char, &str)> {
        self.prompt.as_ref().map(|(kind, text)| (if *kind == PromptKind::Command { ':' } else { '/' }, text.as_str()))
    }

    /// The Visual selection as a byte range (the character under the cursor included).
    pub fn selection(&self, text: &str) -> Option<Range<usize>> {
        let Mode::Visual { line } = self.mode else { return None };
        let anchor = self.anchor?;
        let (low, high) = if anchor <= self.cursor { (anchor, self.cursor) } else { (self.cursor, anchor) };
        if line {
            let start = line_start(text, low);
            let end = line_bounds(text, high).1;
            let end = if end < text.len() { end + 1 } else { end };
            return Some(start..end);
        }
        let end = (high + char_len(text, high)).min(text.len());
        Some(low..end)
    }

    /// Take this frame's keyboard events. In Normal and Visual mode everything is ours; in
    /// Insert mode only Escape and the undo keys are.
    pub fn handle(&mut self, ui: &egui::Ui, text: &mut String, has_focus: bool) {
        self.changed = false;
        self.moved = false;
        if !has_focus {
            return;
        }
        let events = ui.input(|input| input.events.clone());
        if events.is_empty() {
            return;
        }
        let insert = self.mode == Mode::Insert;
        let prompt = self.prompt.is_some();
        let mut keep: Vec<egui::Event> = Vec::with_capacity(events.len());
        let mut keys: Vec<(Key, bool, bool)> = Vec::new();
        for event in events {
            if matches!(&event, egui::Event::Key { pressed: false, .. }) {
                // Key releases are harmless in every mode; the TextEdit may want them.
                keep.push(event);
                continue;
            }
            match event {
                egui::Event::Key { key, repeat, modifiers, .. } => {
                    let ctrl = modifiers.ctrl || modifiers.command;
                    if insert {
                        if key == Key::Escape || (ctrl && key == Key::OpenBracket) {
                            self.leave_insert(text);
                        } else if ctrl && key == Key::Z {
                            self.undo(text);
                            self.mode = Mode::Normal;
                            self.normalize(text);
                        } else if ctrl && (key == Key::Y || (key == Key::Z && modifiers.shift)) {
                            self.redo(text);
                        } else {
                            keep.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat, modifiers });
                        }
                    } else {
                        keys.push((key, modifiers.shift, ctrl));
                    }
                }
                egui::Event::Text(value) => {
                    if insert {
                        keep.push(egui::Event::Text(value));
                    } else if prompt {
                        if let Some((_, line)) = &mut self.prompt {
                            line.push_str(&value);
                        }
                    }
                }
                egui::Event::Paste(value) => {
                    if insert {
                        keep.push(egui::Event::Paste(value));
                    } else if prompt {
                        if let Some((_, line)) = &mut self.prompt {
                            line.push_str(&value);
                        }
                    }
                }
                egui::Event::Copy | egui::Event::Cut => {
                    if insert {
                        keep.push(event);
                    }
                }
                other => keep.push(other),
            }
        }
        ui.ctx().input_mut(|input| input.events = keep);
        for (key, shift, ctrl) in keys {
            self.key(text, key, shift, ctrl);
        }
    }

    fn key(&mut self, text: &mut String, key: Key, shift: bool, ctrl: bool) {
        if self.prompt.is_some() {
            self.prompt_key(text, key, ctrl);
            return;
        }
        if ctrl {
            match key {
                Key::D => self.scroll_lines(text, 12, false),
                Key::U => self.scroll_lines(text, 12, true),
                Key::F => self.scroll_lines(text, 32, false),
                Key::B => self.scroll_lines(text, 32, true),
                Key::R => self.redo(text),
                Key::N => self.step_line(text, 1),
                Key::P => self.step_line(text, -1),
                Key::OpenBracket => {
                    self.mode = Mode::Normal;
                    self.anchor = None;
                    self.pending = Pending::None;
                }
                _ => {}
            }
            self.count = 0;
            self.pending = Pending::None;
            return;
        }

        if key == Key::Escape {
            self.pending = Pending::None;
            self.count = 0;
            self.mode = if self.mode == Mode::Command { Mode::Normal } else { Mode::Normal };
            self.anchor = None;
            return;
        }
        let ch = char_of(key, shift);

        // A pending operator, find or replace takes the next key as its argument.
        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::Operator { op, count } => {
                if let Some(digit) = ch.and_then(|value| value.to_digit(10)) {
                    let count = count.saturating_mul(10).saturating_add(digit).min(100_000);
                    self.pending = Pending::Operator { op, count };
                    return;
                }
                return self.operator_key(text, op, count, ch);
            }
            Pending::Object { op, count, around } => {
                if let Some(object) = ch {
                    if let Some((range, linewise)) = text_object(text, self.cursor, object, around) {
                        self.apply(text, op, range, linewise, count.max(1) as usize);
                    }
                }
                return;
            }
            Pending::Find { kind, count, op } => {
                if let Some(target) = ch {
                    self.find(text, kind, target, count.max(1) as usize, op);
                }
                return;
            }
            Pending::Replace { count } => {
                if let Some(replacement) = ch {
                    self.replace_chars(text, replacement, count.max(1) as usize);
                }
                return;
            }
            Pending::G { count } => {
                if ch == Some('g') {
                    self.goto_line(text, if count > 0 { count as usize - 1 } else { 0 });
                }
                return;
            }
            Pending::None => {}
        }

        // Counts: `0` is a motion unless a count is already being typed.
        if let Some(digit) = ch.and_then(|value| value.to_digit(10)) {
            if !(digit == 0 && self.count == 0) {
                self.count = self.count.saturating_mul(10).saturating_add(digit).min(100_000);
                return;
            }
        }
        let count = self.count.max(1) as usize;
        self.count = 0;

        match self.mode {
            Mode::Visual { .. } => self.visual_key(text, key, ch, count),
            _ => self.normal_key(text, key, ch, count),
        }
    }

    fn normal_key(&mut self, text: &mut String, key: Key, ch: Option<char>, count: usize) {
        if let Some(motion) = self.motion(text, key, ch, count) {
            self.desired_column = if matches!(key, Key::J | Key::K | Key::ArrowDown | Key::ArrowUp) { self.desired_column } else { None };
            self.move_to(text, motion.target);
            return;
        }
        let Some(ch) = ch else { return };
        match ch {
            'd' => self.pending = Pending::Operator { op: Op::Delete, count: count as u32 },
            'c' => self.pending = Pending::Operator { op: Op::Change, count: count as u32 },
            'y' => self.pending = Pending::Operator { op: Op::Yank, count: count as u32 },
            '>' => self.pending = Pending::Operator { op: Op::Indent, count: count as u32 },
            '<' => self.pending = Pending::Operator { op: Op::Dedent, count: count as u32 },
            'i' => self.enter_insert(text, self.cursor),
            'I' => {
                let target = first_nonblank(text, line_start(text, self.cursor));
                self.enter_insert(text, target);
            }
            'a' => {
                let target = if char_at(text, self.cursor).is_some() { next_char(text, self.cursor) } else { self.cursor };
                self.enter_insert(text, target);
            }
            'A' => {
                let target = line_bounds(text, self.cursor).1;
                self.enter_insert(text, target);
            }
            'o' => self.open_line(text, true),
            'O' => self.open_line(text, false),
            'x' => self.delete_chars(text, count, false),
            'X' => self.delete_chars(text, count, true),
            's' => {
                let end = next_char(text, self.cursor);
                self.apply(text, Op::Change, self.cursor..end, false, 1);
            }
            'S' => {
                let (start, end) = line_bounds(text, self.cursor);
                self.apply(text, Op::Change, start..end, false, 1);
            }
            'D' => {
                let (_, end) = line_bounds(text, self.cursor);
                self.apply(text, Op::Delete, self.cursor..end.max(self.cursor), false, 1);
            }
            'C' => {
                let (_, end) = line_bounds(text, self.cursor);
                self.apply(text, Op::Change, self.cursor..end.max(self.cursor), false, 1);
            }
            'Y' => {
                let (start, end) = line_bounds(text, self.cursor);
                self.apply(text, Op::Yank, start..end, true, 1);
            }
            'p' => self.paste(text, true, count),
            'P' => self.paste(text, false, count),
            'u' => self.undo(text),
            'r' => self.pending = Pending::Replace { count: count as u32 },
            'f' => self.pending = Pending::Find { kind: FindKind::Forward, count: count as u32, op: None },
            'F' => self.pending = Pending::Find { kind: FindKind::Backward, count: count as u32, op: None },
            't' => self.pending = Pending::Find { kind: FindKind::TillForward, count: count as u32, op: None },
            'T' => self.pending = Pending::Find { kind: FindKind::TillBackward, count: count as u32, op: None },
            ';' => {
                if let Some((kind, target)) = self.last_find {
                    self.find(text, kind, target, count, None);
                }
            }
            ',' => {
                if let Some((kind, target)) = self.last_find {
                    self.find(text, flip_find(kind), target, count, None);
                }
            }
            'n' => self.search_next(text, count, false),
            'N' => self.search_next(text, count, true),
            'g' => self.pending = Pending::G { count: count as u32 },
            'G' => self.goto_line(text, if count > 1 { count - 1 } else { line_count(text).saturating_sub(1) }),
            'J' => self.join(text, count),
            '~' => self.toggle_case(text, count),
            'v' => {
                self.mode = Mode::Visual { line: false };
                self.anchor = Some(self.cursor);
            }
            'V' => {
                self.mode = Mode::Visual { line: true };
                self.anchor = Some(self.cursor);
            }
            ':' => self.open_prompt(PromptKind::Command),
            '/' => self.open_prompt(PromptKind::Search),
            'm' | '\'' | '`' => self.message = "marks are not implemented".to_string(),
            '.' => self.message = ". (repeat) is not implemented".to_string(),
            'Z' => self.message = "use :wq to write and close".to_string(),
            _ => {}
        }
    }

    fn visual_key(&mut self, text: &mut String, key: Key, ch: Option<char>, count: usize) {
        if let Some(motion) = self.motion(text, key, ch, count) {
            self.move_to(text, motion.target);
            return;
        }
        let Some(ch) = ch else { return };
        match ch {
            'd' | 'x' => self.selection_op(text, Op::Delete, count),
            'c' | 's' => self.selection_op(text, Op::Change, count),
            'y' => self.selection_op(text, Op::Yank, count),
            '>' => self.selection_op(text, Op::Indent, count),
            '<' => self.selection_op(text, Op::Dedent, count),
            '~' => self.selection_op(text, Op::ToggleCase, count),
            'J' => {
                self.mode = Mode::Normal;
                self.anchor = None;
                self.join(text, count);
            }
            'p' | 'P' => {
                if let Some(range) = self.selection(text) {
                    self.push_undo(text);
                    let cut = if matches!(self.mode, Mode::Visual { line: true }) { linewise_range(text, range) } else { range };
                    self.cursor = cut.start;
                    text.replace_range(cut.clone(), "");
                    self.mode = Mode::Normal;
                    self.anchor = None;
                    self.changed = true;
                    self.paste(text, true, 1);
                }
            }
            'o' => {
                if let Some(anchor) = self.anchor {
                    self.anchor = Some(self.cursor);
                    self.cursor = anchor;
                    self.desired_column = None;
                    self.moved = true;
                }
            }
            'v' => {
                if matches!(self.mode, Mode::Visual { line: false }) {
                    self.mode = Mode::Normal;
                    self.anchor = None;
                } else {
                    self.mode = Mode::Visual { line: false };
                }
            }
            'V' => {
                if matches!(self.mode, Mode::Visual { line: true }) {
                    self.mode = Mode::Normal;
                    self.anchor = None;
                } else {
                    self.mode = Mode::Visual { line: true };
                }
            }
            ':' => self.open_prompt(PromptKind::Command),
            _ => {}
        }
    }

    fn operator_key(&mut self, text: &mut String, op: Op, count: u32, ch: Option<char>) {
        // `dd cc yy >> <<`: the operator repeated works line-wise.
        if ch == Some(op_key_char(op)) {
            let count = count.max(1) as usize;
            let start = line_start(text, self.cursor);
            let mut end = line_bounds(text, self.cursor).1;
            if end < text.len() {
                end += 1;
            }
            for _ in 1..count {
                let Some(next) = next_line(text, end) else { break };
                let line_end = line_bounds(text, next).1;
                end = if line_end < text.len() { line_end + 1 } else { line_end };
            }
            self.apply(text, op, start..end, true, 1);
            self.cursor = floor_boundary(text, start);
            return;
        }
        let Some(ch) = ch else { return };
        match ch {
            'i' | 'a' => {
                self.pending = Pending::Object { op, count, around: ch == 'a' };
                return;
            }
            'f' => {
                self.pending = Pending::Find { kind: FindKind::Forward, count, op: Some(op) };
                return;
            }
            'F' => {
                self.pending = Pending::Find { kind: FindKind::Backward, count, op: Some(op) };
                return;
            }
            't' => {
                self.pending = Pending::Find { kind: FindKind::TillForward, count, op: Some(op) };
                return;
            }
            'T' => {
                self.pending = Pending::Find { kind: FindKind::TillBackward, count, op: Some(op) };
                return;
            }
            'd' | 'c' | 'y' | '>' | '<' => return,
            _ => {}
        }
        let Some(motion) = self.motion(text, Key::Escape, Some(ch), count.max(1) as usize) else { return };
        let (mut start, mut end) = if motion.target >= self.cursor { (self.cursor, motion.target) } else { (motion.target, self.cursor) };
        if motion.inclusive {
            end = (end + char_len(text, end)).min(text.len());
        }
        if motion.linewise {
            start = line_start(text, start);
            let last = line_bounds(text, end.saturating_sub(1).max(start)).1;
            end = if last < text.len() { last + 1 } else { last };
        } else if end > self.cursor && line_of(text, end) != line_of(text, self.cursor) {
            // As Vim does: a word motion that would cross a line break stops at the end of this
            // line, so `dw` on the last word does not join the next line.
            end = line_bounds(text, self.cursor).1;
        }
        self.apply(text, op, start..end, motion.linewise, 1);
    }

    /// A motion by key or character, without moving the cursor.
    fn motion(&mut self, text: &str, key: Key, ch: Option<char>, count: usize) -> Option<Motion> {
        let count = count.max(1);
        let cursor = self.cursor;
        match key {
            Key::ArrowLeft => return Some(Motion { target: self.step_left(text, cursor, count), linewise: false, inclusive: false }),
            Key::ArrowRight => return Some(Motion { target: self.step_right(text, cursor, count), linewise: false, inclusive: false }),
            Key::ArrowDown => return Some(Motion { target: self.line_step(text, cursor, count, 1)?, linewise: true, inclusive: false }),
            Key::ArrowUp => return Some(Motion { target: self.line_step(text, cursor, count, -1)?, linewise: true, inclusive: false }),
            Key::Home => return Some(Motion { target: line_start(text, cursor), linewise: false, inclusive: false }),
            Key::End => return Some(Motion { target: last_char(text, line_bounds(text, cursor).1), linewise: false, inclusive: true }),
            Key::PageDown => return Some(Motion { target: self.scroll_target(text, cursor, 12, false), linewise: true, inclusive: false }),
            Key::PageUp => return Some(Motion { target: self.scroll_target(text, cursor, 12, true), linewise: true, inclusive: false }),
            _ => {}
        }
        match ch? {
            'h' => Some(Motion { target: self.step_left(text, cursor, count), linewise: false, inclusive: false }),
            'l' => Some(Motion { target: self.step_right(text, cursor, count), linewise: false, inclusive: false }),
            'j' => Some(Motion { target: self.line_step(text, cursor, count, 1)?, linewise: true, inclusive: false }),
            'k' => Some(Motion { target: self.line_step(text, cursor, count, -1)?, linewise: true, inclusive: false }),
            'w' => Some(Motion { target: repeat(cursor, count, |at| word_forward(text, at, false)), linewise: false, inclusive: false }),
            'W' => Some(Motion { target: repeat(cursor, count, |at| word_forward(text, at, true)), linewise: false, inclusive: false }),
            'b' => Some(Motion { target: repeat(cursor, count, |at| word_back(text, at, false)), linewise: false, inclusive: false }),
            'B' => Some(Motion { target: repeat(cursor, count, |at| word_back(text, at, true)), linewise: false, inclusive: false }),
            'e' => Some(Motion { target: repeat(cursor, count, |at| word_end(text, at, false)), linewise: false, inclusive: true }),
            'E' => Some(Motion { target: repeat(cursor, count, |at| word_end(text, at, true)), linewise: false, inclusive: true }),
            '0' => Some(Motion { target: line_start(text, cursor), linewise: false, inclusive: false }),
            '^' => Some(Motion { target: first_nonblank(text, line_start(text, cursor)), linewise: false, inclusive: false }),
            '$' => {
                let mut end = line_bounds(text, cursor).1;
                for _ in 1..count {
                    if end >= text.len() {
                        break;
                    }
                    end = line_bounds(text, end + 1).1;
                }
                Some(Motion { target: last_char(text, end), linewise: false, inclusive: true })
            }
            '|' => {
                let (start, end) = line_bounds(text, cursor);
                Some(Motion { target: nth_char(text, start, count.saturating_sub(1), end), linewise: false, inclusive: false })
            }
            '{' => Some(Motion { target: paragraph(text, cursor, count, true), linewise: false, inclusive: false }),
            '}' => Some(Motion { target: paragraph(text, cursor, count, false), linewise: false, inclusive: false }),
            '+' => {
                let target = self.line_step(text, cursor, count, 1)?;
                Some(Motion { target: first_nonblank(text, line_start(text, target)), linewise: true, inclusive: false })
            }
            '-' => {
                let target = self.line_step(text, cursor, count, -1)?;
                Some(Motion { target: first_nonblank(text, line_start(text, target)), linewise: true, inclusive: false })
            }
            '_' => {
                let target = self.line_step(text, cursor, count.saturating_sub(1), 1).unwrap_or(cursor);
                Some(Motion { target: first_nonblank(text, line_start(text, target)), linewise: true, inclusive: false })
            }
            _ => None,
        }
    }

    fn step_left(&self, text: &str, cursor: usize, count: usize) -> usize {
        repeat(cursor, count, |at| prev_char(text, at))
    }

    fn step_right(&self, text: &str, cursor: usize, count: usize) -> usize {
        let mut at = cursor;
        for _ in 0..count {
            let end = line_bounds(text, at).1;
            if at >= end {
                break;
            }
            at = next_char(text, at);
        }
        at
    }

    /// `j`/`k`: keep the column where it was rather than where the last line allowed.
    fn line_step(&mut self, text: &str, cursor: usize, count: usize, direction: isize) -> Option<usize> {
        let column = self.desired_column.unwrap_or_else(|| char_column(text, line_start(text, cursor), cursor));
        let mut at = cursor;
        for _ in 0..count {
            at = if direction > 0 { next_line(text, at)? } else { prev_line(text, at)? };
        }
        let (start, end) = line_bounds(text, at);
        self.desired_column = Some(column);
        Some(nth_char(text, start, column, end))
    }

    fn scroll_target(&self, text: &str, cursor: usize, lines: usize, up: bool) -> usize {
        let mut at = cursor;
        for _ in 0..lines {
            let step = if up { prev_line(text, at) } else { next_line(text, at) };
            match step {
                Some(next) => at = next,
                None => break,
            }
        }
        at
    }

    fn scroll_lines(&mut self, text: &str, lines: usize, up: bool) {
        let target = self.scroll_target(text, self.cursor, lines, up);
        self.desired_column = None;
        self.move_to(text, target);
    }

    fn step_line(&mut self, text: &str, direction: isize) {
        if let Some(target) = self.line_step(text, self.cursor, 1, direction) {
            self.move_to(text, target);
        }
    }

    fn goto_line(&mut self, text: &str, line: usize) {
        let (start, _) = line_range(text, line.min(line_count(text).saturating_sub(1)));
        self.desired_column = None;
        self.move_to(text, first_nonblank(text, start));
    }

    fn move_to(&mut self, text: &str, target: usize) {
        let target = floor_boundary(text, target.min(text.len()));
        if target != self.cursor {
            self.cursor = target;
            self.moved = true;
        }
        self.normalize(text);
    }

    /// In Normal and Visual mode the cursor sits on a character; on an empty line it sits at
    /// the line's start.
    fn normalize(&mut self, text: &str) {
        if self.mode == Mode::Insert {
            self.cursor = floor_boundary(text, self.cursor.min(text.len()));
            return;
        }
        let (start, end) = line_bounds(text, self.cursor);
        if self.cursor >= end && end > start {
            self.cursor = last_char(text, end);
        }
    }

    // ------------------------------------------------------------------ editing

    fn push_undo(&mut self, text: &str) {
        self.redo.clear();
        self.undo.push(Snapshot { text: text.to_string(), cursor: self.cursor });
        if self.undo.len() > 400 {
            self.undo.remove(0);
        }
    }

    fn enter_insert(&mut self, text: &mut String, at: usize) {
        self.push_undo(text);
        self.cursor = floor_boundary(text, at.min(text.len()));
        self.mode = Mode::Insert;
        self.anchor = None;
        self.desired_column = None;
        self.moved = true;
    }

    fn leave_insert(&mut self, text: &str) {
        self.mode = Mode::Normal;
        self.cursor = floor_boundary(text, self.cursor.min(text.len()));
        if self.cursor > 0 {
            self.cursor = prev_char(text, self.cursor);
        }
        self.anchor = None;
        self.moved = true;
        self.normalize(text);
    }

    fn undo(&mut self, text: &mut String) {
        let Some(snapshot) = self.undo.pop() else {
            self.message = "already at the oldest change".to_string();
            return;
        };
        self.redo.push(Snapshot { text: text.clone(), cursor: self.cursor });
        *text = snapshot.text;
        self.cursor = floor_boundary(text, snapshot.cursor.min(text.len()));
        self.changed = true;
        self.moved = true;
        self.normalize(text);
        self.message = "undo".to_string();
    }

    fn redo(&mut self, text: &mut String) {
        let Some(snapshot) = self.redo.pop() else {
            self.message = "already at the newest change".to_string();
            return;
        };
        self.undo.push(Snapshot { text: text.clone(), cursor: self.cursor });
        *text = snapshot.text;
        self.cursor = floor_boundary(text, snapshot.cursor.min(text.len()));
        self.changed = true;
        self.moved = true;
        self.normalize(text);
        self.message = "redo".to_string();
    }

    fn delete_chars(&mut self, text: &mut String, count: usize, before: bool) {
        let (start, end) = line_bounds(text, self.cursor);
        let range = if before {
            repeat(self.cursor, count, |at| prev_char(text, at)).max(start)..self.cursor
        } else {
            let to = repeat(self.cursor, count, |at| if at < end { next_char(text, at) } else { at });
            self.cursor..to
        };
        if range.is_empty() {
            return;
        }
        self.apply(text, Op::Delete, range, false, 1);
    }

    fn replace_chars(&mut self, text: &mut String, replacement: char, count: usize) {
        let (_, end) = line_bounds(text, self.cursor);
        let to = repeat(self.cursor, count, |at| if at < end { next_char(text, at) } else { at });
        if to <= self.cursor {
            return;
        }
        self.push_undo(text);
        let replaced: String = std::iter::repeat_n(replacement, text[self.cursor..to].chars().count()).collect();
        text.replace_range(self.cursor..to, &replaced);
        self.changed = true;
        self.moved = true;
    }

    fn toggle_case(&mut self, text: &mut String, count: usize) {
        let (_, end) = line_bounds(text, self.cursor);
        let to = repeat(self.cursor, count, |at| if at < end { next_char(text, at) } else { at });
        if to <= self.cursor {
            return;
        }
        self.push_undo(text);
        let swapped: String = text[self.cursor..to].chars().map(swap_case).collect();
        text.replace_range(self.cursor..to, &swapped);
        self.changed = true;
        self.moved = true;
    }

    fn join(&mut self, text: &mut String, count: usize) {
        let mut changed = false;
        for _ in 0..count.max(1) {
            let (_, end) = line_bounds(text, self.cursor);
            if end >= text.len() {
                break;
            }
            if !changed {
                self.push_undo(text);
                changed = true;
            }
            // Replace the line break and the next line's indentation with one space.
            let next_end = line_bounds(text, end + 1).1;
            let space = if end > 0 && text[..end].ends_with([' ', '\t']) { "" } else { " " };
            text.replace_range(end..next_end, space);
            self.cursor = last_char(text, end);
        }
        if changed {
            self.changed = true;
            self.moved = true;
        }
    }

    fn open_line(&mut self, text: &mut String, below: bool) {
        let (start, end) = line_bounds(text, self.cursor);
        let indent: String = text[start..first_nonblank(text, start)].to_string();
        self.push_undo(text);
        if below {
            if end < text.len() {
                text.insert(end + 1, '\n');
                text.insert_str(end + 1, &indent);
                self.cursor = end + 1 + indent.len();
            } else {
                text.push('\n');
                text.push_str(&indent);
                self.cursor = text.len();
            }
        } else {
            let insert_at = start;
            text.insert_str(insert_at, &indent);
            text.insert(insert_at + indent.len(), '\n');
            self.cursor = insert_at + indent.len();
        }
        self.mode = Mode::Insert;
        self.changed = true;
        self.moved = true;
        self.anchor = None;
    }

    fn paste(&mut self, text: &mut String, after: bool, count: usize) {
        if self.register.text.is_empty() {
            self.message = "the register is empty".to_string();
            return;
        }
        self.push_undo(text);
        let content = self.register.text.clone();
        if self.register.linewise {
            let (line_start_of_cursor, line_end_of_cursor) = line_bounds(text, self.cursor);
            let mut block = String::new();
            for _ in 0..count.max(1) {
                block.push_str(&content);
                if !block.ends_with('\n') {
                    block.push('\n');
                }
            }
            if after {
                if line_end_of_cursor < text.len() {
                    text.insert_str(line_end_of_cursor + 1, &block);
                    self.cursor = first_nonblank(text, line_end_of_cursor + 1);
                } else {
                    text.push('\n');
                    let at = text.len();
                    text.push_str(&block);
                    self.cursor = first_nonblank(text, at);
                }
            } else {
                text.insert_str(line_start_of_cursor, &block);
                self.cursor = first_nonblank(text, line_start_of_cursor);
            }
        } else {
            let base = if after && char_at(text, self.cursor).is_some() { next_char(text, self.cursor) } else { self.cursor };
            let block = content.repeat(count.max(1));
            text.insert_str(base, &block);
            self.cursor = prev_char(text, base + block.len());
        }
        self.changed = true;
        self.moved = true;
        self.normalize(text);
    }

    fn find(&mut self, text: &mut String, kind: FindKind, target: char, count: usize, op: Option<Op>) {
        let mut position = self.cursor;
        let mut found = None;
        for _ in 0..count {
            match find_char(text, position, kind, target) {
                Some(next) => {
                    position = next;
                    found = Some(next);
                }
                None => break,
            }
        }
        let Some(found) = found else {
            self.message = format!("{target} not found on this line");
            return;
        };
        self.last_find = Some((kind, target));
        match op {
            Some(operator) => {
                let (start, end) = if found >= self.cursor { (self.cursor, found) } else { (found, self.cursor) };
                let inclusive = !matches!(kind, FindKind::TillForward | FindKind::TillBackward);
                let end = if inclusive { (end + char_len(text, end)).min(text.len()) } else { end };
                self.apply(text, operator, start..end, false, 1);
            }
            None => {
                self.desired_column = None;
                self.move_to(text, found);
            }
        }
    }

    fn search_next(&mut self, text: &mut String, count: usize, backward: bool) {
        let Some(pattern) = self.search.clone() else {
            self.message = "no previous search".to_string();
            return;
        };
        let mut found = None;
        let mut cursor = self.cursor;
        for _ in 0..count.max(1) {
            let hit = if backward {
                text[..cursor].rfind(&pattern).or_else(|| text.rfind(&pattern))
            } else {
                let from = next_char(text, cursor);
                text[from.min(text.len())..].find(&pattern).map(|offset| from + offset).or_else(|| text.find(&pattern))
            };
            match hit {
                Some(index) => {
                    found = Some(index);
                    cursor = index;
                }
                None => break,
            }
        }
        match found {
            Some(index) => {
                self.desired_column = None;
                self.cursor = floor_boundary(text, index);
                self.moved = true;
                self.message = format!("/{pattern}");
                self.normalize(text);
            }
            None => self.message = format!("pattern not found: {pattern}"),
        }
    }

    fn open_prompt(&mut self, kind: PromptKind) {
        self.mode = Mode::Command;
        self.prompt = Some((kind, String::new()));
        self.count = 0;
        self.pending = Pending::None;
    }

    fn prompt_key(&mut self, text: &mut String, key: Key, ctrl: bool) {
        match key {
            Key::Escape => {
                self.prompt = None;
                self.mode = Mode::Normal;
            }
            Key::Enter => {
                let Some((kind, line)) = self.prompt.take() else { return };
                self.mode = Mode::Normal;
                match kind {
                    PromptKind::Command => self.run_command(text, &line),
                    PromptKind::Search => {
                        if !line.is_empty() {
                            self.search = Some(line);
                            self.search_next(text, 1, false);
                        }
                    }
                }
            }
            Key::Backspace => {
                if let Some((_, line)) = &mut self.prompt {
                    line.pop();
                }
            }
            _ if ctrl => {}
            _ => {}
        }
    }

    fn run_command(&mut self, text: &mut String, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let (command, argument) = match line.split_once(char::is_whitespace) {
            Some((command, argument)) => (command, argument.trim()),
            None => (line, ""),
        };
        match command {
            "w" => {
                self.request = Some(Request::Save);
                self.message = "writing…".to_string();
            }
            "wq" | "x" => {
                self.request = Some(Request::Save);
                self.message = "writing… (:q closes the tab)".to_string();
            }
            "q" | "q!" => self.request = Some(Request::Close),
            "e" | "e!" => {
                if argument.is_empty() {
                    self.message = ":e needs a file".to_string();
                } else {
                    self.request = Some(Request::Open(argument.to_string()));
                }
            }
            "comp" | "compile" => self.request = Some(Request::Compile),
            _ => {
                if let Ok(number) = command.parse::<usize>() {
                    self.goto_line(text, number.saturating_sub(1));
                } else {
                    self.message = format!("not a command: :{command}");
                }
            }
        }
    }

    fn selection_op(&mut self, text: &mut String, op: Op, count: usize) {
        let Some(range) = self.selection(text) else { return };
        let linewise = matches!(self.mode, Mode::Visual { line: true });
        let range = if linewise { linewise_range(text, range) } else { range };
        let start = range.start;
        let change = op == Op::Change;
        self.apply(text, op, range, linewise, count);
        if change {
            self.cursor = floor_boundary(text, start);
            self.mode = Mode::Insert;
            self.anchor = None;
        } else {
            self.cursor = floor_boundary(text, start);
            self.mode = Mode::Normal;
            self.anchor = None;
            self.normalize(text);
        }
    }

    /// Apply an operator to a range.
    fn apply(&mut self, text: &mut String, op: Op, range: Range<usize>, linewise: bool, count: usize) {
        let start = floor_boundary(text, range.start.min(text.len()));
        let end = ceil_boundary(text, range.end.min(text.len()));
        if start >= end && !matches!(op, Op::Indent | Op::Dedent) {
            return;
        }
        match op {
            Op::Yank => {
                self.register = Register { text: text[start..end].to_string(), linewise };
                let lines = line_of(text, end.saturating_sub(1)).saturating_sub(line_of(text, start)) + 1;
                self.message = if linewise {
                    format!("{lines} line{} yanked", plural(lines))
                } else {
                    format!("{} character{} yanked", end - start, plural(end - start))
                };
                self.cursor = start;
                self.moved = true;
            }
            Op::Indent | Op::Dedent => {
                self.push_undo(text);
                // `end` may sit on the newline that ends the last line in the range.
                let last = line_bounds(text, end.saturating_sub(1).max(start)).1;
                let mut line = line_start(text, start);
                let mut shift = 0isize;
                loop {
                    if op == Op::Indent {
                        let spaces = 2 * count.max(1);
                        text.insert_str(line, &" ".repeat(spaces));
                        shift += spaces as isize;
                        line += spaces;
                    } else {
                        let strip = text[line..].chars().take_while(|value| *value == ' ').take(2).count();
                        if strip > 0 {
                            text.replace_range(line..line + strip, "");
                            shift -= strip as isize;
                        }
                    }
                    match next_line(text, line) {
                        Some(next) if next <= last => line = next,
                        _ => break,
                    }
                }
                if self.cursor >= start {
                    self.cursor = floor_boundary(text, (self.cursor as isize + shift).max(0) as usize);
                }
                self.changed = true;
                self.moved = true;
            }
            Op::ToggleCase => {
                self.push_undo(text);
                let swapped: String = text[start..end].chars().map(swap_case).collect();
                text.replace_range(start..end, &swapped);
                self.cursor = start;
                self.changed = true;
                self.moved = true;
            }
            Op::Delete | Op::Change => {
                self.push_undo(text);
                self.register = Register { text: text[start..end].to_string(), linewise };
                text.replace_range(start..end, "");
                self.changed = true;
                self.cursor = floor_boundary(text, start);
                self.moved = true;
                if op == Op::Change {
                    self.mode = Mode::Insert;
                    self.anchor = None;
                    self.normalize(text);
                } else {
                    self.normalize(text);
                }
            }
        }
    }
}

impl Default for Vim {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------- helpers

fn repeat(mut value: usize, count: usize, mut step: impl FnMut(usize) -> usize) -> usize {
    for _ in 0..count {
        let next = step(value);
        if next == value {
            break;
        }
        value = next;
    }
    value
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn swap_case(value: char) -> char {
    if value.is_uppercase() {
        value.to_lowercase().next().unwrap_or(value)
    } else {
        value.to_uppercase().next().unwrap_or(value)
    }
}

fn op_key(op: Op) -> &'static str {
    match op {
        Op::Delete => "d",
        Op::Change => "c",
        Op::Yank => "y",
        Op::Indent => ">",
        Op::Dedent => "<",
        Op::ToggleCase => "~",
    }
}

fn op_key_char(op: Op) -> char {
    match op {
        Op::Delete => 'd',
        Op::Change => 'c',
        Op::Yank => 'y',
        Op::Indent => '>',
        Op::Dedent => '<',
        Op::ToggleCase => '~',
    }
}

fn find_key(kind: FindKind) -> &'static str {
    match kind {
        FindKind::Forward => "f",
        FindKind::Backward => "F",
        FindKind::TillForward => "t",
        FindKind::TillBackward => "T",
    }
}

fn flip_find(kind: FindKind) -> FindKind {
    match kind {
        FindKind::Forward => FindKind::Backward,
        FindKind::Backward => FindKind::Forward,
        FindKind::TillForward => FindKind::TillBackward,
        FindKind::TillBackward => FindKind::TillForward,
    }
}

/// The character a key stands for, with Shift applied to letters and digits.
fn char_of(key: Key, shift: bool) -> Option<char> {
    let value = match key {
        Key::Space => ' ',
        Key::Colon => ':',
        Key::Comma => ',',
        Key::Backslash => '\\',
        Key::Slash => '/',
        Key::Pipe => '|',
        Key::Questionmark => '?',
        Key::Exclamationmark => '!',
        Key::OpenBracket => '[',
        Key::CloseBracket => ']',
        Key::OpenCurlyBracket => '{',
        Key::CloseCurlyBracket => '}',
        Key::Backtick => '`',
        Key::Minus => '-',
        Key::Period => '.',
        Key::Plus => '+',
        Key::Equals => '=',
        Key::Semicolon => ';',
        Key::Quote => '\'',
        Key::Num0 => '0',
        Key::Num1 => '1',
        Key::Num2 => '2',
        Key::Num3 => '3',
        Key::Num4 => '4',
        Key::Num5 => '5',
        Key::Num6 => '6',
        Key::Num7 => '7',
        Key::Num8 => '8',
        Key::Num9 => '9',
        other => {
            let lower = letter(other)?.to_ascii_lowercase();
            return Some(if shift { lower.to_ascii_uppercase() } else { lower });
        }
    };
    // egui has dedicated variants for some shifted symbols (`Colon`, `Plus`, `OpenCurlyBracket`)
    // and plain ones for the rest; apply Shift so `>`, `_`, `"` … arrive as themselves.
    Some(if shift { shifted(value) } else { value })
}

fn shifted(value: char) -> char {
    match value {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        other => other,
    }
}

fn letter(key: Key) -> Option<char> {
    Some(match key {
        Key::A => 'A',
        Key::B => 'B',
        Key::C => 'C',
        Key::D => 'D',
        Key::E => 'E',
        Key::F => 'F',
        Key::G => 'G',
        Key::H => 'H',
        Key::I => 'I',
        Key::J => 'J',
        Key::K => 'K',
        Key::L => 'L',
        Key::M => 'M',
        Key::N => 'N',
        Key::O => 'O',
        Key::P => 'P',
        Key::Q => 'Q',
        Key::R => 'R',
        Key::S => 'S',
        Key::T => 'T',
        Key::U => 'U',
        Key::V => 'V',
        Key::W => 'W',
        Key::X => 'X',
        Key::Y => 'Y',
        Key::Z => 'Z',
        _ => return None,
    })
}

pub fn char_len(text: &str, position: usize) -> usize {
    text[position.min(text.len())..].chars().next().map(char::len_utf8).unwrap_or(0)
}

fn char_at(text: &str, position: usize) -> Option<char> {
    text[position.min(text.len())..].chars().next()
}

fn next_char(text: &str, position: usize) -> usize {
    (position + char_len(text, position)).min(text.len())
}

fn prev_char(text: &str, position: usize) -> usize {
    if position == 0 {
        return 0;
    }
    text[..position.min(text.len())].char_indices().next_back().map(|(index, _)| index).unwrap_or(0)
}

fn floor_boundary(text: &str, mut position: usize) -> usize {
    position = position.min(text.len());
    while position > 0 && !text.is_char_boundary(position) {
        position -= 1;
    }
    position
}

fn ceil_boundary(text: &str, mut position: usize) -> usize {
    position = position.min(text.len());
    while position < text.len() && !text.is_char_boundary(position) {
        position += 1;
    }
    position
}

fn line_start(text: &str, position: usize) -> usize {
    text[..position.min(text.len())].rfind('\n').map(|index| index + 1).unwrap_or(0)
}

fn line_end(text: &str, position: usize) -> usize {
    let position = position.min(text.len());
    text[position..].find('\n').map(|index| position + index).unwrap_or(text.len())
}

/// The line's text without its newline.
fn line_bounds(text: &str, position: usize) -> (usize, usize) {
    let start = line_start(text, position);
    (start, line_end(text, position.min(text.len())))
}

fn line_of(text: &str, position: usize) -> usize {
    text[..position.min(text.len())].matches('\n').count()
}

fn line_count(text: &str) -> usize {
    text.matches('\n').count() + 1
}

fn line_range(text: &str, index: usize) -> (usize, usize) {
    let mut start = 0;
    for _ in 0..index {
        match text[start..].find('\n') {
            Some(offset) => start += offset + 1,
            None => return (text.len(), text.len()),
        }
    }
    (start, line_end(text, start))
}

fn next_line(text: &str, position: usize) -> Option<usize> {
    let end = line_end(text, position);
    (end < text.len()).then_some(end + 1)
}

fn prev_line(text: &str, position: usize) -> Option<usize> {
    let start = line_start(text, position);
    (start > 0).then(|| line_start(text, start.saturating_sub(1)))
}

fn last_char(text: &str, end: usize) -> usize {
    let start = line_start(text, end);
    if end <= start {
        return start;
    }
    prev_char(text, end)
}

fn first_nonblank(text: &str, start: usize) -> usize {
    let end = line_end(text, start);
    text[start..end].char_indices().find(|(_, value)| !value.is_whitespace()).map(|(offset, _)| start + offset).unwrap_or(start)
}

fn char_column(text: &str, line: usize, position: usize) -> usize {
    text[line..position.min(text.len())].chars().count()
}

fn nth_char(text: &str, line_begin: usize, column: usize, line_finish: usize) -> usize {
    let mut position = line_begin;
    for _ in 0..column {
        if position >= line_finish {
            break;
        }
        position = next_char(text, position);
    }
    floor_boundary(text, position.min(line_finish))
}

/// A Visual range widened to whole lines (and to the newline that ends the last one).
fn linewise_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = line_start(text, range.start);
    let last = line_bounds(text, range.end.saturating_sub(1).max(range.start)).1;
    let end = if last < text.len() { last + 1 } else { last };
    start..end
}

fn paragraph(text: &str, cursor: usize, count: usize, backward: bool) -> usize {
    let mut target = cursor;
    for _ in 0..count.max(1) {
        if backward {
            let mut line = line_start(text, target);
            loop {
                let Some(previous) = prev_line(text, line) else {
                    target = 0;
                    break;
                };
                let (start, end) = line_bounds(text, previous);
                if text[start..end].trim().is_empty() {
                    target = start;
                    break;
                }
                line = start;
            }
        } else {
            let mut line = line_end(text, target);
            loop {
                if line >= text.len() {
                    target = text.len();
                    break;
                }
                let next = next_line(text, line).unwrap_or(text.len());
                let (start, end) = line_bounds(text, next);
                if text[start..end].trim().is_empty() {
                    target = start;
                    break;
                }
                line = next;
            }
        }
        if target >= text.len() {
            break;
        }
    }
    floor_boundary(text, target)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class_of(value: char, big: bool) -> Class {
    if value.is_whitespace() {
        Class::Space
    } else if big || value.is_alphanumeric() || value == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn word_forward(text: &str, position: usize, big: bool) -> usize {
    let mut index = position;
    let Some(first) = char_at(text, index) else { return text.len() };
    let class = class_of(first, big);
    while let Some(value) = char_at(text, index) {
        if class_of(value, big) != class {
            break;
        }
        index = next_char(text, index);
    }
    while let Some(value) = char_at(text, index) {
        if class_of(value, big) != Class::Space {
            break;
        }
        index = next_char(text, index);
    }
    index
}

fn word_back(text: &str, position: usize, big: bool) -> usize {
    if position == 0 {
        return 0;
    }
    let mut index = prev_char(text, position);
    while index > 0 {
        let value = char_at(text, index);
        if value.map(|value| class_of(value, big)) != Some(Class::Space) {
            break;
        }
        index = prev_char(text, index);
    }
    let class = char_at(text, index).map(|value| class_of(value, big));
    while index > 0 {
        let previous = prev_char(text, index);
        match char_at(text, previous) {
            Some(value) if Some(class_of(value, big)) == class => index = previous,
            _ => break,
        }
    }
    index
}

fn word_end(text: &str, position: usize, big: bool) -> usize {
    let mut index = position;
    let Some(first) = char_at(text, index) else { return text.len() };
    let at_end = match char_at(text, next_char(text, index)) {
        Some(value) => class_of(value, big) != class_of(first, big),
        None => true,
    };
    if at_end {
        index = next_char(text, index);
    }
    while let Some(value) = char_at(text, index) {
        if class_of(value, big) != Class::Space {
            break;
        }
        index = next_char(text, index);
    }
    let Some(first) = char_at(text, index) else { return text.len() };
    let class = class_of(first, big);
    loop {
        let next = next_char(text, index);
        match char_at(text, next) {
            Some(value) if class_of(value, big) == class => index = next,
            _ => break,
        }
    }
    index
}

fn find_char(text: &str, position: usize, kind: FindKind, target: char) -> Option<usize> {
    let (start, end) = line_bounds(text, position);
    match kind {
        FindKind::Forward | FindKind::TillForward => {
            let from = next_char(text, position);
            if from >= end {
                return None;
            }
            let hit = from + text[from..end].find(target)?;
            Some(if kind == FindKind::TillForward { prev_char(text, hit) } else { hit })
        }
        FindKind::Backward | FindKind::TillBackward => {
            if position <= start {
                return None;
            }
            let hit = start + text[start..position].rfind(target)?;
            Some(if kind == FindKind::TillBackward { next_char(text, hit) } else { hit })
        }
    }
}

/// A text object: the range an operator covers for `i`/`a` plus an object character.
fn text_object(text: &str, cursor: usize, object: char, around: bool) -> Option<(Range<usize>, bool)> {
    let (line_begin, line_finish) = line_bounds(text, cursor);
    match object {
        'w' | 'W' => {
            let big = object == 'W';
            let class = char_at(text, cursor).map(|value| class_of(value, big)).unwrap_or(Class::Space);
            let mut start = cursor;
            while start > line_begin {
                let previous = prev_char(text, start);
                if class_of(char_at(text, previous)?, big) != class {
                    break;
                }
                start = previous;
            }
            let mut end = cursor;
            while end < line_finish {
                if class_of(char_at(text, end)?, big) != class {
                    break;
                }
                end = next_char(text, end);
            }
            if around && class != Class::Space {
                let mut after = end;
                while after < line_finish && char_at(text, after).is_some_and(|value| value.is_whitespace()) {
                    after = next_char(text, after);
                }
                if after > end {
                    end = after;
                } else {
                    while start > line_begin && char_at(text, prev_char(text, start)).is_some_and(|value| value.is_whitespace()) {
                        start = prev_char(text, start);
                    }
                }
            }
            Some((start..end, false))
        }
        '"' | '\'' | '`' => {
            let start = text[line_begin..cursor].rfind(object).map(|offset| line_begin + offset)?;
            let end = text[cursor..line_finish].find(object).map(|offset| cursor + offset)?;
            if around {
                Some((start..end + object.len_utf8(), false))
            } else {
                Some((start + object.len_utf8()..end, false))
            }
        }
        '(' | '[' | '{' => {
            let close = closing(object);
            let start = text[..cursor + char_len(text, cursor)].rfind(object)?;
            let end = text[cursor..].find(close).map(|offset| cursor + offset)?;
            if around {
                Some((start..end + close.len_utf8(), false))
            } else {
                Some((start + object.len_utf8()..end, false))
            }
        }
        ')' | ']' | '}' => {
            let open = opening(object);
            let start = text[..cursor + char_len(text, cursor)].rfind(open)?;
            let end = text[cursor..].find(object).map(|offset| cursor + offset)?;
            if around {
                Some((start..end + object.len_utf8(), false))
            } else {
                Some((start + open.len_utf8()..end, false))
            }
        }
        _ => None,
    }
}

fn closing(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        _ => '}',
    }
}

fn opening(close: char) -> char {
    match close {
        ')' => '(',
        ']' => '[',
        _ => '{',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_move_over_runs_and_whitespace() {
        let text = "let x = 12; // done\nnext line";
        assert_eq!(&text[word_forward(text, 0, false)..], "x = 12; // done\nnext line");
        assert_eq!(&text[word_end(text, 0, false)..], "t x = 12; // done\nnext line");
        assert_eq!(&text[word_back(text, 5, false)..], "x = 12; // done\nnext line");
        assert_eq!(&text[word_end(text, 24, false)..], "e");
    }

    #[test]
    fn line_geometry() {
        let text = "one\ntwo\n\nfour";
        assert_eq!(line_range(text, 0), (0, 3));
        assert_eq!(line_range(text, 2), (8, 8));
        assert_eq!(line_range(text, 3), (9, 13));
        assert_eq!(line_count(text), 4);
        assert_eq!(last_char(text, 3), 2);
        assert_eq!(first_nonblank(text, 9), 9);
    }

    #[test]
    fn inclusive_and_linewise_ranges() {
        let text = "#set page(a)\n= Heading\n\nBody text here.\n";
        let end = last_char(text, line_bounds(text, 0).1);
        assert_eq!(&text[0..end + 1], "#set page(a)");
        // The second line, with the newline that ends it, is what `yy` covers.
        let (start, finish) = line_range(text, 1);
        assert_eq!(&text[start..finish + 1], "= Heading\n");
    }

    #[test]
    fn text_objects_find_the_word_and_the_quotes() {
        let text = "text(font: \"Libertinus\")";
        let inside = text_object(text, 5, 'w', false).unwrap();
        assert_eq!(&text[inside.0], "font");
        let inner = text_object(text, 16, '"', false).unwrap();
        assert_eq!(&text[inner.0], "Libertinus");
        let around = text_object(text, 16, '"', true).unwrap();
        assert_eq!(&text[around.0], "\"Libertinus\"");
        let brackets = text_object(text, 5, '(', false).unwrap();
        assert_eq!(&text[brackets.0], "font: \"Libertinus\"");
    }

    #[test]
    fn visual_selection_grows_with_motion() {
        let mut vim = Vim::new();
        let text = "hello world\nsecond line";
        vim.set_cursor(text, 0);
        vim.mode = Mode::Visual { line: false };
        vim.anchor = Some(0);
        vim.cursor = 4;
        assert_eq!(vim.selection(text), Some(0..5));
        vim.mode = Mode::Visual { line: true };
        assert_eq!(vim.selection(text), Some(0..12));
    }

    #[test]
    fn the_double_operator_touches_only_the_cursors_line() {
        let mut vim = Vim::new();
        let mut text = "one\ntwo\nthree\n".to_string();
        vim.set_cursor(&text, 0);
        // `>>` — `>` is shift+period in egui's key set.
        vim.key(&mut text, Key::Period, true, false);
        vim.key(&mut text, Key::Period, true, false);
        assert_eq!(text, "  one\ntwo\nthree\n");
        // and `<<` takes it back
        vim.key(&mut text, Key::Comma, true, false);
        vim.key(&mut text, Key::Comma, true, false);
        assert_eq!(text, "one\ntwo\nthree\n");
    }

    #[test]
    fn dd_then_p_round_trips_a_line() {
        let mut vim = Vim::new();
        let mut text = "one\ntwo\nthree\n".to_string();
        vim.set_cursor(&text, 4);
        vim.key(&mut text, Key::D, false, false);
        vim.key(&mut text, Key::D, false, false);
        assert_eq!(text, "one\nthree\n");
        vim.key(&mut text, Key::P, true, false); // P: put the line back above
        assert_eq!(text, "one\ntwo\nthree\n");
    }

    #[test]
    fn ciw_changes_the_word_under_the_cursor() {
        let mut vim = Vim::new();
        let mut text = "Click any word here".to_string();
        vim.set_cursor(&text, 7);
        vim.key(&mut text, Key::C, false, false);
        vim.key(&mut text, Key::I, false, false);
        vim.key(&mut text, Key::W, false, false);
        assert_eq!(vim.mode, Mode::Insert);
        assert_eq!(text, "Click  word here");
    }

    #[test]
    fn undo_restores_the_buffer_and_the_cursor() {
        let mut vim = Vim::new();
        let mut text = "alpha beta".to_string();
        vim.set_cursor(&text, 0);
        vim.enter_insert(&mut text, 0);
        text.insert_str(0, "new ");
        vim.cursor = 4;
        vim.leave_insert(&text);
        assert_eq!(text, "new alpha beta");
        vim.undo(&mut text);
        assert_eq!(text, "alpha beta");
        vim.redo(&mut text);
        assert_eq!(text, "new alpha beta");
    }
}
