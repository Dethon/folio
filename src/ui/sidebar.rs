//! The sidebar: document outline and page thumbnails.
//!
//! Both lists are virtualised. Rows have a fixed height, so the rows in view
//! follow directly from the scroll offset and only those are built; spacers
//! stand in for the rest. An outline with thousands of entries costs the same
//! as one with ten.

use std::collections::HashSet;
use std::ops::Range;

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Center, Element, Fill, Length, Task, border};

use crate::app::{App, Message};
use crate::engine::DocInfo;
use crate::ui::icon::Icon;
use crate::ui::theme::{self, Palette};

/// Height of the row of tabs above the list.
pub const HEADER: f32 = 42.0;
const OUTLINE_ROW: f32 = 28.0;
const PAGE_ROW: f32 = 206.0;
const THUMBNAIL_BOX: (f32, f32) = (136.0, 168.0);
/// Rows built beyond each edge of the viewport, so fast scrolling does not
/// outrun the list.
const OVERSCAN: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Outline,
    Pages,
}

impl Tab {
    fn row_height(self) -> f32 {
        match self {
            Tab::Outline => OUTLINE_ROW,
            Tab::Pages => PAGE_ROW,
        }
    }

    fn scroll_id(self) -> iced::widget::Id {
        iced::widget::Id::new(match self {
            Tab::Outline => "sidebar-outline",
            Tab::Pages => "sidebar-pages",
        })
    }
}

pub struct State {
    pub open: bool,
    pub tab: Tab,
    /// Scroll position and height of the list, as last reported by it.
    pub offset: f32,
    pub height: f32,
    expanded: HashSet<usize>,
    /// Outline entries not hidden under a collapsed parent.
    rows: Vec<usize>,
    /// The outline entry containing the page being read.
    active: Option<usize>,
    followed_page: Option<u32>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: true,
            tab: Tab::Outline,
            offset: 0.0,
            height: 1200.0,
            expanded: HashSet::new(),
            rows: Vec::new(),
            active: None,
            followed_page: None,
        }
    }
}

/// Scrolls both lists back to the top, for a newly opened document.
pub fn reset_scroll() -> Task<Message> {
    Task::batch([Tab::Outline, Tab::Pages].map(|tab| {
        iced::widget::operation::snap_to(tab.scroll_id(), scrollable::RelativeOffset::START)
    }))
}

fn has_children(doc: &DocInfo, index: usize) -> bool {
    doc.outline
        .get(index + 1)
        .is_some_and(|next| next.depth > doc.outline[index].depth)
}

impl State {
    pub fn set_document(&mut self, doc: &DocInfo) {
        // A short outline opens fully; a long one shows its top level.
        self.expanded = if doc.outline.len() <= 60 {
            (0..doc.outline.len()).collect()
        } else {
            HashSet::new()
        };
        self.tab = if doc.outline.is_empty() {
            Tab::Pages
        } else {
            Tab::Outline
        };
        self.offset = 0.0;
        self.active = None;
        self.followed_page = None;
        self.rebuild(doc);
    }

    pub fn show(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.offset = 0.0;
        }
    }

    pub fn toggle(&mut self, doc: Option<&DocInfo>, index: usize) {
        if !self.expanded.remove(&index) {
            self.expanded.insert(index);
        }

        if let Some(doc) = doc {
            self.rebuild(doc);
        }
    }

    fn rebuild(&mut self, doc: &DocInfo) {
        self.rows.clear();

        // Entries deeper than this sit under a collapsed ancestor.
        let mut hidden_below: Option<u16> = None;

        for (index, item) in doc.outline.iter().enumerate() {
            if let Some(depth) = hidden_below {
                if item.depth > depth {
                    continue;
                }
                hidden_below = None;
            }

            self.rows.push(index);

            if !self.expanded.contains(&index) {
                hidden_below = Some(item.depth);
            }
        }
    }

    /// Tracks which outline entry the reader is in.
    pub fn follow(&mut self, doc: Option<&DocInfo>, page: u32) {
        if self.followed_page == Some(page) {
            return;
        }

        self.followed_page = Some(page);
        self.active = doc.and_then(|doc| {
            doc.outline
                .iter()
                .enumerate()
                .filter(|(_, item)| item.page.is_some_and(|target| target <= page))
                .max_by_key(|(index, item)| (item.page, *index))
                .map(|(index, _)| index)
        });
    }

    /// The visible outline row for the entry being read: the entry itself,
    /// or the nearest ancestor when it is collapsed away.
    fn active_row(&self) -> Option<usize> {
        let active = self.active?;

        self.rows
            .partition_point(|row| *row <= active)
            .checked_sub(1)
            .map(|position| self.rows[position])
    }

    /// Brings the thumbnail of `page` into view if it is not already.
    pub fn reveal_page(&mut self, page: u32) -> Task<Message> {
        if !self.open || self.tab != Tab::Pages {
            return Task::none();
        }

        let top = page as f32 * PAGE_ROW;

        if top >= self.offset && top + PAGE_ROW <= self.offset + self.height {
            return Task::none();
        }

        // Centre it, as far as the list allows.
        self.offset = (top - (self.height - PAGE_ROW) / 2.0).max(0.0);

        iced::widget::operation::scroll_to(
            self.tab.scroll_id(),
            scrollable::AbsoluteOffset {
                x: 0.0,
                y: self.offset,
            },
        )
    }

    /// Rows of the current list that should exist, out of `count`.
    pub fn visible_rows(&self, count: usize) -> Range<usize> {
        let height = self.tab.row_height();
        let first = ((self.offset / height) as usize).saturating_sub(OVERSCAN);
        let last = first + (self.height / height).ceil() as usize + 2 * OVERSCAN;

        first.min(count)..last.min(count)
    }
}

pub fn view(app: &App) -> Element<'_, Message> {
    let palette = app.palette;
    let state = &app.sidebar;

    let tab = |tab: Tab, icon: Icon, label: &'static str| {
        let active = state.tab == tab;
        let color = if active { palette.text } else { palette.muted };

        button(
            row![
                icon.view(14.0, color),
                text(label).size(theme::TEXT_SMALL).color(color)
            ]
            .spacing(6)
            .align_y(Center),
        )
        .padding([0, 10])
        .height(26)
        .on_press(Message::ShowTab(tab))
        .style(move |_, status| button::Style {
            background: match status {
                _ if active => Some(palette.hover.into()),
                button::Status::Hovered => Some(palette.hover.into()),
                _ => None,
            },
            border: border::rounded(theme::RADIUS - 1.0),
            ..button::Style::default()
        })
    };

    let header = container(
        row![
            tab(Tab::Outline, Icon::Outline, "Outline"),
            tab(Tab::Pages, Icon::Pages, "Pages"),
        ]
        .spacing(4),
    )
    .height(HEADER)
    .padding([8, 10]);

    let body: Element<'_, Message> = match (app.viewer.document(), state.tab) {
        (None, _) => empty(palette, "No document open"),
        (Some(doc), Tab::Outline) if doc.outline.is_empty() => {
            empty(palette, "This document has no outline")
        }
        (Some(doc), Tab::Outline) => {
            let active = state.active_row();

            list(state, palette, state.rows.len(), |row| {
                let index = state.rows[row];
                outline_row(state, palette, doc, index, active == Some(index))
            })
        }
        (Some(doc), Tab::Pages) => {
            let current = app.viewer.current_page();

            list(state, palette, doc.pages.len(), |row| {
                page_row(app, palette, doc, row as u32, current)
            })
        }
    };

    container(column![header, body])
        .width(theme::SIDEBAR_WIDTH)
        .height(Fill)
        .style(move |_| theme::surface(palette))
        .into()
}

fn empty<'a>(palette: Palette, message: &'a str) -> Element<'a, Message> {
    container(text(message).size(theme::TEXT_SMALL).color(palette.faint))
        .center_x(Fill)
        .padding(24)
        .into()
}

/// A scrolling list that only builds the rows in view.
fn list<'a>(
    state: &State,
    palette: Palette,
    count: usize,
    build: impl Fn(usize) -> Element<'a, Message>,
) -> Element<'a, Message> {
    let visible = state.visible_rows(count);
    let height = state.tab.row_height();

    let mut rows = column![space().height(visible.start as f32 * height)];

    for row in visible.clone() {
        rows = rows.push(build(row));
    }

    rows = rows.push(space().height((count - visible.end) as f32 * height));

    scrollable(container(rows).padding(iced::Padding {
        top: 0.0,
        right: 12.0,
        bottom: 8.0,
        left: 8.0,
    }))
    .id(state.tab.scroll_id())
    .direction(scrollable::Direction::Vertical(
        scrollable::Scrollbar::new()
            .width(8)
            .scroller_width(8)
            .margin(2),
    ))
    .on_scroll(Message::SidebarScrolled)
    .width(Fill)
    .height(Fill)
    .style(move |_, status| theme::scroller(palette, status))
    .into()
}

fn outline_row<'a>(
    state: &State,
    palette: Palette,
    doc: &'a DocInfo,
    index: usize,
    active: bool,
) -> Element<'a, Message> {
    let item = &doc.outline[index];

    let disclosure: Element<'a, Message> = if has_children(doc, index) {
        let icon = if state.expanded.contains(&index) {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };

        button(container(icon.view(13.0, palette.faint)).center(18))
            .padding(0)
            .on_press(Message::ToggleOutline(index))
            .style(move |_, status| theme::ghost_button(palette, status, false))
            .into()
    } else {
        space().width(18).into()
    };

    let label = container(
        text(item.title.as_str())
            .size(theme::TEXT)
            .wrapping(text::Wrapping::None),
    )
    .width(Fill)
    .clip(true);

    let number = item
        .page
        .map(|page| text(page + 1).size(11).color(palette.faint));

    button(
        row![
            space().width(item.depth.min(8) as f32 * 12.0),
            disclosure,
            label,
        ]
        .push(number)
        .spacing(6)
        .align_y(Center),
    )
    .padding([0, 6])
    .width(Fill)
    .height(OUTLINE_ROW)
    .on_press_maybe(item.page.map(Message::GoTo))
    .style(move |_, status| theme::row_button(palette, status, active))
    .into()
}

fn page_row<'a>(
    app: &'a App,
    palette: Palette,
    doc: &DocInfo,
    page: u32,
    current: u32,
) -> Element<'a, Message> {
    let size = doc.pages[page as usize];
    let active = page == current;

    // Fit the page's shape inside the thumbnail box.
    let scale = (THUMBNAIL_BOX.0 / size.width).min(THUMBNAIL_BOX.1 / size.height);
    let (width, height) = (
        (size.width * scale).round().max(8.0),
        (size.height * scale).round().max(8.0),
    );

    let picture: Element<'a, Message> = match app.viewer.thumbnail(page) {
        Some(handle) => iced::widget::image(handle.clone())
            .width(width)
            .height(height)
            .into(),
        None => space().width(width).height(height).into(),
    };

    let frame = container(picture)
        .padding(if active { 2 } else { 1 })
        .style(move |_| container::Style {
            background: Some(
                if active {
                    palette.accent
                } else {
                    palette.border
                }
                .into(),
            ),
            border: border::rounded(3.0),
            ..container::Style::default()
        });

    let paper = container(frame).style(|_| container::Style {
        background: None,
        ..container::Style::default()
    });

    let label = text(page + 1)
        .size(11)
        .color(if active { palette.text } else { palette.muted });

    button(
        column![
            container(paper)
                .center_x(Fill)
                .center_y(THUMBNAIL_BOX.1 + 4.0),
            label,
        ]
        .spacing(6)
        .align_x(Center),
    )
    .padding([8, 0])
    .width(Length::Fill)
    .height(PAGE_ROW)
    .on_press(Message::GoTo(page))
    .style(move |_, status| theme::row_button(palette, status, false))
    .into()
}
