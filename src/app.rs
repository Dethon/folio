//! Application state and the message loop.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::futures::StreamExt;
use iced::futures::channel::{mpsc, oneshot};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::scrollable;
use iced::window;
use iced::{Element, Size, Subscription, Task};

use crate::engine::{self, DocInfo, Engine, JobKind, print};
use crate::ui::theme::Palette;
use crate::ui::viewer::{Fit, Interaction, Viewer};
use crate::ui::{self, sidebar};

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
    Engine(Once<engine::Event>),
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
    Key(keyboard::Event),
    Window(window::Event),
    ScaleFactor(f32),
    Tick(Instant),
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

pub struct App {
    engine: Engine,
    pub viewer: Viewer,
    pub sidebar: sidebar::State,
    pub palette: Palette,
    pub page_input: String,
    pub toast: Option<Toast>,
    pub opening: Option<PathBuf>,
    options: Options,
    /// Frames to wait before taking the requested screenshot.
    screenshot_countdown: Option<u8>,
}

impl App {
    pub fn boot(options: Options) -> (Self, Task<Message>) {
        let (sender, receiver) = mpsc::unbounded();
        let engine = Engine::spawn(move |event| {
            let _ = sender.unbounded_send(event);
        });

        let mut app = Self {
            engine,
            viewer: Viewer::default(),
            sidebar: sidebar::State::default(),
            palette: if options.light {
                Palette::LIGHT
            } else {
                Palette::DARK
            },
            page_input: String::new(),
            toast: None,
            opening: None,
            options,
            screenshot_countdown: None,
        };

        if let Some(path) = app.options.file.clone() {
            app.open(path);
        } else if app.options.screenshot.is_some() {
            app.screenshot_countdown = Some(3);
        }

        let events = Task::stream(receiver.map(|event| Message::Engine(Once::new(event))));
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
            window::events().map(|(_, event)| Message::Window(event)),
        ];

        if self.toast.is_some() || self.screenshot_countdown.is_some() {
            subscriptions.push(iced::time::every(Duration::from_millis(200)).map(Message::Tick));
        }

        Subscription::batch(subscriptions)
    }

    pub fn view(&self) -> Element<'_, Message> {
        ui::view(self)
    }

    fn open(&mut self, path: PathBuf) {
        self.opening = Some(path.clone());
        self.engine.open(path);
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

    /// Pages whose thumbnails the sidebar is showing.
    fn sidebar_thumbnails(&self) -> Range<u32> {
        if self.sidebar.open && self.sidebar.tab == sidebar::Tab::Pages {
            let rows = self.sidebar.visible_rows(self.viewer.page_count() as usize);
            rows.start as u32..rows.end as u32
        } else {
            0..0
        }
    }

    /// Tells the engine what the current view is missing. Called after
    /// anything that changes what is on screen.
    fn sync(&mut self) {
        let wanted = self.viewer.wanted(self.sidebar_thumbnails());

        if self.screenshot_countdown.is_none()
            && self.options.screenshot.is_some()
            && self.opening.is_none()
            && self.viewer.has_viewport()
            && wanted.tiles.is_empty()
            && wanted.texts.is_empty()
            && wanted.thumbnails.is_empty()
        {
            self.screenshot_countdown = Some(2);
        }

        self.engine
            .want(wanted.tiles, wanted.texts, wanted.thumbnails);

        let page = self.viewer.current_page();

        if let Some(doc) = self.document().cloned() {
            self.sidebar.follow(Some(&doc), page);
        }
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
        let task = self.handle(message);

        self.sync();

        let page = self.viewer.current_page();

        if page != page_before {
            self.refresh_page_input();
            return Task::batch([task, self.sidebar.reveal_page(page)]);
        }

        task
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Engine(event) => {
                if let Some(event) = event.take() {
                    return self.engine_event(event);
                }
            }
            Message::Viewer(interaction) => match interaction {
                Interaction::Resized(size) => self.viewer.set_viewport(size),
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

                // Hand the keyboard back to the document: focusing an id
                // nothing carries takes focus away from the field.
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
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                return self.key_pressed(key, modifiers);
            }
            Message::Key(_) => {}
            Message::Window(event) => match event {
                window::Event::FileDropped(path) => self.open(path),
                window::Event::Rescaled(scale_factor) => {
                    self.viewer.set_scale_factor(scale_factor);
                }
                _ => {}
            },
            Message::ScaleFactor(scale_factor) => self.viewer.set_scale_factor(scale_factor),
            Message::Tick(now) => {
                if self
                    .toast
                    .as_ref()
                    .and_then(|toast| toast.expires)
                    .is_some_and(|expires| now >= expires)
                {
                    self.toast = None;
                }

                match self.screenshot_countdown {
                    Some(0) => {
                        self.screenshot_countdown = Some(u8::MAX);

                        return window::oldest()
                            .and_then(window::screenshot)
                            .map(|shot| Message::Screenshot(Once::new(shot)));
                    }
                    Some(u8::MAX) => {}
                    Some(frames) => self.screenshot_countdown = Some(frames - 1),
                    None => {}
                }
            }
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
                self.opening = None;
                self.sidebar.set_document(&doc);
                self.viewer.set_document(doc);

                match self.options.sidebar.take().as_deref() {
                    Some("pages") => self.sidebar.show(sidebar::Tab::Pages),
                    Some("outline") => self.sidebar.show(sidebar::Tab::Outline),
                    Some("hidden") => self.sidebar.open = false,
                    _ => {}
                }

                if self.options.select_all {
                    self.viewer.select_all();
                }

                if let Some(zoom) = self.options.zoom.take() {
                    self.viewer.zoom_to(zoom, None);
                }

                if let Some(page) = self.options.page.take() {
                    self.viewer.go_to_page(page.saturating_sub(1));
                }

                self.refresh_page_input();

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
                self.viewer.insert_tile(key, bitmap);
            }
            engine::Event::Thumbnail { doc, page, bitmap } if Some(doc) == current => {
                self.viewer.insert_thumbnail(page, bitmap);
            }
            engine::Event::Text { doc, page, text } if Some(doc) == current => {
                self.viewer.insert_text(page, text);
            }
            engine::Event::Tile { .. }
            | engine::Event::Thumbnail { .. }
            | engine::Event::Text { .. } => {}
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

    fn key_pressed(&mut self, key: Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        let command = modifiers.command();

        match key.as_ref() {
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
            Key::Named(Named::Escape) => self.viewer.clear_selection(),
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

/// The native handle the print dialog should be owned by.
fn window_handle(window: &dyn window::Window) -> isize {
    use window::raw_window_handle::RawWindowHandle;

    match window.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        _ => 0,
    }
}

pub fn run(options: Options) -> iced::Result {
    iced::application(move || App::boot(options.clone()), App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .settings(iced::Settings {
            default_text_size: ui::theme::TEXT.into(),
            default_font: ui::font(),
            ..iced::Settings::default()
        })
        .window(window::Settings {
            size: Size::new(1180.0, 840.0),
            min_size: Some(Size::new(560.0, 380.0)),
            icon: window::icon::from_file_data(
                include_bytes!("../assets/icon/folio.png"),
                Some(image::ImageFormat::Png),
            )
            .ok(),
            ..window::Settings::default()
        })
        .run()
}
