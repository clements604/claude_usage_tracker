//! "Come back to the computer" notifications.
//!
//! Claude Code can run a command when it stops to ask for approval (its
//! `Notification` hook), so someone who has walked away with the Pi in their
//! pocket can see that they are needed. The command is a `curl` to this
//! listener: `POST /attention` raises the flag, `POST /clear` lowers it, and
//! it lowers itself after a timeout so a missed clear never leaves the icon
//! stuck on.
//!
//! Plain HTTP over `std::net`, one request at a time, like the sign-in page
//! in [`web`](crate::web) (whose request reader it reuses). Unlike that page
//! it runs for as long as the service does, so every request must carry the
//! shared secret from the config as `Authorization: Bearer <token>`; anything
//! else is turned away. The token is compared without early exit and is never
//! logged.

use std::fmt;
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

use crate::source::Attention;
use crate::web::{Request, read_request};

const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the server thread checks whether it should stop.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

type Now = Box<dyn Fn() -> DateTime<Utc> + Send + Sync>;

struct Shared {
    token: Vec<u8>,
    timeout: TimeDelta,
    now: Now,
    /// When attention was last requested; `None` once cleared.
    raised_at: Mutex<Option<DateTime<Utc>>>,
}

/// An [`Attention`] source fed by HTTP requests on the local network.
pub struct NotifyListener {
    shared: Arc<Shared>,
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for NotifyListener {
    // Without the token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotifyListener")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl NotifyListener {
    /// Start listening on `port` (0 picks a free one). Requests must carry
    /// `token`; a raised flag clears itself after `timeout`. `now` must be
    /// trusted time.
    pub fn start(
        port: u16,
        token: &str,
        timeout: Duration,
        now: impl Fn() -> DateTime<Utc> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            token: token.as_bytes().to_vec(),
            timeout: TimeDelta::from_std(timeout).unwrap_or(TimeDelta::MAX),
            now: Box::new(now),
            raised_at: Mutex::new(None),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
            move || serve(&listener, &shared, &stop)
        });
        Ok(Self {
            shared,
            port,
            stop,
            thread: Some(thread),
        })
    }

    /// The port actually bound.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Attention for NotifyListener {
    fn raised(&mut self) -> bool {
        let at = *self
            .shared
            .raised_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        at.is_some_and(|at| (self.shared.now)() - at < self.shared.timeout)
    }
}

impl Drop for NotifyListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            // A panicked server thread has nothing left to clean up.
            let _ = t.join();
        }
    }
}

fn serve(listener: &TcpListener, shared: &Shared, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            // One bad client must not stop the listener; it just gets dropped.
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
    let status = match read_request(&mut stream)? {
        Some(req) => route(&req, shared),
        None => "400 Bad Request",
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{status}\n",
        status.len() + 1
    )?;
    stream.flush()
}

/// Equal contents, without stopping at the first difference, so response
/// timing does not leak how much of a guess was right.
fn same(a: &[u8], b: &[u8]) -> bool {
    let diff = a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y));
    a.len() == b.len() && std::hint::black_box(diff) == 0
}

fn route(req: &Request, shared: &Shared) -> &'static str {
    let presented = req
        .authorization
        .as_deref()
        .and_then(|h| h.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim().as_bytes());
    let allowed = presented.is_some_and(|t| same(t, &shared.token));
    if !allowed {
        return "401 Unauthorized";
    }
    let path = req.path.split('?').next().unwrap_or_default();
    let raised = match (req.method.as_str(), path) {
        ("POST", "/attention") => Some((shared.now)()),
        ("POST", "/clear") => None,
        ("POST", _) | ("GET", _) => return "404 Not Found",
        _ => return "405 Method Not Allowed",
    };
    *shared
        .raised_at
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = raised;
    "200 OK"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::atomic::AtomicI64;

    const TOKEN: &str = "0123456789abcdef-secret";

    /// A clock the test moves by hand, in seconds.
    fn listener(timeout_secs: u64) -> (NotifyListener, Arc<AtomicI64>) {
        let secs = Arc::new(AtomicI64::new(0));
        let clock = Arc::clone(&secs);
        let l = NotifyListener::start(0, TOKEN, Duration::from_secs(timeout_secs), move || {
            DateTime::from_timestamp(1_800_000_000 + clock.load(Ordering::Relaxed), 0)
                .expect("in range")
        })
        .unwrap();
        (l, secs)
    }

    /// Send one raw request and return the status line.
    fn send(l: &NotifyListener, method: &str, path: &str, auth: Option<&str>) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", l.port())).unwrap();
        let auth = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: pi\r\n{auth}Content-Length: 0\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out.lines().next().unwrap_or_default().to_owned()
    }

    fn bearer() -> Option<&'static str> {
        Some("Bearer 0123456789abcdef-secret")
    }

    #[test]
    fn raise_then_clear() {
        let (mut l, _) = listener(1800);
        assert!(!l.raised());
        assert_eq!(send(&l, "POST", "/attention", bearer()), "HTTP/1.1 200 OK");
        assert!(l.raised());
        assert_eq!(send(&l, "POST", "/clear", bearer()), "HTTP/1.1 200 OK");
        assert!(!l.raised());
    }

    #[test]
    fn raised_flag_times_out_and_a_new_request_restarts_it() {
        let (mut l, secs) = listener(600);
        send(&l, "POST", "/attention", bearer());
        secs.store(599, Ordering::Relaxed);
        assert!(l.raised());
        secs.store(600, Ordering::Relaxed);
        assert!(!l.raised());
        send(&l, "POST", "/attention", bearer());
        assert!(l.raised());
    }

    #[test]
    fn wrong_or_missing_token_is_refused_and_changes_nothing() {
        let (mut l, _) = listener(1800);
        for auth in [
            None,
            Some("Bearer nope"),
            Some("Bearer 0123456789abcdef-secre"),
            Some("Basic 0123456789abcdef-secret"),
            Some("0123456789abcdef-secret"),
        ] {
            assert_eq!(
                send(&l, "POST", "/attention", auth),
                "HTTP/1.1 401 Unauthorized",
                "{auth:?}"
            );
        }
        assert!(!l.raised());
        // And a bad clear cannot lower a real request.
        send(&l, "POST", "/attention", bearer());
        send(&l, "POST", "/clear", Some("Bearer nope"));
        assert!(l.raised());
    }

    #[test]
    fn other_paths_and_methods_are_refused() {
        let (mut l, _) = listener(1800);
        assert_eq!(
            send(&l, "POST", "/other", bearer()),
            "HTTP/1.1 404 Not Found"
        );
        assert_eq!(
            send(&l, "GET", "/attention", bearer()),
            "HTTP/1.1 404 Not Found"
        );
        assert_eq!(
            send(&l, "DELETE", "/attention", bearer()),
            "HTTP/1.1 405 Method Not Allowed"
        );
        assert!(!l.raised());
    }

    #[test]
    fn debug_output_has_no_token() {
        let (l, _) = listener(1800);
        assert!(!format!("{l:?}").contains("secret"));
    }
}
