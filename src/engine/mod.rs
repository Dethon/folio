//! The document engine.
//!
//! PDFium is not thread safe, so a single worker thread owns the library and
//! the open document. The UI never blocks on it: it publishes what it would
//! like to have (tiles, thumbnails, text) and receives [`Event`]s as the
//! worker gets to them. Wanted lists are replaced wholesale on every viewport
//! change, which means work for content that has scrolled away is simply
//! never started.

pub mod markdown;
pub mod print;
pub mod table;
pub mod text;
mod worker;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

pub use text::{Anchor, Granularity, Link, PageText, Rect, Target};

/// Edge length of a render tile in device pixels.
///
/// The renderer packs images into 2048² textures with a pixel of padding all
/// round, so sixteen tiles of 510 fill one exactly; at 512, only nine would
/// fit. A tile is also just under 1 MiB of RGBA, well below the renderer's
/// threshold for synchronous texture uploads, so tiles appear the frame
/// they arrive.
pub const TILE: u32 = 510;

/// Width in device pixels of page thumbnails.
pub const THUMBNAIL_WIDTH: u32 = 200;

/// A search stops after this many matches: beyond it nobody is stepping
/// through them, and the list would only cost memory.
pub const MATCH_LIMIT: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: u32,
    /// Device pixels per point, as `f32` bits so the key is hashable.
    pub scale: u32,
    pub col: u16,
    pub row: u16,
}

impl TileKey {
    pub fn scale(&self) -> f32 {
        f32::from_bits(self.scale)
    }
}

/// Size in device pixels of a page rendered at `scale` pixels per point.
pub fn page_pixels(size: PageSize, scale: f32) -> (u32, u32) {
    (
        ((size.width * scale).round() as u32).max(1),
        ((size.height * scale).round() as u32).max(1),
    )
}

/// Tightly packed RGBA8 pixels.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bitmap({}x{})", self.width, self.height)
    }
}

/// Page dimensions in points, after rotation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone)]
pub struct OutlineItem {
    pub title: String,
    pub depth: u16,
    pub page: Option<u32>,
}

#[derive(Debug)]
pub struct DocInfo {
    /// Identifies this load; events carry it so stale ones can be dropped.
    pub id: u64,
    pub path: PathBuf,
    pub title: String,
    pub pages: Vec<PageSize>,
    pub outline: Vec<OutlineItem>,
}

/// One occurrence of a search query.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub page: u32,
    /// Index of the first glyph, which orders the matches of a page.
    pub start: u32,
    /// One rectangle in page space per line the match touches.
    pub rects: Vec<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Export,
    Print,
}

#[derive(Debug)]
pub enum Event {
    Opened(Arc<DocInfo>),
    OpenFailed {
        path: PathBuf,
        error: String,
    },
    Tile {
        doc: u64,
        key: TileKey,
        bitmap: Bitmap,
    },
    Thumbnail {
        doc: u64,
        page: u32,
        bitmap: Bitmap,
    },
    Text {
        doc: u64,
        page: u32,
        text: Arc<PageText>,
    },
    Progress {
        kind: JobKind,
        done: u32,
        total: u32,
    },
    Copied(String),
    /// The matches of one page. Pages arrive in the order they are searched,
    /// which starts from the page the search was asked from.
    Matches {
        doc: u64,
        search: u64,
        matches: Vec<Match>,
    },
    /// Every page has been searched, or the search hit [`MATCH_LIMIT`].
    SearchDone {
        doc: u64,
        search: u64,
        capped: bool,
    },
    Exported(PathBuf),
    Printed,
    JobFailed {
        kind: JobKind,
        error: String,
    },
}

/// Content the UI is missing, most urgent first.
#[derive(Debug, Default, PartialEq)]
pub struct Wanted {
    /// How many tiles of the document the UI had taken in when it drew this
    /// up. A tile sent since then is on its way and is not rendered again;
    /// one sent before and wanted all the same has been dropped, and is.
    pub received: u64,
    pub tiles: Vec<TileKey>,
    pub texts: Vec<u32>,
    pub thumbnails: Vec<u32>,
}

/// Says what a document will be asked for first, given its description. The
/// worker runs it as soon as the document is loaded and starts on the
/// answer, without waiting to hear from the UI.
pub type Plan = Box<dyn FnOnce(&Arc<DocInfo>) -> Wanted + Send>;

pub(crate) struct Open {
    pub path: PathBuf,
    pub plan: Option<Plan>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CopyRequest {
    pub from: Anchor,
    pub to: Anchor,
    pub granularity: Granularity,
}

pub(crate) struct SearchRequest {
    pub id: u64,
    pub query: String,
    pub from: u32,
}

pub(crate) enum Job {
    Export { dest: PathBuf },
    Print(print::Target),
}

#[derive(Default)]
pub(crate) struct Queue {
    pub open: Option<Open>,
    /// The document `wanted` was drawn up for.
    pub doc: u64,
    pub wanted: Wanted,
    pub copy: Option<CopyRequest>,
    pub search: Option<SearchRequest>,
    pub jobs: VecDeque<Job>,
    pub shutdown: bool,
}

pub(crate) struct Shared {
    pub queue: Mutex<Queue>,
    pub wake: Condvar,
}

/// Handle to the worker thread. Cheap to clone; the worker stops when the
/// last handle is dropped.
#[derive(Clone)]
pub struct Engine {
    shared: Arc<Shared>,
    _guard: Arc<Guard>,
}

struct Guard(Arc<Shared>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.0.queue.lock() {
            queue.shutdown = true;
        }
        self.0.wake.notify_all();
    }
}

impl Engine {
    /// Starts the worker. `emit` is called on the worker thread.
    pub fn spawn(emit: impl Fn(Event) + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });

        std::thread::Builder::new()
            .name("pdf-engine".into())
            .spawn({
                let shared = shared.clone();
                move || worker::run(shared, Box::new(emit))
            })
            .expect("spawn engine thread");

        Self {
            _guard: Arc::new(Guard(shared.clone())),
            shared,
        }
    }

    fn with_queue(&self, f: impl FnOnce(&mut Queue)) {
        if let Ok(mut queue) = self.shared.queue.lock() {
            f(&mut queue);
        }
        self.shared.wake.notify_one();
    }

    /// Opens a document, replacing the current one and cancelling its work.
    /// With a `plan`, rendering starts before [`Event::Opened`] is answered.
    pub fn open(&self, path: PathBuf, plan: Option<Plan>) {
        self.with_queue(|queue| {
            queue.open = Some(Open { path, plan });
            queue.wanted = Wanted::default();
            queue.copy = None;
            queue.search = None;
            queue.jobs.clear();
        });
    }

    /// Replaces the set of content the UI is missing from document `doc`.
    /// Ignored if that is no longer the document open.
    pub fn want(&self, doc: u64, wanted: Wanted) {
        self.with_queue(|queue| {
            queue.doc = doc;
            queue.wanted = wanted;
        });
    }

    /// Extracts the text between two anchors (in document order); answers
    /// with [`Event::Copied`].
    pub fn copy(&self, from: Anchor, to: Anchor, granularity: Granularity) {
        self.with_queue(|queue| {
            queue.copy = Some(CopyRequest {
                from,
                to,
                granularity,
            });
        });
    }

    /// Searches the document for `query`, starting at page `from` and
    /// wrapping round; answers with [`Event::Matches`] as pages turn some up
    /// and [`Event::SearchDone`] at the end, each carrying `id`. Replaces any
    /// search still running, and a blank query only does that.
    pub fn search(&self, id: u64, query: String, from: u32) {
        self.with_queue(|queue| queue.search = Some(SearchRequest { id, query, from }));
    }

    pub fn export_markdown(&self, dest: PathBuf) {
        self.with_queue(|queue| queue.jobs.push_back(Job::Export { dest }));
    }

    pub fn print(&self, target: print::Target) {
        self.with_queue(|queue| queue.jobs.push_back(Job::Print(target)));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    use super::*;

    pub(crate) struct Fixture {
        pub engine: Engine,
        pub events: Receiver<Event>,
        pub doc: Arc<DocInfo>,
    }

    impl Fixture {
        pub fn want(&self, wanted: Wanted) {
            self.engine.want(self.doc.id, wanted);
        }

        /// Waits for the first event `pick` accepts.
        pub fn wait<T>(&self, mut pick: impl FnMut(Event) -> Option<T>) -> T {
            loop {
                let event = self
                    .events
                    .recv_timeout(Duration::from_secs(20))
                    .expect("engine answered in time");

                if let Event::OpenFailed { error, .. } | Event::JobFailed { error, .. } = &event {
                    panic!("engine reported: {error}");
                }

                if let Some(value) = pick(event) {
                    return value;
                }
            }
        }
    }

    /// The sample document, written out where the engine can open it. The
    /// bytes are embedded so the tests also run on a machine other than the
    /// one that built them.
    pub(crate) fn fixture_path() -> PathBuf {
        static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

        PATH.get_or_init(|| {
            let path =
                std::env::temp_dir().join(format!("folio-fixture-{}.pdf", std::process::id()));

            std::fs::write(&path, include_bytes!("../../tests/fixtures/sample.pdf"))
                .expect("write fixture");
            path
        })
        .clone()
    }

    pub(crate) fn open_fixture() -> Fixture {
        let (sender, events) = mpsc::channel();
        let engine = Engine::spawn(move |event| {
            let _ = sender.send(event);
        });

        engine.open(fixture_path(), None);

        let mut fixture = Fixture {
            engine,
            events,
            doc: Arc::new(DocInfo {
                id: 0,
                path: PathBuf::new(),
                title: String::new(),
                pages: Vec::new(),
                outline: Vec::new(),
            }),
        };

        fixture.doc = fixture.wait(|event| match event {
            Event::Opened(doc) => Some(doc),
            _ => None,
        });

        fixture
    }

    #[test]
    fn describes_the_document() {
        let fixture = open_fixture();
        let doc = &fixture.doc;

        assert_eq!(doc.title, "Sample Document");
        assert_eq!(doc.pages.len(), 3);
        assert_eq!(
            doc.pages[0],
            PageSize {
                width: 612.0,
                height: 792.0
            }
        );

        let outline: Vec<_> = doc
            .outline
            .iter()
            .map(|item| (item.title.as_str(), item.depth, item.page))
            .collect();

        assert_eq!(
            outline,
            [("Introduction", 0, Some(0)), ("Results", 0, Some(1))]
        );
    }

    #[test]
    fn renders_tiles_at_the_requested_scale() {
        let fixture = open_fixture();
        let scale = 2.0f32;

        // At 2 px/pt a letter page is 1224 x 1584: the last column is a
        // partial tile.
        let corner = TileKey {
            page: 0,
            scale: scale.to_bits(),
            col: 2,
            row: 0,
        };
        let title = TileKey { col: 0, ..corner };

        fixture.want(Wanted {
            tiles: vec![title, corner],
            ..Wanted::default()
        });

        let mut seen = Vec::new();

        while seen.len() < 2 {
            let (key, bitmap) = fixture.wait(|event| match event {
                Event::Tile { key, bitmap, .. } => Some((key, bitmap)),
                _ => None,
            });

            assert_eq!(bitmap.rgba.len() as u32, bitmap.width * bitmap.height * 4);

            if key == title {
                assert_eq!((bitmap.width, bitmap.height), (TILE, TILE));
                // The heading puts ink on this tile, on white paper.
                assert!(bitmap.rgba.chunks(4).any(|pixel| pixel[0] < 64));
                assert_eq!(&bitmap.rgba[..4], [255, 255, 255, 255]);
            } else {
                assert_eq!((bitmap.width, bitmap.height), (1224 - 2 * TILE, TILE));
            }

            seen.push(key);
        }
    }

    #[test]
    fn renders_a_tile_again_only_once_it_is_known_to_have_arrived() {
        let fixture = open_fixture();
        let key = TileKey {
            page: 0,
            scale: 1.0f32.to_bits(),
            col: 0,
            row: 0,
        };
        // Whether asking for the tile, of a UI that has taken in `received`
        // of them, has it rendered. The text is asked for along with it
        // because it always answers, and after any tile.
        let ask = |received| {
            let mut rendered = false;

            fixture.want(Wanted {
                received,
                tiles: vec![key],
                texts: vec![0],
                ..Wanted::default()
            });

            fixture.wait(|event| match event {
                Event::Tile { .. } => {
                    rendered = true;
                    None
                }
                Event::Text { .. } => Some(()),
                _ => None,
            });

            rendered
        };

        assert!(ask(0), "rendered the first time");

        // Asked for by a UI that has not seen it yet: it is on its way.
        assert!(!ask(0), "not rendered while in flight");

        // The UI has had it, and wants it still: it let go of it.
        assert!(ask(1), "rendered again once dropped");
        assert!(!ask(1), "and is on its way again");
    }

    #[test]
    fn renders_thumbnails() {
        let fixture = open_fixture();

        fixture.want(Wanted {
            thumbnails: vec![1],
            ..Wanted::default()
        });

        let (page, bitmap) = fixture.wait(|event| match event {
            Event::Thumbnail { page, bitmap, .. } => Some((page, bitmap)),
            _ => None,
        });

        assert_eq!(page, 1);
        assert_eq!(bitmap.width, THUMBNAIL_WIDTH);
        assert_eq!(
            bitmap.height,
            (792.0 * THUMBNAIL_WIDTH as f32 / 612.0).round() as u32
        );
    }

    #[test]
    fn starts_on_the_plan_before_being_asked() {
        let (sender, events) = mpsc::channel();
        let engine = Engine::spawn(move |event| {
            let _ = sender.send(event);
        });

        engine.open(
            fixture_path(),
            Some(Box::new(|doc| {
                assert_eq!(doc.pages.len(), 3);

                Wanted {
                    thumbnails: vec![2],
                    ..Wanted::default()
                }
            })),
        );

        let mut opened = false;

        // Nothing else is asked of the engine, and the thumbnail still comes.
        loop {
            match events.recv_timeout(Duration::from_secs(20)) {
                Ok(Event::Opened(_)) => opened = true,
                Ok(Event::Thumbnail { page, .. }) => {
                    assert_eq!(page, 2);
                    break;
                }
                other => panic!("expected the planned thumbnail, got {other:?}"),
            }
        }

        assert!(opened);
    }

    #[test]
    fn ignores_what_was_wanted_of_another_document() {
        let fixture = open_fixture();

        fixture.engine.want(
            fixture.doc.id + 1,
            Wanted {
                thumbnails: vec![0],
                ..Wanted::default()
            },
        );
        fixture.want(Wanted {
            texts: vec![1],
            ..Wanted::default()
        });

        // Had the first request been served, its thumbnail would come first.
        match fixture.events.recv_timeout(Duration::from_secs(20)) {
            Ok(Event::Text { page: 1, .. }) => {}
            other => panic!("expected the text of page 2, got {other:?}"),
        }
    }

    #[test]
    fn extracts_text_in_view_space() {
        let fixture = open_fixture();

        fixture.want(Wanted {
            texts: vec![0],
            ..Wanted::default()
        });

        let text = fixture.wait(|event| match event {
            Event::Text { page: 0, text, .. } => Some(text),
            _ => None,
        });

        let first = &text.lines[0];

        assert_eq!(text.text(first.glyphs.clone()).trim(), "Sample Document");
        // The title's baseline is 72pt from the top of a 792pt page, so in
        // view space (origin top-left) the line sits just above y = 72.
        assert!(
            first.rect.y0 > 40.0 && first.rect.y1 < 85.0,
            "{:?}",
            first.rect
        );
        assert!((first.rect.x0 - 72.0).abs() < 3.0, "{:?}", first.rect);
    }

    #[test]
    fn reads_the_links_of_a_page() {
        let fixture = open_fixture();

        fixture.want(Wanted {
            texts: vec![0],
            ..Wanted::default()
        });

        let text = fixture.wait(|event| match event {
            Event::Text { page: 0, text, .. } => Some(text),
            _ => None,
        });

        // One to an address, one to a place on the second page, one to the
        // third page. Neither the annotation that is not a link nor the link
        // that would run a program is among them.
        let targets: Vec<_> = text.links.iter().map(|link| link.target.clone()).collect();

        assert_eq!(
            targets,
            [
                Target::Uri("https://example.com/folio?q=1".into()),
                Target::Page {
                    page: 1,
                    top: Some(52.0)
                },
                Target::Page { page: 2, top: None },
            ]
        );

        // In view space, like the text: 653 to 667pt up a 792pt page.
        assert_eq!(
            text.links[0].rect,
            Rect {
                x0: 72.0,
                y0: 125.0,
                x1: 200.0,
                y1: 139.0
            }
        );
        assert_eq!(text.link_at(100.0, 130.0), Some(&text.links[0]));
        assert_eq!(text.link_at(100.0, 150.0), None);
    }

    #[test]
    fn reads_the_addresses_a_page_writes_out() {
        let fixture = open_fixture();

        fixture.want(Wanted {
            texts: vec![1],
            ..Wanted::default()
        });

        let text = fixture.wait(|event| match event {
            Event::Text { page: 1, text, .. } => Some(text),
            _ => None,
        });

        let targets: Vec<_> = text.links.iter().map(|link| link.target.clone()).collect();

        // The address in the text, whole, in place of the link to its first
        // part that is marked over it; and a marked address with letters
        // outside ASCII, written a byte to the letter.
        assert_eq!(
            targets,
            [
                Target::Uri("https://example.com/docs".into()),
                Target::Uri("http://example.com/b\u{fa}squeda#caf\u{e9}".into())
            ]
        );

        // Under the address and not the words before it, nor the full stop.
        let rect = text.links[0].rect;
        let line = text
            .lines
            .iter()
            .find(|line| text.text(line.glyphs.clone()).contains("described"))
            .expect("line exists");

        assert!(rect.x0 > line.rect.x0 + 60.0, "{rect:?} in {:?}", line.rect);
        assert!(rect.x1 < line.rect.x1 - 1.0, "{rect:?} in {:?}", line.rect);
        assert!((rect.y0 - line.rect.y0).abs() < 0.01, "{rect:?}");
    }

    #[test]
    fn copies_a_selection_across_pages() {
        let fixture = open_fixture();

        fixture.want(Wanted {
            texts: vec![0],
            ..Wanted::default()
        });

        let text = fixture.wait(|event| match event {
            Event::Text { page: 0, text, .. } => Some(text),
            _ => None,
        });

        // From the start of the "second paragraph" line to the end of the
        // document.
        let line = text
            .lines
            .iter()
            .find(|line| {
                text.text(line.glyphs.clone())
                    .starts_with("The second paragraph")
            })
            .expect("line exists");

        fixture.engine.copy(
            Anchor {
                page: 0,
                x: line.rect.x0 - 1.0,
                y: line.rect.center_y(),
            },
            Anchor::end_of(1),
            Granularity::Char,
        );

        let copied = fixture.wait(|event| match event {
            Event::Copied(text) => Some(text),
            _ => None,
        });

        assert!(
            copied.starts_with("The second paragraph is separated"),
            "{copied:?}"
        );
        assert!(copied.contains("first item"), "{copied:?}");
        assert!(
            copied.contains("closing remarks of the document."),
            "{copied:?}"
        );
        assert!(
            copied.ends_with("described at https://example.com/docs."),
            "{copied:?}"
        );
        assert!(!copied.contains("Introduction"), "{copied:?}");
    }

    /// Runs a search to its end, returning the matches in arrival order.
    fn search(fixture: &Fixture, id: u64, query: &str, from: u32) -> Vec<Match> {
        let mut found = Vec::new();

        fixture.engine.search(id, query.into(), from);

        fixture.wait(|event| match event {
            Event::Matches {
                search, matches, ..
            } if search == id => {
                found.extend(matches);
                None
            }
            Event::SearchDone { search, capped, .. } if search == id => {
                assert!(!capped);
                Some(())
            }
            _ => None,
        });

        found
    }

    #[test]
    fn searches_the_document() {
        let fixture = open_fixture();

        // Twice on the first page, in any case, with a highlight each.
        let found = search(&fixture, 1, "PARAGRAPH", 0);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|found| found.page == 0));
        assert!(found[0].start < found[1].start);

        for found in &found {
            let [rect] = found.rects[..] else {
                panic!("one line each: {found:?}");
            };

            assert!(rect.x0 >= 70.0 && rect.x1 <= 545.0, "{rect:?}");
            assert!(rect.width() > 30.0 && rect.height() > 6.0, "{rect:?}");
        }

        // A word the fixture breaks across two lines with a hyphen.
        let found = search(&fixture, 2, "hyphenated", 0);

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rects.len(), 2, "{found:?}");

        // Pages are searched from the one asked for, wrapping round.
        let pages: Vec<u32> = search(&fixture, 3, "the", 1)
            .iter()
            .map(|found| found.page)
            .collect();
        let first_page = pages.iter().position(|page| *page == 0).expect("on page 1");

        assert_eq!(pages[0], 1, "{pages:?}");
        assert!(
            pages[first_page..].iter().all(|page| *page == 0),
            "{pages:?}"
        );

        assert!(search(&fixture, 4, "no such words", 0).is_empty());
    }

    #[test]
    fn a_new_search_replaces_the_one_running() {
        let fixture = open_fixture();

        fixture.engine.search(1, "the".into(), 0);
        fixture.engine.search(2, "   ".into(), 0);

        let found = search(&fixture, 3, "closing remarks", 0);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].page, 1);
    }

    #[test]
    fn carries_on_across_a_change_of_handle() {
        let fixture = open_fixture();

        let first_line = |fixture: &Fixture| {
            fixture.want(Wanted {
                texts: vec![0],
                ..Wanted::default()
            });

            let text = fixture.wait(|event| match event {
                Event::Text { page: 0, text, .. } => Some(text),
                _ => None,
            });

            text.text(text.lines[0].glyphs.clone())
        };

        let before = first_line(&fixture);

        // Thumbnails parse their page afresh each time, so enough of them
        // has the engine trade its handle on the document for a new one.
        for round in 0..150 {
            fixture.want(Wanted {
                thumbnails: vec![1 + round % 2],
                ..Wanted::default()
            });

            fixture.wait(|event| match event {
                Event::Thumbnail { .. } => Some(()),
                _ => None,
            });
        }

        assert_eq!(first_line(&fixture), before);
        assert_eq!(search(&fixture, 9, "closing remarks", 0).len(), 1);
    }

    #[test]
    fn exports_markdown() {
        let fixture = open_fixture();
        let dest = std::env::temp_dir().join(format!("folio-export-{}.md", std::process::id()));

        fixture.engine.export_markdown(dest.clone());

        let written = fixture.wait(|event| match event {
            Event::Exported(path) => Some(path),
            _ => None,
        });

        let markdown = std::fs::read_to_string(&written).expect("export exists");
        let _ = std::fs::remove_file(&written);

        assert_eq!(
            markdown,
            "# Sample Document\n\n\
             ## Introduction\n\n\
             This fixture exercises the text layer of the reader. It has enough body text \
             for the exporter to find the body size, a hyphenated word, and a second \
             paragraph below.\n\n\
             The second paragraph is separated by vertical space.\n\n\
             - first item\n- second item\n\n\
             ```\nlet answer = 42;\n```\n\n\
             ## Results\n\n\
             The second page holds the closing remarks of the document.\n\n\
             It is described at https://example.com/docs.\n\n\
             Table 1: Scores by model.\n\n\
             | Model | Score | Cost |\n\
             | --- | --- | --- |\n\
             | Small | 27.3 | 3.3 |\n\
             | Big net | 28.4 | 2.3 |\n\n\
             | Option | Description |\n\
             | --- | --- |\n\
             | `-p` | Show the patch introduced with each commit. |\n\
             | `--stat` | Show statistics. |\n"
        );
    }

    #[test]
    fn engines_share_the_library_without_harm() {
        // Each on a thread of its own, with nothing to keep them apart but
        // the engines themselves: a document read while another is being
        // parsed or drawn comes back whole.
        let readers: Vec<_> = (0..16)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..4 {
                        let fixture = open_fixture();

                        assert_eq!(fixture.doc.pages.len(), 3);
                        assert_eq!(fixture.doc.outline.len(), 2);

                        fixture.want(Wanted {
                            tiles: vec![TileKey {
                                page: 0,
                                scale: 2.0f32.to_bits(),
                                col: 0,
                                row: 0,
                            }],
                            texts: vec![0, 1, 2],
                            ..Wanted::default()
                        });

                        let glyphs: usize = (0..3)
                            .map(|_| {
                                fixture.wait(|event| match event {
                                    Event::Text { text, .. } => Some(text.glyphs.len()),
                                    _ => None,
                                })
                            })
                            .sum();

                        assert!(glyphs > 500, "{glyphs} glyphs");
                    }
                })
            })
            .collect();

        for reader in readers {
            reader.join().expect("the document was read whole");
        }
    }

    #[test]
    fn reports_files_that_are_not_pdfs() {
        let (sender, events) = mpsc::channel();
        let engine = Engine::spawn(move |event| {
            let _ = sender.send(event);
        });

        let path = std::env::temp_dir().join(format!("folio-not-a-pdf-{}.txt", std::process::id()));
        std::fs::write(&path, "plain text").expect("write file");

        engine.open(path, None);

        match events.recv_timeout(Duration::from_secs(20)) {
            Ok(Event::OpenFailed { error, .. }) => assert!(error.contains("not a valid PDF")),
            other => panic!("expected a failure, got {other:?}"),
        }
    }
}
