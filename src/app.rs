//! Application state and the message loop.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::futures::StreamExt;
use iced::futures::channel::{mpsc, oneshot};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::scrollable;
use iced::window;
use iced::{Element, Size, Subscription, Task};

use crate::engine::{self, DocInfo, Engine, JobKind, print};
use crate::trace;
use crate::ui::theme::Palette;
use crate::ui::viewer::{Fit, Interaction, Viewer};
use crate::ui::{self, icon, sidebar};

/// Options taken from the command line.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub file: Option<PathBuf>,
    pub light: bool,
    /// Development aid: once the first view has rendered, save it as a PNG
    /// and quit.
    pub screenshot: Option<PathBuf>,
    pub page: Option<u32>,
    pub zoom: Option<f32>,
    /// `outline`, `pages` or `hidden`.
    pub sidebar: Option<String>,
    /// Development aid: start with the whole document selected.
    pub select_all: bool,
    /// Show the print dialog as soon as the document has loaded.
    pub print: bool,
    /// Development aid: start with the find bar open on this query.
    pub find: Option<String>,
    /// Development aid: once the first view has rendered, scroll down for
    /// this many frames, report how they went to the trace, and quit.
    pub scroll_test: Option<u32>,
    /// Device pixels per logical pixel on the display the window is likely
    /// to open on, when that can be told before there is a window.
    pub scale_factor: Option<f32>,
}

/// Carries a value that cannot be cloned through a message, which must be.
pub struct Once<T>(Arc<Mutex<Option<T>>>);

impl<T> Once<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    pub fn take(&self) -> Option<T> {
        self.0.lock().ok()?.take()
    }
}

impl<T> Clone for Once<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> std::fmt::Debug for Once<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Once")
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    /// The engine has events waiting.
    Engine,
    Viewer(Interaction),
    OpenDialog,
    Open(Option<PathBuf>),
    ToggleSidebar,
    ShowTab(sidebar::Tab),
    ToggleOutline(usize),
    GoTo(u32),
    SidebarScrolled(scrollable::Viewport),
    PageInput(String),
    PageSubmit,
    Zoom(i8),
    ZoomReset,
    Fit(Fit),
    Export,
    ExportTo(Option<PathBuf>),
    Print,
    PrintOwner(isize),
    PrintTarget(Once<Result<Option<print::Target>, String>>),
    ToggleTheme,
    Find,
    FindInput(String),
    /// Enter in the find field.
    FindSubmit,
    FindStep(i8),
    Escape,
    Key(keyboard::Event),
    Window(window::Event),
    ScaleFactor(f32),
    Tick(Instant),
    /// A frame of the scroll test.
    Frame(Instant),
    DismissToast,
    Screenshot(Once<window::Screenshot>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub tone: Tone,
    /// `None` while the work it reports on is still running.
    pub expires: Option<Instant>,
    pub progress: Option<f32>,
}

/// The find bar.
#[derive(Debug, Default)]
pub struct Find {
    pub open: bool,
    pub query: String,
    /// Identifies the latest search, so results of earlier ones are dropped.
    id: u64,
    pub searching: bool,
    /// The search stopped at the engine's limit: there are more matches.
    pub capped: bool,
}

impl Find {
    /// Identifies the find field for focus operations.
    pub const FIELD: &'static str = "find";
}

pub struct App {
    engine: Engine,
    events: std::sync::mpsc::Receiver<engine::Event>,
    /// Set by the engine when it adds to `events`; while it is set, a
    /// [`Message::Engine`] is on its way.
    pending: Arc<AtomicBool>,
    pub viewer: Viewer,
    pub sidebar: sidebar::State,
    pub palette: Palette,
    pub page_input: String,
    pub find: Find,
    pub toast: Option<Toast>,
    pub opening: Option<PathBuf>,
    modifiers: keyboard::Modifiers,
    options: Options,
    /// Frames to wait before taking the requested screenshot.
    screenshot_countdown: Option<u8>,
    scroll_test: Option<ScrollTest>,
    /// When the scroll test finished scrolling.
    resting_since: Option<Instant>,
}

/// The size the window opens at, in logical pixels.
const WINDOW: Size = Size::new(1180.0, 840.0);

/// Distance the scroll test moves per frame, in logical pixels.
const SCROLL_TEST_STEP: f32 = 30.0;

/// How long the scroll test waits after scrolling before it reports the
/// memory in use at rest.
const SCROLL_TEST_REST: Duration = Duration::from_secs(5);

/// A scroll test in progress.
struct ScrollTest {
    last: Instant,
    /// Milliseconds between frames.
    intervals: Vec<f32>,
    /// Whether tiles in view are missing right now.
    missing: bool,
    /// Frames drawn while they were.
    incomplete: u32,
}

impl App {
    pub fn boot(options: Options) -> (Self, Task<Message>) {
        // Events are queued here and taken in a batch; the stream only
        // carries the news that there are some. A burst of tiles is then one
        // update of the interface rather than one each.
        let (sender, events) = std::sync::mpsc::channel();
        let (waker, woken) = mpsc::unbounded();
        let pending = Arc::new(AtomicBool::new(false));

        let engine = Engine::spawn({
            let pending = pending.clone();

            move |event| {
                let _ = sender.send(event);

                if !pending.swap(true, Ordering::AcqRel) {
                    let _ = waker.unbounded_send(());
                }
            }
        });

        let mut app = Self {
            engine,
            events,
            pending,
            viewer: Viewer::default(),
            sidebar: sidebar::State::default(),
            palette: if options.light {
                Palette::LIGHT
            } else {
                Palette::DARK
            },
            page_input: String::new(),
            find: Find::default(),
            toast: None,
            opening: None,
            modifiers: keyboard::Modifiers::default(),
            options,
            screenshot_countdown: None,
            scroll_test: None,
            resting_since: None,
        };

        trace::mark("boot");

        // The window does not exist yet, but where the page view will be in
        // it is known. Assuming that much lets the engine render the first
        // screen while the graphics device is still starting; if the guess
        // is off, the view corrects it when it is first laid out.
        if let Some(scale_factor) = app.options.scale_factor {
            app.viewer.set_scale_factor(scale_factor);
            icon::set_scale_factor(scale_factor);
        }

        let view = ui::page_view_size(WINDOW, app.options.sidebar.as_deref() != Some("hidden"));

        app.viewer.set_viewport(view);
        // The sidebar's list reports its height only once it is scrolled.
        app.sidebar.height = view.height - sidebar::HEADER;

        if let Some(path) = app.options.file.clone() {
            app.open(path);
        } else if app.options.screenshot.is_some() {
            app.screenshot_countdown = Some(3);
        }

        let events = Task::stream(woken.map(|()| Message::Engine));
        let scale = window::oldest()
            .and_then(window::scale_factor)
            .map(Message::ScaleFactor);

        (app, Task::batch([events, scale]))
    }

    pub fn title(&self) -> String {
        match self.viewer.document() {
            Some(doc) if !doc.title.is_empty() => format!("{} — Folio", doc.title),
            _ => "Folio".into(),
        }
    }

    pub fn theme(&self) -> iced::Theme {
        self.palette.iced()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            keyboard::listen().map(Message::Key),
            iced::event::listen_with(escape),
            window::events().map(|(_, event)| Message::Window(event)),
        ];

        if self.toast.is_some()
            || self.screenshot_countdown.is_some()
            || self.resting_since.is_some()
        {
            subscriptions.push(iced::time::every(Duration::from_millis(200)).map(Message::Tick));
        }

        if self.scroll_test.is_some() {
            subscriptions.push(window::frames().map(Message::Frame));
        }

        Subscription::batch(subscriptions)
    }

    pub fn view(&self) -> Element<'_, Message> {
        trace::mark("first view");
        ui::view(self)
    }

    fn open(&mut self, path: PathBuf) {
        self.opening = Some(path.clone());
        self.engine.open(path, Some(self.plan()));
    }

    /// Works out what a document about to be opened will be asked for
    /// first, by laying it out in a view the shape of this one.
    fn plan(&self) -> engine::Plan {
        let mut options = self.options.clone();
        let viewport = self.viewer.viewport();
        let scale_factor = self.viewer.scale_factor();
        let (open, height) = (self.sidebar.open, self.sidebar.height);

        Box::new(move |doc| {
            let mut viewer = Viewer::default();
            let mut sidebar = sidebar::State::default();

            viewer.set_scale_factor(scale_factor);
            viewer.set_viewport(viewport);
            sidebar.open = open;
            sidebar.height = height;

            present(&mut viewer, &mut sidebar, &mut options, doc.clone());

            let thumbnails = sidebar_thumbnails(&sidebar, &viewer);

            viewer.wanted(thumbnails)
        })
    }

    /// Takes in everything the engine has produced since last asked.
    fn drain(&mut self) -> Task<Message> {
        self.pending.store(false, Ordering::Release);

        let mut tasks = Vec::new();

        while let Ok(event) = self.events.try_recv() {
            tasks.push(self.engine_event(event));
        }

        Task::batch(tasks)
    }

    fn notify(&mut self, text: impl Into<String>, tone: Tone) {
        self.toast = Some(Toast {
            text: text.into(),
            tone,
            expires: Some(
                Instant::now()
                    + Duration::from_millis(if tone == Tone::Error { 6000 } else { 2600 }),
            ),
            progress: None,
        });
    }

    fn document(&self) -> Option<&Arc<DocInfo>> {
        self.viewer.document()
    }

    fn set_scale_factor(&mut self, scale_factor: f32) {
        self.viewer.set_scale_factor(scale_factor);
        icon::set_scale_factor(scale_factor);
    }

    /// Tells the engine what the current view is missing. Called after
    /// anything that changes what is on screen.
    fn sync(&mut self) {
        let wanted = self
            .viewer
            .wanted(sidebar_thumbnails(&self.sidebar, &self.viewer));

        if self.document().is_some() && self.viewer.has_viewport() && self.viewer.is_complete() {
            trace::mark("view complete");
        }

        if let Some(test) = self.scroll_test.as_mut() {
            test.missing = !self.viewer.is_complete();
        }

        let settled = self.opening.is_none()
            && !self.find.searching
            && self.viewer.has_viewport()
            && wanted.tiles.is_empty()
            && wanted.texts.is_empty()
            && wanted.thumbnails.is_empty();

        if settled && self.screenshot_countdown.is_none() && self.options.screenshot.is_some() {
            self.screenshot_countdown = Some(2);
        }

        if settled
            && self.document().is_some()
            && self.scroll_test.is_none()
            && self.options.scroll_test.is_some()
        {
            trace::memory("before scrolling");
            self.scroll_test = Some(ScrollTest {
                last: Instant::now(),
                intervals: Vec::new(),
                missing: false,
                incomplete: 0,
            });
        }

        let page = self.viewer.current_page();

        if let Some(doc) = self.document().cloned() {
            self.engine.want(doc.id, wanted);
            self.sidebar.follow(Some(&doc), page);
        }
    }

    /// Searches the document for the find bar's query, from where the reader
    /// is. A blank query only clears the last search.
    fn search(&mut self) {
        let from = self.viewer.begin_search();

        self.find.id += 1;
        self.find.searching = !self.find.query.trim().is_empty();
        self.find.capped = false;
        trace::note(|| format!("search for {:?}", self.find.query));
        self.engine
            .search(self.find.id, self.find.query.clone(), from);
    }

    fn open_find(&mut self) -> Task<Message> {
        if self.document().is_none() {
            return Task::none();
        }

        // Reopening brings the last query's matches back.
        if !self.find.open {
            self.find.open = true;
            self.search();
        }

        Task::batch([
            iced::widget::operation::focus(Find::FIELD),
            iced::widget::operation::select_all(Find::FIELD),
        ])
    }

    fn close_find(&mut self) -> Task<Message> {
        self.find.open = false;
        self.find.id += 1;
        self.find.searching = false;
        self.engine.search(self.find.id, String::new(), 0);
        self.viewer.clear_matches();

        // Hand the keyboard back to the document: focusing an id nothing
        // carries takes focus away from the field.
        iced::widget::operation::focus(iced::widget::Id::new("document"))
    }

    fn refresh_page_input(&mut self) {
        self.page_input = if self.document().is_some() {
            (self.viewer.current_page() + 1).to_string()
        } else {
            String::new()
        };
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let page_before = self.viewer.current_page();
        let produced = self.drain();
        let task = self.handle(message);

        self.sync();

        let page = self.viewer.current_page();

        if page != page_before {
            self.refresh_page_input();

            // After what the engine's events asked for: a new document
            // sends the sidebar to the top first, then to the page.
            return Task::batch([produced.chain(self.sidebar.reveal_page(page)), task]);
        }

        Task::batch([produced, task])
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            // Every update takes in what the engine has produced.
            Message::Engine => {}
            Message::Viewer(interaction) => match interaction {
                Interaction::Resized(size) => {
                    // Published while the first frame was being drawn.
                    trace::mark("first frame drawn");
                    self.viewer.set_viewport(size);
                }
                Interaction::Scrolled { dx, dy } => self.viewer.scroll_by(dx, dy),
                Interaction::Zoomed { factor, at } => self.viewer.zoom_by(factor, Some(at)),
                Interaction::Pressed {
                    at,
                    granularity,
                    extend,
                } => self.viewer.press(at, granularity, extend),
                Interaction::Dragged { at } => self.viewer.drag(at),
                Interaction::Released => {}
                Interaction::ScrolledTo { fraction } => self.viewer.scroll_to_fraction(fraction),
            },
            Message::OpenDialog => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Open PDF")
                            .add_filter("PDF documents", &["pdf"])
                            .add_filter("All files", &["*"])
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::Open,
                );
            }
            Message::Open(path) => {
                if let Some(path) = path {
                    self.open(path);
                }
            }
            Message::ToggleSidebar => self.sidebar.open = !self.sidebar.open,
            Message::ShowTab(tab) => {
                self.sidebar.show(tab);
                return self.sidebar.reveal_page(self.viewer.current_page());
            }
            Message::ToggleOutline(index) => {
                let doc = self.document().cloned();
                self.sidebar.toggle(doc.as_deref(), index);
            }
            Message::GoTo(page) => self.viewer.go_to_page(page),
            Message::SidebarScrolled(viewport) => {
                self.sidebar.offset = viewport.absolute_offset().y;
                self.sidebar.height = viewport.bounds().height;
            }
            Message::PageInput(value) => {
                self.page_input = value.chars().filter(char::is_ascii_digit).take(7).collect();
            }
            Message::PageSubmit => {
                if let Ok(page) = self.page_input.parse::<u32>() {
                    self.viewer.go_to_page(page.saturating_sub(1));
                }
                self.refresh_page_input();

                // Hand the keyboard back to the document, as closing the
                // find bar does.
                return iced::widget::operation::focus(iced::widget::Id::new("document"));
            }
            Message::Zoom(direction) => self.viewer.zoom_step(direction),
            Message::ZoomReset => self.viewer.zoom_to(1.0, None),
            Message::Fit(fit) => {
                // Pressing the active mode again releases it.
                let fit = if self.viewer.fit() == fit {
                    Fit::Manual
                } else {
                    fit
                };
                self.viewer.set_fit(fit);
            }
            Message::Export => {
                let Some(doc) = self.document() else {
                    return Task::none();
                };

                let name = doc
                    .path
                    .file_stem()
                    .map(|stem| format!("{}.md", stem.to_string_lossy()))
                    .unwrap_or_else(|| "document.md".into());
                let directory = doc.path.parent().map(PathBuf::from);

                return Task::perform(
                    async move {
                        let mut dialog = rfd::AsyncFileDialog::new()
                            .set_title("Export as Markdown")
                            .set_file_name(name)
                            .add_filter("Markdown", &["md"]);

                        if let Some(directory) = directory {
                            dialog = dialog.set_directory(directory);
                        }

                        dialog
                            .save_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::ExportTo,
                );
            }
            Message::ExportTo(dest) => {
                if let Some(dest) = dest {
                    self.toast = Some(Toast {
                        text: "Exporting to Markdown…".into(),
                        tone: Tone::Info,
                        expires: None,
                        progress: Some(0.0),
                    });
                    self.engine.export_markdown(dest);
                }
            }
            Message::Print => {
                if self.document().is_some() {
                    return window::oldest()
                        .and_then(|id| window::run(id, |window| window_handle(window)))
                        .map(Message::PrintOwner);
                }
            }
            Message::PrintOwner(owner) => {
                let pages = self.viewer.page_count();
                let current = self.viewer.current_page();

                // The system dialog is modal and blocks its thread.
                let (sender, receiver) = oneshot::channel();

                std::thread::spawn(move || {
                    let _ = sender.send(print::prompt(owner, pages, current));
                });

                return Task::perform(receiver, |target| {
                    Message::PrintTarget(Once::new(target.unwrap_or(Ok(None))))
                });
            }
            Message::PrintTarget(target) => match target.take() {
                Some(Ok(Some(target))) => {
                    self.toast = Some(Toast {
                        text: "Printing…".into(),
                        tone: Tone::Info,
                        expires: None,
                        progress: Some(0.0),
                    });
                    self.engine.print(target);
                }
                Some(Err(error)) => self.notify(error, Tone::Error),
                _ => {}
            },
            Message::ToggleTheme => {
                self.palette = if self.palette.is_dark {
                    Palette::LIGHT
                } else {
                    Palette::DARK
                };
            }
            Message::Find => return self.open_find(),
            Message::FindInput(query) => {
                self.find.query = query;
                self.search();
            }
            Message::FindSubmit => {
                self.viewer
                    .step_match(if self.modifiers.shift() { -1 } else { 1 });
            }
            Message::FindStep(direction) => self.viewer.step_match(direction),
            Message::Escape => {
                if self.find.open {
                    return self.close_find();
                }

                self.viewer.clear_selection();
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                return self.key_pressed(key, modifiers);
            }
            Message::Key(keyboard::Event::ModifiersChanged(modifiers)) => {
                self.modifiers = modifiers;
            }
            Message::Key(_) => {}
            Message::Window(event) => match event {
                window::Event::FileDropped(path) => self.open(path),
                window::Event::Rescaled(scale_factor) => self.set_scale_factor(scale_factor),
                _ => {}
            },
            Message::ScaleFactor(scale_factor) => self.set_scale_factor(scale_factor),
            Message::Tick(now) => {
                if self
                    .toast
                    .as_ref()
                    .and_then(|toast| toast.expires)
                    .is_some_and(|expires| now >= expires)
                {
                    self.toast = None;
                }

                if self
                    .resting_since
                    .is_some_and(|since| now.duration_since(since) >= SCROLL_TEST_REST)
                {
                    trace::memory("at rest");
                    return iced::exit();
                }

                match self.screenshot_countdown {
                    Some(0) => {
                        self.screenshot_countdown = Some(u8::MAX);
                        trace::memory("settled");
                        trace::note(|| self.viewer.cache_report());

                        return window::oldest()
                            .and_then(window::screenshot)
                            .map(|shot| Message::Screenshot(Once::new(shot)));
                    }
                    Some(u8::MAX) => {}
                    Some(frames) => self.screenshot_countdown = Some(frames - 1),
                    None => {}
                }
            }
            Message::Frame(now) => return self.scroll_test_frame(now),
            Message::DismissToast => self.toast = None,
            Message::Screenshot(shot) => {
                if let (Some(shot), Some(path)) = (shot.take(), self.options.screenshot.clone()) {
                    let saved = image::save_buffer(
                        &path,
                        &shot.rgba,
                        shot.size.width,
                        shot.size.height,
                        image::ColorType::Rgba8,
                    );

                    if let Err(error) = saved {
                        eprintln!("screenshot failed: {error}");
                    }
                }

                return iced::exit();
            }
        }

        Task::none()
    }

    fn engine_event(&mut self, event: engine::Event) -> Task<Message> {
        let current = self.document().map(|doc| doc.id);

        match event {
            engine::Event::Opened(doc) => {
                trace::mark("opened received");
                self.opening = None;

                present(&mut self.viewer, &mut self.sidebar, &mut self.options, doc);

                self.refresh_page_input();

                if let Some(query) = self.options.find.take() {
                    self.find.open = true;
                    self.find.query = query;
                }

                // The find bar stays open across documents, on its query.
                if self.find.open {
                    self.search();
                }

                let print = if std::mem::take(&mut self.options.print) {
                    Task::done(Message::Print)
                } else {
                    Task::none()
                };

                return Task::batch([sidebar::reset_scroll(), print]);
            }
            engine::Event::OpenFailed { path, error } => {
                self.opening = None;

                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();

                self.notify(format!("Could not open {name}. {error}"), Tone::Error);
            }
            engine::Event::Tile { doc, key, bitmap } if Some(doc) == current => {
                trace::mark("first tile received");
                self.viewer.insert_tile(key, bitmap);
            }
            engine::Event::Thumbnail { doc, page, bitmap } if Some(doc) == current => {
                self.viewer.insert_thumbnail(page, bitmap);
            }
            engine::Event::Text { doc, page, text } if Some(doc) == current => {
                self.viewer.insert_text(page, text);
            }
            engine::Event::Matches {
                doc,
                search,
                matches,
            } if Some(doc) == current && search == self.find.id => {
                self.viewer.add_matches(matches);
            }
            engine::Event::SearchDone {
                doc,
                search,
                capped,
            } if Some(doc) == current && search == self.find.id => {
                trace::note(|| format!("search done: {} matches", self.viewer.match_count()));
                self.find.searching = false;
                self.find.capped = capped;
                self.viewer.finish_search();
            }
            engine::Event::Tile { .. }
            | engine::Event::Thumbnail { .. }
            | engine::Event::Text { .. }
            | engine::Event::Matches { .. }
            | engine::Event::SearchDone { .. } => {}
            engine::Event::Progress { kind, done, total } => {
                let label = match kind {
                    JobKind::Export => "Exporting to Markdown…",
                    JobKind::Print => "Printing…",
                };

                self.toast = Some(Toast {
                    text: label.into(),
                    tone: Tone::Info,
                    expires: None,
                    progress: Some(done as f32 / total.max(1) as f32),
                });
            }
            engine::Event::Copied(text) => {
                if !text.is_empty() {
                    self.notify("Copied to clipboard", Tone::Info);
                    return iced::clipboard::write(text);
                }
            }
            engine::Event::Exported(path) => {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();

                self.notify(format!("Exported {name}"), Tone::Info);
            }
            engine::Event::Printed => self.notify("Sent to printer", Tone::Info),
            engine::Event::JobFailed { kind, error } => {
                let what = match kind {
                    JobKind::Export => "Export failed",
                    JobKind::Print => "Printing failed",
                };

                self.notify(format!("{what}. {error}"), Tone::Error);
            }
        }

        Task::none()
    }

    /// Advances the scroll test by a frame; at its end, reports and quits.
    fn scroll_test_frame(&mut self, now: Instant) -> Task<Message> {
        let frames = self.options.scroll_test.unwrap_or(0) as usize;

        let Some(test) = self.scroll_test.as_mut() else {
            return Task::none();
        };

        test.intervals
            .push(now.duration_since(test.last).as_secs_f32() * 1000.0);
        test.last = now;
        test.incomplete += test.missing as u32;

        if test.intervals.len() <= frames {
            self.viewer.scroll_by(0.0, SCROLL_TEST_STEP);
            return Task::none();
        }

        // The first interval runs from the end of loading, not from a frame.
        let mut intervals = test.intervals.split_off(1);
        let incomplete = test.incomplete;
        let slowest = intervals
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(frame, _)| frame + 1);

        intervals.sort_by(f32::total_cmp);

        let at =
            |quantile: f32| intervals[((intervals.len() - 1) as f32 * quantile).round() as usize];

        trace::note(|| {
            format!(
                "scroll test: {} frames, interval p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, \
                 max {:.2} ms at frame {slowest}; {incomplete} frames with tiles in view missing",
                intervals.len(),
                at(0.5),
                at(0.95),
                at(0.99),
                at(1.0),
            )
        });
        trace::memory("after scrolling");
        trace::note(|| self.viewer.cache_report());

        // Stay a while, to see what is still held once things have settled.
        self.scroll_test = None;
        self.options.scroll_test = None;
        self.resting_since = Some(Instant::now());

        Task::none()
    }

    fn key_pressed(&mut self, key: Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        let command = modifiers.command();
        let step = if modifiers.shift() { -1 } else { 1 };

        match key.as_ref() {
            Key::Character("f") if command => return self.open_find(),
            Key::Character("g") if command && self.find.open => self.viewer.step_match(step),
            Key::Named(Named::F3) if self.find.open => self.viewer.step_match(step),
            Key::Named(Named::F3) => return self.open_find(),
            Key::Named(Named::Enter) if self.find.open => self.viewer.step_match(step),
            Key::Character("o") if command => return self.handle(Message::OpenDialog),
            Key::Character("p") if command => return self.handle(Message::Print),
            Key::Character("e") if command => return self.handle(Message::Export),
            Key::Character("b") if command => self.sidebar.open = !self.sidebar.open,
            Key::Character("c") if command => {
                if let Some((from, to, granularity)) = self.viewer.selection() {
                    self.engine.copy(from, to, granularity);
                }
            }
            Key::Character("a") if command => self.viewer.select_all(),
            Key::Character("=" | "+") if command => self.viewer.zoom_step(1),
            Key::Character("-") if command => self.viewer.zoom_step(-1),
            Key::Character("0") if command => self.viewer.zoom_to(1.0, None),
            Key::Character("1") if command => self.viewer.set_fit(Fit::Width),
            Key::Character("2") if command => self.viewer.set_fit(Fit::Page),
            Key::Named(Named::ArrowDown) => self.viewer.scroll_by(0.0, 56.0),
            Key::Named(Named::ArrowUp) => self.viewer.scroll_by(0.0, -56.0),
            Key::Named(Named::ArrowRight) => self.viewer.scroll_by(56.0, 0.0),
            Key::Named(Named::ArrowLeft) => self.viewer.scroll_by(-56.0, 0.0),
            Key::Named(Named::PageDown) => self.viewer.scroll_by_viewport(1.0),
            Key::Named(Named::PageUp) => self.viewer.scroll_by_viewport(-1.0),
            Key::Named(Named::Space) if modifiers.shift() => self.viewer.scroll_by_viewport(-1.0),
            Key::Named(Named::Space) => self.viewer.scroll_by_viewport(1.0),
            Key::Named(Named::Home) => self.viewer.go_to_start(),
            Key::Named(Named::End) => self.viewer.go_to_end(),
            _ => {}
        }

        Task::none()
    }
}

/// Shows a newly loaded document: from the top, or the first time, the way
/// the command line asked.
fn present(
    viewer: &mut Viewer,
    sidebar: &mut sidebar::State,
    options: &mut Options,
    doc: Arc<DocInfo>,
) {
    sidebar.set_document(&doc);
    viewer.set_document(doc);

    match options.sidebar.take().as_deref() {
        Some("pages") => sidebar.show(sidebar::Tab::Pages),
        Some("outline") => sidebar.show(sidebar::Tab::Outline),
        Some("hidden") => sidebar.open = false,
        _ => {}
    }

    if options.select_all {
        viewer.select_all();
    }

    if let Some(zoom) = options.zoom.take() {
        viewer.zoom_to(zoom, None);
    }

    if let Some(page) = options.page.take() {
        viewer.go_to_page(page.saturating_sub(1));
    }
}

/// Pages whose thumbnails the sidebar is showing.
fn sidebar_thumbnails(sidebar: &sidebar::State, viewer: &Viewer) -> Range<u32> {
    if sidebar.open && sidebar.tab == sidebar::Tab::Pages {
        let rows = sidebar.visible_rows(viewer.page_count() as usize);
        rows.start as u32..rows.end as u32
    } else {
        0..0
    }
}

/// Escape, whether or not a widget took it: a focused text field does, to
/// drop its focus, and the key should still close the find bar.
fn escape(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Named(Named::Escape),
            ..
        }) => Some(Message::Escape),
        _ => None,
    }
}

/// The native handle the print dialog should be owned by.
fn window_handle(window: &dyn window::Window) -> isize {
    use window::raw_window_handle::RawWindowHandle;

    match window.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        _ => 0,
    }
}

/// The default executor starts a thread per core. All that runs on it here
/// is a timer and the wait for a dialog to close, so one thread will do.
struct Executor(tokio::runtime::Runtime);

impl iced::Executor for Executor {
    fn new() -> Result<Self, std::io::Error> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map(Self)
    }

    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        drop(self.0.spawn(future));
    }

    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.0.block_on(future)
    }

    fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.0.enter();
        f()
    }
}

fn window_icon() -> Option<window::Icon> {
    let icon = image::load_from_memory_with_format(
        include_bytes!("../assets/icon/folio.png"),
        image::ImageFormat::Png,
    )
    .ok()?
    .into_rgba8();

    let (width, height) = icon.dimensions();

    window::icon::from_rgba(icon.into_raw(), width, height).ok()
}

pub fn run(options: Options) -> iced::Result {
    iced::application(move || App::boot(options.clone()), App::update, App::view)
        .executor::<Executor>()
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .settings(iced::Settings {
            default_text_size: ui::theme::TEXT.into(),
            default_font: ui::font(),
            ..iced::Settings::default()
        })
        .window(window::Settings {
            size: WINDOW,
            min_size: Some(Size::new(560.0, 380.0)),
            icon: window_icon(),
            ..window::Settings::default()
        })
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::fixture_path;

    /// Runs updates until `done`, as the engine's wake-ups would.
    fn settle(app: &mut App, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);

        while !done(app) {
            assert!(Instant::now() < deadline, "the view never settled");

            std::thread::sleep(Duration::from_millis(2));
            let _ = app.update(Message::Engine);
        }
    }

    #[test]
    fn shows_a_document_given_at_start_up() {
        let (mut app, _) = App::boot(Options {
            file: Some(fixture_path()),
            page: Some(2),
            ..Options::default()
        });

        // There is no window, so no layout: the view is the size assumed.
        assert_eq!(app.viewer.viewport(), ui::page_view_size(WINDOW, true));
        assert!(app.opening.is_some());

        settle(&mut app, |app| {
            app.document().is_some() && app.viewer.is_complete()
        });

        assert!(app.opening.is_none());
        assert_eq!(app.title(), "Sample Document — Folio");
        assert_eq!(app.viewer.current_page(), 1);
        assert_eq!(app.page_input, "2");
    }

    #[test]
    fn opens_another_document_over_the_first() {
        let (mut app, _) = App::boot(Options {
            file: Some(fixture_path()),
            ..Options::default()
        });

        settle(&mut app, |app| app.document().is_some());

        let first = app.document().map(|doc| doc.id);

        app.viewer.go_to_page(2);

        let _ = app.update(Message::Open(Some(fixture_path())));

        settle(&mut app, |app| {
            app.document().map(|doc| doc.id) != first && app.viewer.is_complete()
        });

        // From the top again, and whole.
        assert_eq!(app.viewer.current_page(), 0);
        assert!(app.opening.is_none());
    }

    #[test]
    fn reports_a_file_it_cannot_open() {
        let (mut app, _) = App::boot(Options {
            file: Some(PathBuf::from("no-such-file.pdf")),
            ..Options::default()
        });

        settle(&mut app, |app| app.toast.is_some());

        let toast = app.toast.as_ref().unwrap();

        assert_eq!(toast.tone, Tone::Error);
        assert!(toast.text.starts_with("Could not open no-such-file.pdf."));
        assert!(app.document().is_none() && app.opening.is_none());
    }

    #[test]
    fn finds_as_the_query_is_typed() {
        let (mut app, _) = App::boot(Options {
            file: Some(fixture_path()),
            ..Options::default()
        });

        settle(&mut app, |app| app.document().is_some());

        let _ = app.update(Message::Find);

        for query in ["p", "pa", "paragraph"] {
            let _ = app.update(Message::FindInput(query.into()));
        }

        settle(&mut app, |app| !app.find.searching);

        assert_eq!(app.viewer.match_count(), 2);
        assert_eq!(app.viewer.current_match(), Some(1));
    }
}
