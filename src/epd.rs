//! The real Waveshare 2.13" e-paper HAT (V2/V3/V4) on a Raspberry Pi.
//!
//! Kept deliberately thin: everything interesting (what to draw, when to do a
//! full refresh) is decided elsewhere and tested on the desktop. This module
//! only rotates the landscape frame into the panel's native portrait buffer
//! and drives `epd-waveshare` over SPI. It is not unit-tested; it must compile
//! with `--features epd` and be checked on hardware. Linux only (rppal).

use embedded_graphics::prelude::*;
use epd_waveshare::color::Color;
use epd_waveshare::epd2in13_v2::{Display2in13, Epd2in13};
use epd_waveshare::graphics::DisplayRotation;
use epd_waveshare::prelude::{RefreshLut, WaveshareDisplay};
use rppal::gpio::{Gpio, InputPin, OutputPin};
use rppal::hal::Delay;
use rppal::spi::{Bus, Mode, SimpleHalSpiDevice, SlaveSelect, Spi};

use crate::framebuffer::Framebuffer;
use crate::panel::{Panel, Refresh};

const SPI_CLOCK_HZ: u32 = 4_000_000;
const PIN_RST: u8 = 17;
const PIN_DC: u8 = 25;
const PIN_BUSY: u8 = 24;
/// Panel power enable on newer HAT revisions; harmless on older ones.
const PIN_PWR: u8 = 18;

/// Why the HAT could not be driven.
#[derive(Debug, thiserror::Error)]
pub enum EpdError {
    /// GPIO could not be opened or a pin claimed.
    #[error(
        "gpio error: {0}; run on a Raspberry Pi and check no other program is using pins 17/18/24/25"
    )]
    Gpio(#[from] rppal::gpio::Error),
    /// SPI could not be opened or a transfer failed.
    #[error("spi error: {0}; enable SPI with dtparam=spi=on in /boot/firmware/config.txt")]
    Spi(#[from] rppal::spi::Error),
}

type Driver = Epd2in13<SimpleHalSpiDevice, InputPin, OutputPin, OutputPin, Delay>;

/// [`Panel`] for the physical HAT.
pub struct EpdPanel {
    spi: SimpleHalSpiDevice,
    epd: Driver,
    delay: Delay,
    buffer: Display2in13,
    asleep: bool,
    // Held so the power pin stays driven high for the life of the panel.
    _power: OutputPin,
}

impl EpdPanel {
    /// Open SPI0/CE0 and the HAT pins, power the panel and initialise it.
    /// `flipped` rotates the image 180° for an upside-down mount.
    pub fn new(flipped: bool) -> Result<Self, EpdError> {
        let gpio = Gpio::new()?;
        let power = gpio.get(PIN_PWR)?.into_output_high();
        let busy = gpio.get(PIN_BUSY)?.into_input();
        let dc = gpio.get(PIN_DC)?.into_output();
        let rst = gpio.get(PIN_RST)?.into_output();

        let bus = Spi::new(Bus::Spi0, SlaveSelect::Ss0, SPI_CLOCK_HZ, Mode::Mode0)?;
        let mut spi = SimpleHalSpiDevice::new(bus);
        let mut delay = Delay::new();
        let epd = Epd2in13::new(&mut spi, busy, dc, rst, &mut delay, None)?;

        let mut buffer = Display2in13::default();
        buffer.set_rotation(if flipped {
            DisplayRotation::Rotate270
        } else {
            DisplayRotation::Rotate90
        });

        Ok(Self {
            spi,
            epd,
            delay,
            buffer,
            asleep: false,
            _power: power,
        })
    }
}

impl Panel for EpdPanel {
    type Error = EpdError;

    fn present(&mut self, frame: &Framebuffer, refresh: Refresh) -> Result<(), EpdError> {
        if self.asleep {
            self.epd.wake_up(&mut self.spi, &mut self.delay)?;
            self.asleep = false;
        }

        let pixels = frame.pixels().map(|Pixel(p, c)| Pixel(p, Color::from(c)));
        let Ok(()) = self.buffer.draw_iter(pixels);

        let lut = match refresh {
            Refresh::Full => RefreshLut::Full,
            Refresh::Partial => RefreshLut::Quick,
        };
        self.epd.set_refresh(&mut self.spi, &mut self.delay, lut)?;
        self.epd
            .update_and_display_frame(&mut self.spi, self.buffer.buffer(), &mut self.delay)?;
        Ok(())
    }

    fn sleep(&mut self) -> Result<(), EpdError> {
        if !self.asleep {
            self.epd.sleep(&mut self.spi, &mut self.delay)?;
            self.asleep = true;
        }
        Ok(())
    }
}
