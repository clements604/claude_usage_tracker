//! The real usage source: Claude's OAuth usage endpoint over HTTPS, plus the
//! one-off `login` that gives the Pi its own sign-in.
//!
//! This is the same undocumented endpoint Claude Code's `/usage` reads, signed
//! in through Claude Code's own (public) OAuth client. Access tokens last
//! hours, so they are refreshed here; the refresh token **rotates** on every
//! refresh, so the new pair is written straight back to the credentials file
//! (atomically: temp file, then rename). The Pi must have its own sign-in, not
//! share one with a Mac that is also refreshing it, or each would keep
//! invalidating the other. [`Login`] creates that sign-in on the Pi itself,
//! with the same paste-a-code flow Claude Code uses on headless machines.
//!
//! The file keeps Claude Code's layout, `{"claudeAiOauth": {"accessToken",
//! "refreshToken", "expiresAt" (ms), "refreshTokenExpiresAt" (ms), "scopes",
//! ...}}`, so a copy out of the macOS Keychain works too. Unknown fields are
//! preserved. Without a `refreshToken` the access token is used until it stops
//! working.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, TimeDelta, Utc};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use ureq::Agent;

use crate::source::{SourceError, UsageSource};
use crate::usage::Usage;

/// Usage endpoint.
pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// OAuth token endpoint, for both the login code exchange and refreshes.
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Where the login link points: sign in with a Claude.ai (Pro/Max) account.
pub const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
/// The "manual" redirect: shows the code to paste instead of calling back to
/// a local web server, which a headless Pi does not have.
pub const REDIRECT_URL: &str = "https://platform.claude.com/oauth/code/callback";
/// Claude Code's public OAuth client id; tokens are bound to it.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Scopes `login` asks for: enough to read usage and refresh. A subset of
/// what Claude Code requests (no API-key minting, uploads or MCP).
pub const LOGIN_SCOPES: [&str; 3] = [
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
];
const BETA_HEADER: &str = "oauth-2025-04-20";
const TIMEOUT: Duration = Duration::from_secs(20);
/// Refresh this long before the access token actually expires.
const EXPIRY_MARGIN: TimeDelta = TimeDelta::minutes(5);
const USER_AGENT: &str = concat!("claude-ticker/", env!("CARGO_PKG_VERSION"));

fn agent() -> Agent {
    Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/// [`UsageSource`] backed by the Claude API.
#[derive(Debug)]
pub struct ApiSource {
    path: PathBuf,
    agent: Agent,
    creds: Option<Value>,
    /// Modification time of the file as last read or written; a different
    /// one means someone (e.g. `claude-ticker login`) replaced it.
    seen: Option<SystemTime>,
}

impl ApiSource {
    /// Read credentials from `path` (lazily, on the first fetch, and again
    /// whenever the file changes).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            agent: agent(),
            creds: None,
            seen: None,
        }
    }

    fn oauth(&self) -> Option<&Value> {
        self.creds.as_ref()?.get("claudeAiOauth")
    }

    fn field(&self, key: &str) -> Option<&str> {
        self.oauth()?.get(key)?.as_str()
    }

    fn time_field(&self, key: &str) -> Option<DateTime<Utc>> {
        let ms = self.oauth()?.get(key)?.as_i64()?;
        DateTime::from_timestamp_millis(ms)
    }

    fn modified(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
    }

    fn load(&mut self) -> Result<(), SourceError> {
        let bad = |reason: String| SourceError::Credentials {
            path: self.path.clone(),
            reason,
        };
        let text = std::fs::read_to_string(&self.path).map_err(|e| bad(e.to_string()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| bad(format!("not json: {e}")))?;
        if v.pointer("/claudeAiOauth/accessToken")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(bad("no claudeAiOauth.accessToken".to_owned()));
        }
        self.creds = Some(v);
        self.seen = self.modified();
        Ok(())
    }

    fn expired(&self, now: DateTime<Utc>) -> bool {
        self.time_field("expiresAt")
            .is_some_and(|e| now + EXPIRY_MARGIN >= e)
    }

    /// Swap the refresh token for a new pair and save it.
    fn refresh(&mut self, now: DateTime<Utc>) -> Result<(), SourceError> {
        let Some(refresh_token) = self.field("refreshToken").map(str::to_owned) else {
            return Err(SourceError::Auth(
                "access token expired and there is no refresh token".to_owned(),
            ));
        };
        let mut body = json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        });
        if let Some(scopes) = self
            .oauth()
            .and_then(|o| o.get("scopes"))
            .and_then(Value::as_array)
        {
            let scopes: Vec<&str> = scopes.iter().filter_map(Value::as_str).collect();
            body["scope"] = Value::from(scopes.join(" "));
        }

        let (status, text) = post_json(&self.agent, TOKEN_URL, &body)?;
        match status {
            200 => {}
            400 | 401 | 403 => {
                return Err(SourceError::Auth(format!(
                    "token refresh refused with http {status}"
                )));
            }
            _ => return Err(SourceError::Http(status)),
        }
        let Some(oauth) = self.creds.as_mut().and_then(|c| c.get_mut("claudeAiOauth")) else {
            return Err(SourceError::Auth("credentials not loaded".to_owned()));
        };
        apply_token_response(oauth, &text, now)?;
        self.save()
    }

    fn save(&mut self) -> Result<(), SourceError> {
        let Some(creds) = &self.creds else {
            return Ok(());
        };
        write_atomic(&self.path, creds.to_string().as_bytes()).map_err(|e| SourceError::Save {
            path: self.path.clone(),
            reason: e.to_string(),
        })?;
        self.seen = self.modified();
        Ok(())
    }

    fn get_usage(&self) -> Result<(u16, String), SourceError> {
        let token = self.field("accessToken").unwrap_or_default();
        send(
            self.agent
                .get(USAGE_URL)
                .header("Authorization", &format!("Bearer {token}"))
                .header("anthropic-beta", BETA_HEADER)
                .header("Accept", "application/json")
                .call(),
        )
    }
}

impl UsageSource for ApiSource {
    fn fetch(&mut self, now: DateTime<Utc>) -> Result<Usage, SourceError> {
        if self.creds.is_none() || self.modified() != self.seen {
            self.load()?;
        }
        let mut refreshed = false;
        if self.expired(now) {
            self.refresh(now)?;
            refreshed = true;
        }
        let (mut status, mut text) = self.get_usage()?;
        if status == 401 && !refreshed {
            // Expired early or revoked; one refresh decides which.
            self.refresh(now)?;
            (status, text) = self.get_usage()?;
        }
        match status {
            200 => Ok(Usage::from_json(&text)?),
            401 | 403 => Err(SourceError::Auth(format!(
                "usage api returned http {status}"
            ))),
            _ => Err(SourceError::Http(status)),
        }
    }

    fn sign_in_expires(&self) -> Option<DateTime<Utc>> {
        self.time_field("refreshTokenExpiresAt")
    }
}

/// Why `claude-ticker login` failed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// The OS random number generator failed.
    #[error("cannot get secure random bytes from the os")]
    Random,
    /// The pasted text is not `code#state`.
    #[error(
        "that is not a sign-in code; paste the whole code shown after approving, including the part after #"
    )]
    Malformed,
    /// The code belongs to a different login attempt.
    #[error("that code is from a different sign-in; open the link printed above and try again")]
    WrongState,
    /// The token endpoint rejected the code.
    #[error("sign-in refused (http {0}); the code may have expired, run login again")]
    Refused(u16),
    /// Network trouble, a bad response, or the file could not be written.
    #[error(transparent)]
    Source(#[from] SourceError),
}

/// One login attempt: a link to open, then the code it shows exchanged for
/// tokens (OAuth authorization code with PKCE).
#[derive(Debug)]
pub struct Login {
    verifier: String,
    state: String,
    url: String,
}

impl Login {
    /// Start a login with fresh random PKCE verifier and state.
    pub fn start() -> Result<Self, LoginError> {
        let random = || random_base64url::<32>();
        let (verifier, state) = (random()?, random()?);
        let url = authorize_url(&pkce_challenge(&verifier), &state);
        Ok(Self {
            verifier,
            state,
            url,
        })
    }

    /// The link to open in a browser (any device) to sign in.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Exchange the pasted `code#state` for tokens and save them to `path`
    /// (creating it, or replacing its `claudeAiOauth` section).
    pub fn finish(&self, pasted: &str, path: &Path, now: DateTime<Utc>) -> Result<(), LoginError> {
        let code = parse_pasted(pasted, &self.state)?;
        let body = json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": REDIRECT_URL,
            "client_id": CLIENT_ID,
            "code_verifier": self.verifier,
            "state": self.state,
        });
        let (status, text) = post_json(&agent(), TOKEN_URL, &body)?;
        if status != 200 {
            return Err(LoginError::Refused(status));
        }

        let mut oauth = json!({ "scopes": LOGIN_SCOPES });
        apply_token_response(&mut oauth, &text, now)?;
        // Keep anything else an existing file holds (e.g. from a Keychain copy).
        let mut creds = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        creds["claudeAiOauth"] = oauth;

        let save_err = |e: std::io::Error| SourceError::Save {
            path: path.to_owned(),
            reason: e.to_string(),
        };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            create_private_dir(dir).map_err(save_err)?;
        }
        write_atomic(path, creds.to_string().as_bytes()).map_err(save_err)?;
        Ok(())
    }
}

/// `N` bytes from the OS's secure random number generator, base64url
/// encoded (so `N = 32` gives 43 characters).
pub(crate) fn random_base64url<const N: usize>() -> Result<String, LoginError> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| LoginError::Random)?;
    Ok(base64url(&bytes))
}

/// `code` from a pasted `code#state`, after checking the state matches.
fn parse_pasted<'a>(pasted: &'a str, state: &str) -> Result<&'a str, LoginError> {
    let (code, got) = pasted.trim().split_once('#').ok_or(LoginError::Malformed)?;
    if code.is_empty() || got.is_empty() {
        return Err(LoginError::Malformed);
    }
    if got != state {
        return Err(LoginError::WrongState);
    }
    Ok(code)
}

fn authorize_url(challenge: &str, state: &str) -> String {
    let params = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URL),
        ("scope", &LOGIN_SCOPES.join(" ")),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect();
    format!("{AUTHORIZE_URL}?{}", query.join("&"))
}

/// PKCE S256 challenge for `verifier` (RFC 7636 §4.2).
fn pkce_challenge(verifier: &str) -> String {
    base64url(digest(&SHA256, verifier.as_bytes()).as_ref())
}

/// Unpadded base64url (RFC 4648 §5).
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

/// Percent-encode everything but RFC 3986 unreserved characters.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Copy the tokens and expiry times from a token endpoint response into a
/// `claudeAiOauth` object.
fn apply_token_response(
    oauth: &mut Value,
    text: &str,
    now: DateTime<Utc>,
) -> Result<(), SourceError> {
    let r: Value = serde_json::from_str(text)
        .map_err(|e| SourceError::Network(format!("bad token response: {e}")))?;
    let access = r
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::Network("token response has no access_token".to_owned()))?;
    let at = |secs: i64| {
        now.checked_add_signed(TimeDelta::try_seconds(secs)?)
            .map(|t| Value::from(t.timestamp_millis()))
    };
    oauth["accessToken"] = Value::from(access);
    if let Some(refresh) = r.get("refresh_token").and_then(Value::as_str) {
        oauth["refreshToken"] = Value::from(refresh);
    }
    if let Some(t) = r.get("expires_in").and_then(Value::as_i64).and_then(at) {
        oauth["expiresAt"] = t;
    }
    if let Some(t) = r
        .get("refresh_token_expires_in")
        .and_then(Value::as_i64)
        .and_then(at)
    {
        oauth["refreshTokenExpiresAt"] = t;
    }
    if let Some(scope) = r.get("scope").and_then(Value::as_str) {
        oauth["scopes"] = scope.split_whitespace().collect();
    }
    Ok(())
}

fn post_json(agent: &Agent, url: &str, body: &Value) -> Result<(u16, String), SourceError> {
    send(
        agent
            .post(url)
            .header("Content-Type", "application/json")
            .send(body.to_string()),
    )
}

fn send(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<(u16, String), SourceError> {
    let mut resp = result.map_err(|e| SourceError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| SourceError::Network(e.to_string()))?;
    Ok((status, text))
}

/// Create `dir` (owner-only on Unix) if it does not exist yet.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Write `data` to `path` via a temp file in the same directory and a rename,
/// so a power cut never leaves half a credentials file. Owner-only on Unix.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    // Keep the owner of the file being replaced (or, for a new file, of its
    // directory), so `sudo claude-ticker login` or `--check` does not leave a
    // root-owned file the service's dynamic user cannot read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let parent = path.parent().filter(|d| !d.as_os_str().is_empty());
        let owner = std::fs::metadata(path).or_else(|e| parent.map_or(Err(e), std::fs::metadata));
        if let Ok(meta) = owner {
            // Only root may change owners; for anyone else it already matches.
            let _ = std::os::unix::fs::chown(&tmp, Some(meta.uid()), Some(meta.gid()));
        }
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("claude-ticker-{name}-{}", std::process::id()))
    }

    #[test]
    fn missing_or_bad_credentials_are_reported() {
        let path = temp("missing");
        let _ = std::fs::remove_file(&path);
        let err = ApiSource::new(&path).fetch(Utc::now()).unwrap_err();
        assert!(matches!(err, SourceError::Credentials { .. }), "{err}");
        assert!(err.needs_attention());

        std::fs::write(&path, r#"{"claudeAiOauth": {}}"#).unwrap();
        let err = ApiSource::new(&path).fetch(Utc::now()).unwrap_err();
        assert!(
            err.to_string().contains("no claudeAiOauth.accessToken"),
            "{err}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn expiry_uses_margin_and_tolerates_missing_field() {
        let mut s = ApiSource::new("unused");
        let now = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        s.creds = Some(json!({"claudeAiOauth": {"accessToken": "a"}}));
        assert!(!s.expired(now));
        assert_eq!(s.sign_in_expires(), None);
        let in_ms = |m: i64| 1_800_000_000_000 + m * 60_000;
        s.creds = Some(json!({"claudeAiOauth": {"accessToken": "a", "expiresAt": in_ms(6)}}));
        assert!(!s.expired(now));
        s.creds = Some(json!({"claudeAiOauth": {
            "accessToken": "a", "expiresAt": in_ms(5), "refreshTokenExpiresAt": in_ms(60)
        }}));
        assert!(s.expired(now));
        assert_eq!(s.sign_in_expires(), Some(now + TimeDelta::hours(1)));
    }

    #[test]
    fn token_response_updates_tokens_and_expiries() {
        let now = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        let mut oauth = json!({"accessToken": "old", "refreshToken": "r0", "keep": 1});
        apply_token_response(
            &mut oauth,
            r#"{"access_token": "a1", "refresh_token": "r1", "expires_in": 3600,
                "refresh_token_expires_in": 86400, "scope": "user:profile user:inference"}"#,
            now,
        )
        .unwrap();
        assert_eq!(
            oauth,
            json!({
                "accessToken": "a1",
                "refreshToken": "r1",
                "expiresAt": 1_800_003_600_000_i64,
                "refreshTokenExpiresAt": 1_800_086_400_000_i64,
                "scopes": ["user:profile", "user:inference"],
                "keep": 1
            })
        );
        // Refresh token omitted: keep the old one.
        apply_token_response(&mut oauth, r#"{"access_token": "a2"}"#, now).unwrap();
        assert_eq!(oauth["refreshToken"], "r1");
        assert!(apply_token_response(&mut oauth, r#"{"error": "x"}"#, now).is_err());
    }

    #[test]
    fn reloads_credentials_when_file_changes() {
        let path = temp("reload");
        std::fs::write(&path, r#"{"claudeAiOauth": {"accessToken": "a"}}"#).unwrap();
        let mut s = ApiSource::new(&path);
        s.load().unwrap();
        assert_eq!(s.modified(), s.seen);
        // Make sure the mtime moves even on coarse-timestamp filesystems.
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::write(&path, r#"{"claudeAiOauth": {"accessToken": "b"}}"#).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_ne!(s.modified(), s.seen);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn write_atomic_replaces_file_owner_only() {
        let path = temp("atomic");
        std::fs::write(&path, "old").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn pkce_matches_rfc7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn base64url_vectors() {
        // RFC 4648 §10, unpadded, with the URL-safe alphabet.
        for (input, want) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64url(input.as_bytes()), want);
        }
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn login_url_has_pkce_and_encoded_params() {
        let login = Login::start().unwrap();
        let url = login.url();
        assert!(url.starts_with("https://claude.com/cai/oauth/authorize?code=true&"));
        assert!(url.contains(
            "&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&"
        ));
        assert!(
            url.contains(
                "&scope=user%3Aprofile%20user%3Ainference%20user%3Asessions%3Aclaude_code&"
            )
        );
        assert!(url.contains(&format!(
            "&code_challenge={}&code_challenge_method=S256&state={}",
            pkce_challenge(&login.verifier),
            login.state
        )));
        assert_eq!(login.verifier.len(), 43);
        assert_ne!(login.verifier, login.state);
    }

    #[test]
    fn pasted_code_is_checked() {
        assert_eq!(parse_pasted("  abc#st8\n", "st8").unwrap(), "abc");
        assert!(matches!(
            parse_pasted("abc", "st8"),
            Err(LoginError::Malformed)
        ));
        assert!(matches!(
            parse_pasted("#st8", "st8"),
            Err(LoginError::Malformed)
        ));
        assert!(matches!(
            parse_pasted("abc#other", "st8"),
            Err(LoginError::WrongState)
        ));
    }
}
