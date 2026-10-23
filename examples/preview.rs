//! Render every screen for given usage percentages to PNGs at 3× scale, and
//! tile them into the design mockup.
//!
//! ```sh
//! cargo run --example preview --features preview -- 62 23 4 out/
//! ```
//!
//! Time is fixed at the reference moment (Mon 2026-10-19 12:16 at UTC+10),
//! with the session resetting at 14:30 and the week at Fri 09:00, so the
//! output is reproducible. Writes `dashboard.png` (session and week, the Pro
//! case), `dashboard-model.png` (plus a model-specific weekly row),
//! `stale.png`, `expiry-warning.png` (stale, with the sign-in expiry
//! warning), `sign-in.png` (signed out: the QR code for the phone sign-in
//! page), `waiting.png`, `error.png`, and `mockup.png`: those seven tiled
//! two-up with 12 px gutters, which at `62 23 4` is `docs/ui-mockup.png`.

use std::error::Error;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use claude_ticker::framebuffer::Framebuffer;
use claude_ticker::preview::save_png;
use claude_ticker::source::SourceError;
use claude_ticker::usage::{ModelQuota, Quota, Usage, sign_in_warning};
use claude_ticker::view::{self, Screen};
use image::{GenericImageView, RgbImage};

const SCALE: u32 = 3;
/// A typical sign-in page address: LAN IP, port and the random path.
const SIGN_IN_URL: &str = "http://192.168.1.23:8080/k7Qp2xNa";
const GUTTER: u32 = 12;
const USAGE: &str = "usage: preview <SESSION%> <WEEK%> <MODEL%> <OUT_DIR>";

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .expect("hard-coded time is valid")
}

/// The reference display timezone, UTC+10 (Brisbane, no daylight saving).
fn local(t: DateTime<Utc>) -> NaiveDateTime {
    t.naive_utc() + TimeDelta::hours(10)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [session, week, model, out] = args.as_slice() else {
        return Err(USAGE.into());
    };
    let (session, week, model): (f64, f64, f64) = (session.parse()?, week.parse()?, model.parse()?);
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out)?;

    let now = utc("2026-10-19T02:16:00Z");
    let quota = |percent, resets| Quota {
        percent,
        resets_at: Some(utc(resets)),
    };
    let pro = Usage {
        session: Some(quota(session, "2026-10-19T04:30:00Z")),
        weekly: Some(quota(week, "2026-10-22T23:00:00Z")),
        model: None,
    };
    let max = Usage {
        model: Some(ModelQuota {
            model: "OPUS".to_owned(),
            quota: quota(model, "2026-10-22T23:00:00Z"),
        }),
        ..pro.clone()
    };
    let fresh = TimeDelta::minutes(15);

    let mut tiles = Vec::new();
    let mut render = |screen: Screen<'_>, name: &str| -> Result<(), Box<dyn Error>> {
        let mut fb = Framebuffer::new();
        let Ok(()) = view::draw(&screen, &mut fb);
        let path = out.join(name);
        save_png(&fb, &path, SCALE)?;
        println!("wrote {}", path.display());
        tiles.push(path);
        Ok(())
    };

    render(
        Screen::Dashboard(&pro.status(now, now, fresh, local)?),
        "dashboard.png",
    )?;
    render(
        Screen::Dashboard(&max.status(now, now, fresh, local)?),
        "dashboard-model.png",
    )?;
    let later = now + TimeDelta::minutes(40);
    render(
        Screen::Dashboard(&pro.status(now, later, fresh, local)?),
        "stale.png",
    )?;
    let mut warned = pro.status(now, later, fresh, local)?;
    warned.sign_in_left = sign_in_warning(Some(later + TimeDelta::minutes(23 * 60 + 59)), later);
    render(Screen::Dashboard(&warned), "expiry-warning.png")?;
    render(Screen::SignIn(SIGN_IN_URL), "sign-in.png")?;
    render(Screen::WaitingForTime, "waiting.png")?;
    let err = SourceError::Auth("token refresh refused with http 400".to_owned());
    render(Screen::Error(&err.to_string()), "error.png")?;

    let mockup = out.join("mockup.png");
    tile(&tiles, &mockup)?;
    println!("wrote {}", mockup.display());
    Ok(())
}

/// Lay `paths` out two per row on white, with `GUTTER` px around each.
fn tile(paths: &[PathBuf], to: &Path) -> Result<(), Box<dyn Error>> {
    let images = paths
        .iter()
        .map(image::open)
        .collect::<Result<Vec<_>, _>>()?;
    let (w, h) = images.first().ok_or("nothing to tile")?.dimensions();
    let rows = images.len().div_ceil(2) as u32;
    let mut sheet = RgbImage::from_pixel(
        GUTTER + 2 * (w + GUTTER),
        GUTTER + rows * (h + GUTTER),
        image::Rgb([255, 255, 255]),
    );
    for (i, img) in images.iter().enumerate() {
        let (col, row) = (i as u32 % 2, i as u32 / 2);
        image::imageops::replace(
            &mut sheet,
            &img.to_rgb8(),
            i64::from(GUTTER + col * (w + GUTTER)),
            i64::from(GUTTER + row * (h + GUTTER)),
        );
    }
    sheet.save(to)?;
    Ok(())
}
