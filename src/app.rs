//! The main loop: clock → fetch → status → view → panel → wait.
//!
//! [`App::tick`] does one frame and says how long to wait before the next.
//! The production [`App::run`] loop just sleeps for that long; the desktop
//! simulator calls `tick` itself so it can keep its window responsive and
//! jump around in time. Hardware and network are reached only through
//! [`Panel`], [`Clock`] and [`UsageSource`], which is what makes this testable
//! with fakes.
//!
//! The Pi has no real-time clock and may reboot at any moment, so after every
//! start the time is untrusted until the OS has set it from the network.
//! Until then the app shows [`Screen::WaitingForTime`] instead of guessing
//! (TLS certificate checks would fail with a wrong clock anyway).
//!
//! When the account needs signing in, and a [`Portal`] was given, the app
//! opens it and shows [`Screen::SignIn`] with its address as a QR code, then
//! checks every [`SIGN_IN_POLL`] whether a sign-in completed.
//!
//! A failed fetch keeps the last good numbers on screen (flagged STALE once
//! old) because Wi-Fi blips are normal; only errors that need a person, like
//! expired credentials, replace them straight away.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, NaiveDateTime, TimeDelta, Utc};
use chrono_tz::Tz;

use crate::config::DisplaySettings;
use crate::framebuffer::Framebuffer;
use crate::panel::{Panel, RefreshPolicy};
use crate::source::{Portal, SourceError, UsageSource};
use crate::usage::{Usage, sign_in_warning};
use crate::view::{self, Screen};

/// File systemd-timesyncd creates once it has set the clock from NTP. It lives
/// in `/run`, so it is cleared on every reboot.
pub const TIMESYNC_FLAG: &str = "/run/systemd/timesync/synchronized";

/// A clock reading before this year means the time was never set.
pub const MIN_PLAUSIBLE_YEAR: i32 = 2026;

/// How often to re-check the clock while waiting for it to be set.
pub const SYNC_POLL: Duration = Duration::from_secs(5);

/// How soon to retry when there are no numbers to show at all (e.g. the
/// Wi-Fi was not up yet on the first fetch after boot).
pub const ERROR_RETRY: Duration = Duration::from_secs(60);

/// How often to check for a finished sign-in while the sign-in screen is up.
/// Cheap: it only asks the portal; usage is fetched once one completes.
pub const SIGN_IN_POLL: Duration = Duration::from_secs(3);

/// Source of the current time.
pub trait Clock {
    /// Current UTC time.
    fn now(&self) -> DateTime<Utc>;

    /// `t` as wall-clock time where the display is.
    fn local(&self, t: DateTime<Utc>) -> NaiveDateTime;

    /// Whether [`now`](Clock::now) can be trusted yet.
    fn is_synced(&self) -> bool {
        true
    }
}

/// The real system clock, shown in a configured timezone or the OS one.
#[derive(Debug, Clone, Default)]
pub struct SystemClock {
    timezone: Option<Tz>,
    sync_flag: Option<PathBuf>,
}

impl SystemClock {
    /// Clock in `timezone`, or the OS timezone when `None`. Trusted as soon
    /// as the year is plausible.
    pub fn new(timezone: Option<Tz>) -> Self {
        Self {
            timezone,
            sync_flag: None,
        }
    }

    /// Also require `path` (normally [`TIMESYNC_FLAG`]) to exist before the
    /// time is trusted. A plausible year alone is not enough on the Pi:
    /// `fake-hwclock` restores the last shutdown time on boot.
    pub fn require_sync_flag(mut self, path: impl Into<PathBuf>) -> Self {
        self.sync_flag = Some(path.into());
        self
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn local(&self, t: DateTime<Utc>) -> NaiveDateTime {
        match self.timezone {
            Some(tz) => t.with_timezone(&tz).naive_local(),
            None => t.with_timezone(&Local).naive_local(),
        }
    }

    fn is_synced(&self) -> bool {
        self.now().year() >= MIN_PLAUSIBLE_YEAR
            && self.sync_flag.as_ref().is_none_or(|flag| flag.exists())
    }
}

/// Settings the loop needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSettings {
    /// Time between fetches.
    pub poll_interval: Duration,
    /// Numbers older than this are flagged STALE.
    pub stale_after: Duration,
    /// Partial refreshes between full refreshes.
    pub full_refresh_every: u32,
    /// Extra credit to add to the credits row, in hundredths.
    pub credits_bonus_cents: i64,
}

impl From<&DisplaySettings> for AppSettings {
    fn from(d: &DisplaySettings) -> Self {
        Self {
            poll_interval: d.poll_interval,
            stale_after: d.stale_after,
            full_refresh_every: d.full_refresh_every,
            credits_bonus_cents: d.credits_bonus_cents,
        }
    }
}

/// Why the loop stopped.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The panel failed to draw or sleep.
    #[error("display error: {0}; check the HAT is seated and SPI is enabled")]
    Panel(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The last good fetch.
#[derive(Debug, Clone)]
struct Fetched {
    usage: Usage,
    at: DateTime<Utc>,
}

/// The display loop, generic over where usage comes from and where frames go.
#[derive(Debug)]
pub struct App<S, P> {
    source: S,
    settings: AppSettings,
    panel: P,
    policy: RefreshPolicy,
    frame: Framebuffer,
    /// What is on glass now, if known; identical frames are not re-sent.
    on_glass: Option<Framebuffer>,
    last: Option<Fetched>,
    error: Option<SourceError>,
    showing_wait: bool,
    portal: Option<Box<dyn Portal>>,
    /// Address the open portal is serving; `Some` while signing in.
    portal_url: Option<String>,
    last_attempt: Option<DateTime<Utc>>,
    /// Hostname for the dashboard footer.
    host: Option<String>,
}

impl<S: UsageSource, P: Panel> App<S, P> {
    /// A new loop. The first frame is always a full refresh.
    pub fn new(source: S, settings: AppSettings, panel: P) -> Self {
        Self {
            source,
            settings,
            panel,
            policy: RefreshPolicy::new(settings.full_refresh_every),
            frame: Framebuffer::new(),
            on_glass: None,
            last: None,
            error: None,
            showing_wait: false,
            portal: None,
            portal_url: None,
            last_attempt: None,
            host: None,
        }
    }

    /// Show `host` (the device's hostname) in the dashboard footer.
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Offer `portal` (the phone sign-in page) whenever the account needs
    /// signing in. Without one, the error screen points at the
    /// `claude-ticker login` command instead.
    pub fn with_portal(mut self, portal: Box<dyn Portal>) -> Self {
        self.portal = Some(portal);
        self
    }

    /// One iteration of the loop: fetch and draw if `clock` is trusted,
    /// otherwise the waiting screen. Returns how long to wait before the next.
    pub fn tick(&mut self, clock: &impl Clock) -> Result<Duration, AppError> {
        if clock.is_synced() {
            self.showing_wait = false;
            self.step(clock)
        } else {
            self.wait_for_time()
        }
    }

    /// Show the waiting screen once (then sleep the panel) and poll again soon.
    fn wait_for_time(&mut self) -> Result<Duration, AppError> {
        if !self.showing_wait {
            let Ok(()) = view::draw(&Screen::WaitingForTime, &mut self.frame);
            let refresh = self.policy.next_undated();
            self.panel
                .present(&self.frame, refresh)
                .map_err(|e| AppError::Panel(Box::new(e)))?;
            self.panel
                .sleep()
                .map_err(|e| AppError::Panel(Box::new(e)))?;
            self.policy.force_full();
            self.on_glass = None;
            self.showing_wait = true;
        }
        Ok(SYNC_POLL)
    }

    /// Fetch, then draw for `clock`, which must be trusted; returns how long
    /// to wait before the next call. Prefer [`tick`](Self::tick).
    pub fn step(&mut self, clock: &impl Clock) -> Result<Duration, AppError> {
        let now = clock.now();
        if self.portal_url.is_some() {
            // Signing in: only fetch once the page reports success, or at the
            // normal interval (someone may have run `claude-ticker login`).
            let completed = self.portal.as_mut().is_some_and(|p| p.take_completed());
            let poll = TimeDelta::from_std(self.settings.poll_interval).unwrap_or(TimeDelta::MAX);
            let due = self.last_attempt.is_none_or(|t| now - t >= poll);
            if !completed && !due {
                self.redraw(clock)?;
                return Ok(SIGN_IN_POLL);
            }
        }

        self.last_attempt = Some(now);
        match self.source.fetch(now) {
            Ok(usage) => {
                let usage = usage.with_credits_bonus(self.settings.credits_bonus_cents);
                self.last = Some(Fetched { usage, at: now });
                self.error = None;
                if self.portal_url.take().is_some()
                    && let Some(p) = &mut self.portal
                {
                    p.close();
                }
            }
            Err(e) => {
                if e.needs_sign_in() && self.portal_url.is_none() {
                    // On failure keep the plain error screen and retry next time.
                    self.portal_url = self.portal.as_mut().and_then(|p| p.open().ok());
                }
                self.error = Some(e);
            }
        }
        self.redraw(clock)?;
        Ok(if self.portal_url.is_some() {
            SIGN_IN_POLL
        } else if self.last.is_none() {
            ERROR_RETRY.min(self.settings.poll_interval)
        } else {
            self.settings.poll_interval
        })
    }

    /// Draw the current state without fetching (e.g. after the simulator
    /// moved time). Skips the panel if the frame has not changed.
    pub fn redraw(&mut self, clock: &impl Clock) -> Result<(), AppError> {
        let now = clock.now();
        let stale_after = TimeDelta::from_std(self.settings.stale_after).unwrap_or(TimeDelta::MAX);
        let status = match (&self.error, &self.last) {
            (Some(e), _) if e.needs_attention() => Err(e.to_string()),
            (error, Some(f)) => f
                .usage
                .status(f.at, now, stale_after, |t| clock.local(t))
                .map_err(|stale| match error {
                    Some(e) => format!("{stale}; {e}"),
                    None => stale.to_string(),
                }),
            (Some(e), None) => Err(e.to_string()),
            (None, None) => Err("no usage fetched yet".to_owned()),
        };
        let status = status.map(|mut s| {
            s.sign_in_left = sign_in_warning(self.source.sign_in_expires(), now);
            s.host.clone_from(&self.host);
            s
        });
        // The portal is closed by the first good fetch, so while it is open
        // the sign-in is still the thing to fix, whatever failed last.
        let screen = match (&status, self.portal_url.as_deref()) {
            (_, Some(url)) => Screen::SignIn(url),
            (Ok(s), None) => Screen::Dashboard(s),
            (Err(message), None) => Screen::Error(message),
        };

        let Ok(()) = view::draw(&screen, &mut self.frame);
        if self.on_glass.as_ref() == Some(&self.frame) {
            return Ok(());
        }
        let refresh = self.policy.next(clock.local(now).date());
        self.panel
            .present(&self.frame, refresh)
            .map_err(|e| AppError::Panel(Box::new(e)))?;
        self.on_glass = Some(self.frame.clone());

        // An error or sign-in screen may stay up for hours: let the controller
        // sleep. The dashboard keeps it awake so updates can use partial
        // refresh.
        if !matches!(screen, Screen::Dashboard(_)) {
            self.panel
                .sleep()
                .map_err(|e| AppError::Panel(Box::new(e)))?;
            self.policy.force_full();
        }
        Ok(())
    }

    /// Run forever against a real clock.
    pub fn run(&mut self, clock: &impl Clock) -> Result<(), AppError> {
        loop {
            let wait = self.tick(clock)?;
            std::thread::sleep(wait);
        }
    }

    /// Make the next frame a full refresh and send it even if unchanged.
    pub fn force_full(&mut self) {
        self.policy.force_full();
        self.on_glass = None;
    }

    /// Current settings.
    pub fn settings(&self) -> &AppSettings {
        &self.settings
    }

    /// Partial refreshes since the last full one.
    pub fn partials_since_full(&self) -> u32 {
        self.policy.partials_since_full()
    }

    /// The usage source, e.g. to steer a fake one in the simulator.
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// The panel, e.g. to pump a simulator window's events.
    pub fn panel_mut(&mut self) -> &mut P {
        &mut self.panel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::Refresh;
    use crate::usage::{Quota, Span, Status};
    use std::convert::Infallible;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Present(Refresh),
        Sleep,
    }

    #[derive(Default)]
    struct RecordingPanel {
        events: Vec<Event>,
        frames: Vec<Framebuffer>,
    }

    impl Panel for RecordingPanel {
        type Error = Infallible;
        fn present(&mut self, frame: &Framebuffer, refresh: Refresh) -> Result<(), Infallible> {
            self.events.push(Event::Present(refresh));
            self.frames.push(frame.clone());
            Ok(())
        }
        fn sleep(&mut self) -> Result<(), Infallible> {
            self.events.push(Event::Sleep);
            Ok(())
        }
    }

    /// Returns whatever the test last put in `next`.
    struct FakeSource {
        next: Result<Usage, SourceError>,
        sign_in_expires: Option<DateTime<Utc>>,
        fetches: u32,
    }

    impl UsageSource for FakeSource {
        fn fetch(&mut self, _now: DateTime<Utc>) -> Result<Usage, SourceError> {
            self.fetches += 1;
            self.next.clone()
        }
        fn sign_in_expires(&self) -> Option<DateTime<Utc>> {
            self.sign_in_expires
        }
    }

    /// Fixed UTC time, displayed at UTC+10.
    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
        fn local(&self, t: DateTime<Utc>) -> NaiveDateTime {
            t.naive_utc() + TimeDelta::hours(10)
        }
    }

    struct UnsyncedClock;
    impl Clock for UnsyncedClock {
        fn now(&self) -> DateTime<Utc> {
            at("2025-01-01 00:00")
        }
        fn local(&self, t: DateTime<Utc>) -> NaiveDateTime {
            t.naive_utc()
        }
        fn is_synced(&self) -> bool {
            false
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
            .unwrap()
            .and_utc()
    }

    fn clock(s: &str) -> FixedClock {
        FixedClock(at(s))
    }

    fn usage(session: f64) -> Usage {
        Usage {
            session: Some(Quota {
                percent: session,
                resets_at: Some(at("2026-10-19 04:30")),
            }),
            weekly: Some(Quota {
                percent: 23.0,
                resets_at: Some(at("2026-10-22 23:00")),
            }),
            model: None,
            credits: None,
        }
    }

    const POLL: Duration = Duration::from_secs(300);

    fn app(next: Result<Usage, SourceError>) -> App<FakeSource, RecordingPanel> {
        App::new(
            FakeSource {
                next,
                sign_in_expires: None,
                fetches: 0,
            },
            AppSettings {
                poll_interval: POLL,
                stale_after: Duration::from_secs(15 * 60),
                full_refresh_every: 2,
                credits_bonus_cents: 0,
            },
            RecordingPanel::default(),
        )
    }

    fn expected_frame(screen: Screen<'_>) -> Framebuffer {
        let mut fb = Framebuffer::new();
        let Ok(()) = view::draw(&screen, &mut fb);
        fb
    }

    fn status(u: &Usage, fetched: &str, now: &FixedClock) -> Status {
        u.status(at(fetched), now.0, TimeDelta::minutes(15), |t| now.local(t))
            .unwrap()
    }

    #[test]
    fn dashboard_stays_awake_and_skips_unchanged_frames() {
        use Event::*;
        use Refresh::*;
        let mut a = app(Ok(usage(62.0)));
        let c = clock("2026-10-19 02:16");
        assert_eq!(a.tick(&c).unwrap(), POLL);
        assert_eq!(a.tick(&c).unwrap(), POLL);
        assert_eq!(a.panel.events, [Present(Full)]);
        assert_eq!(
            a.panel.frames[0],
            expected_frame(Screen::Dashboard(&status(
                &usage(62.0),
                "2026-10-19 02:16",
                &c
            )))
        );

        for (i, minute) in ["21", "26", "31"].into_iter().enumerate() {
            a.source.next = Ok(usage(62.0 + i as f64));
            a.tick(&clock(&format!("2026-10-19 02:{minute}"))).unwrap();
        }
        assert_eq!(
            a.panel.events,
            [
                Present(Full),
                Present(Partial),
                Present(Partial),
                Present(Full)
            ]
        );
    }

    #[test]
    fn network_error_keeps_numbers_then_goes_stale_then_errors() {
        let mut a = app(Ok(usage(62.0)));
        a.tick(&clock("2026-10-19 02:16")).unwrap();
        a.source.next = Err(SourceError::Network("timed out".into()));

        let c = clock("2026-10-19 02:21");
        assert_eq!(a.tick(&c).unwrap(), POLL);
        let fresh = status(&usage(62.0), "2026-10-19 02:16", &c);
        assert!(!fresh.stale);
        assert_eq!(a.panel.frames[1], expected_frame(Screen::Dashboard(&fresh)));

        let c = clock("2026-10-19 02:40");
        a.tick(&c).unwrap();
        let stale = status(&usage(62.0), "2026-10-19 02:16", &c);
        assert!(stale.stale);
        assert_eq!(a.panel.frames[2], expected_frame(Screen::Dashboard(&stale)));
        assert!(!a.panel.events.contains(&Event::Sleep));

        a.tick(&clock("2026-10-19 03:47")).unwrap();
        assert_eq!(
            a.panel.frames[3],
            expected_frame(Screen::Error(
                "last update was 1h 31m ago; cannot reach the usage api: timed out; check the wifi"
            ))
        );
        assert_eq!(a.panel.events[4], Event::Sleep);
    }

    #[test]
    fn auth_error_replaces_numbers_at_once_and_recovers_with_full_refresh() {
        use Event::*;
        let mut a = app(Ok(usage(62.0)));
        a.tick(&clock("2026-10-19 02:16")).unwrap();
        let err = SourceError::Auth("http 401".into());
        a.source.next = Err(err.clone());
        a.tick(&clock("2026-10-19 02:21")).unwrap();
        assert_eq!(
            a.panel.frames[1],
            expected_frame(Screen::Error(&err.to_string()))
        );
        // Same error again: nothing sent, panel left asleep.
        a.tick(&clock("2026-10-19 02:26")).unwrap();
        a.source.next = Ok(usage(10.0));
        a.tick(&clock("2026-10-19 02:31")).unwrap();
        assert_eq!(
            a.panel.events,
            [
                Present(Refresh::Full),
                Present(Refresh::Partial),
                Sleep,
                Present(Refresh::Full)
            ]
        );
    }

    #[test]
    fn no_data_yet_shows_error_and_retries_sooner() {
        let mut a = app(Err(SourceError::Http(503)));
        assert_eq!(a.tick(&clock("2026-10-19 02:16")).unwrap(), ERROR_RETRY);
        assert_eq!(
            a.panel.frames[0],
            expected_frame(Screen::Error("usage api returned http 503; will retry"))
        );
        assert_eq!(a.panel.events[1], Event::Sleep);
    }

    #[test]
    fn waits_for_time_sync_then_draws_once_synced() {
        let mut a = app(Ok(usage(62.0)));
        assert_eq!(a.tick(&UnsyncedClock).unwrap(), SYNC_POLL);
        assert_eq!(a.tick(&UnsyncedClock).unwrap(), SYNC_POLL);
        use Event::*;
        // Drawn once, panel slept; polling does not redraw.
        assert_eq!(a.panel.events, [Present(Refresh::Full), Sleep]);
        assert_eq!(a.panel.frames[0], expected_frame(Screen::WaitingForTime));

        let c = clock("2026-10-19 02:16");
        assert_eq!(a.tick(&c).unwrap(), POLL);
        assert_eq!(a.panel.events[2], Present(Refresh::Full));
        assert_eq!(
            a.panel.frames[1],
            expected_frame(Screen::Dashboard(&status(
                &usage(62.0),
                "2026-10-19 02:16",
                &c
            )))
        );
    }

    #[test]
    fn warns_when_sign_in_expires_soon() {
        let mut a = app(Ok(usage(62.0)));
        let c = clock("2026-10-19 02:16");
        a.source.sign_in_expires = Some(at("2026-10-29 02:16"));
        a.tick(&c).unwrap();
        let quiet = status(&usage(62.0), "2026-10-19 02:16", &c);
        assert_eq!(a.panel.frames[0], expected_frame(Screen::Dashboard(&quiet)));

        a.source.sign_in_expires = Some(at("2026-10-21 05:16"));
        a.tick(&c).unwrap();
        let warned = Status {
            sign_in_left: Some(Span::from_minutes((2 * 24 + 3) * 60)),
            ..quiet
        };
        assert_eq!(
            a.panel.frames[1],
            expected_frame(Screen::Dashboard(&warned))
        );
    }

    #[test]
    fn shows_hostname_on_the_dashboard() {
        let mut a = app(Ok(usage(62.0))).with_host("claude-ticker");
        let c = clock("2026-10-19 02:16");
        a.tick(&c).unwrap();
        let named = Status {
            host: Some("claude-ticker".to_owned()),
            ..status(&usage(62.0), "2026-10-19 02:16", &c)
        };
        assert_eq!(a.panel.frames[0], expected_frame(Screen::Dashboard(&named)));
    }

    /// What the fake portal was asked to do, shared with the test.
    #[derive(Debug, Default)]
    struct PortalLog {
        opens: u32,
        closes: u32,
        open: bool,
        fail: bool,
        completed: bool,
    }

    #[derive(Debug)]
    struct FakePortal(std::rc::Rc<std::cell::RefCell<PortalLog>>);

    impl Portal for FakePortal {
        fn open(&mut self) -> Result<String, crate::source::PortalError> {
            let mut log = self.0.borrow_mut();
            log.opens += 1;
            if log.fail {
                return Err(crate::source::PortalError("no network".into()));
            }
            log.open = true;
            Ok(SIGN_IN_URL.to_owned())
        }
        fn take_completed(&mut self) -> bool {
            std::mem::take(&mut self.0.borrow_mut().completed)
        }
        fn close(&mut self) {
            let mut log = self.0.borrow_mut();
            log.closes += 1;
            log.open = false;
        }
    }

    const SIGN_IN_URL: &str = "http://192.168.1.23:8080/k7Qp2xNa";

    fn app_with_portal(
        next: Result<Usage, SourceError>,
    ) -> (
        App<FakeSource, RecordingPanel>,
        std::rc::Rc<std::cell::RefCell<PortalLog>>,
    ) {
        let log = std::rc::Rc::default();
        let a = app(next).with_portal(Box::new(FakePortal(std::rc::Rc::clone(&log))));
        (a, log)
    }

    #[test]
    fn signed_out_shows_qr_until_sign_in_completes() {
        use Event::*;
        let (mut a, log) = app_with_portal(Err(SourceError::Auth("http 401".into())));
        let c = clock("2026-10-19 02:16");
        assert_eq!(a.tick(&c).unwrap(), SIGN_IN_POLL);
        assert_eq!(
            a.panel.frames[0],
            expected_frame(Screen::SignIn(SIGN_IN_URL))
        );
        assert_eq!(a.panel.events, [Present(Refresh::Full), Sleep]);

        // Polling only asks the portal: no fetches, no redraws, opened once.
        for _ in 0..5 {
            assert_eq!(a.tick(&c).unwrap(), SIGN_IN_POLL);
        }
        assert_eq!(a.source.fetches, 1);
        assert_eq!(a.panel.events.len(), 2);
        assert_eq!(log.borrow().opens, 1);

        // The phone finishes signing in: fetch at once, dashboard, page closed.
        a.source.next = Ok(usage(62.0));
        log.borrow_mut().completed = true;
        assert_eq!(a.tick(&c).unwrap(), POLL);
        assert_eq!(a.source.fetches, 2);
        assert_eq!(
            a.panel.frames[1],
            expected_frame(Screen::Dashboard(&status(
                &usage(62.0),
                "2026-10-19 02:16",
                &c
            )))
        );
        assert_eq!(a.panel.events[2], Present(Refresh::Full));
        assert!(!log.borrow().open);
        assert_eq!(log.borrow().closes, 1);
    }

    #[test]
    fn signing_in_still_fetches_at_poll_interval() {
        // E.g. someone ran `claude-ticker login` over SSH instead.
        let (mut a, log) = app_with_portal(Err(SourceError::Credentials {
            path: "/x".into(),
            reason: "missing".into(),
        }));
        a.tick(&clock("2026-10-19 02:16")).unwrap();
        a.tick(&clock("2026-10-19 02:20")).unwrap();
        assert_eq!(a.source.fetches, 1);
        a.source.next = Ok(usage(62.0));
        assert_eq!(a.tick(&clock("2026-10-19 02:21")).unwrap(), POLL);
        assert_eq!(a.source.fetches, 2);
        assert_eq!(log.borrow().closes, 1);
    }

    #[test]
    fn network_errors_do_not_open_the_portal() {
        let (mut a, log) = app_with_portal(Err(SourceError::Network("down".into())));
        assert_eq!(a.tick(&clock("2026-10-19 02:16")).unwrap(), ERROR_RETRY);
        assert_eq!(log.borrow().opens, 0);
    }

    #[test]
    fn portal_failure_falls_back_to_error_screen_and_retries() {
        let err = SourceError::Auth("http 401".into());
        let (mut a, log) = app_with_portal(Err(err.clone()));
        log.borrow_mut().fail = true;
        assert_eq!(a.tick(&clock("2026-10-19 02:16")).unwrap(), ERROR_RETRY);
        assert_eq!(
            a.panel.frames[0],
            expected_frame(Screen::Error(&err.to_string()))
        );
        log.borrow_mut().fail = false;
        assert_eq!(a.tick(&clock("2026-10-19 02:17")).unwrap(), SIGN_IN_POLL);
        assert_eq!(log.borrow().opens, 2);
        assert_eq!(
            a.panel.frames[1],
            expected_frame(Screen::SignIn(SIGN_IN_URL))
        );
    }

    #[test]
    fn force_full_resends_unchanged_frame() {
        let mut a = app(Ok(usage(62.0)));
        let c = clock("2026-10-19 02:16");
        a.tick(&c).unwrap();
        a.force_full();
        a.tick(&c).unwrap();
        assert_eq!(
            a.panel.events,
            [Event::Present(Refresh::Full), Event::Present(Refresh::Full)]
        );
    }

    #[test]
    fn system_clock_uses_configured_timezone() {
        // Kiritimati is UTC+14 all year (no daylight saving).
        let clock = SystemClock::new(Some(chrono_tz::Pacific::Kiritimati));
        let now = clock.now();
        assert_eq!(clock.local(now) - now.naive_utc(), TimeDelta::hours(14));
    }

    #[test]
    fn system_clock_requires_sync_flag_when_configured() {
        let flag = std::env::temp_dir().join(format!("claude-ticker-sync-{}", std::process::id()));
        let _ = std::fs::remove_file(&flag);
        let clock = SystemClock::new(None).require_sync_flag(&flag);
        assert!(!clock.is_synced());
        std::fs::write(&flag, b"").unwrap();
        assert!(clock.is_synced());
        std::fs::remove_file(&flag).unwrap();
        assert!(SystemClock::new(None).is_synced());
    }
}
