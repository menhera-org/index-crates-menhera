//! index.crates.menhera.org — a minimum-age-gated proxy for the Cargo sparse-index.
//!
//! `/Nd/<path>` (with N in 0..=30) proxies `https://index.crates.io/<path>` and
//! strips any version lines whose `pubtime` is newer than N days ago. `N = 0`
//! disables filtering entirely (pure pass-through). `config.toml` and non-200
//! responses are passed through unchanged.

use fastly::http::{header, Method, StatusCode};
use fastly::{Error, Request, Response};
use std::time::{SystemTime, UNIX_EPOCH};

const BACKEND: &str = "index_crates_io";
const UPSTREAM_HOST: &str = "index.crates.io";
const SECS_PER_DAY: u64 = 86_400;

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    println!(
        "FASTLY_SERVICE_VERSION: {}",
        std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_default()
    );
    match handle(req) {
        Ok(resp) => Ok(resp),
        Err(err) => {
            eprintln!("handler error: {err:#}");
            Ok(Response::from_status(StatusCode::BAD_GATEWAY)
                .with_body_text_plain("upstream error\n"))
        }
    }
}

fn handle(req: Request) -> Result<Response, Error> {
    let method = req.get_method().clone();
    match method {
        Method::GET | Method::HEAD => {}
        _ => {
            return Ok(Response::from_status(StatusCode::METHOD_NOT_ALLOWED)
                .with_header(header::ALLOW, "GET, HEAD")
                .with_body_text_plain("This method is not allowed\n"));
        }
    }

    let Some((days, rest)) = parse_prefix(req.get_path()) else {
        return Ok(not_found());
    };

    let upstream_url = format!("https://{}/{}", UPSTREAM_HOST, rest);
    let bereq = Request::new(method.clone(), &upstream_url)
        .with_header(header::ACCEPT_ENCODING, "identity");
    let beresp = match bereq.send(BACKEND) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("backend send failed for {upstream_url}: {e:#}");
            return Ok(Response::from_status(StatusCode::BAD_GATEWAY)
                .with_body_text_plain("upstream fetch failed\n"));
        }
    };

    let passthrough = days == 0
        || method == Method::HEAD
        || beresp.get_status() != StatusCode::OK
        || rest.is_empty()
        || rest == "config.toml"
        || response_is_compressed(&beresp);
    if passthrough {
        return Ok(beresp);
    }

    Ok(filter_response(beresp, days))
}

fn response_is_compressed(resp: &Response) -> bool {
    match resp.get_header(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()) {
        None => false,
        Some(enc) => !enc.eq_ignore_ascii_case("identity"),
    }
}

fn not_found() -> Response {
    Response::from_status(StatusCode::NOT_FOUND).with_body_text_plain("Not Found\n")
}

/// Match `/<N>d/<rest>` with 0 <= N <= 30. Returns (N, rest-without-leading-slash).
/// `N = 0` means pass-through (no filtering).
fn parse_prefix(path: &str) -> Option<(u32, &str)> {
    let rest = path.strip_prefix('/')?;
    let (prefix, tail) = rest.split_once('/')?;
    let days_str = prefix.strip_suffix('d')?;
    let days: u32 = days_str.parse().ok()?;
    if days > 30 {
        return None;
    }
    Some((days, tail))
}

fn filter_response(mut beresp: Response, days: u32) -> Response {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cutoff = now_secs.saturating_sub(u64::from(days) * SECS_PER_DAY);

    let body_bytes = beresp.take_body().into_bytes();
    let out = match std::str::from_utf8(&body_bytes) {
        Ok(body) => filter_body(body, cutoff),
        Err(e) => {
            eprintln!("upstream body not UTF-8 ({e}); passing through unfiltered");
            body_bytes
        }
    };

    let mut resp = Response::from_status(StatusCode::OK);
    for name in ["content-type", "etag", "last-modified", "cache-control"] {
        if let Some(v) = beresp.get_header(name) {
            resp.set_header(name, v.clone());
        }
    }
    resp.set_body(out);
    resp
}

fn filter_body(body: &str, cutoff: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            out.extend_from_slice(line.as_bytes());
            continue;
        }
        match line_pubtime_secs(trimmed) {
            Some(secs) if secs > cutoff => {}
            _ => out.extend_from_slice(line.as_bytes()),
        }
    }
    out
}

fn line_pubtime_secs(line: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    parse_rfc3339z(v.get("pubtime")?.as_str()?)
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z` into unix seconds. Fractional seconds are truncated.
fn parse_rfc3339z(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-');
    let y: i32 = dp.next()?.parse().ok()?;
    let mo: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    if dp.next().is_some() {
        return None;
    }
    let mut tp = time.split(':');
    let h: u64 = tp.next()?.parse().ok()?;
    let mi: u64 = tp.next()?.parse().ok()?;
    let sec: u64 = tp.next()?.split('.').next()?.parse().ok()?;
    if tp.next().is_some() || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = days_since_epoch(y, mo, d)?;
    if days < 0 {
        return None;
    }
    Some(days as u64 * SECS_PER_DAY + h * 3_600 + mi * 60 + sec)
}

/// Civil UTC date → days since 1970-01-01. Based on Howard Hinnant's `days_from_civil`.
fn days_since_epoch(y: i32, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let m = m as i64;
    let d = d as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y as i64 - era as i64 * 400) as i64;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era as i64 * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_ok() {
        assert_eq!(parse_prefix("/3d/config.toml"), Some((3, "config.toml")));
        assert_eq!(parse_prefix("/1d/se/rd/serde"), Some((1, "se/rd/serde")));
        assert_eq!(parse_prefix("/30d/"), Some((30, "")));
        assert_eq!(parse_prefix("/0d/se/rd/serde"), Some((0, "se/rd/serde")));
        assert_eq!(parse_prefix("/0d/config.toml"), Some((0, "config.toml")));
    }

    #[test]
    fn prefix_rejects() {
        assert_eq!(parse_prefix("/31d/x"), None);
        assert_eq!(parse_prefix("/3/x"), None);
        assert_eq!(parse_prefix("/d/x"), None);
        assert_eq!(parse_prefix("/3dx/x"), None);
        assert_eq!(parse_prefix("/foo"), None);
        assert_eq!(parse_prefix("/"), None);
    }

    #[test]
    fn rfc3339_epoch() {
        assert_eq!(parse_rfc3339z("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn rfc3339_sample() {
        // 2026-03-20T03:13:45Z
        let got = parse_rfc3339z("2026-03-20T03:13:45Z").unwrap();
        // 2026-03-20 is day 20532 since 1970-01-01 (56y * 365 + 14 leap days + 78 days into 2026).
        assert_eq!(got, 20532 * 86_400 + 3 * 3600 + 13 * 60 + 45);
    }

    #[test]
    fn rfc3339_fractional() {
        assert_eq!(
            parse_rfc3339z("2026-03-20T03:13:45.999Z"),
            parse_rfc3339z("2026-03-20T03:13:45Z"),
        );
    }

    #[test]
    fn line_with_pubtime() {
        let line = r#"{"name":"a","vers":"1","pubtime":"2026-03-20T03:13:45Z"}"#;
        assert_eq!(line_pubtime_secs(line), parse_rfc3339z("2026-03-20T03:13:45Z"));
    }

    #[test]
    fn line_without_pubtime() {
        let line = r#"{"name":"a","vers":"1"}"#;
        assert_eq!(line_pubtime_secs(line), None);
    }
}
