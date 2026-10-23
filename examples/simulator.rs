//! Desktop debug view: run the real app loop in an SDL window with a fake
//! usage source you steer from the keyboard.
//!
//! ```sh
//! brew install sdl2                       # once (macOS); apt install libsdl2-dev on Linux
//! cargo run --example simulator --features simulator
//! cargo run --example simulator --features simulator -- config.toml --poll-seconds 5
//! ```
//!
//! Keys: ↓/↑ session −/+5 %, [/] week −/+5 %, ←/→ time −/+1 h, O toggle the
//! Opus weekly row, X toggle network failures, A toggle "signed out" (shows
//! the sign-in QR code; press A again to play the phone finishing), L toggle
//! "sign-in expires in 2 days", Space
//! fetch now, W toggle "clock not synced yet", F force full refresh, Esc quit.
//!
//! Like a fresh boot on the Pi, it starts with the clock unsynced (the
//! "SETTING THE CLOCK" screen) and fakes an NTP sync after `--sync-seconds`
//! (default 5; 0 = synced from the start). `--unsynced` never syncs by itself;
//! press W. Credentials in the config are ignored; nothing touches the network.

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use claude_ticker::app::{App, AppSettings, Clock, SystemClock};
use claude_ticker::config::{Config, DisplaySettings};
use claude_ticker::simulator::SimPanel;
use claude_ticker::source::{Portal, PortalError, SourceError, UsageSource};
use claude_ticker::usage::{ModelQuota, Quota, Usage};
use claude_ticker::view::TIME_FORMAT;
use embedded_graphics_simulator::SimulatorEvent;
use embedded_graphics_simulator::sdl2::Keycode;

const USAGE: &str =
    "usage: simulator [CONFIG.toml] [--poll-seconds N] [--sync-seconds N] [--unsynced]";
/// Fake NTP delay after start, like a Pi that has just booted.
const DEFAULT_SYNC_SECONDS: u64 = 5;
const SESSION_WINDOW: TimeDelta = TimeDelta::hours(5);
const WEEK_WINDOW: TimeDelta = TimeDelta::days(7);

/// Real time plus an offset moved with the keyboard. `synced` fakes the
/// Pi's NTP state.
struct SimClock {
    real: SystemClock,
    offset: TimeDelta,
    synced: bool,
}

impl Clock for SimClock {
    fn now(&self) -> DateTime<Utc> {
        self.real.now() + self.offset
    }

    fn local(&self, t: DateTime<Utc>) -> NaiveDateTime {
        self.real.local(t)
    }

    fn is_synced(&self) -> bool {
        self.synced
    }
}

/// Usage numbers set from the keyboard. Windows roll over like the real
/// ones: once a reset time passes, that limit drops to 0 % on the next fetch.
struct FakeSource {
    session: Quota,
    week: Quota,
    opus: Option<Quota>,
    failure: Option<SourceError>,
    sign_in_expires: Option<DateTime<Utc>>,
}

impl FakeSource {
    fn new(now: DateTime<Utc>) -> Self {
        let quota = |percent, left| Quota {
            percent,
            resets_at: Some(now + left),
        };
        Self {
            session: quota(62.0, TimeDelta::minutes(2 * 60 + 14)),
            week: quota(23.0, TimeDelta::hours(3 * 24 + 20)),
            opus: None,
            failure: None,
            sign_in_expires: None,
        }
    }

    fn opus_default(&self) -> Quota {
        Quota {
            percent: 4.0,
            ..self.week
        }
    }
}

fn roll(q: &mut Quota, now: DateTime<Utc>, window: TimeDelta) {
    while let Some(at) = q.resets_at.filter(|at| *at <= now) {
        q.resets_at = Some(at + window);
        q.percent = 0.0;
    }
}

fn bump(q: &mut Quota, by: f64) {
    q.percent = (q.percent + by).clamp(0.0, 100.0);
}

impl UsageSource for FakeSource {
    fn fetch(&mut self, now: DateTime<Utc>) -> Result<Usage, SourceError> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        roll(&mut self.session, now, SESSION_WINDOW);
        roll(&mut self.week, now, WEEK_WINDOW);
        if let Some(q) = &mut self.opus {
            roll(q, now, WEEK_WINDOW);
        }
        Ok(Usage {
            session: Some(self.session),
            weekly: Some(self.week),
            model: self.opus.map(|quota| ModelQuota {
                model: "OPUS".to_owned(),
                quota,
            }),
        })
    }

    fn sign_in_expires(&self) -> Option<DateTime<Utc>> {
        self.sign_in_expires
    }
}

/// Stands in for the phone sign-in page: hands out a fixed address and
/// "completes" when the simulator says so. Nothing is served.
#[derive(Debug, Default)]
struct FakePortal(std::rc::Rc<std::cell::Cell<bool>>);

impl Portal for FakePortal {
    fn open(&mut self) -> Result<String, PortalError> {
        Ok("http://192.168.1.23:8080/k7Qp2xNa".to_owned())
    }

    fn take_completed(&mut self) -> bool {
        self.0.take()
    }

    fn close(&mut self) {}
}

struct Options {
    config: Option<PathBuf>,
    poll_seconds: Option<u64>,
    sync_seconds: u64,
    unsynced: bool,
}

fn parse_args() -> Result<Options, Box<dyn Error>> {
    let mut opts = Options {
        config: None,
        poll_seconds: None,
        sync_seconds: DEFAULT_SYNC_SECONDS,
        unsynced: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--poll-seconds" => {
                let v = args.next().ok_or(USAGE)?;
                opts.poll_seconds = Some(v.parse::<u64>()?.max(1));
            }
            "--sync-seconds" => {
                let v = args.next().ok_or(USAGE)?;
                opts.sync_seconds = v.parse()?;
            }
            "--unsynced" => opts.unsynced = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n{USAGE}").into());
            }
            path => opts.config = Some(PathBuf::from(path)),
        }
    }
    Ok(opts)
}

fn main() -> Result<(), Box<dyn Error>> {
    let opts = parse_args()?;

    let config = match &opts.config {
        Some(path) => Config::load(path)?,
        None => Config::default(),
    };
    let mut display: DisplaySettings = config.display;
    // Simulator only: the 60 s minimum protects real e-paper and the API.
    if let Some(s) = opts.poll_seconds {
        display.poll_interval = Duration::from_secs(s);
    }

    let mut clock = SimClock {
        real: SystemClock::new(config.timezone),
        offset: TimeDelta::zero(),
        synced: !opts.unsynced && opts.sync_seconds == 0,
    };
    // When the fake NTP sync lands; `None` once synced or with --unsynced.
    let mut sync_at = (!clock.synced && !opts.unsynced)
        .then(|| Instant::now() + Duration::from_secs(opts.sync_seconds));

    let mut panel = SimPanel::new(3);
    // EG_SIMULATOR_DUMP saves the first window update and exits; skip the
    // flash so that first update is the real frame.
    panel.set_flash(std::env::var_os("EG_SIMULATOR_DUMP").is_none());
    let source = FakeSource::new(clock.now());
    let signed_in = std::rc::Rc::default();
    let portal = FakePortal(std::rc::Rc::clone(&signed_in));
    let mut app =
        App::new(source, AppSettings::from(&display), panel).with_portal(Box::new(portal));

    println!("claude-ticker simulator (fake usage, no network)");
    if sync_at.is_some() {
        println!("clock syncs in {}s (fake NTP)", opts.sync_seconds);
    }
    println!(
        "keys: ↓/↑ session  [/] week  ←/→ ±1h  O opus row  X net fail  A signed out  L sign-in expiry  Space fetch  W sync  F full  Esc quit"
    );

    let mut next_fetch = Instant::now();
    let mut fetch = true;
    let mut redraw = false;
    loop {
        if sync_at.is_some_and(|at| Instant::now() >= at) {
            sync_at = None;
            clock.synced = true;
            fetch = true;
        }
        if fetch || redraw || Instant::now() >= next_fetch {
            let failing = match &app.source_mut().failure {
                Some(SourceError::Auth(_)) => "  SIGNED OUT",
                Some(_) => "  NET FAIL",
                None => "",
            };
            let note = if clock.synced {
                let now = clock.local(clock.now()).format(TIME_FORMAT);
                format!(
                    "{now}  poll {}s{failing}",
                    app.settings().poll_interval.as_secs()
                )
            } else {
                "unsynced".to_owned()
            };
            app.panel_mut().set_note(note);
            if redraw && !fetch && clock.synced {
                // Time moved: show what the Pi would show without fetching.
                app.redraw(&clock)?;
            } else {
                let wait = app.tick(&clock)?;
                next_fetch = Instant::now() + wait;
            }
        }

        app.panel_mut().update();

        (fetch, redraw) = (false, false);
        for event in app.panel_mut().events() {
            let keycode = match event {
                SimulatorEvent::Quit => return Ok(()),
                SimulatorEvent::KeyDown { keycode, .. } => keycode,
                _ => continue,
            };
            let src = app.source_mut();
            match keycode {
                Keycode::LEFT | Keycode::RIGHT => {
                    let hour = TimeDelta::hours(1);
                    clock.offset += if keycode == Keycode::LEFT {
                        -hour
                    } else {
                        hour
                    };
                    redraw = true;
                    continue;
                }
                Keycode::UP => bump(&mut src.session, 5.0),
                Keycode::DOWN => bump(&mut src.session, -5.0),
                Keycode::RIGHTBRACKET => bump(&mut src.week, 5.0),
                Keycode::LEFTBRACKET => bump(&mut src.week, -5.0),
                Keycode::O => {
                    src.opus = match src.opus {
                        Some(_) => None,
                        None => Some(src.opus_default()),
                    };
                }
                Keycode::X => {
                    src.failure = match src.failure {
                        Some(SourceError::Network(_)) => None,
                        _ => Some(SourceError::Network("simulated outage".to_owned())),
                    };
                }
                Keycode::A => {
                    src.failure = match src.failure {
                        Some(SourceError::Auth(_)) => {
                            // As if the phone had just finished signing in.
                            signed_in.set(true);
                            None
                        }
                        _ => Some(SourceError::Auth("simulated".to_owned())),
                    };
                }
                Keycode::L => {
                    src.sign_in_expires = match src.sign_in_expires {
                        Some(_) => None,
                        None => Some(clock.now() + TimeDelta::hours(2 * 24 + 3)),
                    };
                }
                Keycode::W => {
                    sync_at = None;
                    clock.synced = !clock.synced;
                }
                Keycode::F => app.force_full(),
                Keycode::SPACE => {}
                _ => continue,
            }
            fetch = true;
        }

        std::thread::sleep(Duration::from_millis(16));
    }
}
