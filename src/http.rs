//! Read-only registry API lookups through `curl` (bundled with Windows 10+,
//! macOS and most Linux), so `ship` needs no HTTP library.

use crate::error::{Code, Result, ShipError};
use crate::log::Logger;
use crate::proc::{self, Cmd};
use std::time::Duration;

pub const USER_AGENT: &str = concat!("shipkit/", env!("CARGO_PKG_VERSION"), " (+https://github.com/acus9345/shipkit)");

fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

/// HTTP status of a GET, following redirects. 429 maps to `E_RATE_LIMITED`.
pub fn status(url: &str, log: &Logger) -> Result<u16> {
    let cmd = Cmd::new(
        "curl",
        &["-sL", "-o", null_device(), "-w", "%{http_code}", "--max-time", "20", "-A", USER_AGENT, url],
    )
    .timeout(Duration::from_secs(30));
    let out = proc::run_ok(&cmd, log)?;
    let code: u16 = out.stdout.trim().parse().unwrap_or(0);
    if code == 429 {
        return Err(ShipError::new(Code::RateLimited, format!("{url} answered 429 (rate limited); try again later")));
    }
    Ok(code)
}

/// True for 200, false for 404, an error for anything else.
pub fn exists(url: &str, log: &Logger) -> Result<bool> {
    match status(url, log)? {
        200 => Ok(true),
        404 => Ok(false),
        code => Err(ShipError::new(Code::CommandFailed, format!("{url} returned HTTP {code}"))),
    }
}
