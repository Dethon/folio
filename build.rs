//! Places the PDFium library next to the built executable, where the engine
//! looks for it at run time. Fetch it first with `scripts/fetch-pdfium.sh`.
//!
//! On Windows it also gives the executable its icon.

use std::path::{Path, PathBuf};

fn main() {
    let target = std::env::var("TARGET").unwrap();

    if target.contains("windows") {
        embed_icon();
    }

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

/// Links `assets/icon/folio.ico` into the executable as its icon resource.
///
/// The resource file is written here rather than by a resource compiler,
/// which the cross-build does not have: the linker takes a `.res` as it is.
fn embed_icon() {
    let icon =
        Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/icon/folio.ico");

    println!("cargo:rerun-if-changed={}", icon.display());

    let ico = std::fs::read(&icon).expect("read folio.ico");
    let resource = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("icon.res");

    std::fs::write(&resource, icon_resource(&ico)).expect("write icon.res");

    // Binaries only: test executables have no use for an icon.
    println!("cargo:rustc-link-arg-bins={}", resource.display());
}

/// A `.res` file holding the images of an `.ico` file and the group that
/// lists them.
fn icon_resource(ico: &[u8]) -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    /// Size of the `.ico` header, and of each entry of its directory.
    const HEADER: usize = 6;
    const ENTRY: usize = 16;

    let count = usize::from(u16::from_le_bytes([ico[4], ico[5]]));

    // Every .res file opens with an empty entry.
    let mut resource = Vec::new();
    push_resource(&mut resource, 0, 0, &[]);

    // The group is the .ico directory again, with each image's offset in
    // the file replaced by the identifier of its resource.
    let mut group = ico[..HEADER].to_vec();

    for (index, entry) in ico[HEADER..].chunks_exact(ENTRY).take(count).enumerate() {
        let id = u16::try_from(index + 1).unwrap();
        let size = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;

        push_resource(&mut resource, RT_ICON, id, &ico[offset..offset + size]);
        group.extend_from_slice(&entry[..12]);
        group.extend_from_slice(&id.to_le_bytes());
    }

    // Explorer shows the group with the lowest identifier.
    push_resource(&mut resource, RT_GROUP_ICON, 1, &group);

    resource
}

/// Appends one entry of a `.res` file, named by number.
fn push_resource(resource: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    const HEADER_SIZE: u32 = 32;
    /// MOVEABLE | PURE | DISCARDABLE, as a resource compiler marks icons.
    const MEMORY_FLAGS: u16 = 0x1030;
    const LANGUAGE_NEUTRAL: u16 = 0;

    resource.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    resource.extend_from_slice(&HEADER_SIZE.to_le_bytes());

    // 0xFFFF marks a number where a name could stand.
    for ordinal in [kind, id] {
        resource.extend_from_slice(&0xFFFF_u16.to_le_bytes());
        resource.extend_from_slice(&ordinal.to_le_bytes());
    }

    resource.extend_from_slice(&0_u32.to_le_bytes()); // data version
    resource.extend_from_slice(&MEMORY_FLAGS.to_le_bytes());
    resource.extend_from_slice(&LANGUAGE_NEUTRAL.to_le_bytes());
    resource.extend_from_slice(&0_u32.to_le_bytes()); // version
    resource.extend_from_slice(&0_u32.to_le_bytes()); // characteristics
    resource.extend_from_slice(data);

    // Entries start on four-byte boundaries.
    resource.resize(resource.len().next_multiple_of(4), 0);
}
