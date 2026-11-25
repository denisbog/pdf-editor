//! PDF Editor on iced — the same Typst engine and the same vim machine as the egui build,
//! with `typst-iced-editor` (cosmic-text) as the source editor, so bidirectional and complex
//! scripts shape properly and modal editing is a first-class key binding.
//!
//! The debug helpers live in the egui binary; this one is the window.

mod app;
mod icons;
mod pages;
mod theme;
mod vim;
mod widgets;

use iced::{Font, Size, window};

use pdf_editor_core::document;

fn main() -> iced::Result {
    let settings = document::load_settings();
    iced::application(move || app::App::boot(settings.clone()), app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .antialiasing(true)
        .window(window::Settings {
            size: Size::new(1440.0, 920.0),
            min_size: Some(Size::new(880.0, 520.0)),
            ..window::Settings::default()
        })
        // The bundled Inter face registers under "Inter Variable", not "Inter".
        .default_font(Font::with_name("Inter Variable"))
        .font(damascene_fonts_inter::INTER_VARIABLE)
        .font(damascene_fonts_jetbrains_mono::JETBRAINS_MONO_VARIABLE)
        .run()
}
