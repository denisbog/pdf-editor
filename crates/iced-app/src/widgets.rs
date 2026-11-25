//! The design-system vocabulary: every control the window draws is constructed here, so the
//! chrome, rails, panels and overlays cannot drift apart. Widgets take `Tokens` by value and
//! a message; they never look at `App`.
//!
//! The rules these follow (see the `rust-iced-desktop-ui` skill):
//! - an icon button's box comes from padding, never from pinning a fixed glyph inside a
//!   fixed-size button (iced clamps the glyph *up* to the box);
//! - content in a fixed-height button is wrapped in `center_y`, because iced parks it at the
//!   padding's top-left;
//! - one scrollable style, with a 7 px lane and no rail background;
//! - disabled controls use `on_press_maybe`, so they are genuinely inert.

use iced::widget::{
    button, center_y, container, scrollable, space, text, text_input, tooltip, Scrollable,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Shadow, Theme, Vector};

use crate::icons;
use crate::theme::{self, Tokens};

// ---------------------------------------------------------------------- spacers and dividers

pub fn vspace<'a, M: 'a>(px: f32) -> Element<'a, M> {
    space::vertical().height(Length::Fixed(px)).into()
}

pub fn h_divider<'a, M: 'a>(t: Tokens) -> Element<'a, M> {
    container(space())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(move |_| container::Style { background: Some(t.divider.into()), ..container::Style::default() })
        .into()
}

// ---------------------------------------------------------------------- tooltips

/// A dark-on-light / light-on-dark tooltip around `content`.
pub fn tip<'a, M: 'a>(content: impl Into<Element<'a, M>>, label: impl Into<String>, t: Tokens) -> Element<'a, M> {
    let body = container(text(label.into()).font(theme::regular()).size(12.0).line_height(1.2).color(if t.dark() { t.text } else { Color::WHITE }))
        .padding(Padding { top: 5.0, right: 9.0, bottom: 5.0, left: 9.0 })
        .style(move |_| container::Style {
            background: Some(if t.dark() { t.card } else { Color::from_rgb8(0x2A, 0x2A, 0x2F) }.into()),
            border: Border { radius: 5.0.into(), ..Border::default() },
            ..container::Style::default()
        });
    tooltip(content, body, tooltip::Position::Bottom).gap(6.0).into()
}

// ---------------------------------------------------------------------- buttons

/// A square icon button of exactly `box_size` points.
///
/// `selected` paints `accent_soft` behind an `accent_text` glyph; `enabled = false` makes the
/// control faint and inert. Icon-only controls always carry a tooltip.
#[allow(clippy::too_many_arguments)]
pub fn icon_button<'a, M: Clone + 'a>(
    name: &str,
    box_size: f32,
    selected: bool,
    enabled: bool,
    label: impl Into<String>,
    t: Tokens,
    on_press: M,
) -> Element<'a, M> {
    let tint = if !enabled {
        t.text_faint
    } else if selected {
        t.accent_text
    } else {
        t.icon
    };
    let fill = if selected { t.accent_soft } else { Color::TRANSPARENT };
    // Derive the box from padding: a `Fixed(glyph)` inside a `Fixed(box)` button is clamped up.
    let glyph = (box_size * 0.5).max(11.0).round();
    let inset = ((box_size - glyph) / 2.0).max(0.0);

    let control = button(icons::image::<M>(name, glyph, tint))
        .padding(inset)
        .on_press_maybe(enabled.then_some(on_press))
        .style(move |_theme, status| button::Style {
            background: Some(
                match status {
                    button::Status::Hovered | button::Status::Pressed if enabled => t.hover,
                    _ => fill,
                }
                .into(),
            ),
            text_color: tint,
            border: Border { radius: t.radius.into(), ..Border::default() },
            ..button::Style::default()
        });
    tip(control, label, t)
}

/// An icon + label button for the chrome ("Menu", "New", the tab strip actions).
pub fn ghost_button<'a, M: Clone + 'a>(icon: &str, label: &'a str, t: Tokens, on_press: M) -> Element<'a, M> {
    let content = iced::widget::row![
        icons::image::<M>(icon, 15.0, t.icon),
        text(label).font(theme::medium()).size(13.0).line_height(1.2).color(t.text),
    ]
    .spacing(6.0)
    .align_y(Alignment::Center);

    // Padding-sized rather than `center_y`: a Fill-height wrapper next to a fixed-width
    // scrollable can squeeze the glyph. 16 px of content + 7 px a side is exactly 30 px.
    // 6 px on the left so the glyph's left edge lands on the same 16 px inset as an
    // `icon_button` in the same bar.
    button(content)
        .padding(Padding { top: 7.0, right: 10.0, bottom: 7.0, left: 6.0 })
        .height(Length::Fixed(30.0))
        .on_press(on_press)
        .style(move |_theme, status| button::Style {
            background: Some(
                (if matches!(status, button::Status::Hovered | button::Status::Pressed) { t.hover } else { Color::TRANSPARENT }).into(),
            ),
            text_color: t.text,
            border: Border { radius: t.radius.into(), ..Border::default() },
            ..button::Style::default()
        })
        .into()
}

/// A pill button: `primary` fills with the accent; the secondary role is a card-coloured pill
/// with a hairline outline.
pub fn pill_button<'a, M: Clone + 'a>(label: &'a str, primary: bool, t: Tokens, on_press: M) -> Element<'a, M> {
    button(center_y(text(label).font(theme::medium()).size(12.5).line_height(1.2).color(if primary { Color::WHITE } else { t.text })))
        .padding(Padding { top: 6.0, right: 16.0, bottom: 6.0, left: 16.0 })
        .height(Length::Fixed(30.0))
        .on_press(on_press)
        .style(move |_theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let (fill, border, fg) = if primary {
                (if hovered { t.accent_text } else { t.accent }, Border { radius: 15.0.into(), ..Border::default() }, Color::WHITE)
            } else {
                (if hovered { t.hover } else { t.card }, Border { color: t.border, width: 1.0, radius: 15.0.into() }, t.text)
            };
            button::Style { background: Some(fill.into()), text_color: fg, border, ..button::Style::default() }
        })
        .into()
}

/// A pill with a leading icon, for the welcome screen's two actions.
pub fn icon_pill<'a, M: Clone + 'a>(icon: &str, label: &'a str, primary: bool, t: Tokens, on_press: M) -> Element<'a, M> {
    let fg = if primary { Color::WHITE } else { t.text };
    let content = iced::widget::row![icons::image::<M>(icon, 15.0, fg), text(label).font(theme::medium()).size(12.5).line_height(1.2).color(fg)]
        .spacing(6.0)
        .align_y(Alignment::Center);

    button(content)
        .padding(Padding { top: 7.0, right: 16.0, bottom: 7.0, left: 14.0 })
        .height(Length::Fixed(30.0))
        .on_press(on_press)
        .style(move |_theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let (fill, border) = if primary {
                (if hovered { t.accent_text } else { t.accent }, Border { radius: 15.0.into(), ..Border::default() })
            } else {
                (if hovered { t.hover } else { t.card }, Border { color: t.border, width: 1.0, radius: 15.0.into() })
            };
            button::Style { background: Some(fill.into()), text_color: fg, border, ..button::Style::default() }
        })
        .into()
}

/// One document tab: two halves of a single pill (name, close), same height and no gap.
pub fn tab_name<'a, M: Clone + 'a>(label: impl Into<String>, icon: &str, active: bool, dirty: bool, t: Tokens, on_press: M) -> Element<'a, M> {
    let label = label.into();
    let tint = if active { t.accent } else { t.text_muted };
    let mut content = iced::widget::row![
        icons::image::<M>(icon, 15.0, tint),
        text(label).font(if active { theme::medium() } else { theme::regular() }).size(13.0).line_height(1.2).color(if active { t.text } else { t.text_muted }),
    ]
    .spacing(6.0)
    .align_y(Alignment::Center);
    if dirty {
        content = content.push(text("●").size(7.0).color(t.accent));
    }

    button(center_y(content))
        .height(Length::Fixed(theme::TAB_HEIGHT))
        .padding(Padding { top: 0.0, right: 6.0, bottom: 0.0, left: 10.0 })
        .on_press(on_press)
        .style(move |_theme, status| {
            let background = if active {
                Some(t.chrome)
            } else {
                match status {
                    button::Status::Hovered => Some(t.hover),
                    button::Status::Pressed => Some(t.pressed),
                    _ => None,
                }
            };
            button::Style {
                background: background.map(Background::Color),
                text_color: if active { t.text } else { t.text_muted },
                border: Border { radius: iced::border::Radius { top_left: 7.0, top_right: 0.0, bottom_left: 0.0, bottom_right: 0.0 }, ..Border::default() },
                ..button::Style::default()
            }
        })
        .into()
}

pub fn tab_close<'a, M: Clone + 'a>(active: bool, t: Tokens, on_press: M) -> Element<'a, M> {
    tip(
        button(center_y(text("✕").size(11.0).color(if active { t.text_muted } else { t.text_faint })))
            .height(Length::Fixed(theme::TAB_HEIGHT))
            .padding(Padding { top: 0.0, right: 8.0, bottom: 0.0, left: 4.0 })
            .on_press(on_press)
            .style(move |_theme, status| button::Style {
                background: if active {
                    Some(t.chrome.into())
                } else {
                    match status {
                        button::Status::Hovered => Some(t.hover.into()),
                        button::Status::Pressed => Some(t.pressed.into()),
                        _ => None,
                    }
                },
                text_color: if active { t.text_muted } else { t.text_faint },
                border: Border { radius: iced::border::Radius { top_left: 0.0, top_right: 7.0, bottom_left: 0.0, bottom_right: 0.0 }, ..Border::default() },
                ..button::Style::default()
            }),
        "Close",
        t,
    )
}

/// A full-height mode-bar tab: centred label, 3 px underline at the bar's bottom.
pub fn mode_tab<'a, M: Clone + 'a>(label: &'a str, active: bool, t: Tokens, on_press: M) -> Element<'a, M> {
    let font = if active { theme::semibold() } else { theme::medium() };
    let color = if active { t.text } else { t.text_muted };
    let underline = if active { t.text } else { Color::TRANSPARENT };

    let content = iced::widget::column![
        space::vertical().height(Length::Fill),
        center_y(text(label).font(font).size(13.5).line_height(1.2).color(color)),
        space::vertical().height(Length::Fill),
        container(space())
            .height(Length::Fixed(3.0))
            .width(Length::Fill)
            .style(move |_| container::Style { background: Some(underline.into()), ..container::Style::default() }),
    ]
    .width(Length::Fixed(theme::MODE_TAB_WIDTH))
    .height(Length::Fill)
    .align_x(Alignment::Center);

    button(content)
        .padding(0)
        .width(Length::Fixed(theme::MODE_TAB_WIDTH))
        .height(Length::Fill)
        .on_press(on_press)
        .style(move |_theme, status| button::Style {
            background: match status {
                button::Status::Hovered if !active => Some(t.hover.into()),
                _ => None,
            },
            text_color: color,
            ..button::Style::default()
        })
        .into()
}

/// A search-field lookalike that opens the palette. It is a button on purpose: the palette
/// owns the keyboard, so the chrome never swallows keystrokes.
pub fn search_box<'a, M: Clone + 'a>(placeholder: &'a str, width: f32, t: Tokens, on_press: M) -> Element<'a, M> {
    let content = iced::widget::row![
        icons::image::<M>("search", 14.0, t.text_muted),
        text(placeholder).font(theme::regular()).size(13.0).line_height(1.2).color(t.text_faint),
        space::horizontal(),
        text("Ctrl+K").font(theme::regular()).size(11.5).color(t.text_faint),
    ]
    .spacing(8.0)
    .align_y(Alignment::Center);

    button(content)
        .padding(Padding { top: 8.0, right: 12.0, bottom: 8.0, left: 12.0 })
        .width(Length::Fixed(width))
        .height(Length::Fixed(32.0))
        .on_press(on_press)
        .style(move |_theme, status| button::Style {
            background: Some((if matches!(status, button::Status::Hovered | button::Status::Pressed) { t.hover } else { t.field }).into()),
            text_color: t.text,
            border: Border { color: t.border, width: 1.0, radius: 16.0.into() },
            ..button::Style::default()
        })
        .into()
}

/// A full-width list row: leading icon, label (takes the space), trailing meta.
pub fn row_button<'a, M: Clone + 'a>(
    icon: &str,
    label: impl Into<String>,
    meta: impl Into<String>,
    enabled: bool,
    t: Tokens,
    on_press: M,
) -> Element<'a, M> {
    let (label, meta) = (label.into(), meta.into());
    let color = if enabled { t.text } else { t.text_faint };
    let content = iced::widget::row![
        icons::image::<M>(icon, 15.0, if enabled { t.icon } else { t.text_faint }),
        text(label).font(theme::regular()).size(12.5).line_height(1.2).color(color).width(Length::Fill),
        text(meta).font(theme::regular()).size(11.5).color(t.text_faint),
    ]
    .spacing(9.0)
    .align_y(Alignment::Center);

    button(content)
        .width(Length::Fill)
        .padding(Padding { top: 6.0, right: 8.0, bottom: 6.0, left: 8.0 })
        .on_press_maybe(enabled.then_some(on_press))
        .style(move |_theme, status| button::Style {
            background: match status {
                button::Status::Hovered if enabled => Some(t.hover.into()),
                button::Status::Pressed if enabled => Some(t.selected.into()),
                _ => None,
            },
            text_color: color,
            border: Border { radius: 6.0.into(), ..Border::default() },
            ..button::Style::default()
        })
        .into()
}

/// A compact text-only button for the mode bar's compile status and similar readouts.
pub fn text_button<'a, M: Clone + 'a>(label: impl Into<String>, color: Color, t: Tokens, on_press: M) -> Element<'a, M> {
    button(center_y(text(label.into()).font(theme::medium()).size(12.0).line_height(1.2).color(color)))
        .height(Length::Fixed(theme::MODE_CONTROL))
        .padding(Padding { top: 0.0, right: 8.0, bottom: 0.0, left: 8.0 })
        .on_press(on_press)
        .style(move |_theme, status| button::Style {
            background: match status {
                button::Status::Hovered => Some(t.hover.into()),
                button::Status::Pressed => Some(t.pressed.into()),
                _ => None,
            },
            text_color: color,
            border: Border { radius: t.radius.into(), ..Border::default() },
            ..button::Style::default()
        })
        .into()
}

/// A two-line diagnostic entry in the Problems list.
pub fn problem_row<'a, M: Clone + 'a>(
    message: impl Into<String>,
    location: impl Into<String>,
    hints: &'a [String],
    error: bool,
    jump: Option<M>,
    t: Tokens,
) -> Element<'a, M> {
    let (message, location) = (message.into(), location.into());
    let color = if error { t.danger } else { t.warning };
    let mut details = iced::widget::column![
        iced::widget::row![
            icons::image::<M>(if error { "triangle-alert" } else { "info" }, 13.0, color),
            text(message).font(theme::regular()).size(12.5).color(t.text).width(Length::Fill),
        ]
        .spacing(6.0)
        .align_y(Alignment::Center),
        text(location).font(theme::regular()).size(10.5).color(t.text_faint),
    ]
    .spacing(2.0);
    for hint in hints {
        details = details.push(text(hint.clone()).font(theme::regular()).size(11.0).color(t.text_muted));
    }

    let content = container(details).width(Length::Fill).padding(Padding { top: 6.0, right: 8.0, bottom: 7.0, left: 8.0 });
    match jump {
        Some(message) => button(content)
            .width(Length::Fill)
            .padding(0)
            .on_press(message)
            .style(move |_theme, status| button::Style {
                background: match status {
                    button::Status::Hovered => Some(t.hover.into()),
                    button::Status::Pressed => Some(t.selected.into()),
                    _ => None,
                },
                text_color: t.text,
                border: Border { radius: 6.0.into(), ..Border::default() },
                ..button::Style::default()
            })
            .into(),
        None => content.into(),
    }
}

// ---------------------------------------------------------------------- headings and badges

pub fn section_title<'a, M: 'a>(title: &str, t: Tokens) -> Element<'a, M> {
    text(title.to_uppercase()).font(theme::semibold()).size(10.5).color(t.text_faint).into()
}

pub fn badge<'a, M: 'a>(label: &str, fill: Color, fg: Color) -> Element<'a, M> {
    container(text(label.to_owned()).font(theme::medium()).size(10.0).line_height(1.2).color(fg))
        .padding(Padding { top: 2.0, right: 6.0, bottom: 2.0, left: 6.0 })
        .style(move |_| container::Style {
            background: Some(fill.into()),
            border: Border { radius: 8.0.into(), ..Border::default() },
            ..container::Style::default()
        })
        .into()
}

// ---------------------------------------------------------------------- scrolling and fields

/// The one scrollable: 7 px lane, 5 px rounded thumb, no rail background.
pub fn scrolled<'a, M: 'a>(content: impl Into<Element<'a, M>>, t: Tokens) -> Scrollable<'a, M> {
    scrollable(content)
        .direction(scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(7.0).scroller_width(5.0).margin(1.0)))
        .style(move |_theme, status| {
            let (hovered, dragged) = match status {
                scrollable::Status::Hovered { is_vertical_scrollbar_hovered, .. } => (is_vertical_scrollbar_hovered, false),
                scrollable::Status::Dragged { is_vertical_scrollbar_dragged, .. } => (true, is_vertical_scrollbar_dragged),
                _ => (false, false),
            };
            let thumb = if dragged {
                t.icon
            } else if hovered {
                t.text_muted
            } else {
                t.text_faint
            };
            let scroller = scrollable::Scroller { background: thumb.into(), border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 3.0.into() } };
            let rail = scrollable::Rail { background: None, border: Border::default(), scroller };
            scrollable::Style {
                container: container::Style::default(),
                vertical_rail: rail,
                horizontal_rail: scrollable::Rail { background: None, border: Border::default(), scroller },
                gap: None,
                auto_scroll: scrollable::AutoScroll { background: t.card.into(), border: Border { color: t.border, width: 1.0, radius: 8.0.into() }, shadow: Shadow::default(), icon: t.icon },
            }
        })
}

/// Document scrolling: both axes, so a page wider than the viewport can be scrolled instead of
/// being squeezed into it (iced clamps `Length::Fixed` to the parent's max width unless the
/// scrollable itself allows horizontal overflow).
pub fn scrolled_both<'a, M: 'a>(content: impl Into<Element<'a, M>>, t: Tokens) -> Scrollable<'a, M> {
    let bar = || scrollable::Scrollbar::new().width(7.0).scroller_width(5.0).margin(1.0);
    scrollable(content)
        .direction(scrollable::Direction::Both { horizontal: bar(), vertical: bar() })
        .style(move |_theme, status| {
            let (hovered, dragged) = match status {
                scrollable::Status::Hovered { is_vertical_scrollbar_hovered, is_horizontal_scrollbar_hovered, .. } => {
                    (is_vertical_scrollbar_hovered || is_horizontal_scrollbar_hovered, false)
                }
                scrollable::Status::Dragged { is_vertical_scrollbar_dragged, is_horizontal_scrollbar_dragged, .. } => {
                    (true, is_vertical_scrollbar_dragged || is_horizontal_scrollbar_dragged)
                }
                _ => (false, false),
            };
            let thumb = if dragged {
                t.icon
            } else if hovered {
                t.text_muted
            } else {
                t.text_faint
            };
            let scroller = scrollable::Scroller { background: thumb.into(), border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 3.0.into() } };
            let rail = scrollable::Rail { background: None, border: Border::default(), scroller };
            scrollable::Style {
                container: container::Style::default(),
                vertical_rail: rail,
                horizontal_rail: scrollable::Rail { background: None, border: Border::default(), scroller },
                gap: None,
                auto_scroll: scrollable::AutoScroll { background: t.card.into(), border: Border { color: t.border, width: 1.0, radius: 8.0.into() }, shadow: Shadow::default(), icon: t.icon },
            }
        })
}

/// A horizontal lane with no visible scrollbar: tab strips scroll, they do not show a rail.
pub fn lane<'a, M: 'a>(content: impl Into<Element<'a, M>>, height: f32) -> Scrollable<'a, M> {
    scrollable(content)
        .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::hidden()))
        .height(Length::Fixed(height))
}

/// The one text-field style: focus paints the accent border.
pub fn field_style(t: Tokens, radius: f32) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |_theme, status| text_input::Style {
        background: t.field.into(),
        border: Border {
            color: if matches!(status, text_input::Status::Focused { .. } | text_input::Status::Hovered) { t.accent } else { t.border },
            width: 1.0,
            radius: radius.into(),
        },
        icon: t.text_muted,
        placeholder: t.text_faint,
        value: t.text,
        selection: t.accent_soft,
    }
}

// ---------------------------------------------------------------------- surfaces and layers

pub fn floating_card(t: Tokens, shadow_alpha: f32, offset: f32, blur: f32) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(t.card.into()),
        border: Border { color: t.border, width: 1.0, radius: 12.0.into() },
        shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, shadow_alpha), offset: Vector::new(0.0, offset), blur_radius: blur },
        ..container::Style::default()
    }
}

/// A full-window scrim that closes the layer when clicked.
pub fn scrim<'a, M: Clone + 'a>(alpha: f32, t: Tokens, on_dismiss: M) -> Element<'a, M> {
    let _ = t;
    button(space())
        .width(Length::Fill)
        .height(Length::Fill)
        .on_press(on_dismiss)
        .style(move |_theme, _status| button::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, alpha).into()),
            ..button::Style::default()
        })
        .into()
}

/// The standard empty state: a faint icon, a sentence, centred near the top of a body.
pub fn empty_state<'a, M: 'a>(icon: &str, message: &'a str, t: Tokens) -> Element<'a, M> {
    container(
        iced::widget::column![
            icons::image::<M>(icon, 30.0, t.text_faint),
            text(message).font(theme::regular()).size(12.5).color(t.text_muted).center(),
        ]
        .spacing(8.0)
        .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(Padding { top: 32.0, right: 0.0, bottom: 0.0, left: 0.0 })
    .center_x(Length::Fill)
    .into()
}

/// A transient message: dark inverse pill at the bottom centre, 3.5 s lifetime.
pub fn toast<'a, M: 'a>(message: &str, t: Tokens) -> Element<'a, M> {
    let (fill, fg) = (t.toast_background(), t.toast_text());
    container(
        container(text(message.to_owned()).font(theme::regular()).size(13.0).color(fg))
            .padding(Padding { top: 10.0, right: 16.0, bottom: 10.0, left: 16.0 })
            .style(move |_| container::Style {
                background: Some(fill.into()),
                border: Border { radius: 8.0.into(), ..Border::default() },
                shadow: Shadow { color: Color::from_rgba(0.0, 0.0, 0.0, 0.28), offset: Vector::new(0.0, 6.0), blur_radius: 20.0 },
                ..container::Style::default()
            }),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(Padding { top: 0.0, right: 0.0, bottom: 28.0, left: 0.0 })
    .align_x(Alignment::Center)
    .align_y(Alignment::End)
    .into()
}

/// A side-panel shell: 40 px header with a title and a close control, a divider, and a padded
/// scrolling body. Every panel looks the same because every panel is this function.
pub fn panel<'a, M: Clone + 'a>(
    title: &'a str,
    body: Element<'a, M>,
    width: f32,
    t: Tokens,
    on_close: M,
) -> Element<'a, M> {
    let header = container(
        iced::widget::row![
            text(title).font(theme::semibold()).size(13.5).line_height(1.2).color(t.text),
            space::horizontal(),
            icon_button("x", 24.0, false, true, format!("Close {title}"), t, on_close),
        ]
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(theme::PANEL_HEADER_HEIGHT))
    .padding(Padding { top: 0.0, right: 8.0, bottom: 0.0, left: 12.0 })
    .align_y(Alignment::Center);

    container(
        iced::widget::column![
            header,
            h_divider(t),
            scrolled(container(body).padding(Padding { top: 10.0, right: 12.0, bottom: 16.0, left: 12.0 }), t).height(Length::Fill),
        ]
        .height(Length::Fill),
    )
    .width(Length::Fixed(width))
    .height(Length::Fill)
    .style(move |_| container::Style { background: Some(t.panel.into()), ..container::Style::default() })
    .into()
}
