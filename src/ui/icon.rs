//! Toolbar and sidebar icons, from the Lucide set (ISC licence).

use std::sync::LazyLock;

use iced::widget::svg::{self, Svg};
use iced::{Color, Length};

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Icon {
            $($name),*
        }

        impl Icon {
            fn handle(self) -> svg::Handle {
                match self {
                    $(Icon::$name => {
                        static HANDLE: LazyLock<svg::Handle> = LazyLock::new(|| {
                            svg::Handle::from_memory(
                                include_bytes!(concat!("../../assets/icons/", $file, ".svg"))
                                    .as_slice(),
                            )
                        });

                        HANDLE.clone()
                    })*
                }
            }
        }
    };
}

icons! {
    Sidebar => "panel-left",
    Open => "folder-open",
    ZoomOut => "minus",
    ZoomIn => "plus",
    Print => "printer",
    Export => "file-down",
    Sun => "sun",
    Moon => "moon",
    ChevronRight => "chevron-right",
    ChevronDown => "chevron-down",
    Outline => "list-tree",
    Pages => "gallery-vertical",
    FitWidth => "move-horizontal",
    FitPage => "move-vertical",
    Document => "file-text",
    Close => "x",
}

impl Icon {
    pub fn view<'a>(self, size: f32, color: Color) -> Svg<'a> {
        iced::widget::svg(self.handle())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_, _| svg::Style { color: Some(color) })
    }
}
