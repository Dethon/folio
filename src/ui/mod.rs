//! View composition: toolbar, sidebar, page view and transient overlays.

pub mod icon;
pub mod sidebar;
pub mod theme;
pub mod viewer;

use std::time::Duration;

use iced::widget::{
    button, column, container, progress_bar, row, space, stack, text, text_input, tooltip,
};
use iced::{Center, Element, Fill, Font, border, font};

use crate::app::{App, Message, Tone};
use icon::Icon;
use theme::Palette;
use viewer::Fit;

/// The interface font: the platform's own where we know its name.
pub fn font() -> Font {
    if cfg!(target_os = "windows") {
        Font::with_name("Segoe UI")
    } else {
        Font::DEFAULT
    }
}

/// A heavier cut of the interface font for titles.
fn strong() -> Font {
    Font {
        weight: if cfg!(target_os = "windows") {
            font::Weight::Semibold
        } else {
            font::Weight::Bold
        },
        ..font()
    }
}

pub fn view(app: &App) -> Element<'_, Message> {
    let palette = app.palette;

    let content: Element<'_, Message> = if app.viewer.document().is_some() {
        viewer::view(&app.viewer, palette, Message::Viewer)
    } else {
        welcome(app)
    };

    let main = container(content)
        .width(Fill)
        .height(Fill)
        .style(move |_| theme::canvas(palette));

    let main: Element<'_, Message> = match &app.toast {
        Some(toast) => stack![
            main,
            container(toast_view(palette, toast))
                .width(Fill)
                .height(Fill)
                .align_x(Center)
                .align_bottom(Fill)
                .padding(24),
        ]
        .into(),
        None => main.into(),
    };

    let body: Element<'_, Message> = if app.sidebar.open {
        row![sidebar::view(app), vertical_rule(palette), main].into()
    } else {
        main
    };

    column![toolbar(app), horizontal_rule(palette), body].into()
}

fn horizontal_rule<'a>(palette: Palette) -> Element<'a, Message> {
    container(space().width(Fill).height(1))
        .style(move |_| theme::rule(palette))
        .into()
}

fn vertical_rule<'a>(palette: Palette) -> Element<'a, Message> {
    container(space().width(1).height(Fill))
        .style(move |_| theme::rule(palette))
        .into()
}

fn hint<'a>(palette: Palette, label: &'a str) -> Element<'a, Message> {
    container(text(label).size(theme::TEXT_SMALL))
        .padding([5, 9])
        .style(move |_| theme::popover(palette))
        .into()
}

/// A square toolbar button. `active` marks a toggled state; a `None`
/// message disables it.
fn tool<'a>(
    palette: Palette,
    icon: Icon,
    label: &'a str,
    message: Option<Message>,
    active: bool,
) -> Element<'a, Message> {
    let color = match (&message, active) {
        (None, _) => palette.faint,
        (_, true) => palette.accent,
        _ => palette.muted,
    };

    tooltip(
        button(container(icon.view(16.0, color)).center(theme::CONTROL))
            .padding(0)
            .on_press_maybe(message)
            .style(move |_, status| theme::ghost_button(palette, status, active)),
        hint(palette, label),
        tooltip::Position::Bottom,
    )
    .gap(8)
    .delay(Duration::from_millis(450))
    .into()
}

fn toolbar(app: &App) -> Element<'_, Message> {
    let palette = app.palette;
    let doc = app.viewer.document();
    let when_open = |message: Message| doc.is_some().then_some(message);

    let title: Element<'_, Message> = match (doc, &app.opening) {
        (_, Some(path)) => text(format!(
            "Opening {}…",
            path.file_name().unwrap_or_default().to_string_lossy()
        ))
        .size(theme::TEXT)
        .color(palette.muted)
        .wrapping(text::Wrapping::None)
        .into(),
        (Some(doc), None) => text(doc.title.as_str())
            .size(theme::TEXT)
            .font(strong())
            .color(palette.text)
            .wrapping(text::Wrapping::None)
            .into(),
        (None, None) => space().into(),
    };

    let left = row![
        tool(
            palette,
            Icon::Sidebar,
            "Sidebar  Ctrl+B",
            Some(Message::ToggleSidebar),
            app.sidebar.open,
        ),
        tool(
            palette,
            Icon::Open,
            "Open  Ctrl+O",
            Some(Message::OpenDialog),
            false
        ),
        space().width(6),
        container(title).width(Fill).clip(true),
    ]
    .spacing(2)
    .align_y(Center)
    .width(Fill);

    let pages = row![
        text_input("", &app.page_input)
            .on_input_maybe(doc.is_some().then_some(Message::PageInput))
            .on_submit(Message::PageSubmit)
            .size(theme::TEXT)
            .padding([4, 6])
            .width(52)
            .align_x(Center)
            .style(move |_, status| theme::input(palette, status)),
        text(format!("of {}", app.viewer.page_count().max(1)))
            .size(theme::TEXT)
            .color(palette.muted),
    ]
    .spacing(8)
    .align_y(Center);

    let zoom_label = button(
        container(
            text(format!("{:.0}%", app.viewer.zoom() * 100.0))
                .size(theme::TEXT)
                .color(if doc.is_some() {
                    palette.text
                } else {
                    palette.faint
                }),
        )
        .center_x(52)
        .center_y(theme::CONTROL),
    )
    .padding(0)
    .on_press_maybe(when_open(Message::ZoomReset))
    .style(move |_, status| theme::ghost_button(palette, status, false));

    let zoom = row![
        tool(
            palette,
            Icon::ZoomOut,
            "Zoom out  Ctrl+−",
            when_open(Message::Zoom(-1)),
            false
        ),
        tooltip(
            zoom_label,
            hint(palette, "Actual size  Ctrl+0"),
            tooltip::Position::Bottom
        )
        .gap(8)
        .delay(Duration::from_millis(450)),
        tool(
            palette,
            Icon::ZoomIn,
            "Zoom in  Ctrl++",
            when_open(Message::Zoom(1)),
            false
        ),
        space().width(4),
        tool(
            palette,
            Icon::FitWidth,
            "Fit width  Ctrl+1",
            when_open(Message::Fit(Fit::Width)),
            doc.is_some() && app.viewer.fit() == Fit::Width,
        ),
        tool(
            palette,
            Icon::FitPage,
            "Fit page  Ctrl+2",
            when_open(Message::Fit(Fit::Page)),
            doc.is_some() && app.viewer.fit() == Fit::Page,
        ),
    ]
    .spacing(2)
    .align_y(Center);

    let divider = container(space().width(1).height(18)).style(move |_| theme::rule(palette));

    let centre = row![pages, space().width(10), divider, space().width(8), zoom].align_y(Center);

    let right = container(
        row![
            tool(
                palette,
                Icon::Export,
                "Export as Markdown  Ctrl+E",
                when_open(Message::Export),
                false,
            ),
            tool(
                palette,
                Icon::Print,
                "Print  Ctrl+P",
                when_open(Message::Print),
                false
            ),
            space().width(4),
            tool(
                palette,
                if palette.is_dark {
                    Icon::Sun
                } else {
                    Icon::Moon
                },
                if palette.is_dark {
                    "Light theme"
                } else {
                    "Dark theme"
                },
                Some(Message::ToggleTheme),
                false,
            ),
        ]
        .spacing(2)
        .align_y(Center),
    )
    .width(Fill)
    .align_right(Fill);

    container(row![left, centre, right].spacing(12).align_y(Center))
        .padding([0, 8])
        .height(theme::TOOLBAR_HEIGHT)
        .align_y(Center)
        .style(move |_| theme::surface(palette))
        .into()
}

fn welcome(app: &App) -> Element<'_, Message> {
    let palette = app.palette;

    let open = button(text("Open a PDF").size(theme::TEXT).font(strong()))
        .padding([8, 16])
        .on_press(Message::OpenDialog)
        .style(move |_, status| theme::primary_button(palette, status));

    container(
        column![
            Icon::Document.view(40.0, palette.faint),
            space().height(6),
            text("No document open")
                .size(17)
                .font(strong())
                .color(palette.text),
            text("Drop a PDF here, or press Ctrl+O")
                .size(theme::TEXT)
                .color(palette.muted),
            space().height(10),
            open,
        ]
        .spacing(6)
        .align_x(Center),
    )
    .center(Fill)
    .into()
}

fn toast_view<'a>(palette: Palette, toast: &'a crate::app::Toast) -> Element<'a, Message> {
    let color = match toast.tone {
        Tone::Info => palette.text,
        Tone::Error => palette.danger,
    };

    let mut content = column![text(toast.text.as_str()).size(theme::TEXT).color(color)].spacing(8);

    if let Some(progress) = toast.progress {
        content = content.push(
            container(progress_bar(0.0..=1.0, progress).girth(3).style(move |_| {
                progress_bar::Style {
                    background: palette.hover.into(),
                    bar: palette.accent.into(),
                    border: border::rounded(2.0),
                }
            }))
            .width(220),
        );
    }

    let dismiss = (toast.tone == Tone::Error).then(|| {
        button(container(Icon::Close.view(13.0, palette.muted)).center(20))
            .padding(0)
            .on_press(Message::DismissToast)
            .style(move |_, status| theme::ghost_button(palette, status, false))
    });

    container(row![content].push(dismiss).spacing(12).align_y(Center))
        .padding([10, 14])
        .max_width(520)
        .style(move |_| theme::popover(palette))
        .into()
}
