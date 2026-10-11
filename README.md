# Folio

A fast, minimal PDF reader for Windows, written in Rust. It reads, copies
text, prints, and exports to Markdown — and nothing else.

Rendering is [PDFium](https://pdfium.googlesource.com/pdfium/) (the engine in
Chrome); the interface is [Iced](https://iced.rs) on the GPU.

## What it does

- **Reading.** Continuous scroll, zoom from 10 % to 800 %, fit width / fit
  page, outline and page-thumbnail sidebar, light and dark themes, drag and
  drop to open.
- **Copying.** Drag to select; double-click for a word, triple-click for a
  line, Shift-click to extend, Ctrl+A for everything; Ctrl+C copies.
- **Printing.** The Windows print dialog, with page ranges. Pages are drawn
  onto the printer by PDFium directly, so text and vector art stay vector.
- **Markdown export.** Headings (from font size and the document outline),
  paragraphs with hyphenation undone, nested lists, bold / italic / inline
  code, fenced code blocks; running headers, footers and page numbers are
  dropped. Also available without the window:
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
  scrolled away is never started.
- **Tiles, not pages.** Pages are rendered in 512 px tiles at exactly the
  device resolution, only where they intersect the viewport (plus a screenful
  of prefetch in the direction of travel). Zooming to 800 % costs the same as
  100 %. PDFium renders straight into the buffer that is handed to the GPU.
- **Virtualised everything.** The page stack, the outline and the thumbnail
  list only build what is on screen; cost does not grow with document length.
- **Bounded memory.** Tiles live in a 256 MB LRU; text layers and thumbnails
  in small LRUs. The file itself is read on demand, never loaded whole.
- **Idle means idle.** There are no timers or animation loops; nothing runs
  until there is input.
- **One graphics backend.** On Windows the renderer is pinned to Direct3D 12
  instead of probing Vulkan and OpenGL as well, which saves start-up time and
  about 80 MB. Set `WGPU_BACKEND` to override.

Measured on Windows 11 with a 521-page book (release build):

| | |
|---|---|
| Launch to first page fully rendered | ≈ 0.5 s, nearly all of it GPU start-up |
| Rendering the first screen (12 tiles) | 18 ms |
| Continuous scrolling | steady at the display's 240 Hz, 4.5 ms p95 frame time |
| Markdown export of all 521 pages | 0.35 s |
| Memory with the book open | ≈ 115 MB working set |
| CPU while idle | 0 |

## Layout

```
src/
  main.rs            command line
  app.rs             state, messages, update loop
  engine/            no UI dependencies
    mod.rs           Engine handle, events, work queue
    worker.rs        the PDFium thread: rendering, text extraction, jobs
    text.rs          text layer, hit testing, selection ranges
    markdown.rs      structure inference and Markdown output
    print.rs         Windows print dialog and device rendering
  ui/
    viewer.rs        page view: layout, scrolling, zoom, tile cache, selection
    sidebar.rs       outline and thumbnails
    theme.rs         colour tokens and widget styles
    icon.rs          Lucide icons
installer/folio.nsi  Windows installer (NSIS)
examples/make-icon.rs  renders the icon artwork to .ico / .png / .bmp
```

## Not there yet

- Search, clickable links, and password-protected documents.
- Annotations and form filling (annotations are displayed, not editable).
- Markdown export does not reconstruct tables or extract images.
- Printing outside Windows hands the whole file to `lp`.
- `folio.exe` itself carries no icon resource (that needs a resource compiler
  the cross-build does not have), so the bare file shows a generic icon in
  Explorer. The window, the Start menu shortcut and the installed-apps entry
  all show Folio's icon.
- The installer is unsigned.

## Credits

PDFium (BSD-3-Clause / Apache-2.0), binaries from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries);
[pdfium-render](https://github.com/ajrcarey/pdfium-render); Iced (MIT);
[Lucide](https://lucide.dev) icons (ISC).
