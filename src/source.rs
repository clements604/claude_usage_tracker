//! Where usage numbers come from, and how a person signs in when they stop.
//!
//! Traits, like [`Clock`](crate::app::Clock), so the app loop can be tested
//! with fakes and the network client (`api`, feature `net`) and sign-in web
//! page (`web`, feature `web`) stay out of the pure layers.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::usage::{ParseError, Usage};

/// Something that can fetch the current plan usage.
pub trait UsageSource {
    /// Fetch usage now. `now` is trusted UTC time, for token expiry checks.
    fn fetch(&mut self, now: DateTime<Utc>) -> Result<Usage, SourceError>;

    /// When the sign-in itself (the refresh token) runs out, if known. After
    /// that a person has to run `claude-ticker login` again.
    fn sign_in_expires(&self) -> Option<DateTime<Utc>> {
        None
    }
}

/// Why a fetch failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The credentials file is missing or unreadable.
    #[error("cannot read credentials {path}: {reason}; sign in with: sudo claude-ticker login")]
    Credentials {
        /// The credentials file.
        path: PathBuf,
        /// What went wrong.
        reason: String,
    },
    /// Refreshed tokens could not be written back.
    #[error(
        "cannot save refreshed credentials to {path}: {reason}; check the directory is writable"
    )]
    Save {
        /// The credentials file.
        path: PathBuf,
        /// What went wrong.
        reason: String,
    },
    /// The account rejected the tokens; a person has to sign in again.
    #[error("signed out ({0}); sign in again with: sudo claude-ticker login")]
    Auth(String),
    /// No connection, DNS or TLS failure, timeout.
    #[error("cannot reach the usage api: {0}; check the wifi")]
    Network(String),
    /// The server answered with an unexpected status.
    #[error("usage api returned http {0}; will retry")]
    Http(u16),
    /// The server answered with something we could not read.
    #[error(transparent)]
    BadResponse(#[from] ParseError),
}

impl SourceError {
    /// Waiting will not fix this one; show it straight away rather than
    /// keeping old numbers on screen.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            Self::Credentials { .. } | Self::Save { .. } | Self::Auth(_)
        )
    }

    /// Only a new sign-in fixes this one (no credentials, or rejected ones).
    pub fn needs_sign_in(&self) -> bool {
        matches!(self, Self::Credentials { .. } | Self::Auth(_))
    }
}

/// A way to sign in from a phone: something serving a page at an address
/// the panel shows as a QR code. Only open while the sign-in screen is up.
pub trait Portal: std::fmt::Debug {
    /// Start serving, if not already, and return the address to show.
    fn open(&mut self) -> Result<String, PortalError>;

    /// Whether a sign-in has completed since this was last asked; a `true`
    /// is reported once.
    fn take_completed(&mut self) -> bool;

    /// Stop serving.
    fn close(&mut self);
}

/// Why the sign-in page could not be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot start the sign-in page: {0}")]
pub struct PortalError(pub String);
