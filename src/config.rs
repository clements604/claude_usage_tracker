//! TOML configuration, validated once at startup.
//!
//! A bad config should fail loudly when the service starts, not hours later on
//! first render, so every field is checked in [`Config::from_toml`] and unknown
//! keys are rejected (a typo like `pol_seconds` must not be silently ignored).
//!
//! ```toml
//! timezone = "Australia/Sydney"  # IANA name; omit to use the OS timezone
//!                                # (top-level keys must come before any [section])
//!
//! [account]
//! credentials = "/var/lib/claude-ticker/credentials.json"   # (the default)
//! sign_in_port = 8080          # phone sign-in page when signed out; 0 = off
//!
//! [display]
//! poll_seconds = 300           # how often to fetch usage; minimum 60  (default 300)
//! stale_minutes = 15           # flag data older than this; ≥ poll     (default 15)
//! full_refresh_every = 12      # partial refreshes between fulls       (default 12)
//! flipped = false              # rotate 180° if the Pi is mounted upside down
//! ```
//!
//! Every key is optional. The timezone decides how reset times are written
//! on screen, so it should be where the display physically is.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono_tz::Tz;
use serde::Deserialize;

/// Default config location on the Pi.
pub const DEFAULT_PATH: &str = "/etc/claude-ticker/config.toml";

/// Default credentials location on the Pi: the service's `StateDirectory`,
/// the only place it can write refreshed tokens back to.
pub const DEFAULT_CREDENTIALS: &str = "/var/lib/claude-ticker/credentials.json";

/// Default port for the phone sign-in page.
pub const DEFAULT_SIGN_IN_PORT: u16 = 8080;

/// Shortest allowed poll interval. Protects the panel from constant
/// refreshes and the API from hammering; do not lower it.
pub const MIN_POLL_SECONDS: u64 = 60;

/// Default poll interval.
pub const DEFAULT_POLL_SECONDS: u64 = 300;

/// Default age after which numbers are flagged as stale.
pub const DEFAULT_STALE_MINUTES: u64 = 15;

/// Validated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Claude Code OAuth credentials file.
    pub credentials: PathBuf,
    /// Port for the phone sign-in page; `None` turns the page off (the QR
    /// code is not shown, only the `claude-ticker login` hint).
    pub sign_in_port: Option<u16>,
    /// How it is shown.
    pub display: DisplaySettings,
    /// Where reset times are shown for; `None` means the OS timezone.
    pub timezone: Option<Tz>,
}

/// Validated display settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySettings {
    /// Time between fetches; at least [`MIN_POLL_SECONDS`].
    pub poll_interval: Duration,
    /// Numbers older than this are flagged; at least `poll_interval`.
    pub stale_after: Duration,
    /// Partial refreshes allowed between full refreshes; at least 1.
    pub full_refresh_every: u32,
    /// Rotate the image 180° (panel mounted upside down).
    pub flipped: bool,
}

/// Why the config could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read config {path}: {source}")]
    Read {
        /// File that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Not valid TOML, or unknown/mistyped keys.
    #[error("invalid config: {0}")]
    Parse(#[from] toml::de::Error),
    /// `poll_seconds` below the minimum.
    #[error("poll_seconds = {0} is too short; use at least {MIN_POLL_SECONDS}")]
    PollTooShort(u64),
    /// `stale_minutes` shorter than one poll, so every frame would be stale.
    #[error("stale_minutes = {stale} is shorter than poll_seconds = {poll}; raise stale_minutes")]
    StaleBeforePoll {
        /// Configured `stale_minutes`.
        stale: u64,
        /// Configured `poll_seconds`.
        poll: u64,
    },
    /// `timezone` is not an IANA zone name.
    #[error(
        "timezone = \"{0}\" is not a known timezone; use an IANA name like \"Australia/Sydney\" or \"Europe/London\""
    )]
    BadTimezone(String),
    /// `sign_in_port` below 1024 needs root, which the service does not have.
    #[error("sign_in_port = {0} needs root; use 1024 or above (or 0 to turn the page off)")]
    PrivilegedPort(u16),
    /// `full_refresh_every` of zero.
    #[error("full_refresh_every must be at least 1")]
    FullRefreshZero,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    timezone: Option<String>,
    #[serde(default)]
    account: RawAccount,
    #[serde(default)]
    display: RawDisplay,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawAccount {
    credentials: PathBuf,
    sign_in_port: u16,
}

impl Default for RawAccount {
    fn default() -> Self {
        Self {
            credentials: PathBuf::from(DEFAULT_CREDENTIALS),
            sign_in_port: DEFAULT_SIGN_IN_PORT,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawDisplay {
    poll_seconds: u64,
    stale_minutes: u64,
    full_refresh_every: u32,
    flipped: bool,
}

impl Default for RawDisplay {
    fn default() -> Self {
        Self {
            poll_seconds: DEFAULT_POLL_SECONDS,
            stale_minutes: DEFAULT_STALE_MINUTES,
            full_refresh_every: 12,
            flipped: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        // Infallible: the defaults satisfy every check.
        Self::from_raw(RawConfig::default()).expect("default config is valid")
    }
}

impl Config {
    /// Read and validate a config file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml(&text)
    }

    /// Parse and validate config text.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        Self::from_raw(toml::from_str(text)?)
    }

    fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let timezone = raw
            .timezone
            .map(|name| {
                name.parse::<Tz>()
                    .map_err(|_| ConfigError::BadTimezone(name))
            })
            .transpose()?;

        let port = raw.account.sign_in_port;
        if (1..1024).contains(&port) {
            return Err(ConfigError::PrivilegedPort(port));
        }

        let d = raw.display;
        if d.poll_seconds < MIN_POLL_SECONDS {
            return Err(ConfigError::PollTooShort(d.poll_seconds));
        }
        if d.stale_minutes.saturating_mul(60) < d.poll_seconds {
            return Err(ConfigError::StaleBeforePoll {
                stale: d.stale_minutes,
                poll: d.poll_seconds,
            });
        }
        if d.full_refresh_every == 0 {
            return Err(ConfigError::FullRefreshZero);
        }

        Ok(Self {
            credentials: raw.account.credentials,
            sign_in_port: (port != 0).then_some(port),
            display: DisplaySettings {
                poll_interval: Duration::from_secs(d.poll_seconds),
                stale_after: Duration::from_secs(d.stale_minutes.saturating_mul(60)),
                full_refresh_every: d.full_refresh_every,
                flipped: d.flipped,
            },
            timezone,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() {
        let c = Config::from_toml("").unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(c.credentials, PathBuf::from(DEFAULT_CREDENTIALS));
        assert_eq!(c.sign_in_port, Some(8080));
        assert_eq!(c.display.poll_interval, Duration::from_secs(300));
        assert_eq!(c.display.stale_after, Duration::from_secs(15 * 60));
        assert_eq!(c.display.full_refresh_every, 12);
        assert!(!c.display.flipped);
        assert_eq!(c.timezone, None);
    }

    #[test]
    fn timezone_is_optional_and_validated() {
        let c = Config::from_toml("timezone = \"Australia/Sydney\"\n").unwrap();
        assert_eq!(c.timezone, Some(chrono_tz::Australia::Sydney));

        let err = Config::from_toml("timezone = \"Australia/Sydny\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::BadTimezone(ref n) if n == "Australia/Sydny"));
    }

    #[test]
    fn shipped_example_config_is_valid() {
        let c = Config::from_toml(include_str!("../deploy/config.toml")).unwrap();
        assert_eq!(c.credentials, PathBuf::from(DEFAULT_CREDENTIALS));
        assert_eq!(c.display, Config::default().display);
    }

    #[test]
    fn full_config() {
        let c = Config::from_toml(
            r#"
            [account]
            credentials = "/home/pi/creds.json"
            sign_in_port = 9000
            [display]
            poll_seconds = 120
            stale_minutes = 10
            full_refresh_every = 5
            flipped = true
            "#,
        )
        .unwrap();
        assert_eq!(c.credentials, PathBuf::from("/home/pi/creds.json"));
        assert_eq!(c.sign_in_port, Some(9000));
        assert_eq!(c.display.poll_interval, Duration::from_secs(120));
        assert_eq!(c.display.stale_after, Duration::from_secs(600));
        assert_eq!(c.display.full_refresh_every, 5);
        assert!(c.display.flipped);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        for bad in [
            "[display]\npol_seconds = 300\n",
            "[account]\ntoken = \"x\"\n",
            "[dating]\nlast_period = \"2026-05-04\"\n",
        ] {
            let err = Config::from_toml(bad).unwrap_err();
            assert!(matches!(err, ConfigError::Parse(_)), "{bad}: {err}");
        }
    }

    #[test]
    fn poll_seconds_minimum() {
        assert_eq!(MIN_POLL_SECONDS, 60);
        let err = Config::from_toml("[display]\npoll_seconds = 59\n").unwrap_err();
        assert!(matches!(err, ConfigError::PollTooShort(59)));
        assert!(Config::from_toml("[display]\npoll_seconds = 60\n").is_ok());
    }

    #[test]
    fn stale_must_cover_at_least_one_poll() {
        let err =
            Config::from_toml("[display]\npoll_seconds = 600\nstale_minutes = 9\n").unwrap_err();
        assert!(matches!(
            err,
            ConfigError::StaleBeforePoll {
                stale: 9,
                poll: 600
            }
        ));
        assert!(Config::from_toml("[display]\npoll_seconds = 600\nstale_minutes = 10\n").is_ok());
    }

    #[test]
    fn sign_in_port_zero_disables_and_low_ports_are_rejected() {
        let c = Config::from_toml("[account]\nsign_in_port = 0\n").unwrap();
        assert_eq!(c.sign_in_port, None);
        let err = Config::from_toml("[account]\nsign_in_port = 80\n").unwrap_err();
        assert!(matches!(err, ConfigError::PrivilegedPort(80)));
        assert!(Config::from_toml("[account]\nsign_in_port = 1024\n").is_ok());
    }

    #[test]
    fn full_refresh_every_zero_is_rejected() {
        let err = Config::from_toml("[display]\nfull_refresh_every = 0\n").unwrap_err();
        assert!(matches!(err, ConfigError::FullRefreshZero));
    }
}
