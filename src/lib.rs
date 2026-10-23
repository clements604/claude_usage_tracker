//! claude-ticker: Claude plan usage (session and weekly limits) on a
//! Raspberry Pi Zero with a Waveshare 2.13" e-paper HAT.
//!
//! Layers, from pure logic down to hardware (dependencies only point down):
//! [`usage`] → [`source`] → [`config`] → [`view`] → [`framebuffer`] → [`panel`]
//! → [`app`], with the network client `api`, the sign-in page `web`, and the
//! output backends `epd`, `preview` and `simulator` behind features.

pub mod app;
pub mod config;
pub mod framebuffer;
pub mod panel;
pub mod source;
pub mod usage;
pub mod view;

#[cfg(feature = "net")]
pub mod api;

#[cfg(feature = "web")]
pub mod web;

#[cfg(feature = "preview")]
pub mod preview;

#[cfg(feature = "simulator")]
pub mod simulator;

#[cfg(all(feature = "epd", target_os = "linux"))]
pub mod epd;
