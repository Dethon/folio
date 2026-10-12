//! Toolbar and sidebar icons, from the Lucide set (ISC licence).
//!
//! The icons are rasterised here, at the exact size they are shown, and
//! drawn as images. That keeps the renderer's own SVG support out of the
//! program: it reads every installed font before drawing its first icon, in
//! case one contains text, and holds up the first frame doing so.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};

use iced::advanced::image::{self, Renderer as _};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget};
use iced::mouse;
use iced::{Color, Element, Length, Rectangle, Size};
use resvg::{tiny_skia, usvg};

/// The side of the square the icons are drawn on, in their own units.
const ARTBOARD: f32 = 24.0;

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($name),*
        }

        impl Icon {
            fn source(self) -> &'static [u8] {
                match self {
                    $(Icon::$name => {
                        include_bytes!(concat!("../../assets/icons/", $file, ".svg"))
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
    Search => "search",
    ChevronUp => "chevron-up",
}

/// Device pixels per logical pixel, as `f32` bits. Widgets are not told the
/// scale they are drawn at, so the application passes it on.
static SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// Sets the scale icons are rasterised for.
pub fn set_scale_factor(scale_factor: f32) {
    if scale_factor > 0.0 {
        SCALE.store(scale_factor.to_bits(), Ordering::Relaxed);
    }
}

/// An icon at a size in device pixels, in a colour.
type Key = (Icon, u32, [u8; 4]);

/// Every icon drawn so far. There are a few dozen at most: each icon is
/// shown at one size and in a handful of colours per theme.
static RASTERS: LazyLock<Mutex<HashMap<Key, image::Handle>>> = LazyLock::new(Mutex::default);

impl Icon {
    pub fn view<'a, Message: 'a>(self, size: f32, color: Color) -> Element<'a, Message> {
        Element::new(View {
            icon: self,
            size,
            color,
        })
    }

    fn raster(self, pixels: u32, color: Color) -> Option<image::Handle> {
        let key = (self, pixels, color.into_rgba8());
        let mut rasters = RASTERS.lock().ok()?;

        if let Some(handle) = rasters.get(&key) {
            return Some(handle.clone());
        }

        let handle = self.rasterise(pixels, key.2)?;

        rasters.insert(key, handle.clone());
        Some(handle)
    }

    fn rasterise(self, pixels: u32, [red, green, blue, alpha]: [u8; 4]) -> Option<image::Handle> {
        let tree = usvg::Tree::from_data(self.source(), &usvg::Options::default()).ok()?;
        let mut pixmap = tiny_skia::Pixmap::new(pixels, pixels)?;
        let scale = pixels as f32 / ARTBOARD;

        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );

        let mut rgba = pixmap.take();

        // The artwork is a single ink, so its coverage is all that is kept.
        for pixel in rgba.chunks_exact_mut(4) {
            let coverage = pixel[3] as u32;

            pixel[0] = red;
            pixel[1] = green;
            pixel[2] = blue;
            pixel[3] = (coverage * alpha as u32 / 255) as u8;
        }

        Some(image::Handle::from_rgba(pixels, pixels, rgba))
    }
}

struct View {
    icon: Icon,
    size: f32,
    color: Color,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for View {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.size), Length::Fixed(self.size))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(Size::new(self.size, self.size))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let scale = f32::from_bits(SCALE.load(Ordering::Relaxed));
        let pixels = (self.size * scale).ceil().max(1.0) as u32;

        if let Some(handle) = self.icon.raster(pixels, self.color) {
            let bounds = layout.bounds();

            renderer.draw_image(
                image::Image::new(handle)
                    .filter_method(image::FilterMethod::Nearest)
                    .snap(true),
                bounds,
                bounds,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterises_in_the_colour_asked_for() {
        let ink = Color::from_rgb8(10, 120, 250);
        let handle = Icon::Search.raster(32, ink).expect("icon renders");

        let image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = &handle
        else {
            panic!("expected pixels, got {handle:?}");
        };

        assert_eq!((*width, *height), (32, 32));

        let inked: Vec<&[u8]> = pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0)
            .collect();

        // A magnifying glass: mostly empty, but not blank.
        assert!(
            inked.len() > 40 && inked.len() < 32 * 32 / 2,
            "{}",
            inked.len()
        );
        assert!(inked.iter().all(|pixel| pixel[..3] == [10, 120, 250]));
        assert!(inked.iter().any(|pixel| pixel[3] == 255));

        // The same icon again is the same image, not a second rendering.
        assert_eq!(
            Icon::Search.raster(32, ink).map(|again| again.id()),
            Some(handle.id())
        );
    }
}
