//! Development aid: `folio --bench <FILE>` times the engine on a document,
//! without a window, and prints what it finds.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use crate::engine::{Engine, Event, TILE, TileKey, Wanted, page_pixels};
use crate::trace;

/// Device pixels per point the pages are rendered at: 150 % zoom on a
/// display at 100 % scaling.
const SCALE: f32 = 2.0;
/// Pages rendered, from the first.
const PAGES: u32 = 24;

pub fn run(path: PathBuf) -> i32 {
    let (sender, events) = mpsc::channel();
    let engine = Engine::spawn(move |event| {
        let _ = sender.send(event);
    });

    let started = Instant::now();

    engine.open(path, None);

    let doc = loop {
        match events.recv() {
            Ok(Event::Opened(doc)) => break doc,
            Ok(Event::OpenFailed { error, .. }) => {
                eprintln!("error: {error}");
                return 1;
            }
            Ok(_) => {}
            Err(_) => return 1,
        }
    };

    let pages = doc.pages.len() as u32;

    println!(
        "open            {:8.1} ms   {pages} pages",
        milliseconds(started)
    );
    report("opened");

    // Searches, before anything else has been read: the first reads every
    // page, the second finds all of them ruled out.
    for (id, query) in ["the", "no such phrase anywhere"].into_iter().enumerate() {
        let started = Instant::now();
        let mut found = 0usize;

        engine.search(id as u64 + 100, query.into(), 0);

        wait(&events, 1, |event| match event {
            Event::Matches { matches, .. } => {
                found += matches.len();
                false
            }
            Event::SearchDone { .. } => true,
            _ => false,
        });

        println!(
            "first search    {:8.1} ms   {found} matches of {query:?}",
            milliseconds(started)
        );
    }

    // The engine tidies up once it has nothing to do.
    std::thread::sleep(std::time::Duration::from_millis(100));
    report("searched");

    // Every tile of the first pages.
    let tiles: Vec<TileKey> = (0..pages.min(PAGES))
        .flat_map(|page| {
            let (width, height) = page_pixels(doc.pages[page as usize], SCALE);

            (0..height.div_ceil(TILE)).flat_map(move |row| {
                (0..width.div_ceil(TILE)).map(move |col| TileKey {
                    page,
                    scale: SCALE.to_bits(),
                    col: col as u16,
                    row: row as u16,
                })
            })
        })
        .collect();

    let count = tiles.len();
    let started = Instant::now();
    let mut pixels = 0u64;

    engine.want(
        doc.id,
        Wanted {
            tiles,
            ..Wanted::default()
        },
    );

    wait(&events, count, |event| match event {
        Event::Tile { bitmap, .. } => {
            pixels += bitmap.width as u64 * bitmap.height as u64;
            true
        }
        _ => false,
    });

    let elapsed = milliseconds(started);

    println!(
        "render          {elapsed:8.1} ms   {count} tiles, {:.2} ms each, {:.0} megapixels a second",
        elapsed / count.max(1) as f64,
        pixels as f64 / 1e6 / (elapsed / 1000.0),
    );
    report("rendered");

    let started = Instant::now();

    engine.want(
        doc.id,
        Wanted {
            texts: (0..pages).collect(),
            ..Wanted::default()
        },
    );

    let mut glyphs = 0usize;

    wait(&events, pages as usize, |event| match event {
        Event::Text { text, .. } => {
            glyphs += text.glyphs.len();
            true
        }
        _ => false,
    });

    let elapsed = milliseconds(started);

    println!(
        "text            {elapsed:8.1} ms   {glyphs} glyphs, {:.2} ms a page",
        elapsed / pages.max(1) as f64,
    );
    report("text read");

    for (id, query) in ["the", "rebase"].into_iter().enumerate() {
        let started = Instant::now();
        let mut found = 0usize;

        engine.search(id as u64 + 1, query.into(), 0);

        wait(&events, 1, |event| match event {
            Event::Matches { matches, .. } => {
                found += matches.len();
                false
            }
            Event::SearchDone { .. } => true,
            _ => false,
        });

        println!(
            "search          {:8.1} ms   {found} matches of {query:?}",
            milliseconds(started)
        );
    }

    std::thread::sleep(std::time::Duration::from_millis(100));
    report("searched again");

    0
}

fn milliseconds(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// Takes events until `counts` has accepted `count` of them.
fn wait(events: &Receiver<Event>, count: usize, mut counts: impl FnMut(Event) -> bool) {
    let mut seen = 0;

    while seen < count {
        match events.recv() {
            Ok(event) => seen += counts(event) as usize,
            Err(_) => return,
        }
    }
}

fn report(stage: &str) {
    println!("                {}", trace::usage_line(stage));
}
