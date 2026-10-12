//! The page view: continuous scrolling over a virtualised stack of pages.
//!
//! [`Viewer`] holds the state — layout, scroll position, zoom, the tile cache
//! and the selection — and [`view`] returns a widget that paints it and
//! reports raw [`Interaction`]s. Only pages intersecting the viewport are
//! ever laid out, painted or rendered, so cost is independent of document
//! length.

use std::num::NonZeroUsize;
use std::ops::Range;
use std::sync::Arc;

use iced::advanced::image::{self, Renderer as _};
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse::click;
use iced::advanced::renderer::{self, Quad, Renderer as _};
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::keyboard;
use iced::mouse;
use iced::window;
use iced::{Border, Color, Element, Event, Length, Point, Rectangle, Shadow, Size, Vector, border};
use lru::LruCache;

use crate::engine::text::{self, Rect};
use crate::engine::{
    Anchor, Bitmap, DocInfo, Granularity, Match, PageSize, PageText, TILE, TileKey, Wanted,
    page_pixels,
};
use crate::trace;
use crate::ui::theme::Palette;

/// Space around the page stack, in logical pixels.
const MARGIN: f32 = 20.0;
/// Space between pages.
const GAP: f32 = 14.0;
/// Logical pixels per point at 100 %.
const POINT: f32 = 96.0 / 72.0;

pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 8.0;

const ZOOM_STEPS: [f32; 17] = [
    0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 3.0, 4.0, 6.0, 8.0,
];

/// Memory budget for rendered tiles.
const TILE_BUDGET: usize = 256 * 1024 * 1024;
const TEXT_CACHE: usize = 48;
const THUMBNAIL_CACHE: usize = 256;

const SCROLLBAR_WIDTH: f32 = 8.0;
const SCROLLBAR_INSET: f32 = 3.0;
const SCROLLBAR_MIN: f32 = 36.0;
/// Distance a mouse wheel notch scrolls.
const WHEEL_LINE: f32 = 56.0;

/// How close to the viewport's edges a search match may sit before the view
/// moves to show it. The top allows for the find bar floating there.
const REVEAL_TOP: f64 = 72.0;
const REVEAL_EDGE: f64 = 32.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Zoom follows the window so the widest page fills it.
    #[default]
    Width,
    /// Zoom follows the window so a whole page is visible.
    Page,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Selection {
    anchor: Anchor,
    focus: Anchor,
    granularity: Granularity,
}

struct Tile {
    handle: image::Handle,
    bytes: usize,
}

pub struct Viewer {
    doc: Option<Arc<DocInfo>>,
    /// `offsets[i]` is the summed height in points of the pages before `i`.
    offsets: Vec<f64>,
    widest: f32,
    zoom: f32,
    fit: Fit,
    /// Content position at the viewport's top-left, in logical pixels.
    scroll_x: f64,
    scroll_y: f64,
    viewport: Size,
    scale_factor: f32,
    tiles: LruCache<TileKey, Tile>,
    tile_bytes: usize,
    /// Scale of the tiles on screen before the last zoom change. They are
    /// shown stretched until tiles at the new scale arrive.
    stale_scale: Option<u32>,
    thumbnails: LruCache<u32, image::Handle>,
    texts: LruCache<u32, Arc<PageText>>,
    selection: Option<Selection>,
    /// Search matches, in document order.
    matches: Vec<Match>,
    /// The match the reader is on.
    current: Option<usize>,
    /// Where the search counts from: the first match at or after it becomes
    /// the current one.
    origin: Anchor,
    /// Sign of the last vertical scroll, to prefetch in that direction.
    heading: f64,
    /// Tiles in view that had not been rendered when last asked.
    missing: usize,
}

impl Default for Viewer {
    fn default() -> Self {
        Self {
            doc: None,
            offsets: vec![0.0],
            widest: 1.0,
            zoom: 1.0,
            fit: Fit::Width,
            scroll_x: 0.0,
            scroll_y: 0.0,
            viewport: Size::ZERO,
            scale_factor: 1.0,
            tiles: LruCache::unbounded(),
            tile_bytes: 0,
            stale_scale: None,
            thumbnails: LruCache::new(NonZeroUsize::new(THUMBNAIL_CACHE).unwrap()),
            texts: LruCache::new(NonZeroUsize::new(TEXT_CACHE).unwrap()),
            selection: None,
            matches: Vec::new(),
            current: None,
            origin: Anchor::start_of(0),
            heading: 1.0,
            missing: 0,
        }
    }
}

impl Viewer {
    pub fn document(&self) -> Option<&Arc<DocInfo>> {
        self.doc.as_ref()
    }

    pub fn set_document(&mut self, doc: Arc<DocInfo>) {
        let mut offsets = Vec::with_capacity(doc.pages.len() + 1);
        let mut total = 0.0f64;

        offsets.push(0.0);
        for page in &doc.pages {
            total += page.height as f64;
            offsets.push(total);
        }

        self.widest = doc.pages.iter().map(|page| page.width).fold(1.0, f32::max);
        self.offsets = offsets;
        self.doc = Some(doc);
        self.tiles.clear();
        self.tile_bytes = 0;
        self.stale_scale = None;
        self.thumbnails.clear();
        self.texts.clear();
        self.selection = None;
        self.clear_matches();
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.fit = Fit::Width;
        self.refit();

        // Fitting the width of a small page to a large window would open the
        // document absurdly magnified.
        if self.zoom > 1.5 {
            self.fit = Fit::Manual;
            self.zoom = 1.25;
        }
    }

    /// Whether the size of the view is known yet.
    pub fn has_viewport(&self) -> bool {
        self.viewport.height > 0.0
    }

    pub fn viewport(&self) -> Size {
        self.viewport
    }

    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Whether every tile in view had been rendered when [`wanted`] was last
    /// called.
    ///
    /// [`wanted`]: Self::wanted
    pub fn is_complete(&self) -> bool {
        self.missing == 0
    }

    pub fn page_count(&self) -> u32 {
        self.doc.as_ref().map_or(0, |doc| doc.pages.len() as u32)
    }

    fn size(&self, page: u32) -> PageSize {
        self.doc
            .as_ref()
            .and_then(|doc| doc.pages.get(page as usize).copied())
            .unwrap_or(PageSize {
                width: 1.0,
                height: 1.0,
            })
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn fit(&self) -> Fit {
        self.fit
    }

    /// Logical pixels per point.
    fn ppp(&self) -> f64 {
        (self.zoom * POINT) as f64
    }

    /// Device pixels per point: the scale tiles are rendered at.
    fn device_scale(&self) -> f32 {
        self.zoom * POINT * self.scale_factor
    }

    fn page_top(&self, page: u32) -> f64 {
        MARGIN as f64 + self.offsets[page as usize] * self.ppp() + page as f64 * GAP as f64
    }

    fn content_height(&self) -> f64 {
        let pages = self.page_count();

        if pages == 0 {
            return 0.0;
        }

        self.page_top(pages - 1) + self.size(pages - 1).height as f64 * self.ppp() + MARGIN as f64
    }

    fn content_width(&self) -> f64 {
        (self.widest as f64 * self.ppp() + 2.0 * MARGIN as f64).max(self.viewport.width as f64)
    }

    fn max_scroll(&self) -> (f64, f64) {
        (
            (self.content_width() - self.viewport.width as f64).max(0.0),
            (self.content_height() - self.viewport.height as f64).max(0.0),
        )
    }

    fn clamp(&mut self) {
        let (max_x, max_y) = self.max_scroll();

        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
    }

    /// The first page whose bottom edge is below `y` (content space).
    fn page_at(&self, y: f64) -> u32 {
        let pages = self.page_count();

        if pages == 0 {
            return 0;
        }

        let (mut low, mut high) = (0u32, pages - 1);

        while low < high {
            let middle = (low + high) / 2;
            let bottom = self.page_top(middle) + self.size(middle).height as f64 * self.ppp();

            if bottom < y {
                low = middle + 1;
            } else {
                high = middle;
            }
        }

        low
    }

    fn pages_between(&self, top: f64, bottom: f64) -> Range<u32> {
        let pages = self.page_count();

        if pages == 0 {
            return 0..0;
        }

        let first = self.page_at(top);
        let mut last = self.page_at(bottom);

        if self.page_top(last) <= bottom {
            last += 1;
        }

        first..last.clamp(first, pages)
    }

    pub fn visible_pages(&self) -> Range<u32> {
        self.pages_between(self.scroll_y, self.scroll_y + self.viewport.height as f64)
    }

    /// The page the reader is on: the one under a line a third of the way
    /// down the viewport.
    pub fn current_page(&self) -> u32 {
        self.page_at(self.scroll_y + self.viewport.height as f64 * 0.35)
    }

    /// Geometry of a page in device pixels relative to the viewport origin.
    /// Snapping here is what keeps tiles on the pixel grid.
    fn device_rect(&self, page: u32) -> (i64, i64, u32, u32) {
        let size = self.size(page);
        let scale = self.scale_factor as f64;
        let (width, height) = page_pixels(size, self.device_scale());
        let x = (self.content_width() - size.width as f64 * self.ppp()) / 2.0 - self.scroll_x;
        let y = self.page_top(page) - self.scroll_y;

        (
            (x * scale).round() as i64,
            (y * scale).round() as i64,
            width,
            height,
        )
    }

    /// Geometry of a page in logical pixels relative to the viewport origin.
    fn page_rect(&self, page: u32) -> Rectangle {
        let (x, y, width, height) = self.device_rect(page);
        let scale = self.scale_factor;

        Rectangle {
            x: x as f32 / scale,
            y: y as f32 / scale,
            width: width as f32 / scale,
            height: height as f32 / scale,
        }
    }

    // --- Navigation -------------------------------------------------------

    pub fn scroll_by(&mut self, dx: f32, dy: f32) {
        if dy != 0.0 {
            self.heading = dy.signum() as f64;
        }

        self.scroll_x += dx as f64;
        self.scroll_y += dy as f64;
        self.clamp();
    }

    /// Scrolls by a fraction of the viewport height.
    pub fn scroll_by_viewport(&mut self, fraction: f32) {
        self.scroll_by(0.0, (self.viewport.height - 40.0).max(40.0) * fraction);
    }

    pub fn scroll_to_fraction(&mut self, fraction: f32) {
        self.scroll_y = self.max_scroll().1 * fraction.clamp(0.0, 1.0) as f64;
        self.clamp();
    }

    pub fn go_to_page(&mut self, page: u32) {
        if self.page_count() == 0 {
            return;
        }

        let page = page.min(self.page_count() - 1);

        // Leave the gap above the page in view, so it reads as a page top.
        self.scroll_y = self.page_top(page) - GAP.min(MARGIN) as f64;
        self.clamp();
    }

    pub fn go_to_start(&mut self) {
        self.scroll_y = 0.0;
    }

    pub fn go_to_end(&mut self) {
        self.scroll_y = self.max_scroll().1;
    }

    pub fn set_viewport(&mut self, size: Size) {
        if size == self.viewport {
            return;
        }

        let first = !self.has_viewport();

        self.viewport = size;

        if self.fit != Fit::Manual {
            self.refit();
        }

        self.clamp();

        // A match found before the view had a size could not be shown.
        if first {
            self.reveal_current();
        }
    }

    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        if scale_factor > 0.0 && scale_factor != self.scale_factor {
            self.stale_scale = Some(self.device_scale().to_bits());
            self.scale_factor = scale_factor;
        }
    }

    // --- Zoom -------------------------------------------------------------

    fn fit_zoom(&self, fit: Fit) -> Option<f32> {
        if self.viewport.width <= 0.0 || self.doc.is_none() {
            return None;
        }

        let width = (self.viewport.width - 2.0 * MARGIN - SCROLLBAR_WIDTH) / (self.widest * POINT);

        match fit {
            Fit::Width => Some(width),
            Fit::Page => {
                let page = self.size(self.current_page());
                let height = (self.viewport.height - 2.0 * MARGIN) / (page.height * POINT);

                Some(width.min(height))
            }
            Fit::Manual => None,
        }
    }

    fn refit(&mut self) {
        // Anchor at the top edge so the reader's place is kept.
        if let Some(zoom) = self.fit_zoom(self.fit) {
            self.apply_zoom(zoom, Some(Point::ORIGIN));
        }
    }

    pub fn set_fit(&mut self, fit: Fit) {
        self.fit = fit;

        let page = self.current_page();
        self.refit();

        if fit == Fit::Page {
            self.go_to_page(page);
        }
    }

    /// Zooms keeping the document point under `anchor` (viewport space)
    /// where it is; the viewport centre when `None`.
    pub fn zoom_to(&mut self, zoom: f32, anchor: Option<Point>) {
        self.fit = Fit::Manual;
        self.apply_zoom(zoom, anchor);
    }

    pub fn zoom_by(&mut self, factor: f32, anchor: Option<Point>) {
        self.zoom_to(self.zoom * factor, anchor);
    }

    /// Steps to the next preset level in the given direction.
    pub fn zoom_step(&mut self, direction: i8) {
        let current = self.zoom;
        let next = if direction > 0 {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|step| *step > current * 1.01)
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|step| *step < current * 0.99)
        };

        if let Some(zoom) = next {
            self.zoom_to(zoom, None);
        }
    }

    fn apply_zoom(&mut self, zoom: f32, anchor: Option<Point>) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);

        if zoom == self.zoom || self.doc.is_none() {
            self.zoom = zoom;
            return;
        }

        let anchor = anchor.unwrap_or(Point::new(
            self.viewport.width / 2.0,
            self.viewport.height / 2.0,
        ));

        // Express the anchored point independently of zoom: a page, an
        // offset into it in points, and a horizontal offset from the centre.
        let content_y = self.scroll_y + anchor.y as f64;
        let page = self.page_at(content_y);
        let into_page = (content_y - self.page_top(page)) / self.ppp();
        let from_centre =
            (self.scroll_x + anchor.x as f64 - self.content_width() / 2.0) / self.ppp();

        self.stale_scale = Some(self.device_scale().to_bits());
        self.zoom = zoom;

        self.scroll_y = self.page_top(page) + into_page * self.ppp() - anchor.y as f64;
        self.scroll_x = self.content_width() / 2.0 + from_centre * self.ppp() - anchor.x as f64;
        self.clamp();
    }

    // --- Content ----------------------------------------------------------

    fn tile_range(&self, page: u32, scale: f32, top: f64, bottom: f64) -> (Range<u16>, Range<u16>) {
        // Intersect the viewport with the page in the page's own pixels at
        // `scale`, which may differ from the current scale for stale tiles.
        let ratio = (scale / self.device_scale()) as f64;
        let device = self.scale_factor as f64;
        let (x, _, _, _) = self.device_rect(page);
        let (width, height) = page_pixels(self.size(page), scale);
        let page_y = (self.page_top(page) - self.scroll_y) * device;

        let left = ((-(x as f64)) * ratio).max(0.0);
        let right = ((self.viewport.width as f64 * device - x as f64) * ratio).min(width as f64);
        let top = ((top - self.scroll_y) * device - page_y) * ratio;
        let bottom = ((bottom - self.scroll_y) * device - page_y) * ratio;
        let top = top.max(0.0);
        let bottom = bottom.min(height as f64);

        if right <= left || bottom <= top {
            return (0..0, 0..0);
        }

        let tile = TILE as f64;

        (
            (left / tile) as u16..((right / tile).ceil() as u16),
            (top / tile) as u16..((bottom / tile).ceil() as u16),
        )
    }

    /// Everything missing for the current view, most urgent first: visible
    /// tiles nearest the centre, then text, then a screenful of prefetch in
    /// the direction of travel.
    pub fn wanted(&mut self, sidebar_thumbnails: Range<u32>) -> Wanted {
        let mut wanted = Wanted::default();

        self.missing = 0;

        if self.doc.is_none() || self.viewport.height <= 0.0 {
            return wanted;
        }

        let scale = self.device_scale();
        let top = self.scroll_y;
        let bottom = top + self.viewport.height as f64;
        let centre = (top + bottom) / 2.0;
        let mut complete = true;

        let mut tiles: Vec<(f64, TileKey)> = Vec::new();

        let collect =
            |viewer: &mut Self, top: f64, bottom: f64, tiles: &mut Vec<(f64, TileKey)>| {
                let mut missing = false;

                for page in viewer.pages_between(top, bottom) {
                    let (cols, rows) = viewer.tile_range(page, scale, top, bottom);
                    let page_top = viewer.page_top(page);

                    for row in rows {
                        for col in cols.clone() {
                            let key = TileKey {
                                page,
                                scale: scale.to_bits(),
                                col,
                                row,
                            };

                            // `get` also marks the tile as recently used.
                            if viewer.tiles.get(&key).is_none() {
                                let y = page_top
                                    + (row as f64 + 0.5) * TILE as f64 / viewer.scale_factor as f64;

                                tiles.push(((y - centre).abs(), key));
                                missing = true;
                            }
                        }
                    }
                }

                missing
            };

        if collect(self, top, bottom, &mut tiles) {
            complete = false;
        }

        tiles.sort_by(|a, b| a.0.total_cmp(&b.0));
        wanted.tiles.extend(tiles.drain(..).map(|(_, key)| key));
        self.missing = wanted.tiles.len();

        let visible = self.visible_pages();

        for page in visible.clone() {
            if self.texts.get(&page).is_none() {
                wanted.texts.push(page);
            }
        }

        // Prefetch ahead, then a little behind.
        let span = self.viewport.height as f64;
        let (ahead, behind) = if self.heading >= 0.0 {
            ((bottom, bottom + span), (top - span * 0.5, top))
        } else {
            ((top - span, top), (bottom, bottom + span * 0.5))
        };

        for (from, to) in [ahead, behind] {
            collect(
                self,
                from.max(0.0),
                to.min(self.content_height()),
                &mut tiles,
            );
            tiles.sort_by(|a, b| a.0.total_cmp(&b.0));
            wanted.tiles.extend(tiles.drain(..).map(|(_, key)| key));
        }

        if complete {
            self.stale_scale = None;
        }

        // Thumbnails stand in for pages whose tiles are still rendering.
        for page in visible.chain(sidebar_thumbnails) {
            if self.thumbnails.get(&page).is_none() && !wanted.thumbnails.contains(&page) {
                wanted.thumbnails.push(page);
            }
        }

        wanted
    }

    pub fn insert_tile(&mut self, key: TileKey, bitmap: Bitmap) {
        let bytes = bitmap.rgba.len();
        let handle = image::Handle::from_rgba(bitmap.width, bitmap.height, bitmap.rgba);

        if let Some(previous) = self.tiles.put(key, Tile { handle, bytes }) {
            self.tile_bytes -= previous.bytes;
        }

        self.tile_bytes += bytes;

        while self.tile_bytes > TILE_BUDGET {
            match self.tiles.pop_lru() {
                Some((_, tile)) => self.tile_bytes -= tile.bytes,
                None => break,
            }
        }
    }

    pub fn insert_thumbnail(&mut self, page: u32, bitmap: Bitmap) {
        self.thumbnails.put(
            page,
            image::Handle::from_rgba(bitmap.width, bitmap.height, bitmap.rgba),
        );
    }

    pub fn thumbnail(&self, page: u32) -> Option<&image::Handle> {
        self.thumbnails.peek(&page)
    }

    pub fn insert_text(&mut self, page: u32, text: Arc<PageText>) {
        self.texts.put(page, text);
    }

    // --- Selection --------------------------------------------------------

    /// Converts a viewport position to the nearest position on a page.
    fn locate(&self, point: Point) -> Option<Anchor> {
        if self.page_count() == 0 {
            return None;
        }

        let page = self.page_at(self.scroll_y + point.y as f64);
        let rect = self.page_rect(page);
        let size = self.size(page);

        Some(Anchor {
            page,
            x: (point.x - rect.x) / rect.width * size.width,
            y: (point.y - rect.y) / rect.height * size.height,
        })
    }

    pub fn is_over_text(&self, point: Point) -> bool {
        self.locate(point).is_some_and(|anchor| {
            self.texts
                .peek(&anchor.page)
                .is_some_and(|text| text.hit(anchor.x, anchor.y))
        })
    }

    pub fn press(&mut self, point: Point, granularity: Granularity, extend: bool) {
        let Some(anchor) = self.locate(point) else {
            return;
        };

        self.selection = match self.selection {
            Some(selection) if extend => Some(Selection {
                focus: anchor,
                ..selection
            }),
            _ => Some(Selection {
                anchor,
                focus: anchor,
                granularity,
            }),
        };
    }

    pub fn drag(&mut self, point: Point) {
        // Dragging past the edge scrolls the view along.
        if point.y < 0.0 {
            self.scroll_by(0.0, point.y.max(-60.0));
        } else if point.y > self.viewport.height {
            self.scroll_by(0.0, (point.y - self.viewport.height).min(60.0));
        }

        let clamped = Point::new(point.x, point.y.clamp(0.0, self.viewport.height));

        if let (Some(focus), Some(selection)) = (self.locate(clamped), self.selection.as_mut()) {
            selection.focus = focus;
        }
    }

    pub fn select_all(&mut self) {
        if self.page_count() > 0 {
            self.selection = Some(Selection {
                anchor: Anchor::start_of(0),
                focus: Anchor::end_of(self.page_count() - 1),
                granularity: Granularity::Char,
            });
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// The selection in document order, if it covers anything.
    pub fn selection(&self) -> Option<(Anchor, Anchor, Granularity)> {
        let selection = self.selection?;

        if selection.anchor == selection.focus && selection.granularity == Granularity::Char {
            return None;
        }

        let text = self.texts.peek(&selection.anchor.page).map(Arc::as_ref);

        Some(if selection.anchor.precedes(&selection.focus, text) {
            (selection.anchor, selection.focus, selection.granularity)
        } else {
            (selection.focus, selection.anchor, selection.granularity)
        })
    }

    fn selection_rects(&self, page: u32) -> Vec<Rect> {
        let Some((from, to, granularity)) = self.selection() else {
            return Vec::new();
        };

        if page < from.page || page > to.page {
            return Vec::new();
        }

        self.texts.peek(&page).map_or_else(Vec::new, |text| {
            text.rects(text::selected_range(text, page, &from, &to, granularity))
        })
    }

    // --- Search -----------------------------------------------------------

    /// Forgets the matches of the last search and returns the page the next
    /// one should start from. The reader's place is kept: a query being
    /// refined stays on the match it is on while that is in view, and
    /// otherwise matches count from the top of the view.
    pub fn begin_search(&mut self) -> u32 {
        let visible = self.visible_pages();

        self.origin = match self
            .current
            .and_then(|current| self.matches.get(current))
            .filter(|found| visible.contains(&found.page))
        {
            Some(found) => Anchor {
                page: found.page,
                x: found.rects[0].x0,
                y: found.rects[0].y0,
            },
            None => {
                let page = self.page_at(self.scroll_y);

                Anchor {
                    page,
                    x: f32::NEG_INFINITY,
                    y: ((self.scroll_y - self.page_top(page)) / self.ppp()) as f32,
                }
            }
        };

        self.clear_matches();
        self.origin.page
    }

    pub fn clear_matches(&mut self) {
        self.matches.clear();
        self.current = None;
    }

    /// Takes in the matches of one page, which arrive in the order pages are
    /// searched: from the origin's page to the end, then from the start.
    pub fn add_matches(&mut self, matches: Vec<Match>) {
        let Some(page) = matches.first().map(|found| found.page) else {
            return;
        };

        let at = self.matches.partition_point(|found| found.page < page);
        let count = matches.len();

        // The first match to turn up at or after the origin is the nearest
        // one ahead of the reader.
        let nearest = if self.current.is_some() {
            None
        } else if page == self.origin.page {
            let (x, y) = (self.origin.x, self.origin.y);

            matches.iter().position(|found| {
                let rect = found.rects[0];

                rect.y0 > y + 1.0 || (rect.y0 >= y - 1.0 && rect.x0 >= x - 1.0)
            })
        } else {
            Some(0)
        };

        self.matches.splice(at..at, matches);

        if let Some(current) = self.current.as_mut() {
            if *current >= at {
                *current += count;
            }
        } else if let Some(nearest) = nearest {
            self.current = Some(at + nearest);
            self.reveal_current();
        }
    }

    /// Called once every page has been searched.
    pub fn finish_search(&mut self) {
        // Every match lies before the origin: wrap round to the first.
        if self.current.is_none() && !self.matches.is_empty() {
            self.current = Some(0);
            self.reveal_current();
        }
    }

    /// Moves to the next match (or the previous, for a negative
    /// `direction`), wrapping round at either end.
    pub fn step_match(&mut self, direction: i8) {
        let count = self.matches.len();

        if count == 0 {
            return;
        }

        self.current = Some(match self.current {
            Some(current) if direction < 0 => (current + count - 1) % count,
            Some(current) => (current + 1) % count,
            None => 0,
        });
        self.reveal_current();
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Position of the current match among all of them, counting from one.
    pub fn current_match(&self) -> Option<usize> {
        self.current.map(|current| current + 1)
    }

    /// The matches on `page`, and the index of the first among all matches.
    fn matches_on(&self, page: u32) -> (usize, &[Match]) {
        let start = self.matches.partition_point(|found| found.page < page);
        let end = self.matches.partition_point(|found| found.page <= page);

        (start, &self.matches[start..end])
    }

    fn reveal_current(&mut self) {
        if let Some(found) = self.current.and_then(|current| self.matches.get(current)) {
            let rect = found
                .rects
                .iter()
                .skip(1)
                .fold(found.rects[0], |all, rect| all.union(rect));

            self.reveal(found.page, rect);
        }
    }

    /// Scrolls just enough to bring a rectangle of a page (in points) into
    /// comfortable view; not at all if it already is.
    fn reveal(&mut self, page: u32, rect: Rect) {
        if page >= self.page_count() || self.viewport.height <= 0.0 {
            return;
        }

        let ppp = self.ppp();
        let (width, height) = (self.viewport.width as f64, self.viewport.height as f64);

        let top = self.page_top(page) + rect.y0 as f64 * ppp;
        let bottom = self.page_top(page) + rect.y1 as f64 * ppp;

        if top < self.scroll_y + REVEAL_TOP || bottom > self.scroll_y + height - REVEAL_EDGE {
            // A third of the way down, where the eye rests.
            let target = top - height / 3.0;

            self.heading = if target < self.scroll_y { -1.0 } else { 1.0 };
            self.scroll_y = target;
        }

        let page_left = (self.content_width() - self.size(page).width as f64 * ppp) / 2.0;
        let left = page_left + rect.x0 as f64 * ppp;
        let right = page_left + rect.x1 as f64 * ppp;

        if left < self.scroll_x + REVEAL_EDGE || right > self.scroll_x + width - REVEAL_EDGE {
            self.scroll_x = left - width / 3.0;
        }

        self.clamp();
    }

    // --- Scrollbar --------------------------------------------------------

    /// The scrollbar thumb in viewport space, when the content overflows.
    fn scrollbar(&self) -> Option<Rectangle> {
        let content = self.content_height();
        let view = self.viewport.height as f64;

        if content <= view + 1.0 || view <= 0.0 {
            return None;
        }

        let track = self.viewport.height - 2.0 * SCROLLBAR_INSET;
        let length = ((view / content) as f32 * track)
            .max(SCROLLBAR_MIN)
            .min(track);
        let progress = (self.scroll_y / (content - view)) as f32;

        Some(Rectangle {
            x: self.viewport.width - SCROLLBAR_WIDTH - SCROLLBAR_INSET,
            y: SCROLLBAR_INSET + (track - length) * progress,
            width: SCROLLBAR_WIDTH,
            height: length,
        })
    }
}

/// Raw input from the page view, in viewport coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Interaction {
    Resized(Size),
    Scrolled {
        dx: f32,
        dy: f32,
    },
    Zoomed {
        factor: f32,
        at: Point,
    },
    Pressed {
        at: Point,
        granularity: Granularity,
        extend: bool,
    },
    Dragged {
        at: Point,
    },
    Released,
    ScrolledTo {
        fraction: f32,
    },
}

pub fn view<'a, Message: 'a>(
    viewer: &'a Viewer,
    palette: Palette,
    on_interaction: impl Fn(Interaction) -> Message + 'a,
) -> Element<'a, Message> {
    Element::new(View {
        viewer,
        palette,
        on_interaction: Box::new(on_interaction),
    })
}

struct View<'a, Message> {
    viewer: &'a Viewer,
    palette: Palette,
    on_interaction: Box<dyn Fn(Interaction) -> Message + 'a>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum Drag {
    #[default]
    None,
    Select,
    /// Dragging the scrollbar thumb, grabbed this far below its top.
    Scrollbar {
        grab: f32,
    },
}

#[derive(Default)]
struct State {
    size: Size,
    modifiers: keyboard::Modifiers,
    drag: Drag,
    last_click: Option<click::Click>,
}

impl<Message> View<'_, Message> {
    fn scroll_fraction(&self, thumb: Rectangle, y: f32, grab: f32) -> f32 {
        let track = self.viewer.viewport.height - 2.0 * SCROLLBAR_INSET - thumb.height;

        if track <= 0.0 {
            0.0
        } else {
            (y - grab - SCROLLBAR_INSET) / track
        }
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for View<'_, Message> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let local = |point: Point| Point::new(point.x - bounds.x, point.y - bounds.y);
        let mut publish = |interaction| shell.publish((self.on_interaction)(interaction));

        match event {
            Event::Window(window::Event::RedrawRequested(_)) => {
                trace::mark("first redraw of the page view");

                if state.size != bounds.size() {
                    state.size = bounds.size();
                    publish(Interaction::Resized(bounds.size()));
                }
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers;
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(position) = cursor.position_in(bounds) else {
                    return;
                };

                let (dx, dy) = match *delta {
                    mouse::ScrollDelta::Lines { x, y } => (x * WHEEL_LINE, y * WHEEL_LINE),
                    mouse::ScrollDelta::Pixels { x, y } => (x, y),
                };

                if state.modifiers.command() {
                    publish(Interaction::Zoomed {
                        factor: (dy / WHEEL_LINE * 0.12).exp(),
                        at: position,
                    });
                } else if state.modifiers.shift() {
                    publish(Interaction::Scrolled { dx: -dy, dy: 0.0 });
                } else {
                    publish(Interaction::Scrolled { dx: -dx, dy: -dy });
                }

                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(position) = cursor.position_in(bounds) else {
                    return;
                };

                let thumb = self.viewer.scrollbar();
                let in_gutter =
                    position.x >= bounds.width - SCROLLBAR_WIDTH - 2.0 * SCROLLBAR_INSET;

                match thumb {
                    Some(thumb) if in_gutter => {
                        let grab = if position.y >= thumb.y && position.y <= thumb.y + thumb.height
                        {
                            position.y - thumb.y
                        } else {
                            // Clicking the track jumps the thumb under the cursor.
                            let grab = thumb.height / 2.0;
                            publish(Interaction::ScrolledTo {
                                fraction: self.scroll_fraction(thumb, position.y, grab),
                            });
                            grab
                        };

                        state.drag = Drag::Scrollbar { grab };
                    }
                    _ => {
                        let click =
                            click::Click::new(position, mouse::Button::Left, state.last_click);

                        state.last_click = Some(click);
                        state.drag = Drag::Select;

                        publish(Interaction::Pressed {
                            at: position,
                            granularity: match click.kind() {
                                click::Kind::Single => Granularity::Char,
                                click::Kind::Double => Granularity::Word,
                                click::Kind::Triple => Granularity::Line,
                            },
                            extend: state.modifiers.shift(),
                        });
                    }
                }

                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => match state.drag {
                Drag::Select => publish(Interaction::Dragged {
                    at: local(*position),
                }),
                Drag::Scrollbar { grab } => {
                    if let Some(thumb) = self.viewer.scrollbar() {
                        publish(Interaction::ScrolledTo {
                            fraction: self.scroll_fraction(thumb, local(*position).y, grab),
                        });
                    }
                }
                Drag::None => {}
            },
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.drag != Drag::None =>
            {
                state.drag = Drag::None;
                publish(Interaction::Released);
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();

        match state.drag {
            Drag::Select => return mouse::Interaction::Text,
            Drag::Scrollbar { .. } => return mouse::Interaction::Idle,
            Drag::None => {}
        }

        let Some(position) = cursor.position_in(bounds) else {
            return mouse::Interaction::None;
        };

        let in_gutter = self.viewer.scrollbar().is_some()
            && position.x >= bounds.width - SCROLLBAR_WIDTH - 2.0 * SCROLLBAR_INSET;

        if !in_gutter && self.viewer.is_over_text(position) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::Idle
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let viewer = self.viewer;
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();

        if viewer.doc.is_none() || viewer.viewport.height <= 0.0 {
            return;
        }

        let origin = Vector::new(bounds.x, bounds.y);
        let scale = viewer.device_scale();
        let device = viewer.scale_factor;
        let top = viewer.scroll_y;
        let bottom = top + viewer.viewport.height as f64;
        let pages = viewer.visible_pages();

        renderer.with_layer(bounds, |renderer| {
            for page in pages.clone() {
                let rect = viewer.page_rect(page) + origin;

                // Paper, with a soft shadow lifting it off the canvas. Quads
                // paint beneath images within a layer, so this never covers
                // the tiles drawn below.
                renderer.fill_quad(
                    Quad {
                        bounds: rect,
                        border: Border::default(),
                        shadow: Shadow {
                            color: self.palette.shadow,
                            offset: Vector::new(0.0, 1.0),
                            blur_radius: 10.0,
                        },
                        snap: true,
                    },
                    Color::WHITE,
                );

                let (cols, rows) = viewer.tile_range(page, scale, top, bottom);
                let tile_at = |col, row, scale: f32| {
                    viewer.tiles.peek(&TileKey {
                        page,
                        scale: scale.to_bits(),
                        col,
                        row,
                    })
                };

                let complete = rows
                    .clone()
                    .all(|row| cols.clone().all(|col| tile_at(col, row, scale).is_some()));

                if !complete {
                    // Underlay: the thumbnail, then whatever was on screen
                    // at the previous zoom, both stretched into place.
                    if let Some(thumbnail) = viewer.thumbnails.peek(&page) {
                        renderer.draw_image(image::Image::new(thumbnail.clone()), rect, bounds);
                    }

                    if let Some(stale) = viewer.stale_scale.map(f32::from_bits) {
                        let (stale_cols, stale_rows) = viewer.tile_range(page, stale, top, bottom);
                        let (stale_width, stale_height) = page_pixels(viewer.size(page), stale);
                        let step_x = rect.width / stale_width as f32;
                        let step_y = rect.height / stale_height as f32;

                        for row in stale_rows {
                            for col in stale_cols.clone() {
                                if let Some(tile) = tile_at(col, row, stale) {
                                    let x = (col as u32 * TILE) as f32;
                                    let y = (row as u32 * TILE) as f32;
                                    let width = (stale_width as f32 - x).min(TILE as f32);
                                    let height = (stale_height as f32 - y).min(TILE as f32);

                                    renderer.draw_image(
                                        image::Image::new(tile.handle.clone()),
                                        Rectangle {
                                            x: rect.x + x * step_x,
                                            y: rect.y + y * step_y,
                                            width: width * step_x,
                                            height: height * step_y,
                                        },
                                        bounds,
                                    );
                                }
                            }
                        }
                    }
                }

                let (page_width, page_height) = page_pixels(viewer.size(page), scale);

                for row in rows {
                    for col in cols.clone() {
                        if let Some(tile) = tile_at(col, row, scale) {
                            let x = col as u32 * TILE;
                            let y = row as u32 * TILE;

                            renderer.draw_image(
                                image::Image::new(tile.handle.clone()).snap(true),
                                Rectangle {
                                    x: rect.x + x as f32 / device,
                                    y: rect.y + y as f32 / device,
                                    width: (page_width - x).min(TILE) as f32 / device,
                                    height: (page_height - y).min(TILE) as f32 / device,
                                },
                                bounds,
                            );
                        }
                    }
                }
            }
        });

        // A second layer, so these paint above the page images.
        renderer.with_layer(bounds, |renderer| {
            for page in pages.clone() {
                let rect = viewer.page_rect(page) + origin;
                let size = viewer.size(page);
                let (sx, sy) = (rect.width / size.width, rect.height / size.height);

                let (first, matches) = viewer.matches_on(page);

                for (index, found) in matches.iter().enumerate() {
                    let current = viewer.current == Some(first + index);

                    for highlight in &found.rects {
                        renderer.fill_quad(
                            Quad {
                                bounds: Rectangle {
                                    x: rect.x + highlight.x0 * sx,
                                    y: rect.y + highlight.y0 * sy,
                                    width: highlight.width() * sx,
                                    height: highlight.height() * sy,
                                },
                                border: border::rounded(2.0),
                                ..Quad::default()
                            },
                            if current {
                                self.palette.found_current
                            } else {
                                self.palette.found
                            },
                        );
                    }
                }

                for highlight in viewer.selection_rects(page) {
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle {
                                x: rect.x + highlight.x0 * sx,
                                y: rect.y + highlight.y0 * sy,
                                width: highlight.width() * sx,
                                height: highlight.height() * sy,
                            },
                            ..Quad::default()
                        },
                        self.palette.selection,
                    );
                }
            }

            if let Some(thumb) = viewer.scrollbar() {
                let engaged = matches!(state.drag, Drag::Scrollbar { .. })
                    || cursor.position_in(bounds).is_some_and(|position| {
                        position.x >= bounds.width - SCROLLBAR_WIDTH - 2.0 * SCROLLBAR_INSET
                    });

                renderer.fill_quad(
                    Quad {
                        bounds: thumb + origin,
                        border: border::rounded(SCROLLBAR_WIDTH / 2.0),
                        ..Quad::default()
                    },
                    Color {
                        a: self.palette.scrollbar.a * if engaged { 1.7 } else { 1.0 },
                        ..self.palette.scrollbar
                    },
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use iced::mouse::{Button, ScrollDelta};

    use super::*;
    use crate::engine::text::Glyph;

    const LETTER: PageSize = PageSize {
        width: 612.0,
        height: 792.0,
    };

    fn viewer(pages: usize) -> Viewer {
        let mut viewer = Viewer::default();

        viewer.set_viewport(Size::new(800.0, 600.0));
        viewer.set_document(Arc::new(DocInfo {
            id: 1,
            path: PathBuf::new(),
            title: String::new(),
            pages: vec![LETTER; pages],
            outline: Vec::new(),
        }));
        viewer
    }

    /// A page with the word "hello" at (100, 100), in 10pt-wide glyphs.
    fn hello() -> Arc<PageText> {
        Arc::new(PageText::new(
            "hello"
                .chars()
                .enumerate()
                .map(|(index, ch)| Glyph {
                    ch,
                    rect: Rect {
                        x0: 100.0 + index as f32 * 10.0,
                        y0: 100.0,
                        x1: 110.0 + index as f32 * 10.0,
                        y1: 112.0,
                    },
                    size: 12.0,
                    style: 0,
                })
                .collect(),
        ))
    }

    /// Where a page-space point currently sits in the viewport.
    fn on_screen(viewer: &Viewer, page: u32, x: f32, y: f32) -> Point {
        let rect = viewer.page_rect(page);
        let size = viewer.size(page);

        Point::new(
            rect.x + x / size.width * rect.width,
            rect.y + y / size.height * rect.height,
        )
    }

    #[test]
    fn fits_the_page_width_to_the_viewport() {
        let mut viewer = viewer(3);
        let page = viewer.page_rect(0);

        assert_eq!(viewer.fit(), Fit::Width);
        assert!(page.x >= MARGIN - 1.0);
        assert!(page.x + page.width <= 800.0 - MARGIN + 1.0);

        viewer.set_viewport(Size::new(1000.0, 600.0));

        assert!(
            viewer.page_rect(0).width > page.width,
            "zoom follows the window"
        );
    }

    #[test]
    fn only_pages_in_view_are_visible() {
        let mut viewer = viewer(500);

        assert_eq!(viewer.visible_pages(), 0..1);
        assert_eq!(viewer.current_page(), 0);

        viewer.go_to_page(321);

        assert_eq!(viewer.current_page(), 321);
        assert!(viewer.visible_pages().len() <= 2);
        assert!(viewer.visible_pages().contains(&321));

        viewer.go_to_end();

        assert_eq!(viewer.current_page(), 499);
    }

    #[test]
    fn asks_for_visible_tiles_before_prefetch() {
        let mut viewer = viewer(10);
        let wanted = viewer.wanted(0..0);
        let scale = viewer.device_scale().to_bits();

        assert!(wanted.tiles.iter().all(|tile| tile.scale == scale));
        assert_eq!(wanted.texts, [0]);

        // The viewport shows the top of page 0; what is beyond it comes last.
        let visible: Vec<TileKey> = wanted
            .tiles
            .iter()
            .copied()
            .take_while(|tile| tile.page == 0 && tile.row <= 1)
            .collect();

        assert!(!visible.is_empty());
        assert!(wanted.tiles.len() > visible.len(), "prefetch follows");

        for key in &visible {
            viewer.insert_tile(
                *key,
                Bitmap {
                    width: 1,
                    height: 1,
                    rgba: vec![255; 4],
                },
            );
        }

        let remaining = viewer.wanted(0..0);

        assert!(visible.iter().all(|key| !remaining.tiles.contains(key)));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let mut viewer = viewer(10);

        viewer.go_to_page(4);

        let cursor = Point::new(300.0, 250.0);
        let before = viewer.locate(cursor).unwrap();

        viewer.zoom_by(2.5, Some(cursor));

        let after = viewer.locate(cursor).unwrap();

        assert_eq!(viewer.fit(), Fit::Manual);
        assert_eq!(after.page, before.page);
        assert!((after.x - before.x).abs() < 1.0, "{before:?} -> {after:?}");
        assert!((after.y - before.y).abs() < 1.0, "{before:?} -> {after:?}");
    }

    #[test]
    fn zoom_is_clamped() {
        let mut viewer = viewer(1);

        viewer.zoom_to(100.0, None);
        assert_eq!(viewer.zoom(), MAX_ZOOM);

        viewer.zoom_to(0.0, None);
        assert_eq!(viewer.zoom(), MIN_ZOOM);

        viewer.zoom_step(1);
        assert_eq!(viewer.zoom(), 0.25);
    }

    #[test]
    fn dragging_selects_text() {
        let mut viewer = viewer(2);

        viewer.insert_text(0, hello());

        // From the middle of "e" leftwards to before "h": dragging backwards.
        viewer.press(
            on_screen(&viewer, 0, 119.0, 106.0),
            Granularity::Char,
            false,
        );
        assert_eq!(viewer.selection(), None, "a click alone selects nothing");

        viewer.drag(on_screen(&viewer, 0, 95.0, 106.0));

        let (from, to, _) = viewer.selection().unwrap();
        let text = hello();
        let range = text::selected_range(&text, 0, &from, &to, Granularity::Char);

        assert_eq!(text.text(range), "he");
        assert_eq!(viewer.selection_rects(0).len(), 1);
        assert!(viewer.selection_rects(1).is_empty());

        viewer.clear_selection();
        assert_eq!(viewer.selection(), None);
    }

    #[test]
    fn double_click_selects_a_word() {
        let mut viewer = viewer(1);

        viewer.insert_text(0, hello());
        viewer.press(
            on_screen(&viewer, 0, 125.0, 106.0),
            Granularity::Word,
            false,
        );

        let rects = viewer.selection_rects(0);

        assert_eq!(rects.len(), 1);
        assert_eq!((rects[0].x0, rects[0].x1), (100.0, 150.0));
    }

    #[test]
    fn select_all_spans_the_document() {
        let mut viewer = viewer(7);

        viewer.select_all();

        let (from, to, _) = viewer.selection().unwrap();

        assert_eq!((from.page, to.page), (0, 6));
    }

    #[test]
    fn text_shows_the_text_cursor() {
        let mut viewer = viewer(1);

        viewer.insert_text(0, hello());

        assert!(viewer.is_over_text(on_screen(&viewer, 0, 120.0, 106.0)));
        assert!(!viewer.is_over_text(on_screen(&viewer, 0, 400.0, 400.0)));
    }

    #[test]
    fn widget_translates_input() {
        let viewer = viewer(10);
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            Size::new(800.0, 600.0),
            view(&viewer, Palette::DARK, |interaction| interaction),
        );

        ui.point_at(Point::new(300.0, 200.0));
        ui.simulate([
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(340.0, 260.0),
            }),
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::COMMAND,
            )),
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: ScrollDelta::Lines { x: 0.0, y: 1.0 },
            }),
        ]);

        let messages: Vec<Interaction> = ui.into_messages().collect();

        assert_eq!(
            messages[..4],
            [
                Interaction::Pressed {
                    at: Point::new(300.0, 200.0),
                    granularity: Granularity::Char,
                    extend: false,
                },
                Interaction::Dragged {
                    at: Point::new(340.0, 260.0),
                },
                Interaction::Released,
                Interaction::Scrolled {
                    dx: 0.0,
                    dy: WHEEL_LINE,
                },
            ]
        );

        match messages[4] {
            Interaction::Zoomed { factor, at } => {
                assert!(factor > 1.0);
                assert_eq!(at, Point::new(300.0, 200.0));
            }
            other => panic!("expected a zoom, got {other:?}"),
        }
    }

    /// A search match 50pt wide at (100, `y`) on `page`.
    fn found(page: u32, start: u32, y: f32) -> Match {
        Match {
            page,
            start,
            rects: vec![Rect {
                x0: 100.0,
                y0: y,
                x1: 150.0,
                y1: y + 12.0,
            }],
        }
    }

    /// Whether a match made by [`found`] is in comfortable view.
    fn in_view(viewer: &Viewer, page: u32, y: f32) -> bool {
        let top = on_screen(viewer, page, 100.0, y).y as f64;
        let bottom = on_screen(viewer, page, 100.0, y + 12.0).y as f64;

        top >= REVEAL_TOP - 0.5 && bottom <= 600.0 - REVEAL_EDGE + 0.5
    }

    #[test]
    fn search_goes_to_the_nearest_match_ahead() {
        let mut viewer = viewer(5);

        // A little way into the third page.
        viewer.go_to_page(2);
        viewer.scroll_by(0.0, 300.0);

        assert_eq!(viewer.begin_search(), 2);

        // Matches arrive from the reader's page on, then wrap round. The
        // first on this page has already scrolled past.
        viewer.add_matches(vec![found(2, 0, 100.0), found(2, 9, 500.0)]);
        viewer.add_matches(vec![found(4, 0, 50.0)]);
        viewer.add_matches(vec![found(0, 0, 50.0)]);
        viewer.finish_search();

        assert_eq!(viewer.match_count(), 4);
        assert_eq!(viewer.current_match(), Some(3));
        assert!(in_view(&viewer, 2, 500.0));

        viewer.step_match(1);
        assert_eq!(viewer.current_match(), Some(4));
        assert!(in_view(&viewer, 4, 50.0));

        viewer.step_match(1);
        assert_eq!(viewer.current_match(), Some(1), "wraps to the start");
        assert!(in_view(&viewer, 0, 50.0));

        viewer.step_match(-1);
        assert_eq!(viewer.current_match(), Some(4), "and back to the end");
        assert!(in_view(&viewer, 4, 50.0));
    }

    #[test]
    fn search_wraps_when_no_match_lies_ahead() {
        let mut viewer = viewer(3);

        viewer.go_to_page(1);
        viewer.scroll_by(0.0, 300.0);
        viewer.begin_search();
        viewer.add_matches(vec![found(1, 0, 100.0)]);

        assert_eq!(viewer.current_match(), None, "it may yet turn up ahead");

        viewer.finish_search();

        assert_eq!(viewer.current_match(), Some(1));
        assert!(in_view(&viewer, 1, 100.0));
    }

    #[test]
    fn refining_a_query_stays_on_its_match() {
        let mut viewer = viewer(3);

        viewer.begin_search();
        viewer.add_matches(vec![found(0, 0, 100.0), found(0, 5, 300.0)]);
        viewer.step_match(1);

        assert_eq!(viewer.current_match(), Some(2));

        let scroll = viewer.scroll_y;

        // The longer query still matches at both places.
        assert_eq!(viewer.begin_search(), 0);
        assert_eq!(viewer.match_count(), 0);

        viewer.add_matches(vec![found(0, 0, 100.0), found(0, 7, 300.0)]);

        assert_eq!(viewer.current_match(), Some(2));
        assert_eq!(viewer.scroll_y, scroll, "a match in view does not move");
    }

    #[test]
    fn a_match_off_to_the_side_is_scrolled_to() {
        let mut viewer = viewer(1);

        viewer.zoom_to(4.0, None);
        viewer.scroll_by(-10_000.0, 0.0);
        viewer.begin_search();
        viewer.add_matches(vec![Match {
            page: 0,
            start: 0,
            rects: vec![Rect {
                x0: 500.0,
                y0: 30.0,
                x1: 540.0,
                y1: 42.0,
            }],
        }]);
        viewer.finish_search();

        let left = on_screen(&viewer, 0, 500.0, 30.0).x;
        let right = on_screen(&viewer, 0, 540.0, 30.0).x;

        assert!(left >= 0.0 && right <= 800.0, "{left}..{right}");
    }

    #[test]
    fn widget_drags_the_scrollbar() {
        let viewer = viewer(100);
        let thumb = viewer.scrollbar().expect("content overflows");
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            Size::new(800.0, 600.0),
            view(&viewer, Palette::DARK, |interaction| interaction),
        );

        ui.point_at(Point::new(thumb.x + 2.0, thumb.y + 2.0));
        ui.simulate([
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(thumb.x + 2.0, 600.0),
            }),
        ]);

        let messages: Vec<Interaction> = ui.into_messages().collect();

        // Dragging to the bottom edge runs past the end of the track; the
        // viewer clamps it.
        match messages[..] {
            [Interaction::ScrolledTo { fraction }] => assert!(fraction >= 1.0, "{fraction}"),
            _ => panic!("expected one scroll, got {messages:?}"),
        }
    }
}
