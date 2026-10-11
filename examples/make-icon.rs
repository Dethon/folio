//! Regenerates the application icon from `assets/icon/folio.svg`:
//!
//!     cargo run --example make-icon
//!
//! Writes `folio.ico` (for the installer and shortcuts) and `folio.png` (the
//! window icon) next to the SVG, and `installer/welcome.bmp`, the side panel
//! of the installer's wizard, which shows the same artwork on a dark ground.
//! All three are checked in, so this only needs running when the artwork
//! changes.

use std::path::Path;

use resvg::{tiny_skia, usvg};

/// Sizes Windows asks for across the shell at 100–200 % scaling.
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];
const WINDOW_ICON: u32 = 64;
/// The size NSIS lays its welcome-page bitmap out at.
const SIDE_PANEL: (u32, u32) = (164, 314);

fn main() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon");
    let svg = std::fs::read(directory.join("folio.svg")).expect("read folio.svg");
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("parse folio.svg");

    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|size| {
            let rgba = render(&tree, *size);

            // Large entries are stored as PNG, small ones as bitmaps, which
            // is what every consumer of .ico files expects.
            (
                *size,
                if *size >= 256 {
                    png(&rgba, *size)
                } else {
                    dib(&rgba, *size)
                },
            )
        })
        .collect();

    std::fs::write(directory.join("folio.ico"), ico(&images)).expect("write folio.ico");
    std::fs::write(
        directory.join("folio.png"),
        png(&render(&tree, WINDOW_ICON), WINDOW_ICON),
    )
    .expect("write folio.png");

    let side = usvg::Tree::from_str(&side_panel(&svg), &usvg::Options::default())
        .expect("parse side panel");

    std::fs::write(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("installer/welcome.bmp"),
        bmp(
            &render_sized(&side, SIDE_PANEL.0, SIDE_PANEL.1),
            SIDE_PANEL.0,
            SIDE_PANEL.1,
        ),
    )
    .expect("write welcome.bmp");
}

/// The wizard's side panel: the icon, nested as-is, on a night-blue ground.
fn side_panel(icon: &[u8]) -> String {
    const ROOT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg""#;

    let icon = std::str::from_utf8(icon).expect("folio.svg is UTF-8");
    assert!(
        icon.starts_with(ROOT),
        "folio.svg starts with its <svg> tag"
    );

    let (width, height) = SIDE_PANEL;
    let size = 112;
    let nested = icon.replacen(
        ROOT,
        &format!(
            r#"<svg x="{}" y="{}" width="{size}" height="{size}""#,
            (width - size) / 2,
            height * 2 / 7,
        ),
        1,
    );

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}">
  <defs>
    <linearGradient id="panel" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#3a37a6"/>
      <stop offset="1" stop-color="#16153f"/>
    </linearGradient>
  </defs>
  <rect width="{width}" height="{height}" fill="url(#panel)"/>
  {nested}
</svg>"##
    )
}

/// Rasterises square artwork to straight (not premultiplied) RGBA.
fn render(tree: &usvg::Tree, size: u32) -> Vec<u8> {
    render_sized(tree, size, size)
}

fn render_sized(tree: &usvg::Tree, width: u32, height: u32) -> Vec<u8> {
    let mut pixmap = tiny_skia::Pixmap::new(width, height).expect("allocate pixmap");
    let scale = width as f32 / tree.size().width();

    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect()
}

fn png(rgba: &[u8], size: u32) -> Vec<u8> {
    use image::ImageEncoder;

    let mut out = Vec::new();

    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba, size, size, image::ExtendedColorType::Rgba8)
        .expect("encode png");
    out
}

/// A 32-bit device-independent bitmap as stored inside an icon: a
/// BITMAPINFOHEADER claiming twice the height, bottom-up BGRA rows, then a
/// 1-bit transparency mask.
fn dib(rgba: &[u8], size: u32) -> Vec<u8> {
    let mut out = Vec::new();

    out.extend(40u32.to_le_bytes());
    out.extend((size as i32).to_le_bytes());
    out.extend((size as i32 * 2).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend([0u8; 24]);

    for row in rgba.chunks_exact(size as usize * 4).rev() {
        for pixel in row.chunks_exact(4) {
            out.extend([pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }

    // Mask rows are padded to 32 bits. The alpha channel already carries
    // the transparency, so the mask only marks fully transparent pixels.
    let mask_row = size.div_ceil(32) as usize * 4;

    for row in rgba.chunks_exact(size as usize * 4).rev() {
        let mut bits = vec![0u8; mask_row];

        for (x, pixel) in row.chunks_exact(4).enumerate() {
            if pixel[3] == 0 {
                bits[x / 8] |= 0x80 >> (x % 8);
            }
        }

        out.extend(bits);
    }

    out
}

/// An opaque 24-bit bitmap file, the format the installer's wizard takes.
fn bmp(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row = (width as usize * 3).next_multiple_of(4);
    let pixels = row * height as usize;
    let mut out = Vec::with_capacity(54 + pixels);

    out.extend(b"BM");
    out.extend((54 + pixels as u32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(54u32.to_le_bytes());

    out.extend(40u32.to_le_bytes());
    out.extend((width as i32).to_le_bytes());
    out.extend((height as i32).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(24u16.to_le_bytes());
    out.extend([0u8; 24]);

    for line in rgba.chunks_exact(width as usize * 4).rev() {
        let start = out.len();

        for pixel in line.chunks_exact(4) {
            out.extend([pixel[2], pixel[1], pixel[0]]);
        }

        out.resize(start + row, 0);
    }

    out
}

fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();

    out.extend(0u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend((images.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * images.len() as u32;

    for (size, data) in images {
        // A dimension of 256 is written as 0.
        let dimension = if *size >= 256 { 0 } else { *size as u8 };

        out.extend([dimension, dimension, 0, 0]);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        offset += data.len() as u32;
    }

    for (_, data) in images {
        out.extend(data);
    }

    out
}
