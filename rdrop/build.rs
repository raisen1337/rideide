//! Draws rdrop's icon from its SVG: an .ico for Explorer and the taskbar, and the pixels its
//! window shows.

use std::path::{Path, PathBuf};

fn tree(svg: &Path) -> resvg::usvg::Tree {
    println!("cargo:rerun-if-changed={}", svg.display());
    let data = std::fs::read(svg).expect("reading an icon");
    resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).expect("parsing an icon")
}

/// The icon at `size` pixels square, as straight (not premultiplied) RGBA.
fn render(tree: &resvg::usvg::Tree, size: u32) -> Vec<u8> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
    let scale = size as f32 / tree.size().width();
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(tree, transform, &mut pixmap.as_mut());
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// An .ico holding each size as a 32-bit bitmap.
fn ico(tree: &resvg::usvg::Tree, sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&size| {
            let rgba = render(tree, size);
            let mut dib = Vec::new();
            // BITMAPINFOHEADER; the height counts the colour rows and the mask's.
            for value in [40u32, size, size * 2] {
                dib.extend(value.to_le_bytes());
            }
            dib.extend(1u16.to_le_bytes());
            dib.extend(32u16.to_le_bytes());
            dib.extend([0u8; 24]);
            // Rows bottom-up, as BGRA.
            for row in (0..size as usize).rev() {
                let start = row * size as usize * 4;
                for px in rgba[start..start + size as usize * 4].chunks_exact(4) {
                    dib.extend([px[2], px[1], px[0], px[3]]);
                }
            }
            // The transparency mask, unused with an alpha channel: zeros, rows padded to 4 bytes.
            let mask_row = (size as usize).div_ceil(32) * 4;
            dib.extend(vec![0u8; mask_row * size as usize]);
            dib
        })
        .collect();
    let mut out = Vec::new();
    out.extend(0u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend((sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (size, image) in sizes.iter().zip(&images) {
        let side = if *size >= 256 { 0 } else { *size as u8 };
        out.extend([side, side, 0, 0]);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((image.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend(image);
    }
    out
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let own = tree(&root.join("assets").join("rdrop.svg"));
    std::fs::write(out.join("icon-128.rgba"), render(&own, 128)).unwrap();
    let icon = out.join("rdrop.ico");
    std::fs::write(&icon, ico(&own, &[16, 20, 24, 32, 40, 48, 64, 256])).unwrap();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon(&icon.to_string_lossy())
            .compile()
            .expect("embedding the icon failed");
    }
}
