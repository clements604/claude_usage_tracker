//! Claude plan usage: the API response in, the numbers on screen out.
//!
//! Pure logic with no I/O and no clock, so every rule is unit-tested with
//! fixed times. The usage endpoint is undocumented and has changed shape
//! before, so parsing is deliberately forgiving: every field is optional,
//! unknown fields are ignored, and the only hard failure is a response with
//! neither a session nor a weekly limit in it.
//!
//! The response carries each limit twice. The named objects (`five_hour`,
//! `seven_day`, `seven_day_opus`, ...) have fractional percentages and are
//! read first; the newer `limits` array is the fallback.

use std::fmt;

use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use serde_json::Value;

/// Data older than this many times the configured `stale_after` is not shown
/// at all; the app draws the error screen instead of old numbers.
pub const STALE_LIMIT_FACTOR: i32 = 6;

/// Start warning on the dashboard this long before the sign-in (refresh
/// token) expires, so there is time to run `claude-ticker login`.
pub const SIGN_IN_WARNING: TimeDelta = TimeDelta::days(5);

/// Model-specific weekly limits, in the order they are looked for, with the
/// name shown on the dashboard.
const MODEL_FIELDS: [(&str, &str); 2] =
    [("seven_day_opus", "OPUS"), ("seven_day_sonnet", "SONNET")];

/// One rate limit as reported by the API.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quota {
    /// Percent of the limit used, as reported (not yet clamped).
    pub percent: f64,
    /// When the limit window resets; `None` if the API gave no time.
    pub resets_at: Option<DateTime<Utc>>,
}

/// A weekly limit that applies to one model family only.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelQuota {
    /// Upper-case model name for the label, e.g. `OPUS`.
    pub model: String,
    /// The limit itself.
    pub quota: Quota,
}

/// Everything one fetch of the usage endpoint tells us.
#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    /// The rolling 5-hour session limit.
    pub session: Option<Quota>,
    /// The weekly limit across all models.
    pub weekly: Option<Quota>,
    /// A model-specific weekly limit, if the plan has one.
    pub model: Option<ModelQuota>,
}

/// Why a usage response could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Not JSON at all (an HTML error page, a truncated body).
    #[error("usage response is not json: {0}")]
    NotJson(String),
    /// JSON, but without the limits we need.
    #[error("usage response has no session or weekly limit; the api may have changed")]
    NoLimits,
}

impl Usage {
    /// Parse a response from the usage endpoint.
    pub fn from_json(text: &str) -> Result<Self, ParseError> {
        let v: Value =
            serde_json::from_str(text).map_err(|e| ParseError::NotJson(e.to_string()))?;
        let limits = v
            .get("limits")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let listed = |kind: &str| {
            limits
                .iter()
                .find(|l| l.get("kind").and_then(Value::as_str) == Some(kind))
                .and_then(|l| quota(l, "percent"))
        };

        let session = named(&v, "five_hour").or_else(|| listed("session"));
        let weekly = named(&v, "seven_day").or_else(|| listed("weekly_all"));
        let model = MODEL_FIELDS
            .iter()
            .find_map(|&(field, model)| {
                named(&v, field).map(|quota| ModelQuota {
                    model: model.to_owned(),
                    quota,
                })
            })
            .or_else(|| limits.iter().find_map(listed_model));

        if session.is_none() && weekly.is_none() {
            return Err(ParseError::NoLimits);
        }
        Ok(Self {
            session,
            weekly,
            model,
        })
    }

    /// What to show at `now` for data fetched at `fetched_at`. `local` turns
    /// a UTC instant into wall-clock time where the display is.
    ///
    /// Rules:
    /// - Percentages are rounded and clamped to 0–100.
    /// - Once a limit's reset time has passed it shows 0% and
    ///   [`Reset::Passed`] until the next fetch brings fresh numbers.
    /// - Data older than `stale_after` is flagged [`Status::stale`]; older than
    ///   [`STALE_LIMIT_FACTOR`] × `stale_after` it is an error.
    /// - Durations are never negative; a `fetched_at` in the future counts
    ///   as just fetched.
    pub fn status(
        &self,
        fetched_at: DateTime<Utc>,
        now: DateTime<Utc>,
        stale_after: TimeDelta,
        local: impl Fn(DateTime<Utc>) -> NaiveDateTime,
    ) -> Result<Status, StatusError> {
        let age = (now - fetched_at).max(TimeDelta::zero());
        let limit = stale_after
            .checked_mul(STALE_LIMIT_FACTOR)
            .unwrap_or(TimeDelta::MAX);
        if age > limit {
            return Err(StatusError::Stale {
                age: Span::from_delta(age),
            });
        }

        let row = |label: String, q: &Quota| {
            let reset = match q.resets_at {
                None => Reset::Unknown,
                Some(at) if at <= now => Reset::Passed { at: local(at) },
                Some(at) => Reset::At {
                    at: local(at),
                    left: Span::from_delta(at - now),
                },
            };
            let percent = match reset {
                Reset::Passed { .. } => 0,
                _ => clamp_percent(q.percent),
            };
            Row {
                label,
                percent,
                reset,
            }
        };

        let mut rows = Vec::with_capacity(3);
        if let Some(q) = &self.session {
            rows.push(row("SESSION".to_owned(), q));
        }
        if let Some(q) = &self.weekly {
            rows.push(row("WEEK".to_owned(), q));
        }
        if let Some(m) = &self.model {
            rows.push(row(format!("WEEK {}", m.model), &m.quota));
        }
        Ok(Status {
            rows,
            updated: local(fetched_at),
            stale: age > stale_after,
            sign_in_left: None,
        })
    }
}

fn named(v: &Value, field: &str) -> Option<Quota> {
    quota(v.get(field)?, "utilization")
}

/// A `limits` entry like `{"kind": "weekly_opus", "percent": 4}`.
fn listed_model(l: &Value) -> Option<ModelQuota> {
    let kind = l.get("kind")?.as_str()?;
    let model = kind.strip_prefix("weekly_").filter(|m| *m != "all")?;
    Some(ModelQuota {
        model: model.replace('_', " ").to_ascii_uppercase(),
        quota: quota(l, "percent")?,
    })
}

fn quota(obj: &Value, percent_key: &str) -> Option<Quota> {
    let percent = obj.get(percent_key)?.as_f64().filter(|p| p.is_finite())?;
    let resets_at = obj
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc));
    Some(Quota { percent, resets_at })
}

fn clamp_percent(p: f64) -> u8 {
    // In range after the clamp, so the cast cannot truncate.
    p.clamp(0.0, 100.0).round() as u8
}

/// What the dashboard shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Session, weekly and model rows, in that order; absent limits are
    /// left out.
    pub rows: Vec<Row>,
    /// Local time of the fetch these numbers came from.
    pub updated: NaiveDateTime,
    /// The numbers are older than `stale_after`.
    pub stale: bool,
    /// Time until the sign-in expires, when that is soon enough to warn
    /// about (see [`sign_in_warning`]). [`Usage::status`] leaves it `None`;
    /// the app fills it in from the source.
    pub sign_in_left: Option<Span>,
}

/// How long until the sign-in expires at `expires`, if that is within
/// [`SIGN_IN_WARNING`] of `now`. Unknown expiry means no warning; an expiry
/// already passed reads `0m` (the next refresh will fail and say so).
pub fn sign_in_warning(expires: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<Span> {
    let left = expires? - now;
    (left <= SIGN_IN_WARNING).then(|| Span::from_delta(left))
}

/// One limit on the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `SESSION`, `WEEK` or `WEEK OPUS`.
    pub label: String,
    /// Percent used, 0–100.
    pub percent: u8,
    /// When it resets.
    pub reset: Reset,
}

/// When a limit resets, relative to now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    /// In the future: local time and how long until then.
    At {
        /// Local wall-clock time of the reset.
        at: NaiveDateTime,
        /// Time remaining.
        left: Span,
    },
    /// Already happened; waiting for the next fetch.
    Passed {
        /// Local wall-clock time of the reset.
        at: NaiveDateTime,
    },
    /// The API gave no reset time (e.g. an unused limit).
    Unknown,
}

/// A non-negative length of time, shown coarsely: `14m`, `2h 14m`, `3d 4h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    minutes: i64,
}

impl Span {
    /// From whole minutes; negative values become zero.
    pub fn from_minutes(minutes: i64) -> Self {
        Self {
            minutes: minutes.max(0),
        }
    }

    /// From a delta, rounding part minutes up so a reset 30 s away still
    /// reads `1m`, not `0m`.
    pub fn from_delta(d: TimeDelta) -> Self {
        let secs = d.num_seconds().max(0);
        Self::from_minutes((secs + 59) / 60)
    }

    /// Total minutes.
    pub fn minutes(self) -> i64 {
        self.minutes
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (d, h, m) = (
            self.minutes / (24 * 60),
            self.minutes / 60 % 24,
            self.minutes % 60,
        );
        match (d, h) {
            (0, 0) => write!(f, "{m}m"),
            (0, _) => write!(f, "{h}h {m}m"),
            _ => write!(f, "{d}d {h}h"),
        }
    }
}

/// Why there is nothing to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StatusError {
    /// The last good fetch is too old to trust.
    #[error("last update was {age} ago")]
    Stale {
        /// How old the data is.
        age: Span,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Display in UTC+10 (Brisbane: no daylight saving).
    fn brisbane(t: DateTime<Utc>) -> NaiveDateTime {
        t.naive_utc() + TimeDelta::hours(10)
    }

    fn naive(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    const PRO: &str = r#"{
        "five_hour": {"utilization": 62.4, "resets_at": "2026-10-19T04:30:00+00:00"},
        "seven_day": {"utilization": 23.0, "resets_at": "2026-10-22T23:00:00.123+00:00"},
        "seven_day_opus": null,
        "seven_day_sonnet": null,
        "some_new_thing": {"x": 1}
    }"#;

    const NOW: &str = "2026-10-19T02:16:00Z";

    fn status(json: &str, now: &str) -> Status {
        Usage::from_json(json)
            .unwrap()
            .status(t(NOW), t(now), TimeDelta::minutes(15), brisbane)
            .unwrap()
    }

    #[test]
    fn parses_named_limits_and_ignores_unknown_fields() {
        let u = Usage::from_json(PRO).unwrap();
        assert_eq!(
            u.session,
            Some(Quota {
                percent: 62.4,
                resets_at: Some(t("2026-10-19T04:30:00Z"))
            })
        );
        assert_eq!(u.weekly.unwrap().percent, 23.0);
        assert_eq!(u.model, None);
    }

    #[test]
    fn falls_back_to_limits_array() {
        let u = Usage::from_json(
            r#"{"limits": [
                {"kind": "session", "percent": 6, "resets_at": "2026-10-23T11:59:59Z"},
                {"kind": "weekly_all", "percent": 33, "resets_at": null},
                {"kind": "weekly_opus", "percent": 4}
            ]}"#,
        )
        .unwrap();
        assert_eq!(u.session.unwrap().percent, 6.0);
        assert_eq!(u.weekly.unwrap().resets_at, None);
        let m = u.model.unwrap();
        assert_eq!((m.model.as_str(), m.quota.percent), ("OPUS", 4.0));
    }

    #[test]
    fn named_model_limit_wins_and_labels_row() {
        let json = r#"{
            "five_hour": {"utilization": 1, "resets_at": null},
            "seven_day": {"utilization": 2, "resets_at": null},
            "seven_day_sonnet": {"utilization": 7.6, "resets_at": "2026-10-22T23:00:00Z"},
            "limits": [{"kind": "weekly_opus", "percent": 50}]
        }"#;
        let s = status(json, NOW);
        let labels: Vec<_> = s.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["SESSION", "WEEK", "WEEK SONNET"]);
        assert_eq!(s.rows[2].percent, 8);
    }

    #[test]
    fn rejects_garbage_and_missing_limits() {
        assert!(matches!(
            Usage::from_json("<html>"),
            Err(ParseError::NotJson(_))
        ));
        assert_eq!(Usage::from_json("{}"), Err(ParseError::NoLimits));
        assert_eq!(
            Usage::from_json(r#"{"five_hour": {"utilization": "lots"}, "seven_day": null}"#),
            Err(ParseError::NoLimits)
        );
    }

    #[test]
    fn bad_reset_time_is_unknown_not_an_error() {
        let s = status(
            r#"{"five_hour": {"utilization": 5, "resets_at": "soon"}}"#,
            NOW,
        );
        assert_eq!(s.rows.len(), 1);
        assert_eq!(s.rows[0].reset, Reset::Unknown);
    }

    #[test]
    fn reset_times_are_local_with_time_left() {
        let s = status(PRO, NOW);
        assert_eq!(s.rows[0].percent, 62);
        assert_eq!(
            s.rows[0].reset,
            Reset::At {
                at: naive("2026-10-19 14:30"),
                left: Span::from_minutes(2 * 60 + 14)
            }
        );
        let Reset::At { at, left } = s.rows[1].reset else {
            panic!("weekly reset {:?}", s.rows[1].reset)
        };
        assert_eq!(at.format("%a %H:%M").to_string(), "Fri 09:00");
        assert_eq!(left.to_string(), "3d 20h");
        assert_eq!(s.updated, naive("2026-10-19 12:16"));
        assert!(!s.stale);
    }

    #[test]
    fn passed_reset_shows_zero_until_next_fetch() {
        let s = Usage::from_json(PRO)
            .unwrap()
            .status(
                t(NOW),
                t("2026-10-19T04:30:00Z"),
                TimeDelta::hours(1),
                brisbane,
            )
            .unwrap();
        assert_eq!(s.rows[0].percent, 0);
        assert_eq!(
            s.rows[0].reset,
            Reset::Passed {
                at: naive("2026-10-19 14:30")
            }
        );
        assert_eq!(s.rows[1].percent, 23);
    }

    #[test]
    fn percent_is_rounded_and_clamped() {
        assert_eq!(clamp_percent(62.5), 63);
        assert_eq!(clamp_percent(99.6), 100);
        assert_eq!(clamp_percent(130.0), 100);
        assert_eq!(clamp_percent(-3.0), 0);
    }

    #[test]
    fn stale_flag_then_error() {
        let u = Usage::from_json(PRO).unwrap();
        let at = |mins| {
            u.status(
                t(NOW),
                t(NOW) + TimeDelta::minutes(mins),
                TimeDelta::minutes(15),
                brisbane,
            )
        };
        assert!(!at(15).unwrap().stale);
        assert!(at(16).unwrap().stale);
        assert!(at(90).unwrap().stale);
        let err = at(91).unwrap_err();
        assert_eq!(err.to_string(), "last update was 1h 31m ago");
    }

    #[test]
    fn fetch_in_the_future_counts_as_fresh() {
        let u = Usage::from_json(PRO).unwrap();
        let s = u
            .status(
                t(NOW),
                t(NOW) - TimeDelta::hours(3),
                TimeDelta::minutes(15),
                brisbane,
            )
            .unwrap();
        assert!(!s.stale);
    }

    #[test]
    fn sign_in_warning_only_when_close() {
        let now = t(NOW);
        assert_eq!(sign_in_warning(None, now), None);
        assert_eq!(sign_in_warning(Some(now + TimeDelta::days(30)), now), None);
        assert_eq!(
            sign_in_warning(Some(now + TimeDelta::days(5) + TimeDelta::seconds(1)), now),
            None
        );
        assert_eq!(
            sign_in_warning(Some(now + TimeDelta::days(5)), now),
            Some(Span::from_minutes(5 * 24 * 60))
        );
        assert_eq!(
            sign_in_warning(Some(now + TimeDelta::minutes(90)), now).map(|s| s.to_string()),
            Some("1h 30m".to_owned())
        );
        assert_eq!(
            sign_in_warning(Some(now - TimeDelta::hours(1)), now),
            Some(Span::from_minutes(0))
        );
    }

    #[test]
    fn span_formatting_never_negative() {
        let fmt = |m| Span::from_minutes(m).to_string();
        assert_eq!(fmt(0), "0m");
        assert_eq!(fmt(59), "59m");
        assert_eq!(fmt(60), "1h 0m");
        assert_eq!(fmt(134), "2h 14m");
        assert_eq!(fmt(24 * 60), "1d 0h");
        assert_eq!(fmt(6 * 24 * 60 + 23 * 60 + 59), "6d 23h");
        assert_eq!(fmt(-5), "0m");
        assert_eq!(Span::from_delta(TimeDelta::seconds(30)).to_string(), "1m");
        assert_eq!(Span::from_delta(TimeDelta::seconds(-30)).to_string(), "0m");
    }
}
