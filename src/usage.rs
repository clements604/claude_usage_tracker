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

/// Usage credits ("extra usage"): paid usage once the plan's limits are hit,
/// up to a monthly spending cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credits {
    /// Spent so far this month.
    pub used: Money,
    /// The monthly cap.
    pub limit: Money,
    /// Extra credit (for example a promotional credit) that the web page
    /// counts but the API does not report, in the cap's minor units. Zero
    /// unless set with [`Usage::with_credits_bonus`].
    pub bonus: i64,
}

/// An amount of money in minor units (cents), as the API reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Money {
    /// Amount in minor units, e.g. `2590` for 25.90.
    pub minor: i64,
    /// Digits after the decimal point: 2 for cents.
    pub decimals: u32,
    /// ISO 4217 code, e.g. `AUD`; empty if the API gave none.
    pub currency: String,
}

/// Currencies written with a plain `$`. The panel's fonts are ASCII-only,
/// so others (`€`, `£`, `¥`) are written as their code instead.
const DOLLAR_CURRENCIES: [&str; 6] = ["USD", "AUD", "CAD", "NZD", "SGD", "HKD"];

/// Most decimals honoured; more would overflow `10^decimals`.
const MAX_DECIMALS: u32 = 6;

impl fmt::Display for Money {
    /// `$14.10`, `EUR 14.10`, or `14.10` with no currency.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if DOLLAR_CURRENCIES.contains(&self.currency.as_str()) {
            f.write_str("$")?;
        } else if !self.currency.is_empty() {
            write!(f, "{} ", self.currency)?;
        }
        let unit = 10_i64.pow(self.decimals.min(MAX_DECIMALS));
        let (whole, frac) = (self.minor / unit, self.minor % unit);
        match self.decimals.min(MAX_DECIMALS) {
            0 => write!(f, "{whole}"),
            d => write!(f, "{whole}.{frac:0width$}", width = d as usize),
        }
    }
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
    /// Usage credits, when they are turned on and have a monthly cap.
    pub credits: Option<Credits>,
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
    /// Count `cents` (hundredths of the credits' currency) of extra credit
    /// that the API does not report, so the credits row matches the figure on
    /// claude.ai. Does nothing without credits.
    pub fn with_credits_bonus(mut self, cents: i64) -> Self {
        if let Some(c) = &mut self.credits {
            let scaled = i128::from(cents) * 10_i128.pow(c.limit.decimals) / 100;
            c.bonus = i64::try_from(scaled).unwrap_or(0).max(0);
        }
        self
    }

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
        let credits = v
            .get("extra_usage")
            .and_then(extra_usage)
            .or_else(|| v.get("spend").and_then(spend));
        Ok(Self {
            session,
            weekly,
            model,
            credits,
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
                detail: Detail::Reset(reset),
            }
        };

        let mut rows = Vec::with_capacity(4);
        if let Some(q) = &self.session {
            rows.push(row("SESSION".to_owned(), q));
        }
        if let Some(q) = &self.weekly {
            rows.push(row("WEEK".to_owned(), q));
        }
        if let Some(m) = &self.model {
            rows.push(row(format!("WEEK {}", m.model), &m.quota));
        }
        if let Some(c) = &self.credits {
            rows.push(credits_row(c));
        }
        Ok(Status {
            rows,
            updated: local(fetched_at),
            stale: age > stale_after,
            sign_in_left: None,
            host: None,
        })
    }
}

/// `CREDITS LEFT`: what is left of the monthly cap, plus any bonus. Unlike
/// the other rows, `percent` is the share *remaining*, so the bar and the
/// amount agree.
fn credits_row(c: &Credits) -> Row {
    let left = c
        .limit
        .minor
        .saturating_sub(c.used.minor)
        .max(0)
        .saturating_add(c.bonus);
    let total = c.limit.minor.saturating_add(c.bonus);
    // The cap is above zero (checked when parsed); f64 is exact far beyond
    // any real amount.
    let percent = clamp_percent(left as f64 * 100.0 / total.max(1) as f64);
    Row {
        label: "CREDITS LEFT".to_owned(),
        percent,
        detail: Detail::Credits {
            left: Money {
                minor: left,
                ..c.limit.clone()
            },
            limit: Money {
                minor: total,
                ..c.limit.clone()
            },
            bonus: c.bonus > 0,
        },
    }
}

/// `extra_usage`: `{"is_enabled": true, "monthly_limit": 4000,
/// "used_credits": 2590.0, "currency": "AUD", "decimal_places": 2}`.
fn extra_usage(e: &Value) -> Option<Credits> {
    if e.get("is_enabled")?.as_bool() != Some(true) {
        return None;
    }
    let currency = e.get("currency").and_then(Value::as_str).unwrap_or("");
    let decimals = e.get("decimal_places").and_then(Value::as_u64).unwrap_or(2);
    let money = |key| money(e.get(key)?, currency, decimals);
    credits(money("used_credits")?, money("monthly_limit")?)
}

/// `spend`: `{"enabled": true, "used": {"amount_minor": 2590, "currency":
/// "AUD", "exponent": 2}, "limit": {...}}`.
fn spend(s: &Value) -> Option<Credits> {
    if s.get("enabled")?.as_bool() != Some(true) {
        return None;
    }
    let amount = |key: &str| {
        let a = s.get(key)?;
        let currency = a.get("currency").and_then(Value::as_str).unwrap_or("");
        let decimals = a.get("exponent").and_then(Value::as_u64).unwrap_or(2);
        money(a.get("amount_minor")?, currency, decimals)
    };
    credits(amount("used")?, amount("limit")?)
}

/// Credits only make a row with a cap to measure against.
fn credits(used: Money, limit: Money) -> Option<Credits> {
    (limit.minor > 0).then_some(Credits {
        used,
        limit,
        bonus: 0,
    })
}

fn money(minor: &Value, currency: &str, decimals: u64) -> Option<Money> {
    let minor = minor.as_f64().filter(|m| m.is_finite())?;
    Some(Money {
        // Saturating cast; amounts are whole minor units in practice.
        minor: minor.round().max(0.0) as i64,
        decimals: u32::try_from(decimals)
            .unwrap_or(MAX_DECIMALS)
            .min(MAX_DECIMALS),
        currency: currency.to_owned(),
    })
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
    /// Session, weekly, model and credits rows, in that order; absent ones are
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
    /// The device's hostname, shown in the footer so you know which Pi to
    /// SSH into. [`Usage::status`] leaves it `None`; the app fills it in.
    pub host: Option<String>,
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
    /// `SESSION`, `WEEK`, `WEEK OPUS` or `CREDITS LEFT`.
    pub label: String,
    /// Percent used, 0–100 (for `CREDITS LEFT`, percent remaining).
    pub percent: u8,
    /// The line under the bar.
    pub detail: Detail,
}

impl Row {
    /// When the limit resets; `None` for the credits row.
    pub fn reset(&self) -> Option<Reset> {
        match self.detail {
            Detail::Reset(r) => Some(r),
            Detail::Credits { .. } => None,
        }
    }
}

/// What a row says under its bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detail {
    /// A rate limit: when it resets.
    Reset(Reset),
    /// Usage credits: what is left of the monthly cap. The API gives no
    /// date for the cap's reset.
    Credits {
        /// Cap minus spent, never negative.
        left: Money,
        /// The monthly cap, plus any bonus.
        limit: Money,
        /// `limit` and `left` include a bonus the API does not report.
        bonus: bool,
    },
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

    fn money(minor: i64, currency: &str) -> Money {
        Money {
            minor,
            decimals: 2,
            currency: currency.to_owned(),
        }
    }

    #[test]
    fn credits_come_from_extra_usage_in_minor_units() {
        let json = r#"{
            "five_hour": {"utilization": 9, "resets_at": null},
            "extra_usage": {"is_enabled": true, "monthly_limit": 4000, "used_credits": 2590.0,
                            "utilization": 64.75, "currency": "AUD", "decimal_places": 2},
            "spend": {"enabled": true, "used": {"amount_minor": 1, "currency": "USD", "exponent": 2},
                      "limit": {"amount_minor": 2, "currency": "USD", "exponent": 2}}
        }"#;
        let u = Usage::from_json(json).unwrap();
        assert_eq!(
            u.credits,
            Some(Credits {
                used: money(2590, "AUD"),
                limit: money(4000, "AUD"),
                bonus: 0,
            })
        );
        let s = status(json, NOW);
        let row = s.rows.last().unwrap();
        assert_eq!((row.label.as_str(), row.percent), ("CREDITS LEFT", 35));
        assert_eq!(
            row.detail,
            Detail::Credits {
                left: money(1410, "AUD"),
                limit: money(4000, "AUD"),
                bonus: false,
            }
        );
        assert_eq!(row.reset(), None);
    }

    #[test]
    fn credits_bonus_is_added_to_amount_and_cap() {
        let json = r#"{"five_hour": {"utilization": 9}, "extra_usage": {"is_enabled": true,
            "monthly_limit": 4000, "used_credits": 2885, "currency": "AUD", "decimal_places": 2}}"#;
        let usage = Usage::from_json(json).unwrap().with_credits_bonus(1000);
        assert_eq!(usage.credits.as_ref().unwrap().bonus, 1000);
        let s = usage
            .status(t(NOW), t(NOW), TimeDelta::minutes(15), brisbane)
            .unwrap();
        let row = s.rows.last().unwrap();
        // A$11.15 under the cap, plus A$10.00: A$21.15 of A$50.00.
        assert_eq!(row.percent, 42);
        assert_eq!(
            row.detail,
            Detail::Credits {
                left: money(2115, "AUD"),
                limit: money(5000, "AUD"),
                bonus: true,
            }
        );
        // Without credits, or with none left under the cap, it still adds up.
        let none = Usage::from_json(r#"{"five_hour": {"utilization": 9}}"#).unwrap();
        assert_eq!(none.with_credits_bonus(1000).credits, None);
        let over = json.replace("2885", "9999");
        let row = status_of(&over, 1000);
        assert_eq!(row.percent, 20);
    }

    #[test]
    fn credits_bonus_scales_to_the_currency_decimals() {
        let yen = r#"{"five_hour": {"utilization": 9}, "extra_usage": {"is_enabled": true,
            "monthly_limit": 5000, "used_credits": 0, "currency": "JPY", "decimal_places": 0}}"#;
        let c = Usage::from_json(yen).unwrap().with_credits_bonus(1000);
        assert_eq!(c.credits.unwrap().bonus, 10);
    }

    fn status_of(json: &str, bonus_cents: i64) -> Row {
        Usage::from_json(json)
            .unwrap()
            .with_credits_bonus(bonus_cents)
            .status(t(NOW), t(NOW), TimeDelta::minutes(15), brisbane)
            .unwrap()
            .rows
            .pop()
            .unwrap()
    }

    #[test]
    fn credits_fall_back_to_spend() {
        let json = r#"{
            "five_hour": {"utilization": 9, "resets_at": null},
            "extra_usage": null,
            "spend": {"enabled": true, "used": {"amount_minor": 500, "currency": "USD", "exponent": 2},
                      "limit": {"amount_minor": 2000, "currency": "USD", "exponent": 2}}
        }"#;
        let c = Usage::from_json(json).unwrap().credits.unwrap();
        assert_eq!((c.used, c.limit), (money(500, "USD"), money(2000, "USD")));
    }

    #[test]
    fn credits_need_to_be_enabled_with_a_cap() {
        let with = |extra: &str| {
            let json = format!(r#"{{"five_hour": {{"utilization": 9}}, "extra_usage": {extra}}}"#);
            Usage::from_json(&json).unwrap().credits
        };
        // The 2026-10-23 Pro response: turned off, every amount null.
        assert_eq!(
            with(r#"{"is_enabled": false, "monthly_limit": null}"#),
            None
        );
        assert_eq!(
            with(r#"{"is_enabled": true, "monthly_limit": null, "used_credits": 5}"#),
            None
        );
        assert_eq!(
            with(r#"{"is_enabled": true, "monthly_limit": 0, "used_credits": 0}"#),
            None
        );
        // Overspent: 0% left, nothing left, never negative.
        let json = r#"{"five_hour": {"utilization": 9}, "extra_usage": {"is_enabled": true,
            "monthly_limit": 1000, "used_credits": 1234, "currency": "USD"}}"#;
        let row = status(json, NOW).rows.pop().unwrap();
        assert_eq!(row.percent, 0);
        let Detail::Credits { left, .. } = row.detail else {
            panic!("{row:?}")
        };
        assert_eq!(left.to_string(), "$0.00");
    }

    #[test]
    fn money_is_written_in_ascii() {
        assert_eq!(money(1410, "AUD").to_string(), "$14.10");
        assert_eq!(money(5, "USD").to_string(), "$0.05");
        assert_eq!(money(123456, "EUR").to_string(), "EUR 1234.56");
        assert_eq!(money(4000, "").to_string(), "40.00");
        let yen = Money {
            minor: 500,
            decimals: 0,
            currency: "JPY".to_owned(),
        };
        assert_eq!(yen.to_string(), "JPY 500");
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
        assert_eq!(s.rows[0].reset(), Some(Reset::Unknown));
    }

    #[test]
    fn reset_times_are_local_with_time_left() {
        let s = status(PRO, NOW);
        assert_eq!(s.rows[0].percent, 62);
        assert_eq!(
            s.rows[0].reset(),
            Some(Reset::At {
                at: naive("2026-10-19 14:30"),
                left: Span::from_minutes(2 * 60 + 14)
            })
        );
        let Some(Reset::At { at, left }) = s.rows[1].reset() else {
            panic!("weekly reset {:?}", s.rows[1].reset())
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
            s.rows[0].reset(),
            Some(Reset::Passed {
                at: naive("2026-10-19 14:30")
            })
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
