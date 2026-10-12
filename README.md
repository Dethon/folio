# Folio

A fast, minimal PDF reader for Windows, written in Rust. It reads, searches,
copies text, prints, and exports to Markdown — and nothing else.

Rendering is [PDFium](https://pdfium.googlesource.com/pdfium/) (the engine in
Chrome); the interface is [Iced](https://iced.rs) on the GPU.

## What it does

- **Reading.** Continuous scroll, zoom from 10 % to 800 %, fit width / fit
  page, outline and page-thumbnail sidebar, light and dark themes, drag and
  drop to open.
- **Searching.** Ctrl+F finds as you type, starting from where you are, and
  highlights every match. Case and accents are ignored, a phrase is found
  across a line break, and so is a word hyphenated over two lines.
- **Copying.** Drag to select; double-click for a word, triple-click for a
  line, Shift-click to extend, Ctrl+A for everything; Ctrl+C copies.
- **Printing.** The Windows print dialog, with page ranges. Pages are drawn
  onto the printer by PDFium directly, so text and vector art stay vector.
- **Markdown export.** Headings (from font size and the document outline),
  paragraphs with hyphenation undone, nested lists, bold / italic / inline
  code, fenced code blocks, and tables; running headers, footers and page
  numbers are dropped. Also available without the window:
  `folio --export-markdown in.pdf out.md`.

`folio --help` lists the command-line options.

## Installing

Run `Folio-<version>-setup.exe`. It installs for the current user, so it does
not ask for administrator rights, and adds a Start menu entry. Folio is added
to the *Open with* list for PDF files; whichever program opens PDFs today
keeps doing so until you choose Folio under Settings › Apps › Default apps.
Uninstall from Settings › Apps › Installed apps.

The installer is not code-signed, so Windows SmartScreen will warn on first
run: choose *More info*, then *Run anyway*. `Folio-<version>-setup.exe /S`
installs silently.

There is also a portable folder, `folio-windows-x64`, that runs from anywhere.

## Shortcuts

| | |
|---|---|
| Open | Ctrl+O |
| Print | Ctrl+P |
| Export as Markdown | Ctrl+E |
| Find | Ctrl+F |
| Next / previous match | Enter / Shift+Enter, F3 / Shift+F3, or Ctrl+G / Ctrl+Shift+G |
| Close the find bar | Esc |
| Copy / Select all / Clear selection | Ctrl+C / Ctrl+A / Esc |
| Zoom in / out / actual size | Ctrl++ / Ctrl+− / Ctrl+0, or Ctrl+wheel |
| Fit width / fit page | Ctrl+1 / Ctrl+2 |
| Toggle sidebar | Ctrl+B |
| Scroll | wheel, arrows, PgUp / PgDn, Space, Home / End |

## Building

PDFium is loaded at run time from next to the executable. Fetch the prebuilt
library once (it is not checked in), then build as usual; the build script
copies it into place.

On Windows:

```powershell
.\scripts\fetch-pdfium.ps1
cargo run --release -- path\to\file.pdf
```

On Linux or WSL (for development, and to cross-compile the Windows build):

```sh
./scripts/fetch-pdfium.sh
cargo run --release -- path/to/file.pdf      # native
cargo test                                   # no GPU needed
./scripts/package-windows.sh                 # -> dist/
```

`package-windows.sh` cross-compiles the release and produces both
`dist/Folio-<version>-setup.exe` and the portable `dist/folio-windows-x64/`.
It needs [`cargo-xwin`](https://github.com/rust-cross/cargo-xwin) and
[NSIS](https://nsis.sourceforge.io) (`apt install nsis`; on Debian/Ubuntu
without root the script unpacks NSIS into `target/nsis` by itself). Set
`PUBLISHER="Your Name"` to name a publisher in the installer. The installer
is defined in `installer/folio.nsi`.

The icon is drawn in `assets/icon/folio.svg`; after changing it, run
`cargo run --example make-icon` to regenerate the `.ico`, the window icon and
the installer artwork.

However it is shipped, `folio.exe` and `pdfium.dll` must stay together.

## How it stays fast

- **One engine thread.** PDFium is not thread safe, so a single worker owns it
  and the UI never waits on it. The UI publishes what the current view is
  missing; that list is replaced on every scroll, so work for content that has
  scrolled away is never started. What the engine produces is taken in a batch
  per frame, so a burst of tiles redraws the interface once.
- **The first page is rendered before there is a window.** Nearly all of
  start-up is the graphics driver creating its device. The engine does not
  wait for it: it is told the shape the view will have, and has the first
  screen rendered some 30 ms after launch. The first frame that shows the
  document shows it complete. Fonts are discovered on another thread in the
  same interval.
- **Tiles, not pages.** Pages are rendered in 510 px tiles at exactly the
  device resolution, only where they intersect the viewport (plus a screenful
  of prefetch in the direction of travel). Zooming to 800 % costs the same as
  100 %. PDFium renders straight into the buffer that is handed to the GPU.
- **Virtualised everything.** The page stack, the outline and the thumbnail
  list only build what is on screen; cost does not grow with document length.
- **Bounded memory.** Rendered tiles are kept for the view, its prefetch, and
  half as much again for scrolling back: some twenty megabytes in a window of
  ordinary size. Text layers and thumbnails live in small LRUs.
  The file is read on demand, never loaded whole. PDFium itself keeps
  everything it has parsed for as long as a document is open, so no handle on
  the document is kept for long: searches and exports run through one of
  their own that is closed afterwards, and the main one is traded for a fresh
  one every hundred pages or so.
- **Search reads a page once.** The text of every page searched is remembered
  as search sees it, at about a byte a character. Later searches, which is to
  say every keystroke after the first, only open the pages they will find
  something on.
- **Idle means idle.** There are no timers or animation loops; nothing runs
  until there is input.
- **One graphics backend.** On Windows the renderer is pinned to Direct3D 12
  instead of probing Vulkan and OpenGL as well, which saves start-up time and
  about 80 MB. Set `WGPU_BACKEND` to override.

Measured on Windows 11 (Ryzen 9 9950X3D, RTX 4090, 240 Hz display) with a
501-page book, release build:

| | |
|---|---|
| Launch to first page fully rendered | 0.35 s, of which 0.3 s is the graphics driver starting |
| First screen rendered by the engine | 30 ms after launch |
| Rendering | 0.66 ms per tile, 250 megapixels a second |
| Continuous scrolling | steady at the display's 240 Hz, 4.5 ms p95 frame interval, no frame with a tile missing |
| First search of the whole book | 0.28 s, with matches shown as they are found, from the page being read |
| Later searches | 6 ms for a phrase that is not there, 47 ms for a word found 174 times, 0.27 s for one on nearly every page |
| Markdown export of all 501 pages | 0.42 s |
| Memory with the book open | 120 MB working set, 98 MB of it the window and the graphics driver |
| Memory after scrolling through 40 pages | 140 MB |
| Memory of the engine alone, after searching the whole book | 18 MB |
| Threads | 23 |
| CPU while idle | 0 |
| `folio.exe` | 10 MB |

The graphics driver is what the two headline numbers are made of, and it is
the one part that differs from machine to machine: an empty window is 98 MB
and 0.3 s of the figures above.

### Measuring it

Three undocumented switches produce the numbers above:

```sh
FOLIO_TRACE=trace.txt folio book.pdf --screenshot shot.png   # start-up timeline, memory once settled
FOLIO_TRACE=trace.txt folio book.pdf --scroll-test 1500      # frame intervals over 1500 frames of scrolling
folio --bench book.pdf                                       # the engine alone: open, render, text, search
```

`FOLIO_TRACE` names a file that the timeline is appended to, in milliseconds
since the process was created. Adding `FOLIO_TRACE_LOG=1` includes what the
windowing and graphics libraries log, which is how the start-up time was
attributed, at some cost to the timings themselves.

## Layout

```
src/
  main.rs            command line
  app.rs             state, messages, update loop
  trace.rs           start-up timeline and memory report (FOLIO_TRACE)
  bench.rs           engine benchmark (--bench)
  engine/            no UI dependencies
    mod.rs           Engine handle, events, work queue
    worker.rs        the PDFium thread: rendering, text extraction, jobs
    text.rs          text layer, hit testing, selection ranges
    markdown.rs      structure inference and Markdown output
    table.rs         table detection from ruling lines
    print.rs         Windows print dialog and device rendering
  ui/
    viewer.rs        page view: layout, scrolling, zoom, tile cache, selection
    sidebar.rs       outline and thumbnails
    theme.rs         colour tokens and widget styles
    icon.rs          Lucide icons, rasterised at the size they are shown
installer/folio.nsi  Windows installer (NSIS)
examples/make-icon.rs  renders the icon artwork to .ico / .png / .bmp
```

## Not there yet

- Clickable links and password-protected documents.
- Search matches whole text only: no whole-word or regular-expression
  options, a phrase split across two pages is not found, and it stops
  counting at 20,000 matches.
- Annotations and form filling (annotations are displayed, not editable).
- Markdown export only recognises tables that are drawn: a grid, or the
  horizontal rules of a typeset table. Tables laid out by spacing alone stay
  as text, since they cannot be told from columns of prose. Merged cells are
  flattened (Markdown has none), a table continuing onto the next page
  becomes two tables, and images are not extracted.
- Printing outside Windows hands the whole file to `lp`.
- `folio.exe` itself carries no icon resource (that needs a resource compiler
  the cross-build does not have), so the bare file shows a generic icon in
  Explorer. The window, the Start menu shortcut and the installed-apps entry
  all show Folio's icon.
- The installer is unsigned.

## Licence

Folio is released under the [MIT licence](LICENSE).

## Credits

PDFium (BSD-3-Clause / Apache-2.0), binaries from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries);
[pdfium-render](https://github.com/ajrcarey/pdfium-render); Iced (MIT);
[Lucide](https://lucide.dev) icons (ISC).
