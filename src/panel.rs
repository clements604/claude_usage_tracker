//! "Put this frame on glass", and when to do it the slow, clean way.
//!
//! E-paper has two update modes: a ~2 s full refresh that flashes but leaves no
//! ghosting, and a ~0.3 s partial refresh that ghosts a little each time.
//! [`RefreshPolicy`] is the single place that decides between them, so the
//! app, the hardware driver and the simulator all agree.

use chrono::NaiveDate;

use crate::framebuffer::Framebuffer;

/// How to update the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Slow, flashing, clears ghosting.
    Full,
    /// Fast, no flash, leaves faint ghosts.
    Partial,
}

/// Anything that can show a frame: the e-paper HAT, PNG files, a desktop window.
pub trait Panel {
    /// Backend-specific failure.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Show `frame` using the given refresh mode.
    fn present(&mut self, frame: &Framebuffer, refresh: Refresh) -> Result<(), Self::Error>;

    /// Put the controller into deep sleep. The image stays on screen; the next
    /// `present` must wake the controller and is always a full refresh.
    fn sleep(&mut self) -> Result<(), Self::Error>;
}

/// Decides between full and partial refreshes.
///
/// Forces [`Refresh::Full`] on the first frame, the first frame of each new
/// day, after `full_every` consecutive partials, and after [`force_full`].
///
/// [`force_full`]: RefreshPolicy::force_full
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshPolicy {
    full_every: u32,
    partials: u32,
    last_day: Option<NaiveDate>,
    force: bool,
}

impl RefreshPolicy {
    /// Allow up to `full_every` partial refreshes between full ones (min 1).
    pub fn new(full_every: u32) -> Self {
        Self {
            full_every: full_every.max(1),
            partials: 0,
            last_day: None,
            force: true,
        }
    }

    /// Make the next refresh full (e.g. after the panel slept or config reload).
    pub fn force_full(&mut self) {
        self.force = true;
    }

    /// Partial refreshes since the last full one.
    pub fn partials_since_full(&self) -> u32 {
        self.partials
    }

    /// Refresh mode for a frame with no trusted date (the waiting-for-time
    /// screen): always full, and the next dated frame counts as a new day.
    pub fn next_undated(&mut self) -> Refresh {
        self.force = false;
        self.partials = 0;
        self.last_day = None;
        Refresh::Full
    }

    /// Refresh mode for the next frame shown on `today`.
    pub fn next(&mut self, today: NaiveDate) -> Refresh {
        let new_day = self.last_day != Some(today);
        self.last_day = Some(today);
        if self.force || new_day || self.partials >= self.full_every {
            self.force = false;
            self.partials = 0;
            Refresh::Full
        } else {
            self.partials += 1;
            Refresh::Partial
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn first_frame_is_full_then_partials_up_to_limit() {
        let mut p = RefreshPolicy::new(3);
        let day = d("2026-10-19");
        let got: Vec<_> = (0..9).map(|_| p.next(day)).collect();
        use Refresh::*;
        assert_eq!(
            got,
            [
                Full, Partial, Partial, Partial, Full, Partial, Partial, Partial, Full
            ]
        );
    }

    #[test]
    fn new_day_forces_full() {
        let mut p = RefreshPolicy::new(10);
        assert_eq!(p.next(d("2026-10-19")), Refresh::Full);
        assert_eq!(p.next(d("2026-10-19")), Refresh::Partial);
        assert_eq!(p.next(d("2026-10-20")), Refresh::Full);
        assert_eq!(p.partials_since_full(), 0);
    }

    #[test]
    fn undated_frame_is_full_and_next_dated_frame_too() {
        let mut p = RefreshPolicy::new(10);
        let day = d("2026-10-19");
        assert_eq!(p.next(day), Refresh::Full);
        assert_eq!(p.next(day), Refresh::Partial);
        assert_eq!(p.next_undated(), Refresh::Full);
        assert_eq!(p.next(day), Refresh::Full);
    }

    #[test]
    fn force_full_after_sleep() {
        let mut p = RefreshPolicy::new(10);
        let day = d("2026-10-19");
        assert_eq!(p.next(day), Refresh::Full);
        assert_eq!(p.next(day), Refresh::Partial);
        p.force_full();
        assert_eq!(p.next(day), Refresh::Full);
        assert_eq!(p.next(day), Refresh::Partial);
    }
}
