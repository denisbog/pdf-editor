//! The egui side of the shared vim layer: take the frame's input events, keep what Insert mode
//! must pass through to the `TextEdit`, and hand the rest to `pdf_editor_core::vim` as
//! translated key presses.
//!
//! The modal state machine itself lives in the core crate so the iced front-end can share it;
//! only this toolkit glue is egui-specific.

use egui::Key as EguiKey;
use pdf_editor_core::vim::{Key, Vim};

/// Translate an egui key into the toolkit-neutral key set.
pub fn key(key: EguiKey) -> Option<Key> {
    Some(match key {
        EguiKey::A => Key::A,
        EguiKey::B => Key::B,
        EguiKey::C => Key::C,
        EguiKey::D => Key::D,
        EguiKey::E => Key::E,
        EguiKey::F => Key::F,
        EguiKey::G => Key::G,
        EguiKey::H => Key::H,
        EguiKey::I => Key::I,
        EguiKey::J => Key::J,
        EguiKey::K => Key::K,
        EguiKey::L => Key::L,
        EguiKey::M => Key::M,
        EguiKey::N => Key::N,
        EguiKey::O => Key::O,
        EguiKey::P => Key::P,
        EguiKey::Q => Key::Q,
        EguiKey::R => Key::R,
        EguiKey::S => Key::S,
        EguiKey::T => Key::T,
        EguiKey::U => Key::U,
        EguiKey::V => Key::V,
        EguiKey::W => Key::W,
        EguiKey::X => Key::X,
        EguiKey::Y => Key::Y,
        EguiKey::Z => Key::Z,
        EguiKey::Num0 => Key::Num0,
        EguiKey::Num1 => Key::Num1,
        EguiKey::Num2 => Key::Num2,
        EguiKey::Num3 => Key::Num3,
        EguiKey::Num4 => Key::Num4,
        EguiKey::Num5 => Key::Num5,
        EguiKey::Num6 => Key::Num6,
        EguiKey::Num7 => Key::Num7,
        EguiKey::Num8 => Key::Num8,
        EguiKey::Num9 => Key::Num9,
        EguiKey::ArrowLeft => Key::ArrowLeft,
        EguiKey::ArrowRight => Key::ArrowRight,
        EguiKey::ArrowUp => Key::ArrowUp,
        EguiKey::ArrowDown => Key::ArrowDown,
        EguiKey::Home => Key::Home,
        EguiKey::End => Key::End,
        EguiKey::PageUp => Key::PageUp,
        EguiKey::PageDown => Key::PageDown,
        EguiKey::Escape => Key::Escape,
        EguiKey::Enter => Key::Enter,
        EguiKey::Backspace => Key::Backspace,
        EguiKey::Space => Key::Space,
        EguiKey::Colon => Key::Colon,
        EguiKey::Comma => Key::Comma,
        EguiKey::Period => Key::Period,
        EguiKey::Semicolon => Key::Semicolon,
        EguiKey::Quote => Key::Quote,
        EguiKey::Slash => Key::Slash,
        EguiKey::Backslash => Key::Backslash,
        EguiKey::Pipe => Key::Pipe,
        EguiKey::Questionmark => Key::Questionmark,
        EguiKey::Exclamationmark => Key::Exclamationmark,
        EguiKey::OpenBracket => Key::OpenBracket,
        EguiKey::CloseBracket => Key::CloseBracket,
        EguiKey::OpenCurlyBracket => Key::OpenCurlyBracket,
        EguiKey::CloseCurlyBracket => Key::CloseCurlyBracket,
        EguiKey::Backtick => Key::Backtick,
        EguiKey::Minus => Key::Minus,
        EguiKey::Plus => Key::Plus,
        EguiKey::Equals => Key::Equals,
        _ => return None,
    })
}

/// Take this frame's keyboard events. In Normal and Visual mode everything is ours; in
/// Insert mode only Escape, the undo keys and the command line's typing are.
pub fn handle(vim: &mut Vim, ui: &egui::Ui, text: &mut String, has_focus: bool) {
    if !has_focus {
        return;
    }
    let events = ui.input(|input| input.events.clone());
    if events.is_empty() {
        return;
    }
    let insert = vim.mode == pdf_editor_core::vim::Mode::Insert;
    let prompt = vim.prompt().is_some();
    let mut keep: Vec<egui::Event> = Vec::with_capacity(events.len());
    let mut keys: Vec<(Key, bool, bool)> = Vec::new();
    for event in events {
        if matches!(&event, egui::Event::Key { pressed: false, .. }) {
            // Key releases are harmless in every mode; the TextEdit may want them.
            keep.push(event);
            continue;
        }
        match event {
            egui::Event::Key { key: egui_key, repeat, modifiers, .. } => {
                let ctrl = modifiers.ctrl || modifiers.command;
                let Some(key) = key(egui_key) else {
                    keep.push(event);
                    continue;
                };
                if insert || prompt {
                    if key == Key::Escape || (ctrl && key == Key::OpenBracket) {
                        keys.push((key, modifiers.shift, ctrl));
                    } else if ctrl && (key == Key::Z || key == Key::Y) {
                        keys.push((key, modifiers.shift, ctrl));
                    } else if prompt && matches!(key, Key::Enter | Key::Backspace) {
                        keys.push((key, modifiers.shift, ctrl));
                    } else if !insert {
                        // A character typed on the command line arrives as Text below.
                        if key == Key::Space {
                            vim.type_text(" ");
                        }
                    } else {
                        keep.push(egui::Event::Key { key: egui_key, physical_key: None, pressed: true, repeat, modifiers });
                    }
                } else {
                    keys.push((key, modifiers.shift, ctrl));
                }
            }
            egui::Event::Text(value) => {
                if insert {
                    keep.push(egui::Event::Text(value));
                } else if prompt {
                    vim.type_text(&value);
                }
            }
            egui::Event::Paste(value) => {
                if insert {
                    keep.push(egui::Event::Paste(value));
                } else if prompt {
                    vim.type_text(&value);
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
        vim.press(text, key, shift, ctrl);
    }
}
