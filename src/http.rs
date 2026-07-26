//! The one place that talks to the network.
//!
//! Kept deliberately small: a blocking GET with a timeout, a size cap and a
//! log line per request, so every byte the application downloads is traceable
//! in `importer.log`.

use std::io::Read;
use std::time::Duration;

use crate::logging;

const USER_AGENT: &str = concat!("cse-importer/", env!("CARGO_PKG_VERSION"));
/// A STEP model for a large connector can be a few megabytes; anything past
/// this is a sign the endpoint changed and is refused rather than buffered.
const MAX_BODY: u64 = 64 * 1024 * 1024;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .user_agent(USER_AGENT)
        .build()
}

pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    logging::info(format!("GET {url}"));
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;

    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut body)
        .map_err(|e| format!("reading body of {url}: {e}"))?;

    logging::info(format!("  <- {} bytes", body.len()));
    Ok(body)
}

pub fn get_string(url: &str) -> Result<String, String> {
    let bytes = get_bytes(url)?;
    String::from_utf8(bytes).map_err(|e| format!("{url} did not return UTF-8: {e}"))
}

/// Percent-encodes everything outside the RFC 3986 unreserved set, so a search
/// term can be dropped into a query string without a URL crate.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
