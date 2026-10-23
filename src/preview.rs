//! Frames → PNG files, for checking layouts without hardware.
//!
//! Pixels are scaled up and drawn in "paper" grey and "ink" black, matching
//! `docs/ui-mockup.png`, so previews can be compared against the design side
//! by side.

use std::path::{Path, PathBuf};

use image::{GrayImage, Luma};

use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use crate::panel::{Panel, Refresh};

/// Background (white pixel) shade.
pub const PAPER: u8 = 0xE8;
/// Foreground (black pixel) shade.
pub const INK: u8 = 0x1A;

/// Why a PNG could not be written.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    /// Encoding or writing failed.
    #[error("cannot write {path}: {source}; check the output directory exists and is writable")]
    Write {
        /// Target file.
        path: PathBuf,
        /// Underlying error.
        source: image::ImageError,
    },
}

/// Write `frame` to `path` as a PNG, each pixel scaled to `scale`×`scale`.
pub fn save_png(frame: &Framebuffer, path: &Path, scale: u32) -> Result<(), PreviewError> {
    let scale = scale.max(1);
    let img = GrayImage::from_fn(WIDTH * scale, HEIGHT * scale, |x, y| {
        Luma([if frame.pixel(x / scale, y / scale).is_on() {
            INK
        } else {
            PAPER
        }])
    });
    img.save(path).map_err(|source| PreviewError::Write {
        path: path.to_owned(),
        source,
    })
}

/// A [`Panel`] that writes every presented frame to `<dir>/frame-NNN.png`.
#[derive(Debug)]
pub struct PngPanel {
    dir: PathBuf,
    scale: u32,
    count: u32,
}

impl PngPanel {
    /// Write frames into `dir` (which must exist) at `scale`×.
    pub fn new(dir: impl Into<PathBuf>, scale: u32) -> Self {
        Self {
            dir: dir.into(),
            scale,
            count: 0,
        }
    }
}

impl Panel for PngPanel {
    type Error = PreviewError;

    fn present(&mut self, frame: &Framebuffer, _refresh: Refresh) -> Result<(), PreviewError> {
        let path = self.dir.join(format!("frame-{:03}.png", self.count));
        self.count += 1;
        save_png(frame, &path, self.scale)
    }

    fn sleep(&mut self) -> Result<(), PreviewError> {
        Ok(())
    }
}
