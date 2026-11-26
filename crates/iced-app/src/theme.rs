//! Design tokens for the iced front-end: the same PdfCraft palette the egui build uses,
//! expressed as iced [`Color`]s, plus the widget-style helpers built from them.
//!
//! The tokens carry over verbatim from `crates/egui-app/src/theme.rs`; the widgets around them
//! are iced's own (containers, buttons, rules) rather than egui's immediate-mode painting.

use iced::widget::{container, pick_list};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

use pdf_editor_core::document::ThemeKind;

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// Title/tab strip.
    pub titlebar: Color,
    /// Mode bar, panels.
    pub chrome: Color,
    pub panel: Color,
    /// Document area behind pages.
    pub pasteboard: Color,
    pub card: Color,
    pub border: Color,
    pub divider: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_faint: Color,
    pub icon: Color,
    pub hover: Color,
    pub pressed: Color,
    pub selected: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub accent_soft: Color,
    pub field: Color,
    pub page_shadow: Color,
    /// Reserved status hues; not theme-aware.
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    /// The inverse surface of a toast: dark in light mode, light in dark mode.
    pub toast_fill: Color,
    pub toast_ink: Color,
    pub radius: f32,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Light => Self {
                kind,
                titlebar: Color::from_rgb8(0xE9, 0xE9, 0xEB),
                chrome: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                panel: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                pasteboard: Color::from_rgb8(0xF1, 0xF1, 0xF3),
                card: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                border: Color::from_rgb8(0xDA, 0xDA, 0xDE),
                divider: Color::from_rgb8(0xE8, 0xE8, 0xEB),
                text: Color::from_rgb8(0x22, 0x22, 0x26),
                text_muted: Color::from_rgb8(0x5E, 0x5E, 0x66),
                text_faint: Color::from_rgb8(0x6B, 0x6B, 0x73),
                icon: Color::from_rgb8(0x44, 0x44, 0x4B),
                hover: Color::from_rgb8(0xF0, 0xF0, 0xF3),
                pressed: Color::from_rgb8(0xE4, 0xE4, 0xE9),
                selected: Color::from_rgb8(0xE6, 0xEE, 0xFD),
                accent: Color::from_rgb8(0x1B, 0x63, 0xE0),
                accent_text: Color::from_rgb8(0x17, 0x55, 0xC4),
                accent_soft: Color::from_rgb8(0xE3, 0xEC, 0xFD),
                field: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                page_shadow: Color::from_rgba(0.0, 0.0, 0.0, 34.0 / 255.0),
                success: Color::from_rgb8(0x2D, 0x9D, 0x5B),
                warning: Color::from_rgb8(0xE8, 0x8A, 0x1A),
                danger: Color::from_rgb8(0xE0, 0x3E, 0x3E),
                toast_fill: Color::from_rgb8(0x2A, 0x2A, 0x2F),
                toast_ink: Color::WHITE,
                radius: 6.0,
            },
            ThemeKind::Dark => Self {
                kind,
                titlebar: Color::from_rgb8(0x1B, 0x1B, 0x1E),
                chrome: Color::from_rgb8(0x26, 0x26, 0x2A),
                panel: Color::from_rgb8(0x26, 0x26, 0x2A),
                pasteboard: Color::from_rgb8(0x19, 0x19, 0x1C),
                card: Color::from_rgb8(0x2E, 0x2E, 0x33),
                border: Color::from_rgb8(0x3C, 0x3C, 0x43),
                divider: Color::from_rgb8(0x33, 0x33, 0x39),
                text: Color::from_rgb8(0xEC, 0xEC, 0xEF),
                text_muted: Color::from_rgb8(0xAE, 0xAE, 0xB6),
                text_faint: Color::from_rgb8(0x97, 0x97, 0x9E),
                icon: Color::from_rgb8(0xD4, 0xD4, 0xDA),
                hover: Color::from_rgb8(0x34, 0x34, 0x3A),
                pressed: Color::from_rgb8(0x3E, 0x3E, 0x45),
                selected: Color::from_rgb8(0x23, 0x3A, 0x63),
                accent: Color::from_rgb8(0x4B, 0x8B, 0xF5),
                accent_text: Color::from_rgb8(0x8C, 0xB6, 0xFA),
                accent_soft: Color::from_rgb8(0x24, 0x36, 0x57),
                field: Color::from_rgb8(0x1E, 0x1E, 0x22),
                page_shadow: Color::from_rgba(0.0, 0.0, 0.0, 120.0 / 255.0),
                success: Color::from_rgb8(0x2D, 0x9D, 0x5B),
                warning: Color::from_rgb8(0xE8, 0x8A, 0x1A),
                danger: Color::from_rgb8(0xE0, 0x3E, 0x3E),
                toast_fill: Color::from_rgb8(0xEC, 0xEC, 0xEF),
                toast_ink: Color::from_rgb8(0x22, 0x22, 0x26),
                radius: 6.0,
            },
        }
    }

    pub fn dark(&self) -> bool {
        self.kind.dark()
    }

    /// The iced palette derived from the tokens, so built-in widgets stay coherent.
    pub fn palette(&self) -> iced::theme::Palette {
        iced::theme::Palette { background: self.panel, text: self.text, primary: self.accent, success: self.success, danger: self.danger, warning: self.warning }
    }

    /// The iced [`Theme`] the window reports, so built-in widgets pick sensible defaults.
    pub fn to_theme(&self) -> Theme {
        Theme::custom(if self.dark() { "PDF Editor Dark" } else { "PDF Editor Light" }, self.palette())
    }

    pub fn toast_background(&self) -> Color {
        self.toast_fill
    }

    pub fn toast_text(&self) -> Color {
        self.toast_ink
    }

    // ------------------------------------------------------------------ styles

    pub fn filled(&self, color: Color) -> impl Fn(&Theme) -> container::Style + 'static {
        let style = container::Style { background: Some(Background::Color(color)), ..container::Style::default() };
        move |_| style
    }

    pub fn bordered(&self, fill: Color, border: Color) -> impl Fn(&Theme) -> container::Style + 'static {
        let style = container::Style { background: Some(Background::Color(fill)), border: Border { color: border, width: 1.0, radius: 0.0.into() }, ..container::Style::default() };
        move |_| style
    }

    pub fn page(&self) -> impl Fn(&Theme) -> container::Style + 'static {
        let style = container::Style {
            background: Some(Background::Color(Color::WHITE)),
            border: Border { color: self.border, width: 0.5, radius: 0.0.into() },
            shadow: Shadow { color: self.page_shadow, offset: Vector::new(0.0, 3.0), blur_radius: 14.0 },
            ..container::Style::default()
        };
        move |_| style
    }

    /// The zoom drop-down: a field with the same border as the search box, not the heavy
    /// primary-coloured box the default catalog draws.
    pub fn pick_list(&self) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style + 'static {
        let (field, hover, border, text, muted) = (self.field, self.hover, self.border, self.text, self.text_muted);
        move |_theme, status| pick_list::Style {
            text_color: text,
            placeholder_color: muted,
            handle_color: muted,
            background: Background::Color(match status {
                pick_list::Status::Hovered | pick_list::Status::Opened { .. } => hover,
                pick_list::Status::Active => field,
            }),
            border: Border { color: border, width: 1.0, radius: 6.0.into() },
        }
    }

    /// The editor's own style: background, selection, caret, gutter and squiggle colours.
    pub fn editor_style(&self) -> impl Fn(&Theme, typst_iced_editor::Status) -> typst_iced_editor::Style + 'static {
        let tokens = *self;
        move |_theme, status| {
            let syntax = if tokens.dark() { typst_iced_editor::SyntaxTheme::dark() } else { typst_iced_editor::SyntaxTheme::light() };
            let active = typst_iced_editor::Style {
                background: Background::Color(tokens.panel),
                border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 0.0.into() },
                text: tokens.text,
                selection: tokens.accent_soft,
                search_match: Color { a: 0.28, ..tokens.accent },
                current_search_match: Color { a: 0.55, ..tokens.accent },
                delimiter_match: Color { a: 0.28, ..tokens.accent },
                cursor: tokens.text,
                current_line: Some(if tokens.dark() { Color::from_rgba(1.0, 1.0, 1.0, 0.06) } else { Color::from_rgba(0.0, 0.0, 0.0, 0.055) }),
                gutter_text: Color { a: 0.85, ..tokens.text_faint },
                gutter_current_text: tokens.accent_text,
                fold_guide: Color { a: 0.35, ..tokens.text_faint },
                fold_guide_current: Color { a: 0.7, ..tokens.text_faint },
                syntax,
                diagnostic: typst_iced_editor::DiagnosticStyle {
                    error: Color::from_rgb8(0xF0, 0x7A, 0x7A),
                    warning: Color::from_rgb8(0xE0, 0xA9, 0x6C),
                    info: tokens.accent,
                    hint: tokens.text_faint,
                },
                popup: typst_iced_editor::PopupStyle {
                    background: Background::Color(tokens.card),
                    border: Border { color: tokens.border, width: 1.0, radius: 8.0.into() },
                    text: tokens.text,
                    dim_text: tokens.text_muted,
                    selection: tokens.accent_soft,
                },
                scrollbar: typst_iced_editor::ScrollbarStyle {
                    track: Color::TRANSPARENT,
                    thumb: Color { a: 0.22, ..tokens.text_faint },
                    thumb_hovered: Color { a: 0.42, ..tokens.text_faint },
                    thumb_active: Color { a: 0.62, ..tokens.text_faint },
                },
            };
            match status {
                typst_iced_editor::Status::Disabled => typst_iced_editor::Style { background: Background::Color(tokens.panel), text: tokens.text_faint, ..active },
                _ => active,
            }
        }
    }
}

// ---------------------------------------------------------------------- metrics

/// Chrome heights and widths, defined once: the page-view geometry and the bars both need them.
pub const TAB_STRIP_HEIGHT: f32 = TAB_HEIGHT;
pub const MODE_BAR_HEIGHT: f32 = 48.0;
pub const PANEL_HEADER_HEIGHT: f32 = 40.0;
/// A document tab's pill height inside the strip.
pub const TAB_HEIGHT: f32 = 28.0;
/// One view tab's cell in the mode bar (the widest label plus symmetric padding).
pub const MODE_TAB_WIDTH: f32 = 76.0;
pub const RAIL_WIDTH: f32 = 48.0;
/// The shared horizontal gutter of both bars.
pub const BAR_PAD: f32 = 10.0;
/// A bar's square icon control (16 px glyph in a 28 px box).
pub const ICON_BOX: f32 = 28.0;
/// A bar's non-square control (search, zoom, status).
pub const MODE_CONTROL: f32 = 30.0;
/// The source/pages split grip is drawn this wide on each side of the divider.
pub const GRIP: f32 = 3.0;
/// iced's line-heights take pixels for the editor; the egui build used 1.4 × 12.5 pt.
pub const EDITOR_SIZE: f32 = 12.5;
pub const EDITOR_LINE_HEIGHT: f32 = 17.5;

// ---------------------------------------------------------------------- fonts

/// The chrome's typeface: the Inter variable face loaded in `main`. Its family name is
/// "Inter Variable" — a plain "Inter" request silently falls back to the system sans.
const UI_FAMILY: &str = "Inter Variable";
/// The source editor's monospace face, also loaded in `main`.
pub const MONO_FAMILY: &str = "JetBrains Mono";

pub fn regular() -> iced::Font {
    iced::Font { family: iced::font::Family::Name(UI_FAMILY.into()), ..iced::Font::DEFAULT }
}

pub fn medium() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Medium, ..regular() }
}

pub fn semibold() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Semibold, ..regular() }
}

pub fn mono() -> iced::Font {
    iced::Font::with_name(MONO_FAMILY)
}
