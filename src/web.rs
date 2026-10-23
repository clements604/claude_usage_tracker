//! The phone sign-in page that the QR code on the panel points to.
//!
//! The Pi has no keyboard, and Claude's sign-in can only hand its code back
//! automatically to a server on the *same* device as the browser, which here
//! is the phone. So the panel shows a QR code for a tiny page served by the
//! Pi. That page sends the phone to Claude's sign-in ([`Login`], the same
//! PKCE flow as `claude-ticker login`), and the person pastes the code Claude
//! shows back into it. The page then saves the credentials, and the app sees
//! [`Portal::take_completed`] and switches back to the dashboard.
//!
//! Deliberately small and dependency-free (blocking `std::net`, one request
//! at a time), and only running while the sign-in screen is up. Each opening
//! gets a fresh random path, so only someone who can see the panel can use
//! it, and once a sign-in succeeds the page refuses to start another.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::api::{Login, random_base64url};
use crate::source::{Portal, PortalError};

/// Largest request read (headers plus body); a pasted code is ~100 bytes.
const MAX_REQUEST: usize = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the server thread checks whether it should stop.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

type Now = Box<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// [`Portal`] serving the sign-in page on the local network.
pub struct WebPortal {
    port: u16,
    host: Option<IpAddr>,
    shared: Arc<Shared>,
    running: Option<Running>,
}

struct Shared {
    credentials: PathBuf,
    now: Now,
    session: Mutex<Session>,
}

/// State of one opening of the page.
#[derive(Default)]
struct Session {
    /// The random path segment; requests without it get a 404.
    token: String,
    /// The sign-in started by the most recent page load.
    login: Option<Login>,
    /// Signed in since the app last asked.
    completed: bool,
    /// Signed in during this opening; no further sign-ins.
    signed_in: bool,
}

struct Running {
    url: String,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl fmt::Debug for WebPortal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebPortal")
            .field("port", &self.port)
            .field("credentials", &self.shared.credentials)
            .field("url", &self.running.as_ref().map(|r| &r.url))
            .finish()
    }
}

impl WebPortal {
    /// A portal that saves sign-ins to `credentials`, listening on `port`
    /// (0 picks a free one). `now` must be trusted time, for token expiry.
    pub fn new(
        credentials: impl Into<PathBuf>,
        port: u16,
        now: impl Fn() -> DateTime<Utc> + Send + Sync + 'static,
    ) -> Self {
        Self {
            port,
            host: None,
            shared: Arc::new(Shared {
                credentials: credentials.into(),
                now: Box::new(now),
                session: Mutex::default(),
            }),
            running: None,
        }
    }

    /// Advertise `host` instead of detecting the Pi's network address.
    pub fn with_host(mut self, host: IpAddr) -> Self {
        self.host = Some(host);
        self
    }
}

impl Portal for WebPortal {
    fn open(&mut self) -> Result<String, PortalError> {
        if let Some(r) = &self.running {
            return Ok(r.url.clone());
        }
        let host = match self.host {
            Some(h) => h,
            None => lan_address()?,
        };
        let token = random_base64url::<6>().map_err(|e| PortalError(e.to_string()))?;
        let listener = TcpListener::bind(("0.0.0.0", self.port))
            .and_then(|l| l.set_nonblocking(true).map(|()| l))
            .map_err(|e| PortalError(format!("port {}: {e}", self.port)))?;
        let port = listener
            .local_addr()
            .map_err(|e| PortalError(e.to_string()))?
            .port();

        *lock(&self.shared) = Session {
            token: token.clone(),
            ..Session::default()
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let (shared, stop) = (Arc::clone(&self.shared), Arc::clone(&stop));
            move || serve(&listener, &shared, &stop)
        });
        let url = format!("http://{}/{token}", SocketAddr::new(host, port));
        self.running = Some(Running {
            url: url.clone(),
            stop,
            thread,
        });
        Ok(url)
    }

    fn take_completed(&mut self) -> bool {
        std::mem::take(&mut lock(&self.shared).completed)
    }

    fn close(&mut self) {
        if let Some(r) = self.running.take() {
            r.stop.store(true, Ordering::Relaxed);
            // A panicked server thread has nothing left to clean up.
            let _ = r.thread.join();
        }
    }
}

impl Drop for WebPortal {
    fn drop(&mut self) {
        self.close();
    }
}

fn lock(shared: &Shared) -> MutexGuard<'_, Session> {
    shared
        .session
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The address other devices on the network reach this one at.
fn lan_address() -> Result<IpAddr, PortalError> {
    let no_network = |e: io::Error| PortalError(format!("no network ({e}); check the wifi"));
    let socket = UdpSocket::bind(("0.0.0.0", 0)).map_err(no_network)?;
    // Connecting a UDP socket sends nothing; it just picks the route, and so
    // the local address. 192.0.2.1 is a documentation address (TEST-NET-1).
    socket.connect(("192.0.2.1", 9)).map_err(no_network)?;
    let ip = socket.local_addr().map_err(no_network)?.ip();
    if ip.is_unspecified() || ip.is_loopback() {
        return Err(PortalError("no network; check the wifi".to_owned()));
    }
    Ok(ip)
}

fn serve(listener: &TcpListener, shared: &Shared, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            // One bad client must not stop the page; it just gets dropped.
            Ok((stream, _)) => {
                let _ = handle(stream, shared);
            }
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }
    }
}

fn handle(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let (status, body) = match read_request(&mut stream)? {
        Some(req) => route(&req, shared),
        None => ("400 Bad Request", page("Bad request", "")),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    method: String,
    path: String,
    body: String,
}

/// Read one HTTP/1.1 request; `None` if it is malformed or too big.
fn read_request(stream: &mut impl Read) -> io::Result<Option<Request>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_REQUEST {
            return Ok(None);
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let (Some(method), Some(path)) = (first.next(), first.next()) else {
        return Ok(None);
    };
    let length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_REQUEST {
        return Ok(None);
    }

    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Ok(Some(Request {
        method: method.to_owned(),
        path: path.to_owned(),
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

fn route(req: &Request, shared: &Shared) -> (&'static str, String) {
    let mut session = lock(shared);
    let path = req.path.split('?').next().unwrap_or_default();
    if session.token.is_empty() || path.strip_prefix('/') != Some(session.token.as_str()) {
        return (
            "404 Not Found",
            page(
                "Not found",
                "<p>This link is not valid. Scan the QR code on the display again.</p>",
            ),
        );
    }
    if session.signed_in {
        return ("200 OK", done_page());
    }
    match req.method.as_str() {
        "GET" => match Login::start() {
            Ok(login) => {
                let body = sign_in_page(login.url(), None);
                session.login = Some(login);
                ("200 OK", body)
            }
            Err(e) => (
                "500 Internal Server Error",
                page("Error", &paragraph(&e.to_string())),
            ),
        },
        "POST" => {
            let pasted = form_value(&req.body, "code").unwrap_or_default();
            // The token exchange takes a moment; holding the lock meanwhile
            // just makes a second visitor wait.
            let outcome = session.login.as_ref().map(|login| {
                login
                    .finish(&pasted, &shared.credentials, (shared.now)())
                    .map_err(|e| sign_in_page(login.url(), Some(&e.to_string())))
            });
            match outcome {
                Some(Ok(())) => {
                    session.login = None;
                    session.signed_in = true;
                    session.completed = true;
                    ("200 OK", done_page())
                }
                Some(Err(retry)) => ("200 OK", retry),
                None => (
                    "200 OK",
                    page(
                        "Start again",
                        "<p>This sign-in has expired. <a href=\"\">Start again</a>.</p>",
                    ),
                ),
            }
        }
        _ => ("405 Method Not Allowed", page("Not allowed", "")),
    }
}

fn sign_in_page(auth_url: &str, error: Option<&str>) -> String {
    let error = error.map_or_else(String::new, |e| {
        format!("<p class=\"error\">{}</p>", html_escape(e))
    });
    page(
        "Sign in to Claude",
        &format!(
            "<p>This connects the e-paper display to your Claude account so it can show \
             your plan usage. It only reads usage.</p>\
             {error}\
             <ol>\
             <li><a class=\"button\" href=\"{}\" target=\"_blank\" rel=\"noopener\">Open Claude sign-in</a>\
             <br>Sign in and approve.</li>\
             <li>Copy the code Claude shows you, come back to this tab and paste it here:\
             <form method=\"post\">\
             <input name=\"code\" autocomplete=\"off\" autocapitalize=\"off\" spellcheck=\"false\" \
             placeholder=\"Paste the code\" required>\
             <button>Finish</button></form></li>\
             </ol>",
            html_escape(auth_url)
        ),
    )
}

fn done_page() -> String {
    page(
        "Signed in",
        "<p>Done. The display will show your usage within a few seconds. You can close this page.</p>",
    )
}

fn paragraph(text: &str) -> String {
    format!("<p>{}</p>", html_escape(text))
}

/// A complete page; no external assets, readable on a phone.
fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title} - claude-ticker</title><style>\
         body{{font:17px/1.5 system-ui,sans-serif;max-width:32rem;margin:2rem auto;padding:0 1rem;\
         color:#1a1a1a;background:#fafafa}}\
         li{{margin-bottom:1.2rem}}\
         .button,button{{display:inline-block;padding:.6rem 1rem;border:2px solid #1a1a1a;\
         border-radius:.5rem;background:#1a1a1a;color:#fff;font:inherit;text-decoration:none}}\
         input{{display:block;width:100%;box-sizing:border-box;margin:.5rem 0;padding:.6rem;\
         font:15px ui-monospace,monospace;border:2px solid #888;border-radius:.5rem}}\
         .error{{padding:.6rem;border-left:4px solid #b00020;background:#fdecee}}\
         @media (prefers-color-scheme:dark){{body{{color:#eee;background:#111}}\
         .button,button{{border-color:#eee;background:#eee;color:#111}}}}\
         </style></head><body><h1>{title}</h1>{body}</body></html>"
    )
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// One field of an `application/x-www-form-urlencoded` body.
fn form_value(body: &str, key: &str) -> Option<String> {
    body.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn parses_request_with_body() {
        let raw = b"POST /abc?x=1 HTTP/1.1\r\nHost: pi\r\ncontent-length: 8\r\n\r\ncode=a%23bEXTRA";
        let req = read_request(&mut &raw[..]).unwrap().unwrap();
        assert_eq!(
            req,
            Request {
                method: "POST".into(),
                path: "/abc?x=1".into(),
                body: "code=a%2".into(),
            }
        );
        assert_eq!(read_request(&mut &b"nonsense"[..]).unwrap(), None);
        assert_eq!(read_request(&mut &b"\r\n\r\n"[..]).unwrap(), None);
    }

    #[test]
    fn decodes_form_and_escapes_html() {
        assert_eq!(
            form_value("x=1&code=ab%23cd+e%2", "code").as_deref(),
            Some("ab#cd e%2")
        );
        assert_eq!(form_value("x=1", "code"), None);
        assert_eq!(
            html_escape(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    /// Send a raw request to `url`'s host and return the whole response.
    fn http(url: &str, method: &str, path: &str, body: &str) -> String {
        let addr = url
            .strip_prefix("http://")
            .and_then(|r| r.split('/').next())
            .unwrap();
        let mut s = TcpStream::connect(addr).unwrap();
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn serves_one_time_sign_in_page() {
        let creds = std::env::temp_dir().join(format!("claude-ticker-web-{}", std::process::id()));
        let mut portal =
            WebPortal::new(&creds, 0, Utc::now).with_host(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let url = portal.open().unwrap();
        assert_eq!(portal.open().unwrap(), url, "open is idempotent");
        let token_path = &url[url.rfind('/').unwrap()..];
        assert_eq!(token_path.len(), 1 + 8);

        assert!(http(&url, "GET", "/", "").starts_with("HTTP/1.1 404"));
        assert!(http(&url, "GET", "/wrong", "").starts_with("HTTP/1.1 404"));

        let page = http(&url, "GET", token_path, "");
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("href=\"https://claude.com/cai/oauth/authorize?code=true&amp;"));

        // Mistakes are caught before anything is sent to Claude.
        let page = http(&url, "POST", token_path, "code=garbage");
        assert!(page.contains("not a sign-in code"), "{page}");
        let page = http(&url, "POST", token_path, "code=abc%23not-the-state");
        assert!(page.contains("from a different sign-in"), "{page}");
        assert!(!portal.take_completed());
        assert!(!creds.exists());

        // Closing stops the server; reopening gets a new address.
        portal.close();
        assert!(
            TcpStream::connect(
                url.strip_prefix("http://")
                    .unwrap()
                    .split('/')
                    .next()
                    .unwrap()
            )
            .is_err()
        );
        let again = portal.open().unwrap();
        assert_ne!(again, url);
    }
}
