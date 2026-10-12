// Release builds are a GUI application on Windows: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod engine;
mod trace;
mod ui;

use std::path::PathBuf;
use std::sync::mpsc;

use engine::{Engine, Event};

const USAGE: &str = "\
Folio — a fast PDF reader

Usage:
  folio [FILE]                               Open the reader
  folio --export-markdown <IN.pdf> <OUT.md>  Convert without opening a window

Options:
  --light                Start in the light theme
  --page <N>             Open at page N
  --zoom <PERCENT>       Open at the given zoom
  --sidebar <MODE>       outline, pages or hidden
  --print                Open the print dialog once the file has loaded
  -h, --help             Show this help
";

fn main() -> iced::Result {
    trace::init();

    let mut options = app::Options::default();
    let mut args = std::env::args().skip(1);

    attach_console();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--export-markdown" => match (args.next(), args.next()) {
                (Some(input), Some(output)) => {
                    std::process::exit(export(input.into(), output.into()));
                }
                _ => {
                    eprint!("{USAGE}");
                    std::process::exit(2);
                }
            },
            "--light" => options.light = true,
            "--page" => options.page = args.next().and_then(|value| value.parse().ok()),
            "--zoom" => {
                options.zoom = args
                    .next()
                    .and_then(|value| value.trim_end_matches('%').parse::<f32>().ok())
                    .map(|percent| percent / 100.0);
            }
            "--sidebar" => options.sidebar = args.next(),
            "--print" => options.print = true,
            // Undocumented development aids: start with everything selected;
            // start with a search made; render one frame to a PNG and exit;
            // scroll through the document for a number of frames and exit.
            "--select-all" => options.select_all = true,
            "--find" => options.find = args.next(),
            "--screenshot" => options.screenshot = args.next().map(PathBuf::from),
            "--scroll-test" => {
                options.scroll_test = args.next().and_then(|value| value.parse().ok());
            }
            _ => options.file = Some(PathBuf::from(arg)),
        }
    }

    options.scale_factor = display_scale_factor();

    select_graphics_backend();
    discover_fonts();

    app::run(options)
}

/// The text renderer reads the name of every installed font the first time
/// anything is laid out, then picks out and loads the ones the interface is
/// set in, all of which holds up the first frame. Have that done on another
/// thread while the main one is starting the graphics device.
fn discover_fonts() {
    let _ = std::thread::Builder::new().name("fonts".into()).spawn(|| {
        let _ = iced::advanced::graphics::text::font_system();
        trace::mark("fonts discovered");

        ui::warm_fonts();
        trace::mark("fonts loaded");
    });
}

/// A release build is a GUI program on Windows and starts without a
/// console, so anything printed would vanish. When launched from a terminal,
/// borrow the terminal's console for the command-line modes that print.
fn attach_console() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

        let prints = std::env::args()
            .skip(1)
            .any(|arg| matches!(arg.as_str(), "-h" | "--help" | "--export-markdown"));

        if prints {
            // Fails harmlessly when there is no parent console, or when
            // output is already redirected to a file or pipe.
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
}

/// Windows always has Direct3D 12. Naming it stops the renderer from also
/// loading the Vulkan and OpenGL drivers just to compare adapters, which
/// costs start-up time and tens of megabytes. `WGPU_BACKEND` still overrides.
fn select_graphics_backend() {
    if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
        // SAFETY: nothing else is running yet, so no thread can be reading
        // the environment.
        unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
    }
}

/// Device pixels per logical pixel on the main display, where a new window
/// usually opens.
fn display_scale_factor() -> Option<f32> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem,
            SetProcessDpiAwarenessContext,
        };

        // The windowing library asks for the same awareness when it starts;
        // until someone has, Windows reports 96 dpi whatever the display.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

            Some(GetDpiForSystem() as f32 / 96.0)
        }
    }

    #[cfg(not(windows))]
    None
}

/// Converts a document to Markdown without opening a window.
fn export(input: PathBuf, output: PathBuf) -> i32 {
    let (sender, receiver) = mpsc::channel();
    let engine = Engine::spawn(move |event| {
        let _ = sender.send(event);
    });

    engine.open(input, None);

    for event in receiver {
        match event {
            Event::Opened(_) => engine.export_markdown(output.clone()),
            Event::Exported(path) => {
                println!("{}", path.display());
                return 0;
            }
            Event::OpenFailed { error, .. } | Event::JobFailed { error, .. } => {
                eprintln!("error: {error}");
                return 1;
            }
            _ => {}
        }
    }

    1
}
