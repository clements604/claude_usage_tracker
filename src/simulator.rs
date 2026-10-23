//! A desktop window that stands in for the e-paper HAT.
//!
//! Lets the UI be developed on a laptop with no Pi attached. The window shows
//! the 250×122 frame exactly as the panel would, plus a status strip *below*
//! it (never over it) reporting what the hardware would be doing: full vs
//! partial refresh, partials since the last full, and whether the controller
//! is asleep. Full refreshes briefly flash inverted, like the real panel, so
//! refresh-policy mistakes are visible rather than silent.

use std::convert::Infallible;
use std::time::Duration;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::{BinaryColor, Rgb888};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};
use embedded_graphics_simulator::{
    BinaryColorTheme, OutputSettingsBuilder, SimulatorDisplay, SimulatorEvent, Window,
};

use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use crate::panel::{Panel, Refresh};

/// Height of the debug strip under the panel image.
pub const STRIP_HEIGHT: u32 = 14;

const FLASH: Duration = Duration::from_millis(150);

/// A [`Panel`] that draws into an SDL window.
pub struct SimPanel {
    display: SimulatorDisplay<BinaryColor>,
    window: Window,
    flash: bool,
    last_refresh: Option<Refresh>,
    partials: u32,
    asleep: bool,
    note: String,
}

impl SimPanel {
    /// Open a window at `scale`× (3 matches the mockup).
    pub fn new(scale: u32) -> Self {
        let settings = OutputSettingsBuilder::new()
            .scale(scale.max(1))
            .pixel_spacing(0)
            .theme(BinaryColorTheme::Custom {
                color_off: Rgb888::new(0xE8, 0xE8, 0xE8),
                color_on: Rgb888::new(0x1A, 0x1A, 0x1A),
            })
            .build();
        Self {
            display: SimulatorDisplay::new(Size::new(WIDTH, HEIGHT + STRIP_HEIGHT)),
            window: Window::new("bump-ticker simulator", &settings),
            flash: true,
            last_refresh: None,
            partials: 0,
            asleep: false,
            note: String::new(),
        }
    }

    /// Turn the full-refresh flash on or off.
    pub fn set_flash(&mut self, flash: bool) {
        self.flash = flash;
    }

    /// Extra text for the right-hand side of the strip (e.g. the fake date).
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = note.into();
    }

    /// Redraw the window; call often so it stays responsive.
    pub fn update(&mut self) {
        self.window.update(&self.display);
    }

    /// Pending window events (keys, quit). Call [`update`](Self::update) first.
    pub fn events(&self) -> Vec<SimulatorEvent> {
        self.window.events().collect()
    }

    fn draw_strip(&mut self) {
        let Ok(()) = self.try_draw_strip();
    }

    fn try_draw_strip(&mut self) -> Result<(), Infallible> {
        let top = HEIGHT as i32;
        let d = &mut self.display;
        Rectangle::new(Point::new(0, top), Size::new(WIDTH, STRIP_HEIGHT))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(d)?;
        Line::new(Point::new(0, top), Point::new(WIDTH as i32 - 1, top))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::Off, 1))
            .draw(d)?;

        let refresh = match (self.asleep, self.last_refresh) {
            (true, _) => "SLEEP".to_owned(),
            (false, Some(Refresh::Full)) => "FULL".to_owned(),
            (false, Some(Refresh::Partial)) => format!("PART {}", self.partials),
            (false, None) => "-".to_owned(),
        };
        let line = format!("{refresh:<7} {}", self.note);
        let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::Off);
        Text::with_baseline(&line, Point::new(3, top + 3), style, Baseline::Top).draw(d)?;
        Ok(())
    }

    fn blit(&mut self, frame: &Framebuffer, invert: bool) {
        let pixels = frame
            .pixels()
            .map(|Pixel(p, c)| Pixel(p, if invert { c.invert() } else { c }));
        let Ok(()) = self.display.draw_iter(pixels);
    }
}

impl Panel for SimPanel {
    type Error = Infallible;

    fn present(&mut self, frame: &Framebuffer, refresh: Refresh) -> Result<(), Infallible> {
        self.asleep = false;
        self.last_refresh = Some(refresh);
        match refresh {
            Refresh::Full => self.partials = 0,
            Refresh::Partial => self.partials += 1,
        }
        if refresh == Refresh::Full && self.flash {
            self.blit(frame, true);
            self.update();
            std::thread::sleep(FLASH);
        }
        self.blit(frame, false);
        self.draw_strip();
        self.update();
        Ok(())
    }

    fn sleep(&mut self) -> Result<(), Infallible> {
        self.asleep = true;
        self.draw_strip();
        self.update();
        Ok(())
    }
}
