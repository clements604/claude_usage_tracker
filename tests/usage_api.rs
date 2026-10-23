//! End-to-end checks of the usage rules (AGENTS.md §3) through the public API.
//!
//! `fixtures/usage-pro.json` is a real response from the usage endpoint for a
//! Pro account, captured 2026-10-23. It pins the shape we depend on; if the
//! endpoint changes, capture a new one and keep the old as a regression case.

use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use claude_ticker::config::Config;
use claude_ticker::framebuffer::Framebuffer;
use claude_ticker::usage::{Reset, Span, Usage};
use claude_ticker::view::{self, Screen};

const PRO: &str = include_str!("fixtures/usage-pro.json");

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// Sydney in October is UTC+11 (daylight saving).
fn sydney(t: DateTime<Utc>) -> NaiveDateTime {
    t.with_timezone(&chrono_tz::Australia::Sydney).naive_local()
}

#[test]
fn real_pro_response_parses() {
    let u = Usage::from_json(PRO).unwrap();
    let session = u.session.unwrap();
    assert_eq!(session.percent, 6.0);
    assert_eq!(
        session.resets_at.unwrap().to_rfc3339(),
        "2026-10-23T11:59:59.726259+00:00"
    );
    assert_eq!(u.weekly.unwrap().percent, 33.0);
    // Pro has no model-specific weekly limit; the unrelated `iguana_necktie`
    // and `extra_usage` objects must not be mistaken for one.
    assert_eq!(u.model, None);
}

#[test]
fn real_pro_response_to_dashboard_rows() {
    let now = t("2026-10-23T09:11:38Z");
    let s = Usage::from_json(PRO)
        .unwrap()
        .status(now, now, TimeDelta::minutes(15), sydney)
        .unwrap();
    let rows: Vec<_> = s
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.percent, r.reset))
        .collect();
    let local = |s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f").unwrap();
    assert_eq!(
        rows,
        [
            (
                "SESSION",
                6,
                Reset::At {
                    at: local("2026-10-23 22:59:59.726259"),
                    left: Span::from_minutes(2 * 60 + 49)
                }
            ),
            (
                "WEEK",
                33,
                Reset::At {
                    at: local("2026-10-24 18:59:59.726281"),
                    left: Span::from_minutes(22 * 60 + 49)
                }
            ),
        ]
    );

    // And it draws.
    let mut fb = Framebuffer::new();
    let Ok(()) = view::draw(&Screen::Dashboard(&s), &mut fb);
    assert!(fb.pixels().any(|p| p.1.is_on()));
}

#[test]
fn limits_array_alone_is_enough() {
    // If the named fields ever disappear, the `limits` array still works.
    let mut v: serde_json::Value = serde_json::from_str(PRO).unwrap();
    for key in ["five_hour", "seven_day"] {
        v.as_object_mut().unwrap().remove(key);
    }
    let u = Usage::from_json(&v.to_string()).unwrap();
    assert_eq!(u.session.unwrap().percent, 6.0);
    assert_eq!(u.weekly.unwrap().percent, 33.0);
}

#[test]
fn shipped_config_matches_defaults() {
    let c = Config::from_toml(include_str!("../deploy/config.toml")).unwrap();
    assert_eq!(c, Config::default());
}
