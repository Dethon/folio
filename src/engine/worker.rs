use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use pdfium_render::prelude::*;

use crate::trace;

use super::table::Rule;
use super::text::{self, Glyph, Rect};
use super::{
    Bitmap, CopyRequest, DocInfo, Event, Job, JobKind, MATCH_LIMIT, Match, Open, OutlineItem,
    PageSize, PageText, SearchRequest, Shared, THUMBNAIL_WIDTH, TILE, TileKey, Wanted, markdown,
    page_pixels, print,
};

type Emit = Box<dyn Fn(Event) + Send>;

/// Parsed pages kept alive between tasks. Tiles of the same page arrive in
/// bursts, and re-parsing a content stream per tile would dominate.
const PAGE_CACHE: usize = 8;

/// A tile finished this recently is not rendered again: the UI may have
/// asked for it once more before our result reached it.
const RECENT_WINDOW: Duration = Duration::from_millis(300);

const OUTLINE_LIMIT: usize = 20_000;

pub(crate) fn run(shared: Arc<Shared>, emit: Emit) {
    let pdfium = pdfium();
    let mut next_id = 1u64;

    trace::mark("engine: pdfium bound");

    loop {
        let Some(Open { path, plan }) = next_open(&shared) else {
            return;
        };

        let pdfium = match pdfium {
            Ok(pdfium) => pdfium,
            Err(error) => {
                emit(Event::OpenFailed {
                    path,
                    error: error.clone(),
                });
                continue;
            }
        };

        let document = match pdfium.load_pdf_from_file(&path, None) {
            Ok(document) => document,
            Err(error) => {
                emit(Event::OpenFailed {
                    path,
                    error: describe_open_error(&error),
                });
                continue;
            }
        };

        let info = Arc::new(describe(&document, next_id, &path));
        next_id += 1;

        trace::mark("engine: document described");

        // Start on what the UI is expected to ask for, unless it has moved
        // on to another document already.
        if let Some(plan) = plan {
            let wanted = plan(&info);

            if let Ok(mut queue) = shared.queue.lock()
                && queue.open.is_none()
            {
                queue.doc = info.id;
                queue.wanted = wanted;
            }
        }

        emit(Event::Opened(info.clone()));

        let session = Session {
            document: &document,
            info,
            shared: &shared,
            emit: &emit,
            pages: VecDeque::new(),
            recent: VecDeque::new(),
            copy: None,
            search: None,
            job: None,
        };

        if session.serve() == Exit::Shutdown {
            return;
        }
    }
}

/// PDFium can be bound once per process, so every engine shares this
/// instance. The bindings serialise calls internally.
fn pdfium() -> &'static Result<Pdfium, String> {
    static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();

    PDFIUM.get_or_init(load_pdfium)
}

fn load_pdfium() -> Result<Pdfium, String> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));

    let bindings = beside_exe
        .as_deref()
        .map(Pdfium::pdfium_platform_library_name_at_path)
        .and_then(|path| Pdfium::bind_to_library(path).ok())
        .map(Ok)
        .unwrap_or_else(Pdfium::bind_to_system_library)
        .map_err(|error| {
            format!(
                "Could not load the PDFium library ({error}). \
                 It should sit next to the application executable."
            )
        })?;

    Ok(Pdfium::new(bindings))
}

/// Blocks until a document should be opened. `None` means shut down.
fn next_open(shared: &Shared) -> Option<Open> {
    let mut queue = shared.queue.lock().ok()?;

    loop {
        if queue.shutdown {
            return None;
        }

        if let Some(open) = queue.open.take() {
            return Some(open);
        }

        // Nothing is open, so nothing else in the queue can be served.
        queue.wanted = Wanted::default();
        queue.copy = None;
        queue.search = None;
        queue.jobs.clear();

        queue = shared.wake.wait(queue).ok()?;
    }
}

fn describe_open_error(error: &PdfiumError) -> String {
    match error {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            "This document is password protected.".into()
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError) => {
            "This file is not a valid PDF, or it is damaged.".into()
        }
        PdfiumError::IoError(error) => error.to_string(),
        other => format!("{other:?}"),
    }
}

fn describe(document: &PdfDocument, id: u64, path: &Path) -> DocInfo {
    let pages = document
        .pages()
        .page_sizes()
        .unwrap_or_default()
        .iter()
        .map(|rect| PageSize {
            width: rect.width().value.max(1.0),
            height: rect.height().value.max(1.0),
        })
        .collect();

    let title = document
        .metadata()
        .get(PdfDocumentMetadataTagType::Title)
        .map(|tag| tag.value().trim().to_owned())
        .filter(|title| !title.is_empty())
        .or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_default();

    let mut outline = Vec::new();

    if let Some(root) = document.bookmarks().root() {
        collect_outline(root, 0, &mut outline);
    }

    DocInfo {
        id,
        path: path.to_path_buf(),
        title,
        pages,
        outline,
    }
}

fn collect_outline(first: PdfBookmark, depth: u16, out: &mut Vec<OutlineItem>) {
    let mut next = Some(first);

    while let Some(bookmark) = next {
        if out.len() >= OUTLINE_LIMIT || depth > 32 {
            return;
        }

        out.push(OutlineItem {
            title: bookmark
                .title()
                .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default(),
            depth,
            page: bookmark
                .destination()
                .and_then(|destination| destination.page_index().ok())
                .map(|index| index as u32),
        });

        if let Some(child) = bookmark.first_child() {
            collect_outline(child, depth + 1, out);
        }

        next = bookmark.next_sibling();
    }
}

#[derive(PartialEq)]
enum Exit {
    /// Another document is waiting in the queue.
    Reopen,
    Shutdown,
}

enum Task {
    Tile(TileKey),
    Text(u32),
    Thumbnail(u32),
    Copy(CopyRequest),
    CopyStep,
    Search(SearchRequest),
    SearchStep,
    Start(Job),
    Step,
}

struct Copy {
    request: CopyRequest,
    page: u32,
    out: String,
}

struct Search {
    id: u64,
    needle: text::Needle,
    from: u32,
    /// Pages searched so far.
    done: u32,
    found: usize,
}

enum Active {
    Export {
        dest: PathBuf,
        page: u32,
        pages: Vec<markdown::Page>,
    },
    Print(print::Session),
}

impl Active {
    fn kind(&self) -> JobKind {
        match self {
            Active::Export { .. } => JobKind::Export,
            Active::Print(_) => JobKind::Print,
        }
    }
}

struct Session<'s, 'p> {
    document: &'s PdfDocument<'p>,
    info: Arc<DocInfo>,
    shared: &'s Shared,
    emit: &'s Emit,
    pages: VecDeque<(u32, PdfPage<'p>)>,
    recent: VecDeque<(TileKey, Instant)>,
    copy: Option<Copy>,
    search: Option<Search>,
    job: Option<Active>,
}

impl<'p> Session<'_, 'p> {
    fn serve(mut self) -> Exit {
        loop {
            let task = {
                let Ok(mut queue) = self.shared.queue.lock() else {
                    return Exit::Shutdown;
                };

                loop {
                    if queue.shutdown {
                        return Exit::Shutdown;
                    }

                    if queue.open.is_some() {
                        return Exit::Reopen;
                    }

                    // Left over from a document that has since been replaced.
                    if queue.doc != self.info.id {
                        queue.wanted = Wanted::default();
                    }

                    if !queue.wanted.tiles.is_empty() {
                        break Task::Tile(queue.wanted.tiles.remove(0));
                    }

                    if !queue.wanted.texts.is_empty() {
                        break Task::Text(queue.wanted.texts.remove(0));
                    }

                    // A copy is interactive, so it runs ahead of long jobs.
                    if let Some(request) = queue.copy.take() {
                        break Task::Copy(request);
                    }

                    if self.copy.is_some() {
                        break Task::CopyStep;
                    }

                    // So is a search: its results show as the query is typed.
                    if let Some(request) = queue.search.take() {
                        break Task::Search(request);
                    }

                    if self.search.is_some() {
                        break Task::SearchStep;
                    }

                    if self.job.is_some() {
                        break Task::Step;
                    }

                    if let Some(job) = queue.jobs.pop_front() {
                        break Task::Start(job);
                    }

                    if !queue.wanted.thumbnails.is_empty() {
                        break Task::Thumbnail(queue.wanted.thumbnails.remove(0));
                    }

                    let Ok(guard) = self.shared.wake.wait(queue) else {
                        return Exit::Shutdown;
                    };
                    queue = guard;
                }
            };

            match task {
                Task::Tile(key) => self.tile(key),
                Task::Text(page) => self.text(page),
                Task::Thumbnail(page) => self.thumbnail(page),
                Task::Copy(request) => {
                    self.copy = Some(Copy {
                        page: request.from.page,
                        request,
                        out: String::new(),
                    });
                }
                Task::CopyStep => self.copy_step(),
                Task::Search(request) => {
                    let last = (self.info.pages.len() as u32).saturating_sub(1);

                    self.search = text::Needle::new(&request.query).map(|needle| Search {
                        id: request.id,
                        needle,
                        from: request.from.min(last),
                        done: 0,
                        found: 0,
                    });
                }
                Task::SearchStep => self.search_step(),
                Task::Start(job) => self.start(job),
                Task::Step => self.step(),
            }
        }
    }

    fn page(&mut self, index: u32) -> Option<&PdfPage<'p>> {
        if let Some(position) = self.pages.iter().position(|(page, _)| *page == index) {
            if position != 0 {
                let entry = self.pages.remove(position)?;
                self.pages.push_front(entry);
            }
        } else {
            let page = self.document.pages().get(index as PdfPageIndex).ok()?;

            self.pages.push_front((index, page));
            self.pages.truncate(PAGE_CACHE);
        }

        self.pages.front().map(|(_, page)| page)
    }

    fn size(&self, page: u32) -> Option<PageSize> {
        self.info.pages.get(page as usize).copied()
    }

    fn tile(&mut self, key: TileKey) {
        let now = Instant::now();

        self.recent
            .retain(|(_, at)| now.duration_since(*at) < RECENT_WINDOW);

        if self.recent.iter().any(|(recent, _)| *recent == key) {
            return;
        }

        let Some(size) = self.size(key.page) else {
            return;
        };

        let (width, height) = page_pixels(size, key.scale());
        let x = key.col as u32 * TILE;
        let y = key.row as u32 * TILE;

        if x >= width || y >= height {
            return;
        }

        let region = Region {
            page_width: width,
            page_height: height,
            x,
            y,
            width: TILE.min(width - x),
            height: TILE.min(height - y),
        };

        let Some(page) = self.page(key.page) else {
            return;
        };

        if let Some(bitmap) = render(page, region) {
            trace::mark("engine: first tile rendered");
            self.recent.push_back((key, Instant::now()));

            (self.emit)(Event::Tile {
                doc: self.info.id,
                key,
                bitmap,
            });
        }
    }

    fn thumbnail(&mut self, index: u32) {
        let Some(size) = self.size(index) else {
            return;
        };

        let scale = THUMBNAIL_WIDTH as f32 / size.width;
        let (width, height) = page_pixels(size, scale);
        // Guard against absurdly tall pages.
        let height = height.min(THUMBNAIL_WIDTH * 8);

        let region = Region {
            page_width: width,
            page_height: height,
            x: 0,
            y: 0,
            width,
            height,
        };

        let Some(page) = self.page(index) else {
            return;
        };

        if let Some(bitmap) = render(page, region) {
            (self.emit)(Event::Thumbnail {
                doc: self.info.id,
                page: index,
                bitmap,
            });
        }
    }

    fn text(&mut self, index: u32) {
        let text = self
            .page(index)
            .and_then(|page| extract(page, false))
            .unwrap_or_default();

        (self.emit)(Event::Text {
            doc: self.info.id,
            page: index,
            text: Arc::new(text),
        });
    }

    fn start(&mut self, job: Job) {
        let page_count = self.info.pages.len() as u32;

        self.job = Some(match job {
            Job::Export { dest } => Active::Export {
                dest,
                page: 0,
                pages: Vec::with_capacity(page_count as usize),
            },
            Job::Print(target) => match print::Session::begin(target, &self.info) {
                Ok(session) => Active::Print(session),
                Err(error) => {
                    (self.emit)(Event::JobFailed {
                        kind: JobKind::Print,
                        error,
                    });
                    return;
                }
            },
        });
    }

    /// Appends one page's worth of selected text.
    fn copy_step(&mut self) {
        let Some(mut copy) = self.copy.take() else {
            return;
        };

        let CopyRequest {
            from,
            to,
            granularity,
        } = copy.request;
        let last = (self.info.pages.len() as u32).saturating_sub(1);

        if copy.page > to.page.min(last) || self.info.pages.is_empty() {
            (self.emit)(Event::Copied(
                copy.out.trim_end_matches(['\n', ' ']).to_owned(),
            ));
            return;
        }

        let page = copy.page;

        if let Some(text) = self.page(page).and_then(|page| extract(page, false)) {
            let range = text::selected_range(&text, page, &from, &to, granularity);
            let piece = text.text(range);

            if !copy.out.is_empty() && !copy.out.ends_with('\n') && !piece.is_empty() {
                copy.out.push('\n');
            }
            copy.out.push_str(&piece);
        }

        copy.page += 1;
        self.copy = Some(copy);
    }

    /// Searches one more page.
    fn search_step(&mut self) {
        let Some(mut search) = self.search.take() else {
            return;
        };

        let pages = self.info.pages.len() as u32;

        if search.done >= pages || search.found >= MATCH_LIMIT {
            (self.emit)(Event::SearchDone {
                doc: self.info.id,
                search: search.id,
                capped: search.found >= MATCH_LIMIT,
            });
            return;
        }

        let index = (search.from + search.done) % pages;

        // A search walks the whole document, so a page it has to load is not
        // kept: that would push the pages on screen out of the cache.
        let text = match self.pages.iter().find(|(page, _)| *page == index) {
            Some((_, page)) => extract(page, false),
            None => self
                .document
                .pages()
                .get(index as PdfPageIndex)
                .ok()
                .and_then(|page| extract(&page, false)),
        };

        if let Some(text) = text {
            let matches: Vec<Match> = text
                .find(&search.needle)
                .into_iter()
                .map(|range| Match {
                    page: index,
                    start: range.start,
                    rects: text.rects(range),
                })
                .filter(|found| !found.rects.is_empty())
                .take(MATCH_LIMIT - search.found)
                .collect();

            if !matches.is_empty() {
                search.found += matches.len();

                (self.emit)(Event::Matches {
                    doc: self.info.id,
                    search: search.id,
                    matches,
                });
            }
        }

        search.done += 1;
        self.search = Some(search);
    }

    /// Advances the active job by one page.
    fn step(&mut self) {
        let Some(job) = self.job.take() else {
            return;
        };

        let kind = job.kind();
        let page_count = self.info.pages.len() as u32;

        let outcome: Result<Option<Active>, String> = match job {
            Active::Export {
                dest,
                page,
                mut pages,
            } => {
                if page < page_count {
                    let size = self.size(page).unwrap_or(PageSize {
                        width: 612.0,
                        height: 792.0,
                    });
                    let (text, rules) = self
                        .page(page)
                        .map(|page| (extract(page, true).unwrap_or_default(), rules(page)))
                        .unwrap_or_default();

                    pages.push(markdown::Page::new(&text, size, rules));

                    (self.emit)(Event::Progress {
                        kind,
                        done: page + 1,
                        total: page_count,
                    });

                    Ok(Some(Active::Export {
                        dest,
                        page: page + 1,
                        pages,
                    }))
                } else {
                    let output = markdown::render(&pages, &self.info.outline);

                    std::fs::write(&dest, output)
                        .map(|()| {
                            (self.emit)(Event::Exported(dest));
                            None
                        })
                        .map_err(|error| error.to_string())
                }
            }
            Active::Print(mut session) => match session.step() {
                Ok(Some((done, total))) => {
                    (self.emit)(Event::Progress { kind, done, total });
                    Ok(Some(Active::Print(session)))
                }
                Ok(None) => {
                    (self.emit)(Event::Printed);
                    Ok(None)
                }
                Err(error) => Err(error),
            },
        };

        match outcome {
            Ok(next) => self.job = next,
            Err(error) => (self.emit)(Event::JobFailed { kind, error }),
        }
    }
}

struct Region {
    page_width: u32,
    page_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Renders a region of a page straight into a buffer we own, so the pixels
/// travel to the GPU without an intermediate copy.
fn render(page: &PdfPage, region: Region) -> Option<Bitmap> {
    let mut rgba = vec![0xff_u8; region.width as usize * region.height as usize * 4];

    {
        let mut bitmap = PdfBitmap::from_bytes(
            region.width as Pixels,
            region.height as Pixels,
            PdfBitmapFormat::BGRA,
            &mut rgba,
        )
        .ok()?;

        let config = PdfRenderConfig::new()
            .set_fixed_size(region.page_width as Pixels, region.page_height as Pixels)
            .set_origin(-(region.x as Pixels), -(region.y as Pixels))
            // The buffer starts out opaque white already.
            .clear_before_rendering(false)
            .set_reverse_byte_order(true)
            .render_annotations(true);

        page.render_into_bitmap_with_config(&mut bitmap, &config)
            .ok()?;
    }

    Some(Bitmap {
        width: region.width,
        height: region.height,
        rgba,
    })
}

/// Maps PDF page space (origin bottom-left, unrotated, possibly offset by the
/// crop box) to view space (origin top-left, rotated as displayed).
struct ViewMap {
    origin: (f32, f32),
    x_axis: (f32, f32),
    y_axis: (f32, f32),
}

impl ViewMap {
    fn new(page: &PdfPage) -> Self {
        // Probe PDFium's own transform at a high scale and recover the
        // affine map from three points. Page rotation is always a multiple
        // of 90°, so the axes can be snapped to exact unit vectors.
        const SCALE: f32 = 64.0;
        const SPAN: f32 = 1024.0;

        let config = PdfRenderConfig::new().scale_page_by_factor(SCALE);
        let probe = |x: f32, y: f32| {
            page.points_to_pixels(PdfPoints::new(x), PdfPoints::new(y), &config)
                .map(|(x, y)| (x as f32 / SCALE, y as f32 / SCALE))
        };

        match (probe(0.0, 0.0), probe(SPAN, 0.0), probe(0.0, SPAN)) {
            (Ok(origin), Ok(x), Ok(y)) => Self {
                origin,
                x_axis: (
                    ((x.0 - origin.0) / SPAN).round(),
                    ((x.1 - origin.1) / SPAN).round(),
                ),
                y_axis: (
                    ((y.0 - origin.0) / SPAN).round(),
                    ((y.1 - origin.1) / SPAN).round(),
                ),
            },
            _ => Self {
                origin: (0.0, page.height().value),
                x_axis: (1.0, 0.0),
                y_axis: (0.0, -1.0),
            },
        }
    }

    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.origin.0 + self.x_axis.0 * x + self.y_axis.0 * y,
            self.origin.1 + self.x_axis.1 * x + self.y_axis.1 * y,
        )
    }

    /// Whether text running at `angle` in page space ends up anything other
    /// than left-to-right on screen.
    fn is_rotated(&self, angle: f32) -> bool {
        let (sin, cos) = angle.sin_cos();
        let across = self.x_axis.0 * cos + self.y_axis.0 * sin;
        let down = self.x_axis.1 * cos + self.y_axis.1 * sin;

        down.abs() > 0.2 || across < 0.0
    }

    fn rect(&self, rect: &PdfRect) -> Rect {
        let a = self.point(rect.left().value, rect.bottom().value);
        let b = self.point(rect.right().value, rect.top().value);

        Rect {
            x0: a.0.min(b.0),
            y0: a.1.min(b.1),
            x1: a.0.max(b.0),
            y1: a.1.max(b.1),
        }
    }
}

/// Paths with more segments than this are artwork, not ruling.
const RULE_PATH_SEGMENTS: usize = 4_000;
/// Upper bound on segments examined per page, so a page of dense vector art
/// cannot stall an export.
const RULE_PAGE_SEGMENTS: usize = 40_000;
/// Anything at most this thick is a line, however it was painted.
const RULE_THICKNESS: f32 = 3.0;
const RULE_MIN_LENGTH: f32 = 6.0;

/// Collects the straight horizontal and vertical lines drawn on a page, in
/// view space. These are what tables are recognised by.
fn rules(page: &PdfPage) -> Vec<Rule> {
    let map = ViewMap::new(page);
    let mut out = Vec::new();
    let mut budget = RULE_PAGE_SEGMENTS;

    collect_rules(page.objects().iter(), &[], &map, &mut budget, &mut out);
    out
}

fn collect_rules<'a>(
    objects: impl Iterator<Item = PdfPageObject<'a>>,
    // Matrices of the enclosing form objects, innermost first.
    forms: &[PdfMatrix],
    map: &ViewMap,
    budget: &mut usize,
    out: &mut Vec<Rule>,
) {
    for object in objects {
        if let Some(form) = object.as_x_object_form_object() {
            if forms.len() < 4
                && let Ok(matrix) = form.matrix()
            {
                let mut nested = vec![matrix];
                nested.extend_from_slice(forms);
                collect_rules(form.iter(), &nested, map, budget, out);
            }
            continue;
        }

        let Some(path) = object.as_path_object() else {
            continue;
        };

        let segments = path.segments();
        let count = segments.len() as usize;

        if !(2..=RULE_PATH_SEGMENTS).contains(&count) || count > *budget {
            continue;
        }

        *budget -= count;

        let Ok(own) = path.matrix() else {
            continue;
        };

        // Path coordinates are local: apply the path's matrix, then each
        // enclosing form's, then the page's own transform.
        let place = |x: PdfPoints, y: PdfPoints| {
            let mut point = own.apply_to_points(x, y);

            for form in forms {
                point = form.apply_to_points(point.0, point.1);
            }

            map.point(point.0.value, point.1.value)
        };

        let mut edges: Vec<((f32, f32), (f32, f32))> = Vec::new();
        let mut start = (0.0, 0.0);
        let mut current = (0.0, 0.0);
        let mut bounds = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        let mut curved = false;

        for segment in segments.iter() {
            let point = place(segment.x(), segment.y());

            bounds = (
                bounds.0.min(point.0),
                bounds.1.min(point.1),
                bounds.2.max(point.0),
                bounds.3.max(point.1),
            );

            match segment.segment_type() {
                PdfPathSegmentType::MoveTo => start = point,
                PdfPathSegmentType::LineTo => edges.push((current, point)),
                _ => curved = true,
            }

            current = point;

            if segment.is_close() {
                edges.push((current, start));
                current = start;
            }
        }

        let (width, height) = (bounds.2 - bounds.0, bounds.3 - bounds.1);

        // A sliver, stroked or filled, is a single rule along its length.
        if width.min(height) <= RULE_THICKNESS {
            if !curved && width.max(height) >= RULE_MIN_LENGTH {
                out.push(if width >= height {
                    Rule {
                        horizontal: true,
                        at: (bounds.1 + bounds.3) * 0.5,
                        from: bounds.0,
                        to: bounds.2,
                    }
                } else {
                    Rule {
                        horizontal: false,
                        at: (bounds.0 + bounds.2) * 0.5,
                        from: bounds.1,
                        to: bounds.3,
                    }
                });
            }
            continue;
        }

        // Otherwise only outlines count: the edges of a stroked box or of a
        // whole grid drawn as one path. A filled area is a background.
        if !path.is_stroked().unwrap_or(false) {
            continue;
        }

        let straight: Vec<Rule> = edges
            .iter()
            .filter_map(|(a, b)| {
                let (dx, dy) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());

                if dy <= 0.7 && dx >= RULE_MIN_LENGTH {
                    Some(Rule {
                        horizontal: true,
                        at: (a.1 + b.1) * 0.5,
                        from: a.0.min(b.0),
                        to: a.0.max(b.0),
                    })
                } else if dx <= 0.7 && dy >= RULE_MIN_LENGTH {
                    Some(Rule {
                        horizontal: false,
                        at: (a.0 + b.0) * 0.5,
                        from: a.1.min(b.1),
                        to: a.1.max(b.1),
                    })
                } else {
                    None
                }
            })
            .collect();

        // A box with rounded corners: its four sides stop short of one
        // another, so give the frame its full extent or they would read as
        // unrelated rules.
        let sides = edges
            .iter()
            .filter(|(a, b)| (b.0 - a.0).abs().max((b.1 - a.1).abs()) > 0.5)
            .count();

        if curved && straight.len() == 4 && sides == 4 {
            for (horizontal, at) in [
                (true, bounds.1),
                (true, bounds.3),
                (false, bounds.0),
                (false, bounds.2),
            ] {
                out.push(Rule {
                    horizontal,
                    at,
                    from: if horizontal { bounds.0 } else { bounds.1 },
                    to: if horizontal { bounds.2 } else { bounds.3 },
                });
            }
        } else {
            out.extend(straight);
        }
    }
}

/// Reads the text layer. `styled` additionally resolves font size and style
/// per glyph, which costs extra library calls and is only needed for export.
fn extract(page: &PdfPage, styled: bool) -> Option<PageText> {
    let map = ViewMap::new(page);
    let text = page.text().ok()?;
    let chars = text.chars();
    let mut glyphs = Vec::with_capacity(chars.len());
    let mut fonts = Fonts::default();
    // Which font each glyph uses, parallel to `glyphs`.
    let mut glyph_fonts: Vec<usize> = Vec::new();

    for char in chars.iter() {
        let ch = match char.unicode_char() {
            Some('\r') | None => continue,
            Some('\n') => '\n',
            // PDFium reports a hyphen that breaks a word across lines as U+0002.
            Some('\u{2}') => '-',
            Some('\u{a0}') => ' ',
            Some(ch) if ch.is_control() || matches!(ch, '\u{fffe}' | '\u{ffff}') => continue,
            Some(ch) => ch,
        };

        let rect = if ch.is_whitespace() {
            Rect::default()
        } else {
            char.loose_bounds()
                .or_else(|_| char.tight_bounds())
                .map(|bounds| map.rect(&bounds))
                .unwrap_or_default()
        };

        let mut glyph = Glyph {
            ch,
            rect,
            size: 0.0,
            style: 0,
        };

        if styled {
            let mut font = usize::MAX;

            if !rect.is_empty() {
                glyph.size = char.scaled_font_size().value;
                font = fonts.intern(&char);
                glyph.style = fonts.entries[font].style;
                fonts.entries[font].observe(ch, rect.width() / glyph.size.max(0.1));

                if map.is_rotated(char.angle_radians().unwrap_or(0.0)) {
                    glyph.style |= markdown::ROTATED;
                }
            }

            glyph_fonts.push(font);
        }

        glyphs.push(glyph);
    }

    // Judged once per font, not once per glyph: it sorts every advance the
    // font was seen at.
    let monospaced: Vec<bool> = fonts.entries.iter().map(Font::is_monospaced).collect();

    for (glyph, font) in glyphs.iter_mut().zip(&glyph_fonts) {
        if monospaced.get(*font).copied().unwrap_or(false) {
            glyph.style |= text::MONO;
        }
    }

    Some(PageText::new(glyphs))
}

/// Per-page font table. Style is a property of the font, so it is resolved
/// once per font rather than once per glyph.
#[derive(Default)]
struct Fonts {
    entries: Vec<Font>,
}

struct Font {
    name: String,
    style: u8,
    /// Advance of each glyph seen, in ems.
    advances: Vec<f32>,
    /// Whether letters that differ in width in any proportional design have
    /// both been seen: without them, equal advances prove nothing (digits
    /// alone are the same width in most fonts).
    saw_narrow: bool,
    saw_wide: bool,
}

impl Font {
    fn observe(&mut self, ch: char, advance: f32) {
        if advance > 0.05 {
            self.advances.push(advance);
            self.saw_narrow |= matches!(
                ch,
                'i' | 'l' | 'I' | 'j' | 't' | 'f' | '.' | ',' | ':' | ';' | '\'' | '!' | '|'
            );
            self.saw_wide |= matches!(
                ch,
                'm' | 'w' | 'M' | 'W' | '@' | 'G' | 'O' | 'D' | 'H' | 'N'
            );
        }
    }

    /// Many embedded fonts do not declare themselves fixed pitch; uniform
    /// advances give them away. Nearly uniform is enough: a fixed-pitch
    /// font may still carry the odd wide glyph.
    fn is_monospaced(&self) -> bool {
        if self.style & text::MONO != 0 {
            return true;
        }

        if self.advances.len() < 12 || !self.saw_narrow || !self.saw_wide {
            return false;
        }

        let mut sorted = self.advances.clone();
        sorted.sort_by(f32::total_cmp);

        let median = sorted[sorted.len() / 2];
        let typical = sorted
            .iter()
            .filter(|advance| (**advance - median).abs() <= median * 0.03)
            .count();

        typical * 100 >= sorted.len() * 96
    }
}

impl Fonts {
    fn intern(&mut self, char: &PdfPageTextChar) -> usize {
        let name = char.font_name();

        if let Some(index) = self.entries.iter().position(|font| font.name == name) {
            return index;
        }

        self.entries.push(Font {
            style: style_of(char, &name.to_ascii_lowercase()),
            name,
            advances: Vec::new(),
            saw_narrow: false,
            saw_wide: false,
        });

        self.entries.len() - 1
    }
}

fn style_of(char: &PdfPageTextChar, name: &str) -> u8 {
    let mut style = 0;

    let heavy = matches!(
        char.font_weight(),
        Some(
            PdfFontWeight::Weight600
                | PdfFontWeight::Weight700Bold
                | PdfFontWeight::Weight800
                | PdfFontWeight::Weight900
        )
    );

    if heavy
        || ["bold", "black", "heavy", "semibold", "demi"]
            .iter()
            .any(|hint| name.contains(hint))
    {
        style |= text::BOLD;
    }

    if char.font_is_italic() || name.contains("italic") || name.contains("oblique") {
        style |= text::ITALIC;
    }

    if char.font_is_fixed_pitch()
        || ["mono", "courier", "consolas", "menlo"]
            .iter()
            .any(|hint| name.contains(hint))
    {
        style |= text::MONO;
    }

    style
}
