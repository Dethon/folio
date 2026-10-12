//! View composition: toolbar, sidebar, page view and transient overlays.

pub mod icon;
pub mod sidebar;
pub mod theme;
pub mod viewer;

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget};
use iced::widget::{
    button, column, container, mouse_area, progress_bar, row, space, stack, text, text_input,
    tooltip,
};
use iced::{Center, Element, Fill, Font, Length, Padding, Rectangle, Size, border, font, mouse};

use crate::app::{App, Find, Message, Tone};
use crate::graphics;
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

/// Lays out a line in each cut of the interface font, which leaves the font
/// loaded and matched for the first frame to use.
pub fn warm_fonts() {
    use iced::advanced::graphics::text::Paragraph;
    use iced::advanced::text::{self, Paragraph as _};

    for font in [font(), strong()] {
        let _ = Paragraph::with_text(text::Text {
            content: "Folio 0123456789",
            bounds: Size::INFINITE,
            size: theme::TEXT.into(),
            line_height: text::LineHeight::default(),
            font,
            align_x: text::Alignment::Default,
            align_y: iced::alignment::Vertical::Top,
            shaping: text::Shaping::default(),
            wrapping: text::Wrapping::default(),
        });
    }
}

/// The size the page view is given in a window of the given size: what is
/// left beside the sidebar and below the toolbar, each with its rule.
pub fn page_view_size(window: Size, sidebar: bool) -> Size {
    Size::new(
        window.width
            - if sidebar {
                theme::SIDEBAR_WIDTH + 1.0
            } else {
                0.0
            },
        window.height - theme::TOOLBAR_HEIGHT - 1.0,
    )
}

pub fn view(app: &App) -> Element<'_, Message> {
    let palette = app.palette;

    let content: Element<'_, Message> = if app.viewer.document().is_some() {
        viewer::view(&app.viewer, palette, Message::Viewer)
    } else if app.opening.is_some() {
        // The document is moments away; an invitation to open one would
        // only flash by.
        space().into()
    } else {
        welcome(app)
    };

    let main = container(content)
        .width(Fill)
        .height(Fill)
        .style(move |_| theme::canvas(palette));

    let mut main = stack![main];

    if app.find.open && app.viewer.document().is_some() {
        main = main.push(
            container(find_bar(app))
                .width(Fill)
                .height(Fill)
                .align_right(Fill)
                // Clear of the page view's scrollbar.
                .padding(Padding::new(12.0).right(22.0)),
        );
    }

    if let Some(toast) = &app.toast {
        main = main.push(
            container(toast_view(palette, toast))
                .width(Fill)
                .height(Fill)
                .align_x(Center)
                .align_bottom(Fill)
                .padding(24),
        );
    }

    let main: Element<'_, Message> = main.into();

    let body: Element<'_, Message> = if app.sidebar.open {
        row![sidebar::view(app), vertical_rule(palette), main].into()
    } else {
        main
    };

    column![toolbar(app), horizontal_rule(palette), body].into()
}

/// The rule under the toolbar. It is in every frame, whatever else is
/// showing, which makes it the place to mark the start of one.
fn horizontal_rule<'a>(palette: Palette) -> Element<'a, Message> {
    container(Element::new(FrameMark))
        .style(move |_| theme::rule(palette))
        .into()
}

/// A pixel-high strip that draws nothing, and tells the graphics module that
/// a frame is being drawn.
struct FrameMark;

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for FrameMark {
    fn size(&self) -> Size<Length> {
        Size::new(Fill, Length::Fixed(1.0))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Fill, 1)
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        graphics::pace();
    }
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
                Icon::Search,
                "Find  Ctrl+F",
                when_open(Message::Find),
                app.find.open,
            ),
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

/// The find bar, floating over the top right of the page view.
fn find_bar(app: &App) -> Element<'_, Message> {
    let palette = app.palette;
    let find = &app.find;
    let count = app.viewer.match_count();
    let more = if find.capped { "+" } else { "" };

    let (status, color) = match (count, app.viewer.current_match()) {
        (0, _) if find.query.trim().is_empty() => (String::new(), palette.muted),
        (0, _) if find.searching => ("Searching…".into(), palette.muted),
        (0, _) => ("No results".into(), palette.danger),
        (_, Some(current)) => (format!("{current} of {count}{more}"), palette.muted),
        (_, None) => (format!("{count}{more}"), palette.muted),
    };

    let field = text_input("Find in document", &find.query)
        .id(Find::FIELD)
        .on_input(Message::FindInput)
        .on_submit(Message::FindSubmit)
        .size(theme::TEXT)
        .padding([4, 6])
        .width(210)
        .style(move |_, status| theme::input(palette, status));

    let step = |direction: i8| (count > 0).then_some(Message::FindStep(direction));

    let bar = container(
        row![
            Icon::Search.view(15.0, palette.muted),
            space().width(8),
            field,
            container(
                text(status)
                    .size(theme::TEXT_SMALL)
                    .color(color)
                    .wrapping(text::Wrapping::None)
            )
            .width(96)
            .align_x(iced::Right)
            .padding([0, 8]),
            tool(
                palette,
                Icon::ChevronUp,
                "Previous  Shift+Enter",
                step(-1),
                false
            ),
            tool(palette, Icon::ChevronDown, "Next  Enter", step(1), false),
            tool(
                palette,
                Icon::Close,
                "Close  Esc",
                Some(Message::Escape),
                false
            ),
        ]
        .spacing(2)
        .align_y(Center),
    )
    .padding(Padding::new(6.0).left(10.0))
    .style(move |_| theme::popover(palette));

    // Clicks on the bar itself stay off the page beneath it.
    mouse_area(bar).interaction(mouse::Interaction::Idle).into()
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Instant;

    use iced::window;

    use super::*;
    use crate::app::Options;
    use crate::engine::{DocInfo, PageSize};
    use crate::ui::viewer::Interaction;

    /// Start-up renders the first screen before there is a window to lay
    /// out, so what it assumes about the layout has to hold.
    #[test]
    fn page_view_size_is_where_the_layout_puts_it() {
        let window = Size::new(1180.0, 840.0);

        for sidebar in [true, false] {
            let (mut app, _) = App::boot(Options::default());

            app.sidebar.open = sidebar;
            app.viewer.set_document(Arc::new(DocInfo {
                id: 1,
                path: PathBuf::new(),
                title: String::new(),
                pages: vec![
                    PageSize {
                        width: 612.0,
                        height: 792.0,
                    };
                    3
                ],
                outline: Vec::new(),
            }));

            let mut ui =
                iced_test::Simulator::with_size(iced::Settings::default(), window, app.view());

            ui.simulate([iced::Event::Window(window::Event::RedrawRequested(
                Instant::now(),
            ))]);

            let sizes: Vec<Size> = ui
                .into_messages()
                .filter_map(|message| match message {
                    Message::Viewer(Interaction::Resized(size)) => Some(size),
                    _ => None,
                })
                .collect();

            assert_eq!(sizes, [page_view_size(window, sidebar)]);
        }
    }
}
