//! Printing.
//!
//! On Windows the system print dialog hands back a printer device context and
//! PDFium draws each page straight onto it, so text and vector art reach the
//! printer as text and vector art rather than as a bitmap. Elsewhere the file
//! is handed to the system spooler.

use super::DocInfo;

pub use platform::{Session, Target, prompt};

/// What the user chose in the print dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    All,
    /// Zero-based page indices, in print order.
    #[cfg_attr(not(windows), allow(dead_code))]
    Pages(Vec<u32>),
}

impl Selection {
    fn resolve(&self, page_count: u32) -> Vec<u32> {
        match self {
            Selection::All => (0..page_count).collect(),
            Selection::Pages(pages) => pages
                .iter()
                .copied()
                .filter(|page| *page < page_count)
                .collect(),
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;

    use windows::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
    use windows::Win32::Graphics::Gdi::{
        DeleteDC, GetDeviceCaps, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, VERTRES,
    };
    use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
    use windows::Win32::UI::Controls::Dialogs::{
        PD_ALLPAGES, PD_CURRENTPAGE, PD_NOSELECTION, PD_PAGENUMS, PD_RESULT_PRINT, PD_RETURNDC,
        PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGEXW, PRINTPAGERANGE, PrintDlgExW,
        START_PAGE_GENERAL,
    };
    use windows::core::PCWSTR;

    use super::{DocInfo, Selection};

    const MAX_RANGES: usize = 32;

    const FPDF_ANNOT: i32 = 0x01;
    const FPDF_PRINTING: i32 = 0x800;

    pub struct Target {
        dc: isize,
        selection: Selection,
        /// Spool to this file instead of the device. Tests use it to drive
        /// a file printer without its save dialog.
        output: Option<PathBuf>,
    }

    // The device context is only ever used by the engine thread once the
    // dialog has returned it.
    unsafe impl Send for Target {}

    impl Drop for Target {
        fn drop(&mut self) {
            if self.dc != 0 {
                unsafe {
                    let _ = DeleteDC(HDC(self.dc as *mut c_void));
                }
            }
        }
    }

    /// Shows the system print dialog. Blocks until it is dismissed, so call
    /// it off the UI thread. `Ok(None)` means the user cancelled.
    pub fn prompt(
        owner: isize,
        page_count: u32,
        current_page: u32,
    ) -> Result<Option<Target>, String> {
        let mut ranges = [PRINTPAGERANGE::default(); MAX_RANGES];

        let mut dialog = PRINTDLGEXW {
            lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
            hwndOwner: HWND(owner as *mut c_void),
            Flags: PD_RETURNDC | PD_ALLPAGES | PD_NOSELECTION | PD_USEDEVMODECOPIESANDCOLLATE,
            nMaxPageRanges: MAX_RANGES as u32,
            lpPageRanges: ranges.as_mut_ptr(),
            nMinPage: 1,
            nMaxPage: page_count.max(1),
            nCopies: 1,
            nStartPage: START_PAGE_GENERAL,
            ..Default::default()
        };

        let result = unsafe { PrintDlgExW(&mut dialog) };

        let release = |handle: HGLOBAL| {
            if !handle.is_invalid() {
                unsafe {
                    let _ = GlobalFree(Some(handle));
                }
            }
        };
        release(dialog.hDevMode);
        release(dialog.hDevNames);

        result.map_err(|error| format!("The print dialog failed: {error}"))?;

        if dialog.dwResultAction != PD_RESULT_PRINT || dialog.hDC.is_invalid() {
            if !dialog.hDC.is_invalid() {
                unsafe {
                    let _ = DeleteDC(dialog.hDC);
                }
            }
            return Ok(None);
        }

        let selection = if dialog.Flags.contains(PD_CURRENTPAGE) {
            Selection::Pages(vec![current_page])
        } else if dialog.Flags.contains(PD_PAGENUMS) {
            let count = (dialog.nPageRanges as usize).min(MAX_RANGES);

            Selection::Pages(
                ranges[..count]
                    .iter()
                    .flat_map(|range| {
                        let from = range.nFromPage.max(1);
                        let to = range.nToPage.max(from);
                        (from - 1)..to
                    })
                    .collect(),
            )
        } else {
            Selection::All
        };

        Ok(Some(Target {
            dc: dialog.hDC.0 as isize,
            selection,
            output: None,
        }))
    }

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    impl Target {
        /// Targets a printer by name without showing a dialog.
        #[cfg(test)]
        pub fn printer(
            name: &str,
            selection: Selection,
            output: Option<PathBuf>,
        ) -> Result<Self, String> {
            let name = wide(name.as_ref());
            let dc = unsafe {
                windows::Win32::Graphics::Gdi::CreateDCW(
                    PCWSTR::null(),
                    PCWSTR(name.as_ptr()),
                    PCWSTR::null(),
                    None,
                )
            };

            if dc.is_invalid() {
                return Err("The printer could not be opened.".into());
            }

            Ok(Self {
                dc: dc.0 as isize,
                selection,
                output,
            })
        }
    }

    type Document = *mut c_void;
    type Page = *mut c_void;

    /// The handful of PDFium entry points needed to draw onto a device
    /// context. PDFium is already loaded and initialised by the engine; this
    /// only takes another reference to the same module.
    struct Pdfium {
        load: unsafe extern "C" fn(*const c_void, usize, *const i8) -> Document,
        close: unsafe extern "C" fn(Document),
        load_page: unsafe extern "C" fn(Document, i32) -> Page,
        close_page: unsafe extern "C" fn(Page),
        page_width: unsafe extern "C" fn(Page) -> f32,
        page_height: unsafe extern "C" fn(Page) -> f32,
        render: unsafe extern "C" fn(HDC, Page, i32, i32, i32, i32, i32, i32),
        _library: libloading::Library,
    }

    impl Pdfium {
        fn load() -> Result<Self, String> {
            let path = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("pdfium.dll")))
                .filter(|path| path.exists())
                .unwrap_or_else(|| PathBuf::from("pdfium.dll"));

            unsafe {
                let library = libloading::Library::new(path).map_err(|error| error.to_string())?;

                macro_rules! symbol {
                    ($name:literal) => {
                        *library
                            .get($name)
                            .map_err(|error: libloading::Error| error.to_string())?
                    };
                }

                Ok(Self {
                    load: symbol!(b"FPDF_LoadMemDocument64\0"),
                    close: symbol!(b"FPDF_CloseDocument\0"),
                    load_page: symbol!(b"FPDF_LoadPage\0"),
                    close_page: symbol!(b"FPDF_ClosePage\0"),
                    page_width: symbol!(b"FPDF_GetPageWidthF\0"),
                    page_height: symbol!(b"FPDF_GetPageHeightF\0"),
                    render: symbol!(b"FPDF_RenderPage\0"),
                    _library: library,
                })
            }
        }
    }

    pub struct Session {
        pdfium: Pdfium,
        document: Document,
        // PDFium reads from the mapping for as long as the document is open.
        _data: memmap2::Mmap,
        target: Target,
        pages: Vec<u32>,
        next: usize,
        finished: bool,
    }

    impl Session {
        pub fn begin(target: Target, info: &DocInfo) -> Result<Self, String> {
            let pdfium = Pdfium::load()?;
            let file = std::fs::File::open(&info.path).map_err(|error| error.to_string())?;
            let data = unsafe { memmap2::Mmap::map(&file) }.map_err(|error| error.to_string())?;

            let document =
                unsafe { (pdfium.load)(data.as_ptr().cast(), data.len(), std::ptr::null()) };

            if document.is_null() {
                return Err("The document could not be opened for printing.".into());
            }

            let pages = target.selection.resolve(info.pages.len() as u32);

            let name = wide(info.title.as_ref());
            let output = target.output.as_ref().map(|path| wide(path.as_os_str()));
            let doc_info = DOCINFOW {
                cbSize: std::mem::size_of::<DOCINFOW>() as i32,
                lpszDocName: PCWSTR(name.as_ptr()),
                lpszOutput: output
                    .as_ref()
                    .map_or(PCWSTR::null(), |output| PCWSTR(output.as_ptr())),
                ..Default::default()
            };

            let session = Self {
                pdfium,
                document,
                _data: data,
                target,
                pages,
                next: 0,
                finished: false,
            };

            if unsafe { StartDocW(session.dc(), &doc_info) } <= 0 {
                return Err("The printer did not accept the job.".into());
            }

            Ok(session)
        }

        fn dc(&self) -> HDC {
            HDC(self.target.dc as *mut c_void)
        }

        /// Prints one page. Returns progress, or `None` once the job has
        /// been handed to the spooler.
        pub fn step(&mut self) -> Result<Option<(u32, u32)>, String> {
            let dc = self.dc();

            let Some(index) = self.pages.get(self.next).copied() else {
                self.finished = true;

                return if unsafe { EndDoc(dc) } > 0 {
                    Ok(None)
                } else {
                    Err("The printer rejected the job.".into())
                };
            };

            let page = unsafe { (self.pdfium.load_page)(self.document, index as i32) };

            if !page.is_null() {
                unsafe {
                    if StartPage(dc) <= 0 {
                        (self.pdfium.close_page)(page);
                        return Err("The printer stopped responding.".into());
                    }

                    // Device units of the printable area, whose origin is
                    // also the origin of the device context.
                    let area = (
                        GetDeviceCaps(Some(dc), HORZRES),
                        GetDeviceCaps(Some(dc), VERTRES),
                    );
                    let dpi = (
                        GetDeviceCaps(Some(dc), LOGPIXELSX) as f32,
                        GetDeviceCaps(Some(dc), LOGPIXELSY) as f32,
                    );

                    let mut size = (
                        (self.pdfium.page_width)(page),
                        (self.pdfium.page_height)(page),
                    );

                    // Turn the page when that matches the paper's orientation.
                    let rotate = (size.0 > size.1) != (area.0 > area.1);
                    if rotate {
                        size = (size.1, size.0);
                    }

                    let natural = (size.0 / 72.0 * dpi.0, size.1 / 72.0 * dpi.1);
                    // Print at actual size, shrinking only when the page
                    // would not fit the printable area.
                    let fit = (area.0 as f32 / natural.0)
                        .min(area.1 as f32 / natural.1)
                        .min(1.0);
                    let width = (natural.0 * fit).round() as i32;
                    let height = (natural.1 * fit).round() as i32;

                    (self.pdfium.render)(
                        dc,
                        page,
                        (area.0 - width) / 2,
                        (area.1 - height) / 2,
                        width,
                        height,
                        if rotate { 1 } else { 0 },
                        FPDF_ANNOT | FPDF_PRINTING,
                    );

                    (self.pdfium.close_page)(page);

                    if EndPage(dc) <= 0 {
                        return Err("The printer stopped responding.".into());
                    }
                }
            }

            self.next += 1;

            Ok(Some((self.next as u32, self.pages.len() as u32)))
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                if !self.finished {
                    let _ = AbortDoc(self.dc());
                }

                (self.pdfium.close)(self.document);
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::PathBuf;
    use std::process::Command;

    use super::{DocInfo, Selection};

    pub struct Target {
        selection: Selection,
    }

    /// There is no portable print dialog; the whole document goes to the
    /// default printer.
    pub fn prompt(
        _owner: isize,
        _page_count: u32,
        _current_page: u32,
    ) -> Result<Option<Target>, String> {
        Ok(Some(Target {
            selection: Selection::All,
        }))
    }

    pub struct Session {
        path: PathBuf,
        pages: Vec<u32>,
        sent: bool,
    }

    impl Session {
        pub fn begin(target: Target, info: &DocInfo) -> Result<Self, String> {
            Ok(Self {
                path: info.path.clone(),
                pages: target.selection.resolve(info.pages.len() as u32),
                sent: false,
            })
        }

        pub fn step(&mut self) -> Result<Option<(u32, u32)>, String> {
            if self.sent {
                return Ok(None);
            }

            let status = Command::new("lp")
                .arg("--")
                .arg(&self.path)
                .status()
                .map_err(|_| {
                    "Printing needs the `lp` command (CUPS), which was not found.".to_owned()
                })?;

            if !status.success() {
                return Err("The print spooler rejected the document.".into());
            }

            self.sent = true;

            let total = self.pages.len() as u32;
            Ok(Some((total, total)))
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::engine::PageSize;

    /// Prints the fixture through the driver every Windows install ships,
    /// which writes a PDF, and checks that pages came out the other end.
    #[test]
    fn prints_to_a_file_printer() {
        let fixture = crate::engine::tests::fixture_path();
        let output = std::env::temp_dir().join(format!("folio-print-{}.pdf", std::process::id()));
        let _ = std::fs::remove_file(&output);

        let info = DocInfo {
            id: 1,
            path: fixture,
            title: "Folio print test".into(),
            pages: vec![
                PageSize {
                    width: 612.0,
                    height: 792.0,
                };
                2
            ],
            outline: Vec::new(),
        };

        // PDFium must be initialised before the session borrows it.
        let _engine = crate::engine::tests::open_fixture();

        let target = Target::printer(
            "Microsoft Print to PDF",
            Selection::All,
            Some(output.clone()),
        )
        .expect("open printer");
        let mut session = Session::begin(target, &info).expect("begin job");
        let mut pages = 0;

        while let Some((done, total)) = session.step().expect("print page") {
            assert_eq!(total, 2);
            pages = done;
        }

        drop(session);

        assert_eq!(pages, 2);

        let printed = std::fs::read(&output).expect("printer wrote a file");
        let _ = std::fs::remove_file(&output);

        assert!(printed.starts_with(b"%PDF"), "output is a PDF");
        assert!(printed.len() > 2_000, "output has content");
    }
}
