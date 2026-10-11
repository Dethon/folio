//! Colours and widget styles.
//!
//! Every surface draws from the [`Palette`] tokens below, so the two themes
//! differ only in these values.

use iced::widget::{button, container, scrollable, text_input};
use iced::{Background, Border, Color, Shadow, Vector, border};

pub const TOOLBAR_HEIGHT: f32 = 44.0;
pub const SIDEBAR_WIDTH: f32 = 264.0;
pub const CONTROL: f32 = 28.0;
pub const RADIUS: f32 = 6.0;
pub const TEXT: f32 = 13.0;
pub const TEXT_SMALL: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub is_dark: bool,
    /// Behind the pages.
    pub canvas: Color,
    /// Toolbar and sidebar.
    pub surface: Color,
    /// Inputs and popovers sitting on a surface.
    pub raised: Color,
    pub hover: Color,
    pub pressed: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub on_accent: Color,
    /// Selected rows and toggled buttons.
    pub accent_soft: Color,
    pub selection: Color,
    /// Search matches, and the one the reader is on. Both sit on the paper,
    /// which is white in either theme.
    pub found: Color,
    pub found_current: Color,
    pub danger: Color,
    pub shadow: Color,
    pub scrollbar: Color,
}

const fn rgb(hex: u32) -> Color {
    rgba(hex, 1.0)
}

const fn rgba(hex: u32, a: f32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a,
    }
}

impl Palette {
    pub const DARK: Self = Self {
        is_dark: true,
        canvas: rgb(0x15171b),
        surface: rgb(0x1e2126),
        raised: rgb(0x282c33),
        hover: rgba(0xffffff, 0.07),
        pressed: rgba(0xffffff, 0.11),
        border: rgb(0x30343c),
        text: rgb(0xdfe2e7),
        muted: rgb(0x9aa1ac),
        faint: rgb(0x666d79),
        accent: rgb(0x6ea8f7),
        on_accent: rgb(0x0e1626),
        accent_soft: rgba(0x6ea8f7, 0.16),
        selection: rgba(0x4d8ef5, 0.36),
        found: rgba(0xffc400, 0.40),
        found_current: rgba(0xff7a00, 0.58),
        danger: rgb(0xf0736c),
        shadow: rgba(0x000000, 0.55),
        scrollbar: rgba(0xffffff, 0.22),
    };

    pub const LIGHT: Self = Self {
        is_dark: false,
        canvas: rgb(0xe9ebef),
        surface: rgb(0xf9fafb),
        raised: rgb(0xffffff),
        hover: rgba(0x0f1728, 0.06),
        pressed: rgba(0x0f1728, 0.10),
        border: rgb(0xdcdfe5),
        text: rgb(0x1d2129),
        muted: rgb(0x5d6572),
        faint: rgb(0x98a0ad),
        accent: rgb(0x2f6fe4),
        on_accent: rgb(0xffffff),
        accent_soft: rgba(0x2f6fe4, 0.12),
        selection: rgba(0x2f6fe4, 0.30),
        found: rgba(0xffc400, 0.40),
        found_current: rgba(0xff7a00, 0.58),
        danger: rgb(0xd1423b),
        shadow: rgba(0x1a2233, 0.22),
        scrollbar: rgba(0x0f1728, 0.26),
    };

    pub fn iced(&self) -> iced::Theme {
        iced::Theme::custom(
            if self.is_dark {
                "Folio Dark"
            } else {
                "Folio Light"
            }
            .to_string(),
            iced::theme::Palette {
                background: self.canvas,
                text: self.text,
                primary: self.accent,
                success: self.accent,
                warning: self.danger,
                danger: self.danger,
            },
        )
    }
}

pub fn surface(palette: Palette) -> container::Style {
    container::Style {
        background: Some(palette.surface.into()),
        text_color: Some(palette.text),
        ..container::Style::default()
    }
}

pub fn canvas(palette: Palette) -> container::Style {
    container::Style {
        background: Some(palette.canvas.into()),
        text_color: Some(palette.text),
        ..container::Style::default()
    }
}

/// A one pixel rule.
pub fn rule(palette: Palette) -> container::Style {
    container::Style {
        background: Some(palette.border.into()),
        ..container::Style::default()
    }
}

/// Tooltips, toasts and other floating surfaces.
pub fn popover(palette: Palette) -> container::Style {
    container::Style {
        background: Some(palette.raised.into()),
        text_color: Some(palette.text),
        border: Border {
            color: palette.border,
            width: 1.0,
            radius: border::radius(RADIUS),
        },
        shadow: Shadow {
            color: palette.shadow,
            offset: Vector::new(0.0, 4.0),
            blur_radius: 16.0,
        },
        ..container::Style::default()
    }
}

/// A flat button that only shows a background on interaction, or when
/// `active` (a toggled state).
pub fn ghost_button(palette: Palette, status: button::Status, active: bool) -> button::Style {
    let background = match status {
        button::Status::Pressed => Some(palette.pressed),
        button::Status::Hovered if active => Some(palette.accent_soft),
        button::Status::Hovered => Some(palette.hover),
        _ if active => Some(palette.accent_soft),
        _ => None,
    };

    button::Style {
        background: background.map(Background::Color),
        text_color: match status {
            button::Status::Disabled => palette.faint,
            _ if active => palette.accent,
            _ => palette.text,
        },
        border: border::rounded(RADIUS),
        ..button::Style::default()
    }
}

pub fn primary_button(palette: Palette, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => iced::theme::palette::mix(palette.accent, palette.text, 0.12),
        button::Status::Pressed => iced::theme::palette::mix(palette.accent, palette.canvas, 0.15),
        _ => palette.accent,
    };

    button::Style {
        background: Some(background.into()),
        text_color: palette.on_accent,
        border: border::rounded(RADIUS),
        ..button::Style::default()
    }
}

/// A full-width row in a list.
pub fn row_button(palette: Palette, status: button::Status, active: bool) -> button::Style {
    button::Style {
        text_color: if active { palette.text } else { palette.muted },
        border: border::rounded(RADIUS - 1.0),
        ..ghost_button(palette, status, active)
    }
}

pub fn input(palette: Palette, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });

    text_input::Style {
        background: if focused {
            palette.raised.into()
        } else {
            palette.hover.into()
        },
        border: Border {
            color: if focused {
                palette.accent
            } else {
                Color::TRANSPARENT
            },
            width: 1.0,
            radius: border::radius(RADIUS - 1.0),
        },
        icon: palette.muted,
        placeholder: palette.faint,
        value: palette.text,
        selection: palette.selection,
    }
}

pub fn scroller(palette: Palette, status: scrollable::Status) -> scrollable::Style {
    let engaged = matches!(
        status,
        scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered: true,
            ..
        } | scrollable::Status::Dragged { .. }
    );

    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: if engaged {
                Color {
                    a: palette.scrollbar.a * 1.6,
                    ..palette.scrollbar
                }
            } else {
                palette.scrollbar
            }
            .into(),
            border: border::rounded(4.0),
        },
    };

    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: palette.raised.into(),
            border: border::rounded(RADIUS).color(palette.border).width(1.0),
            shadow: Shadow::default(),
            icon: palette.muted,
        },
    }
}
