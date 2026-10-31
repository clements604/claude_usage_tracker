//! `claude-ticker [--check | --raw | login] [CONFIG]` — load the config and
//! run the display.
//!
//! `--raw` fetches once and prints the usage API's JSON body as received, so
//! a new response shape can be captured as a test fixture.
//!
//! `login` signs the Pi in to Claude: it prints a link to open on any device,
//! reads back the code shown after approving, and saves the credentials where
//! the config says. Run it with sudo so it can write the service's state
//! directory.
//!
//! Built with `--features epd` on Linux this drives the e-paper HAT forever
//! (it is started at boot by `deploy/claude-ticker.service`). `--check`, or any
//! build without the HAT driver, validates the config, fetches usage once (if
//! built with `net`) and prints it; use the `simulator` example for a live view.

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use claude_ticker::config::{self, Config};

fn main() -> ExitCode {
    let (mut check, mut raw, mut login) = (false, false, false);
    let mut path = PathBuf::from(config::DEFAULT_PATH);
    for arg in std::env::args_os().skip(1) {
        if arg == "--check" {
            check = true;
        } else if arg == "--raw" {
            raw = true;
        } else if arg == "login" {
            login = true;
        } else {
            path = PathBuf::from(arg);
        }
    }

    let result = Config::load(&path).map_err(Into::into).and_then(|config| {
        if login {
            login_then_check(&config)
        } else if raw {
            print_raw(&config)
        } else if check {
            print_status(&config)
        } else {
            run(config)
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("claude-ticker: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(all(feature = "epd", target_os = "linux"))]
fn run(config: Config) -> Result<(), Box<dyn Error>> {
    use claude_ticker::api::ApiSource;
    use claude_ticker::app::{App, AppSettings, Clock, SystemClock, TIMESYNC_FLAG};
    use claude_ticker::epd::EpdPanel;
    use claude_ticker::web::WebPortal;

    // The Pi may have just rebooted with a stale clock: wait for NTP.
    let clock = SystemClock::new(config.timezone).require_sync_flag(TIMESYNC_FLAG);
    let panel = EpdPanel::new(config.display.flipped)?;
    let source = ApiSource::new(&config.credentials);
    let mut app = App::new(source, AppSettings::from(&config.display), panel);
    // Shown in the footer, so you know which Pi to SSH into.
    if let Ok(host) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        app = app.with_host(host.trim());
    }
    if let Some(port) = config.sign_in_port {
        // The portal only opens once the clock is trusted, so `now` is too.
        let page_clock = clock.clone();
        let portal = WebPortal::new(&config.credentials, port, move || page_clock.now());
        app = app.with_portal(Box::new(portal));
    }
    app.run(&clock)?;
    Ok(())
}

#[cfg(not(all(feature = "epd", target_os = "linux")))]
fn run(config: Config) -> Result<(), Box<dyn Error>> {
    print_status(&config)
}

/// Interactive sign-in, then one fetch to prove it works.
#[cfg(feature = "net")]
fn login_then_check(config: &Config) -> Result<(), Box<dyn Error>> {
    use std::io::Write;

    use claude_ticker::api::Login;
    use claude_ticker::app::{Clock, SystemClock};

    let login = Login::start()?;
    println!("Open this link on any device and sign in to your Claude account:\n");
    println!("{}\n", login.url());
    print!("Then paste the code shown after you approve: ");
    std::io::stdout().flush()?;
    let mut pasted = String::new();
    std::io::stdin().read_line(&mut pasted)?;

    let now = SystemClock::new(config.timezone).now();
    login.finish(&pasted, &config.credentials, now)?;
    println!("\nsaved {}\n", config.credentials.display());
    print_status(config)?;
    println!("the display picks up the new sign-in at its next update");
    Ok(())
}

#[cfg(not(feature = "net"))]
fn login_then_check(_config: &Config) -> Result<(), Box<dyn Error>> {
    Err("built without the `net` feature, so it cannot sign in".into())
}

/// Fetch once and print each limit, e.g. `SESSION  62%  resets 14:30 (in 2h 14m)`.
#[cfg(feature = "net")]
fn print_status(config: &Config) -> Result<(), Box<dyn Error>> {
    use chrono::TimeDelta;
    use claude_ticker::api::ApiSource;
    use claude_ticker::app::{Clock, SystemClock};
    use claude_ticker::source::UsageSource;
    use claude_ticker::usage::{Detail, Reset};
    use claude_ticker::view::TIME_FORMAT;

    let clock = SystemClock::new(config.timezone);
    let now = clock.now();
    let mut source = ApiSource::new(&config.credentials);
    let usage = source.fetch(now)?;
    let status = usage.status(now, now, TimeDelta::MAX, |t| clock.local(t))?;
    for row in &status.rows {
        let detail = match &row.detail {
            Detail::Reset(Reset::At { at, left }) => {
                format!("resets {} (in {left})", at.format("%a %H:%M"))
            }
            Detail::Reset(Reset::Passed { at }) => format!("reset at {}", at.format(TIME_FORMAT)),
            Detail::Reset(Reset::Unknown) => "no reset time".to_owned(),
            Detail::Credits { left, limit } => format!("{left} left of {limit} this month"),
        };
        println!("{:<12} {:>3}%  {detail}", row.label, row.percent);
    }
    let zone = config
        .timezone
        .map_or("OS timezone".to_owned(), |tz| tz.to_string());
    println!(
        "credentials {} ok; times in {zone}",
        config.credentials.display()
    );
    Ok(())
}

/// Fetch once and print the usage API's JSON body unparsed.
#[cfg(feature = "net")]
fn print_raw(config: &Config) -> Result<(), Box<dyn Error>> {
    use claude_ticker::api::ApiSource;
    use claude_ticker::app::{Clock, SystemClock};

    let now = SystemClock::new(config.timezone).now();
    println!("{}", ApiSource::new(&config.credentials).fetch_raw(now)?);
    Ok(())
}

#[cfg(not(feature = "net"))]
fn print_raw(_config: &Config) -> Result<(), Box<dyn Error>> {
    Err("built without the `net` feature, so it cannot fetch".into())
}

/// Without the network client there is nothing to fetch; the config parsed.
#[cfg(not(feature = "net"))]
fn print_status(config: &Config) -> Result<(), Box<dyn Error>> {
    println!(
        "config ok (credentials {}); built without the `net` feature, so usage was not fetched",
        config.credentials.display()
    );
    Ok(())
}
