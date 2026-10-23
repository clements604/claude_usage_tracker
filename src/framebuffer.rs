//! 1-bit in-memory canvas.
//!
//! The view draws here first; a [`Panel`](crate::panel::Panel) then copies the
//! finished frame to glass (or PNG, or a desktop window). Drawing off-screen
//! means the e-paper controller only ever receives complete frames.

use std::convert::Infallible;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;

/// Landscape width of the 2.13" panel.
pub const WIDTH: u32 = 250;
/// Landscape height of the 2.13" panel.
pub const HEIGHT: u32 = 122;

const STRIDE: usize = WIDTH.div_ceil(8) as usize;

/// A 250×122 black/white frame. [`BinaryColor::On`] is ink (black).
#[derive(Clone, PartialEq, Eq)]
pub struct Framebuffer {
    bits: Vec<u8>,
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ink = self.bits.iter().map(|b| b.count_ones()).sum::<u32>();
        write!(f, "Framebuffer({WIDTH}x{HEIGHT}, {ink} ink pixels)")
    }
}

impl Framebuffer {
    /// A blank (all white) frame.
    pub fn new() -> Self {
        Self {
            bits: vec![0; STRIDE * HEIGHT as usize],
        }
    }

    /// Colour at `(x, y)`; out-of-range coordinates read as white.
    pub fn pixel(&self, x: u32, y: u32) -> BinaryColor {
        if x >= WIDTH || y >= HEIGHT {
            return BinaryColor::Off;
        }
        let byte = self.bits[y as usize * STRIDE + x as usize / 8];
        BinaryColor::from(byte & (0x80 >> (x % 8)) != 0)
    }

    /// Every pixel as `(point, colour)`, row by row.
    pub fn pixels(&self) -> impl Iterator<Item = Pixel<BinaryColor>> + '_ {
        (0..HEIGHT).flat_map(move |y| {
            (0..WIDTH).map(move |x| Pixel(Point::new(x as i32, y as i32), self.pixel(x, y)))
        })
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for Framebuffer {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, color) in pixels {
            let (Ok(x), Ok(y)) = (u32::try_from(p.x), u32::try_from(p.y)) else {
                continue;
            };
            if x >= WIDTH || y >= HEIGHT {
                continue;
            }
            let byte = &mut self.bits[y as usize * STRIDE + x as usize / 8];
            let mask = 0x80 >> (x % 8);
            if color.is_on() {
                *byte |= mask;
            } else {
                *byte &= !mask;
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.bits.fill(if color.is_on() { 0xFF } else { 0 });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_read_pixels() {
        let mut fb = Framebuffer::new();
        let Ok(()) = fb.draw_iter([
            Pixel(Point::new(0, 0), BinaryColor::On),
            Pixel(Point::new(249, 121), BinaryColor::On),
            Pixel(Point::new(250, 0), BinaryColor::On), // clipped
            Pixel(Point::new(-1, 5), BinaryColor::On),  // clipped
        ]);
        assert_eq!(fb.pixel(0, 0), BinaryColor::On);
        assert_eq!(fb.pixel(1, 0), BinaryColor::Off);
        assert_eq!(fb.pixel(249, 121), BinaryColor::On);
        assert_eq!(fb.pixels().filter(|p| p.1.is_on()).count(), 2);
    }
}
