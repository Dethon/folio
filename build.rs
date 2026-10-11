//! Places the PDFium library next to the built executable, where the engine
//! looks for it at run time. Fetch it first with `scripts/fetch-pdfium.sh`.

use std::path::PathBuf;

fn main() {
    let target = std::env::var("TARGET").unwrap();
    let library = if target.contains("windows") {
        "pdfium.dll"
    } else if target.contains("apple") {
        "libpdfium.dylib"
    } else {
        "libpdfium.so"
    };

    let source = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("vendor/pdfium")
        .join(&target)
        .join(library);

    println!("cargo:rerun-if-changed={}", source.display());

    if !source.exists() {
        println!(
            "cargo:warning=PDFium not found at {}; run scripts/fetch-pdfium.sh",
            source.display()
        );
        return;
    }

    // OUT_DIR is <target dir>/<profile>/build/<package>-<hash>/out.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    if let Some(profile_dir) = out_dir.ancestors().nth(3) {
        let _ = std::fs::copy(&source, profile_dir.join(library));
        // Test executables run from `deps`.
        let _ = std::fs::copy(&source, profile_dir.join("deps").join(library));
    }
}
