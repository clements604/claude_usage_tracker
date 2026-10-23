//! Convert the colour logo into the 1-bit bitmap the view embeds.
//!
//! ```sh
//! cargo run --example logo --features preview -- resources/logo.png resources/
//! ```
//!
//! The panel can only show black or white (no greys, no dithering), so a
//! plain greyscale conversion turns the logo's shaded facets into speckle.
//! Instead: the infinity band becomes solid ink, and the boundaries between
//! facets become 1 px white lines, which keeps the faceted look and the
//! over/under crossing readable at 58 px tall. Writes:
//!
//! - `logo.raw`: packed rows, MSB first, 1 = ink (`ImageRaw<BinaryColor>`)
//! - `logo-1bit.png`: the same bitmap at 1×, for looking at
//! - `logo-1bit-preview.png`: 4× zoom in the panel's paper/ink shades

use std::error::Error;
use std::path::PathBuf;

use image::imageops::{self, FilterType};
use image::{GrayImage, Luma, RgbImage};

/// Height on the panel; the width follows the logo's aspect ratio.
const HEIGHT: u32 = 58;
/// Background is pure black; every part of the logo has blue ≥ 150.
const BLUE_MIN: u8 = 60;
/// Summed RGB difference between neighbours that counts as a facet edge.
const EDGE_DIFF: u32 = 30;
/// Share of a downscaled pixel (0–255) that must be edge to draw a line.
const EDGE_COVER: u8 = 110;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, out_dir] = args.as_slice() else {
        return Err("usage: logo <LOGO.png> <OUT_DIR>".into());
    };
    let out = PathBuf::from(out_dir);
    let src = image::open(input)?.to_rgb8();

    let mask = GrayImage::from_fn(src.width(), src.height(), |x, y| {
        Luma([if src.get_pixel(x, y)[2] > BLUE_MIN {
            255
        } else {
            0
        }])
    });
    let edges = facet_edges(&src, &mask);

    let (x0, y0, w0, h0) = bounding_box(&mask).ok_or("logo is empty")?;
    let mask = imageops::crop_imm(&mask, x0, y0, w0, h0).to_image();
    let edges = imageops::crop_imm(&edges, x0, y0, w0, h0).to_image();

    let width = (w0 * HEIGHT + h0 / 2) / h0;
    let shape = threshold(
        &imageops::resize(&mask, width, HEIGHT, FilterType::Lanczos3),
        128,
    );
    let lines = threshold(
        &imageops::resize(&max3(&edges), width, HEIGHT, FilterType::Triangle),
        EDGE_COVER,
    );
    // Lines only inside the band, never on its rim, so the outline stays solid.
    let rim_safe = min3(&shape);
    let ink = GrayImage::from_fn(width, HEIGHT, |x, y| {
        let line = lines.get_pixel(x, y)[0] > 0 && rim_safe.get_pixel(x, y)[0] > 0;
        Luma([if shape.get_pixel(x, y)[0] > 0 && !line {
            255
        } else {
            0
        }])
    });

    std::fs::write(out.join("logo.raw"), pack(&ink))?;
    let png = GrayImage::from_fn(width, HEIGHT, |x, y| {
        Luma([if ink.get_pixel(x, y)[0] > 0 { 0 } else { 255 }])
    });
    png.save(out.join("logo-1bit.png"))?;
    let zoom = GrayImage::from_fn(width * 4, HEIGHT * 4, |x, y| {
        Luma([if ink.get_pixel(x / 4, y / 4)[0] > 0 {
            0x1A
        } else {
            0xE8
        }])
    });
    zoom.save(out.join("logo-1bit-preview.png"))?;
    println!("wrote {width}x{HEIGHT} logo to {}", out.display());
    Ok(())
}

/// 255 where a pixel inside the logo differs from a neighbour by more than
/// [`EDGE_DIFF`], i.e. sits on a boundary between two facets.
fn facet_edges(src: &RgbImage, mask: &GrayImage) -> GrayImage {
    let (w, h) = src.dimensions();
    let inside = |x: u32, y: u32| mask.get_pixel(x, y)[0] > 0;
    GrayImage::from_fn(w, h, |x, y| {
        if x == 0 || y == 0 || x + 1 >= w || y + 1 >= h || !inside(x, y) {
            return Luma([0]);
        }
        let a = src.get_pixel(x, y);
        let neighbours = [(x + 1, y), (x, y + 1), (x + 1, y + 1), (x + 1, y - 1)];
        let edge = neighbours.iter().any(|&(nx, ny)| {
            let b = src.get_pixel(nx, ny);
            inside(nx, ny)
                && (0..3).map(|i| u32::from(a[i].abs_diff(b[i]))).sum::<u32>() > EDGE_DIFF
        });
        Luma([if edge { 255 } else { 0 }])
    })
}

fn bounding_box(mask: &GrayImage) -> Option<(u32, u32, u32, u32)> {
    let ink = mask.enumerate_pixels().filter(|(_, _, p)| p[0] > 0);
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, _) in ink {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    (x0 <= x1).then(|| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

fn threshold(img: &GrayImage, at: u8) -> GrayImage {
    GrayImage::from_fn(img.width(), img.height(), |x, y| {
        Luma([if img.get_pixel(x, y)[0] >= at { 255 } else { 0 }])
    })
}

/// 3×3 max (dilate) or min (erode) filter; edges treat outside as 0.
fn filter3(img: &GrayImage, pick: fn(u8, u8) -> u8, init: u8) -> GrayImage {
    let (w, h) = img.dimensions();
    GrayImage::from_fn(w, h, |x, y| {
        let mut v = init;
        for dy in -1i64..=1 {
            for dx in -1i64..=1 {
                let (nx, ny) = (i64::from(x) + dx, i64::from(y) + dy);
                let p = if nx < 0 || ny < 0 || nx >= i64::from(w) || ny >= i64::from(h) {
                    0
                } else {
                    img.get_pixel(nx as u32, ny as u32)[0]
                };
                v = pick(v, p);
            }
        }
        Luma([v])
    })
}

fn max3(img: &GrayImage) -> GrayImage {
    filter3(img, u8::max, 0)
}

fn min3(img: &GrayImage) -> GrayImage {
    filter3(img, u8::min, 255)
}

/// Row-major, MSB-first, each row padded to a whole byte; 1 = ink.
fn pack(ink: &GrayImage) -> Vec<u8> {
    let stride = ink.width().div_ceil(8) as usize;
    let mut bytes = vec![0u8; stride * ink.height() as usize];
    for (x, y, p) in ink.enumerate_pixels() {
        if p[0] > 0 {
            bytes[y as usize * stride + x as usize / 8] |= 0x80 >> (x % 8);
        }
    }
    bytes
}
