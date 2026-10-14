//! Renders assets/ride.svg into the icon files the build uses. Run after changing the SVG:
//!
//!     cargo run -p ride --example make_icon
//!
//! It writes ride.ico (for the executables), icon-128.rgba (for the window) and
//! ride.png (to look at).

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::{ExtendedColorType, RgbaImage};
use std::path::Path;

fn render(svg: &[u8], size: u32) -> RgbaImage {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default())
        .expect("ride.svg should parse");
    let scale = size as f32 / tree.size().width();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia keeps colour premultiplied by alpha; icon formats want it straight.
    let mut pixels = pixmap.take();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(size, size, pixels).unwrap()
}

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let svg = std::fs::read(assets.join("ride.svg")).expect("assets/ride.svg");

    let frames: Vec<IcoFrame> = [16, 20, 24, 32, 40, 48, 64, 128, 256]
        .into_iter()
        .map(|size| {
            let image = render(&svg, size);
            IcoFrame::as_png(image.as_raw(), size, size, ExtendedColorType::Rgba8).unwrap()
        })
        .collect();
    let ico = std::fs::File::create(assets.join("ride.ico")).unwrap();
    IcoEncoder::new(ico).encode_images(&frames).unwrap();

    std::fs::write(assets.join("icon-128.rgba"), render(&svg, 128).as_raw()).unwrap();
    render(&svg, 256).save(assets.join("ride.png")).unwrap();
    println!(
        "wrote ride.ico, icon-128.rgba and ride.png in {}",
        assets.display()
    );
}
