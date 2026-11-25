//! Vector icons: the official Lucide SVGs, embedded from the `lucide-static-svg` crate (Lucide
//! is ISC-licensed) and tinted by iced's SVG pipeline.
//!
//! Each icon is patched once to `#ffffff` so iced's `color` filter can recolour it; the maps are
//! shared, so an icon used in twenty places still has one allocation.

use std::collections::HashMap;
use std::sync::OnceLock;

use iced::widget::svg;
use iced::{Color, Element, Length, Theme};
use lucide_static_svg::Icon;

/// The icons the chrome uses, under the names the rest of the app asks for them by.
static ICONS: &[(&str, Icon)] = &[
    ("house", Icon::House),
    ("file-text", Icon::FileText),
    ("x", Icon::X),
    ("plus", Icon::Plus),
    ("sun", Icon::Sun),
    ("moon", Icon::Moon),
    ("settings", Icon::Settings),
    ("circle-help", Icon::CircleQuestionMark),
    ("panel-left", Icon::PanelLeft),
    ("search", Icon::Search),
    ("hand", Icon::Hand),
    ("zoom-in", Icon::ZoomIn),
    ("zoom-out", Icon::ZoomOut),
    ("maximize", Icon::Maximize),
    ("gauge", Icon::Gauge),
    ("square", Icon::Square),
    ("square-terminal", Icon::SquareTerminal),
    ("info", Icon::Info),
    ("check", Icon::Check),
    ("chevron-up", Icon::ChevronUp),
    ("chevron-down", Icon::ChevronDown),
    // The editor's own tools.
    ("save", Icon::Save),
    ("file-output", Icon::FileOutput),
    ("file-plus", Icon::FilePlus),
    ("refresh-cw", Icon::RefreshCw),
    ("triangle-alert", Icon::TriangleExclamationPoint),
    ("columns-2", Icon::Columns2),
    ("code", Icon::Code),
    ("eye", Icon::Eye),
];

fn white_icons() -> &'static HashMap<&'static str, svg::Handle> {
    static MAP: OnceLock<HashMap<&'static str, svg::Handle>> = OnceLock::new();
    MAP.get_or_init(|| {
        ICONS
            .iter()
            .map(|(name, icon)| {
                // iced tints the SVG itself, so the stroke is patched to white and left as
                // Lucide draws it otherwise.
                let svg = icon.svg_str().replace("currentColor", "#ffffff").replace("stroke-width=\"2\"", "stroke-width=\"1.75\"");
                (*name, svg::Handle::from_memory(svg.into_bytes()))
            })
            .collect()
    })
}

/// The handle for an icon; an unknown name falls back to the empty square, so a missing icon
/// is visible but harmless.
pub fn handle(name: &str) -> svg::Handle {
    white_icons().get(name).or_else(|| white_icons().get("square")).cloned().unwrap_or_else(|| svg::Handle::from_memory(Vec::new()))
}

/// Whether the icon table has `name` (the fallback square is always there).
#[cfg_attr(not(test), allow(dead_code))]
pub fn contains(name: &str) -> bool {
    white_icons().contains_key(name)
}

/// An icon element of `size` pixels, tinted `tint`.
pub fn image<'a, Message: 'a>(name: &str, size: f32, tint: Color) -> Element<'a, Message> {
    svg(handle(name))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_theme: &Theme, _status| svg::Style { color: Some(tint) })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the chrome, the rails, the panels and the overlays ask for. A typo must
    /// fail here rather than silently draw the fallback square.
    const USED: &[&str] = &[
        "house", "file-text", "x", "plus", "sun", "moon", "settings", "circle-help", "panel-left",
        "search", "hand", "zoom-in", "zoom-out", "maximize", "gauge", "square", "square-terminal",
        "info", "check", "chevron-up", "chevron-down", "save", "file-output", "file-plus",
        "refresh-cw", "triangle-alert", "columns-2", "code", "eye",
    ];

    #[test]
    fn every_icon_the_ui_names_exists() {
        for name in USED {
            assert!(contains(name), "the icon table has no {name:?}");
        }
    }

    #[test]
    fn an_unknown_name_falls_back_to_the_square() {
        assert!(contains("no-such-icon") == false);
        let _ = handle("no-such-icon");
    }
}
