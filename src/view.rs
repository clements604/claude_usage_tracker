//! Status → pixels.
//!
//! Layout follows the approved mockup in `docs/ui-mockup.png`. The view draws
//! onto any `DrawTarget<Color = BinaryColor>` so the same code renders to the
//! in-memory framebuffer, a PNG, or the desktop simulator — what you see while
//! developing is exactly what the e-paper gets.
//!
//! The dashboard stacks one row per limit: label and percentage, a bar, then
//! when it resets (or, for usage credits, what is left). Two rows (session
//! and week, the usual Pro case) get roomy spacing and a large percentage; a
//! third row (a model-specific limit or credits) switches to a compact
//! layout, and a fourth to a dense one that moves the reset onto the label
//! line, so everything still fits in 122 px.

use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_7X13_BOLD, FONT_10X20};
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};

use qrcodegen::{QrCode, QrCodeEcc};

use crate::usage::{Detail, Reset, Row, Status};

/// Side margin in pixels. Nothing is drawn closer to the edge than this.
pub const MARGIN: i32 = 8;

const WIDTH: i32 = 250;
const RIGHT: i32 = WIDTH - MARGIN;
/// Anchor for right-aligned text: the bar's last pixel column, so glyphs
/// that fill their cell still stay inside the margin.
const TEXT_RIGHT: i32 = RIGHT - 1;
const CENTER: i32 = WIDTH / 2;

const HEADER_Y: i32 = 6;
const BAR_H: u32 = 8;
const FOOTER_Y: i32 = 110;

/// Vertical positions for one row, relative to its top.
struct RowLayout {
    /// Top of the first row.
    top: i32,
    /// Distance between row tops.
    pitch: i32,
    /// Font for the percentage, right-aligned on the label line.
    percent_font: &'static MonoFont<'static>,
    /// Offset of the label (so it sits on the percentage's baseline).
    label_dy: i32,
    bar_dy: i32,
    reset_dy: i32,
}

/// Up to two rows: large percentage, generous spacing.
const ROOMY: RowLayout = RowLayout {
    top: 4,
    pitch: 52,
    percent_font: &FONT_10X20,
    label_dy: 4,
    bar_dy: 23,
    reset_dy: 35,
};

/// Three rows: everything on a 35 px pitch.
const COMPACT: RowLayout = RowLayout {
    top: 2,
    pitch: 35,
    percent_font: &FONT_7X13_BOLD,
    label_dy: 0,
    bar_dy: 14,
    reset_dy: 24,
};

/// Four rows: label, short detail and percentage on one line, then the bar.
const DENSE_TOP: i32 = 2;
const DENSE_PITCH: i32 = 27;
const DENSE_BAR_DY: i32 = 15;
/// Right edge of the short detail: clear of the widest percentage, `100%`
/// in `FONT_7X13_BOLD`, plus one character.
const DENSE_DETAIL_RIGHT: i32 = TEXT_RIGHT - 4 * 7 - 6;

/// Something to put on the display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen<'a> {
    /// Every limit on one page.
    Dashboard(&'a Status),
    /// "CAN'T GET USAGE" with a human-readable message.
    Error(&'a str),
    /// Signed out: a QR code of the sign-in page's address, which is also
    /// written out for typing.
    SignIn(&'a str),
    /// The clock has not been set from the network yet, so neither reset
    /// times nor TLS certificates can be trusted. Same layout as the error
    /// screen, plus the logo.
    WaitingForTime,
}

/// Clear `target` and draw `screen` on it.
pub fn draw<D>(screen: &Screen<'_>, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    target.clear(BinaryColor::Off)?;
    match *screen {
        Screen::Dashboard(status) => draw_dashboard(status, target),
        Screen::Error(message) => draw_notice("CAN'T GET USAGE", message, target),
        Screen::SignIn(url) => draw_sign_in(url, target),
        Screen::WaitingForTime => {
            draw_notice(
                "SETTING THE CLOCK",
                "waiting for the network time; this updates by itself once the Pi is online",
                target,
            )?;
            draw_logo(target)
        }
    }
}

fn draw_dashboard<D>(s: &Status, t: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    if s.rows.len() > 3 {
        for (i, row) in s.rows.iter().take(4).enumerate() {
            draw_dense_row(row, DENSE_TOP + DENSE_PITCH * i as i32, t)?;
        }
    } else {
        let layout = if s.rows.len() > 2 { &COMPACT } else { &ROOMY };
        for (i, row) in s.rows.iter().enumerate() {
            draw_row(row, layout, layout.top + layout.pitch * i as i32, t)?;
        }
    }

    text(
        &footer_left(s),
        &FONT_6X10,
        MARGIN,
        FOOTER_Y,
        Alignment::Left,
        t,
    )?;
    if let Some(right) = footer_right(s) {
        text(
            &right,
            &FONT_6X10,
            TEXT_RIGHT,
            FOOTER_Y,
            Alignment::Right,
            t,
        )?;
    }
    Ok(())
}

/// `FONT_6X10` characters that fit across the footer, margin to margin.
const FOOTER_CHARS: usize = ((TEXT_RIGHT + 1 - MARGIN) / 6) as usize;

/// `re-login in 2d 3h` when the sign-in expires soon, since that needs
/// doing; otherwise the hostname, cut short to keep two characters clear of
/// the left text.
fn footer_right(s: &Status) -> Option<String> {
    if let Some(left) = s.sign_in_left {
        return Some(format!("re-login in {left}"));
    }
    let room = FOOTER_CHARS.saturating_sub(footer_left(s).len() + 2);
    let host: String = s
        .host
        .as_deref()?
        .chars()
        .filter(char::is_ascii_graphic)
        .take(room)
        .collect();
    (!host.is_empty()).then_some(host)
}

fn draw_row<D>(row: &Row, l: &RowLayout, y: i32, t: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    text(
        &row.label,
        &FONT_7X13_BOLD,
        MARGIN,
        y + l.label_dy,
        Alignment::Left,
        t,
    )?;
    let pct = value_text(row);
    text(&pct, l.percent_font, TEXT_RIGHT, y, Alignment::Right, t)?;
    progress_bar(row.percent, y + l.bar_dy, t)?;
    text(
        &detail_line(&row.detail),
        &FONT_6X10,
        MARGIN,
        y + l.reset_dy,
        Alignment::Left,
        t,
    )
}

/// One row of four: the short detail sits between label and percentage.
fn draw_dense_row<D>(row: &Row, y: i32, t: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    text(&row.label, &FONT_7X13_BOLD, MARGIN, y, Alignment::Left, t)?;
    let pct = value_text(row);
    text(&pct, &FONT_7X13_BOLD, TEXT_RIGHT, y, Alignment::Right, t)?;
    text(
        &short_detail(&row.detail),
        &FONT_6X10,
        DENSE_DETAIL_RIGHT,
        y + 2,
        Alignment::Right,
        t,
    )?;
    progress_bar(row.percent, y + DENSE_BAR_DY, t)
}

/// Right of the label: `62%`, or for credits the amount left, `$11.15`.
fn value_text(row: &Row) -> String {
    match &row.detail {
        Detail::Credits { left, .. } => left.to_string(),
        Detail::Reset(_) => format!("{}%", row.percent),
    }
}

/// The line under a bar: when it resets, or `of $40.00 monthly limit`.
fn detail_line(detail: &Detail) -> String {
    match detail {
        Detail::Reset(reset) => reset_line(*reset),
        Detail::Credits { limit, bonus, .. } => {
            let tail = if *bonus {
                "incl. bonus"
            } else {
                "monthly limit"
            };
            format!("of {limit} {tail}")
        }
    }
}

/// [`detail_line`] cut down to fit beside the label: `14:30 (2h 14m)`,
/// `Fri 09:00 (3d 20h)`, `reset 14:30`, or `$14.10 left`.
fn short_detail(detail: &Detail) -> String {
    match detail {
        Detail::Reset(Reset::At { at, left }) if left.minutes() < 24 * 60 => {
            format!("{} ({left})", at.format(TIME_FORMAT))
        }
        Detail::Reset(Reset::At { at, left }) => format!("{} ({left})", at.format("%a %H:%M")),
        Detail::Reset(Reset::Passed { at }) => format!("reset {}", at.format(TIME_FORMAT)),
        Detail::Reset(Reset::Unknown) => String::new(),
        // The label and amount already say it all.
        Detail::Credits { .. } => String::new(),
    }
}

/// `updated 12:16`, or `STALE Mon 12:16` once the numbers are old (with the
/// weekday, since stale data can be from another day). Short enough to share
/// the footer with the longest sign-in warning, `re-login in 23h 59m`.
fn footer_left(s: &Status) -> String {
    if s.stale {
        format!("STALE {}", s.updated.format("%a %H:%M"))
    } else {
        format!("updated {}", s.updated.format(TIME_FORMAT))
    }
}

/// `resets 14:30 (in 2h 14m)`; the weekday is added once the reset is a
/// day or more away.
fn reset_line(reset: Reset) -> String {
    match reset {
        Reset::At { at, left } if left.minutes() < 24 * 60 => {
            format!("resets {} (in {left})", at.format(TIME_FORMAT))
        }
        Reset::At { at, left } => format!("resets {} (in {left})", at.format("%a %H:%M")),
        Reset::Passed { at } => format!("reset at {}; updating", at.format(TIME_FORMAT)),
        Reset::Unknown => "no reset time".to_owned(),
    }
}

/// Largest QR module size in pixels; beyond this the code only gets harder
/// to fit, not easier to scan.
const QR_MAX_SCALE: i32 = 4;
/// Height available to the QR code, leaving a few pixels top and bottom.
const QR_SPACE: i32 = 110;

/// QR code on the left, instructions and the address on the right. Falls
/// back to text only if the address is too long for a scannable code.
fn draw_sign_in<D>(url: &str, t: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let address = url.strip_prefix("http://").unwrap_or(url);
    let Some((qr, scale)) = qr_code(url) else {
        let message = format!("open {address} on a phone on the same wifi to sign in to claude");
        return draw_notice("SIGN IN", &message, t);
    };

    let side = qr.size() * scale;
    let top = (122 - side) / 2;
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                Rectangle::new(
                    Point::new(MARGIN + x * scale, top + y * scale),
                    Size::new(scale as u32, scale as u32),
                )
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(t)?;
            }
        }
    }

    // Two modules of quiet zone between the code and the text.
    let left = MARGIN + side + 2 * scale;
    let cols = ((TEXT_RIGHT - left) / 6) as usize;
    text(
        "SIGN IN",
        &FONT_7X13_BOLD,
        left,
        HEADER_Y,
        Alignment::Left,
        t,
    )?;
    for (i, line) in wrap("Scan with your phone to sign in to Claude.", cols)
        .iter()
        .enumerate()
    {
        text(
            line,
            &FONT_6X10,
            left,
            26 + 11 * i as i32,
            Alignment::Left,
            t,
        )?;
    }
    // Break after the port rather than inside the random path.
    let (host, path) = address
        .find('/')
        .map_or((address, ""), |i| address.split_at(i));
    let lines: Vec<String> = [host, path]
        .iter()
        .flat_map(|part| wrap(part, cols))
        .collect();
    let first = 116 - 11 * lines.len() as i32;
    for (i, line) in lines.iter().enumerate() {
        text(
            line,
            &FONT_6X10,
            left,
            first + 11 * i as i32,
            Alignment::Left,
            t,
        )?;
    }
    Ok(())
}

/// `url` as a QR code and the module size that fits it, or `None` if it
/// cannot be drawn at 2 px per module or more.
fn qr_code(url: &str) -> Option<(QrCode, i32)> {
    let qr = QrCode::encode_text(url, QrCodeEcc::Medium).ok()?;
    let scale = (QR_SPACE / qr.size()).min(QR_MAX_SCALE);
    (scale >= 2).then_some((qr, scale))
}

/// Bold header plus a word-wrapped message; no bar, dots or trimester tag.
fn draw_notice<D>(header: &str, message: &str, t: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    text(
        header,
        &FONT_7X13_BOLD,
        MARGIN,
        HEADER_Y,
        Alignment::Left,
        t,
    )?;
    let max_chars = ((WIDTH - 2 * MARGIN) / 6) as usize;
    let line_height = 11;
    for (i, line) in wrap(message, max_chars).iter().take(8).enumerate() {
        let y = 28 + line_height * i as i32;
        text(line, &FONT_6X10, MARGIN, y, Alignment::Left, t)?;
    }
    Ok(())
}

/// The project logo, 1-bit. Regenerate with
/// `cargo run --example logo --features preview -- resources/logo.png resources/`.
const LOGO: &[u8] = include_bytes!("../resources/logo.raw");
const LOGO_WIDTH: u32 = 115;
const LOGO_Y: i32 = 58;

/// Logo centred under the notice text (waiting screen only).
fn draw_logo<D>(t: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let raw = ImageRaw::<BinaryColor>::new(LOGO, LOGO_WIDTH);
    let x = CENTER - LOGO_WIDTH as i32 / 2;
    Image::new(&raw, Point::new(x, LOGO_Y)).draw(t)
}

/// Full-width bar: 1 px outline, solid fill with a 2 px inset.
fn progress_bar<D>(percent: u8, y: i32, t: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let width = (WIDTH - 2 * MARGIN) as u32;
    Rectangle::new(Point::new(MARGIN, y), Size::new(width, BAR_H))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(t)?;
    let inner = width - 4;
    let fill = inner * u32::from(percent.min(100)) / 100;
    if fill > 0 {
        Rectangle::new(Point::new(MARGIN + 2, y + 2), Size::new(fill, BAR_H - 4))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(t)?;
    }
    Ok(())
}

fn text<D>(
    s: &str,
    font: &MonoFont<'_>,
    x: i32,
    y: i32,
    alignment: Alignment,
    t: &mut D,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = MonoTextStyle::new(font, BinaryColor::On);
    let layout = TextStyleBuilder::new()
        .alignment(alignment)
        .baseline(Baseline::Top)
        .build();
    Text::with_text_style(s, Point::new(x, y), style, layout).draw(t)?;
    Ok(())
}

/// How times of day appear on the display: 24-hour `HH:MM`.
pub const TIME_FORMAT: &str = "%H:%M";

/// Greedy word wrap to `max` characters per line; over-long words are split.
fn wrap(text: &str, max: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word;
        while word.chars().count() > max {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let split = word.char_indices().nth(max).map_or(word.len(), |(i, _)| i);
            lines.push(word[..split].to_owned());
            word = &word[split..];
        }
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= max {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::replace(&mut line, word.to_owned()));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::Framebuffer;
    use crate::usage::{Money, Span};
    use chrono::NaiveDateTime;
    use embedded_graphics::primitives::Rectangle;

    fn naive(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    /// Bounding box of all ink, to check margins.
    fn ink_bounds(fb: &Framebuffer) -> Option<Rectangle> {
        let ink: Vec<Point> = fb.pixels().filter(|p| p.1.is_on()).map(|p| p.0).collect();
        let min_x = ink.iter().map(|p| p.x).min()?;
        let max_x = ink.iter().map(|p| p.x).max()?;
        let min_y = ink.iter().map(|p| p.y).min()?;
        let max_y = ink.iter().map(|p| p.y).max()?;
        Some(Rectangle::with_corners(
            Point::new(min_x, min_y),
            Point::new(max_x, max_y),
        ))
    }

    fn assert_within_margins(screen: Screen<'_>) {
        let mut fb = Framebuffer::new();
        let Ok(()) = draw(&screen, &mut fb);
        let b = ink_bounds(&fb).expect("something drawn");
        let br = b.bottom_right().unwrap();
        assert!(b.top_left.x >= MARGIN, "{screen:?} left edge {b:?}");
        assert!(br.x < RIGHT, "{screen:?} right edge {b:?}");
        assert!(
            b.top_left.y >= 0 && br.y <= 121,
            "{screen:?} vertical {b:?}"
        );
    }

    fn row(label: &str, percent: u8, reset: Reset) -> Row {
        Row {
            label: label.to_owned(),
            percent,
            detail: Detail::Reset(reset),
        }
    }

    fn money(minor: i64, currency: &str) -> Money {
        Money {
            minor,
            decimals: 2,
            currency: currency.to_owned(),
        }
    }

    /// Credits at their widest: a non-dollar currency and big amounts.
    fn credits_row() -> Row {
        Row {
            label: "CREDITS LEFT".to_owned(),
            percent: 100,
            detail: Detail::Credits {
                left: money(99_999_999, "EUR"),
                limit: money(99_999_999, "EUR"),
                bonus: true,
            },
        }
    }

    #[test]
    fn longest_strings_fit_inside_margins() {
        let far = Reset::At {
            at: naive("2026-10-21 23:59"),
            left: Span::from_minutes(6 * 24 * 60 + 23 * 60 + 59),
        };
        let passed = Reset::Passed {
            at: naive("2026-10-19 23:59"),
        };
        let rows = [
            row("SESSION", 100, passed),
            row("WEEK", 100, far),
            row("WEEK SONNET", 100, far),
            credits_row(),
        ];
        for n in 1..=4 {
            for stale in [false, true] {
                for sign_in_left in [None, Some(Span::from_minutes(23 * 60 + 59))] {
                    for host in [None, Some("w".repeat(63))] {
                        let s = Status {
                            rows: rows[..n].to_vec(),
                            updated: naive("2026-10-21 23:59"),
                            stale,
                            sign_in_left,
                            host,
                        };
                        assert_within_margins(Screen::Dashboard(&s));
                    }
                }
            }
        }
        assert_within_margins(Screen::WaitingForTime);
        assert_within_margins(Screen::SignIn("http://192.168.100.200:65535/AbCdEfGh"));
        assert_within_margins(Screen::SignIn(&format!("http://{}/", "x".repeat(300))));
        assert_within_margins(Screen::Error(
            "signed out (http 401: invalid refresh token); copy fresh credentials from your Mac (see README)",
        ));
    }

    #[test]
    fn rows_do_not_overlap_the_footer() {
        for l in [&ROOMY, &COMPACT] {
            let rows = if std::ptr::eq(l, &ROOMY) { 2 } else { 3 };
            let last_bottom = l.top + l.pitch * (rows - 1) + l.reset_dy + 10;
            assert!(last_bottom < FOOTER_Y, "{last_bottom}");
        }
        const { assert!(DENSE_TOP + DENSE_PITCH * 3 + DENSE_BAR_DY + (BAR_H as i32) < FOOTER_Y) };
        const { assert!(FOOTER_Y + 10 <= 122) };
    }

    #[test]
    fn dense_rows_keep_label_and_detail_apart() {
        let far = Reset::At {
            at: naive("2026-10-21 23:59"),
            left: Span::from_minutes(6 * 24 * 60 + 23 * 60 + 59),
        };
        let soon = Reset::At {
            at: naive("2026-10-21 23:59"),
            left: Span::from_minutes(23 * 60 + 59),
        };
        let passed = Reset::Passed {
            at: naive("2026-10-21 23:59"),
        };
        let mut rows: Vec<Row> = [far, soon, passed, Reset::Unknown]
            .into_iter()
            .map(|r| row("WEEK SONNET", 100, r))
            .collect();
        rows.push(credits_row());
        for r in &rows {
            let label_end = MARGIN + 7 * r.label.len() as i32;
            let detail = short_detail(&r.detail);
            let detail_start = DENSE_DETAIL_RIGHT - 6 * detail.len() as i32;
            // At least one character of space between them.
            assert!(label_end + 6 <= detail_start, "{} / {detail}", r.label);
        }
        assert_eq!(short_detail(&rows[0].detail), "Wed 23:59 (6d 23h)");
        assert_eq!(short_detail(&rows[1].detail), "23:59 (23h 59m)");
        assert_eq!(short_detail(&rows[2].detail), "reset 23:59");
        assert_eq!(short_detail(&rows[4].detail), "");
    }

    #[test]
    fn credits_line_says_what_is_left() {
        let d = Detail::Credits {
            left: money(1410, "AUD"),
            limit: money(4000, "AUD"),
            bonus: false,
        };
        assert_eq!(detail_line(&d), "of $40.00 monthly limit");
        let with_bonus = Detail::Credits {
            left: money(2410, "AUD"),
            limit: money(5000, "AUD"),
            bonus: true,
        };
        assert_eq!(detail_line(&with_bonus), "of $50.00 incl. bonus");
        assert_eq!(short_detail(&d), "");
        let row = Row {
            label: "CREDITS LEFT".to_owned(),
            percent: 35,
            detail: d,
        };
        assert_eq!(value_text(&row), "$14.10");
    }

    #[test]
    fn sign_in_qr_matches_encoder_and_is_scannable() {
        let url = "http://192.168.100.200:8080/AbCdEfGh";
        let (qr, scale) = qr_code(url).unwrap();
        assert_eq!((qr.size(), scale), (29, 3));
        let mut fb = Framebuffer::new();
        let Ok(()) = draw(&Screen::SignIn(url), &mut fb);
        let top = (122 - 29 * 3) / 2;
        for y in 0..qr.size() {
            for x in 0..qr.size() {
                // Centre pixel of each module.
                let p = Point::new(MARGIN + x * 3 + 1, top + y * 3 + 1);
                assert_eq!(
                    fb.pixel(p.x as u32, p.y as u32).is_on(),
                    qr.get_module(x, y)
                );
            }
        }
        assert!(qr_code(&"x".repeat(300)).is_none());
    }

    #[test]
    fn footer_texts_never_meet() {
        let mut s = Status {
            rows: Vec::new(),
            updated: naive("2026-10-21 23:59"),
            stale: true,
            sign_in_left: None,
            host: Some("w".repeat(63)),
        };
        let longest_left = footer_left(&s).len() as i32;
        let longest_right = "re-login in 23h 59m".len() as i32;
        // FONT_6X10 is 6 px per character; keep at least two characters apart.
        assert!(MARGIN + 6 * (longest_left + 2) <= TEXT_RIGHT - 6 * longest_right);
        for stale in [true, false] {
            s.stale = stale;
            let host = footer_right(&s).unwrap().len();
            assert_eq!(footer_left(&s).len() + 2 + host, FOOTER_CHARS);
        }
    }

    #[test]
    fn footer_shows_host_unless_sign_in_needs_renewing() {
        let mut s = Status {
            rows: Vec::new(),
            updated: naive("2026-10-21 23:59"),
            stale: false,
            sign_in_left: None,
            host: Some("claude-ticker".to_owned()),
        };
        assert_eq!(footer_right(&s).as_deref(), Some("claude-ticker"));
        s.sign_in_left = Some(Span::from_minutes(90));
        assert_eq!(footer_right(&s).as_deref(), Some("re-login in 1h 30m"));
        s.sign_in_left = None;
        s.host = Some("caf\u{e9} \n".to_owned());
        assert_eq!(footer_right(&s).as_deref(), Some("caf"));
        s.host = Some(" ".to_owned());
        assert_eq!(footer_right(&s), None);
        s.host = None;
        assert_eq!(footer_right(&s), None);
    }

    #[test]
    fn reset_line_formats() {
        let at = naive("2026-10-19 14:30");
        assert_eq!(
            reset_line(Reset::At {
                at,
                left: Span::from_minutes(134)
            }),
            "resets 14:30 (in 2h 14m)"
        );
        assert_eq!(
            reset_line(Reset::At {
                at,
                left: Span::from_minutes(24 * 60)
            }),
            "resets Mon 14:30 (in 1d 0h)"
        );
        assert_eq!(reset_line(Reset::Passed { at }), "reset at 14:30; updating");
        assert_eq!(reset_line(Reset::Unknown), "no reset time");
    }

    #[test]
    fn wrap_breaks_on_words() {
        assert_eq!(
            wrap("cannot reach the usage api: timed out; check the wifi", 39),
            ["cannot reach the usage api: timed out;", "check the wifi"]
        );
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert!(wrap("", 10).is_empty());
    }

    #[test]
    fn logo_bitmap_matches_declared_size() {
        const LOGO_HEIGHT: i32 = 58;
        let stride = LOGO_WIDTH.div_ceil(8) as usize;
        assert_eq!(LOGO.len(), stride * LOGO_HEIGHT as usize);
        const { assert!(LOGO_Y + LOGO_HEIGHT <= 122) };
    }
}
