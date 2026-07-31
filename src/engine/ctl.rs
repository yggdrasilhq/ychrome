//! `ychrome ctl <verb> [key=value ...]` — the engine's thin client (§3).
//!
//! Deliberately thin. §3 says "CLI = thin client" and "every verb is also
//! directly curl-able (agents may skip the CLI)", so this does not grow a
//! bespoke flag set per verb that could drift from the API. It builds a JSON
//! body from `key=value` pairs, POSTs it to `/engine/<verb>` on the daemon
//! socket, and prints what comes back. A new engine verb needs no change here.
//!
//! Values are JSON when they parse as JSON and strings otherwise, so
//! `timeout_ms=5000` is a number, `concurrency=8` is a number,
//! `url=https://example.com/` is a string, and `events=[{"type":"click",...}]`
//! is an array. `--out <file>` catches the one binary reply (`/engine/shot`).
//!
//! The transport is the daemon's unix socket, spawning the daemon if none is
//! running — the same `daemon::ensure` any other ychrome verb uses. There is no
//! token: the socket's `0600` is the authority (§4 as corrected).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// Verbs that stream NDJSON rather than answering with one object.
const STREAMING: [&str; 1] = ["batch"];

pub fn run(args: &[String]) -> Result<()> {
    let Some(verb) = args.first() else {
        bail!(
            "usage: ychrome ctl <verb> [key=value ...] [--out FILE]\n\
             verbs: open close pages goto nav wait eval dom shot input\n\
             \x20      park resume pool metrics budget batch egress identity status"
        );
    };
    let mut body = serde_json::Map::new();
    let mut out_path: Option<String> = None;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--out" {
            out_path = args.get(index + 1).cloned();
            index += 2;
            continue;
        }
        if arg == "--json" {
            index += 1;
            continue;
        }
        match arg.split_once('=') {
            Some((key, raw)) => {
                body.insert(key.to_string(), coerce(raw));
            }
            None => bail!("arguments are key=value pairs; got {arg:?}"),
        }
        index += 1;
    }

    let path = format!("/engine/{verb}");
    let payload = Value::Object(body).to_string();
    let mut stream = connect()?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{payload}",
        len = payload.len(),
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let (status, content_type) = read_head(&mut reader)?;

    if let Some(path) = out_path {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        std::fs::write(&path, &bytes)?;
        println!("{} bytes -> {path}", bytes.len());
        return exit_status(status);
    }

    if content_type.contains("ndjson") || STREAMING.contains(&verb.as_str()) {
        // Print each line AS IT ARRIVES. A streaming verb whose client buffers
        // to the end is a JSON array with extra steps.
        let mut lines = 0;
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            println!("{line}");
            let _ = std::io::stdout().flush();
            lines += 1;
        }
        eprintln!("ychrome ctl: {lines} ndjson lines");
        return exit_status(status);
    }

    let mut text = String::new();
    reader.read_to_string(&mut text)?;
    println!("{}", text.trim());
    exit_status(status)
}

/// A `key=value` argument's value: JSON when it parses as JSON, string
/// otherwise.
///
/// This is what lets the client stay thin — `timeout_ms=5000` is a number and
/// `url=https://…` is a string with no per-key schema to keep in sync with the
/// router. It is a named function so the tests exercise THIS code rather than
/// re-implementing the rule beside it, which is how a lock ends up passing
/// against a broken implementation.
fn coerce(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!(raw))
}

/// A non-2xx engine reply is a non-zero exit, so a shell recipe can `set -e`.
fn exit_status(status: u16) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        bail!("engine replied {status}")
    }
}

fn connect() -> Result<UnixStream> {
    let sock = dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("ychrome")
        .join("daemon.sock");
    if let Ok(stream) = UnixStream::connect(&sock) {
        return Ok(stream);
    }
    // No daemon yet: the engine mounts inside one, so start it the same way
    // every other ychrome verb does rather than inventing a second launcher.
    crate::daemon::ensure().context("starting the ychrome daemon for the engine")?;
    UnixStream::connect(&sock)
        .with_context(|| format!("connecting to the ychrome daemon at {}", sock.display()))
}

/// Read the status line and headers, leaving the reader positioned at the body.
fn read_head(reader: &mut BufReader<UnixStream>) -> Result<(u16, String)> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .with_context(|| format!("the daemon did not answer with HTTP: {line:?}"))?;
    let mut content_type = String::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-type")
        {
            content_type = value.trim().to_ascii_lowercase();
        }
    }
    Ok((status, content_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The value rule is the whole reason this client can stay thin: it must not
    // need a per-verb schema. Numbers must arrive as numbers or `timeout_ms`
    // and `concurrency` silently become strings the router ignores.
    #[test]
    fn key_value_pairs_become_typed_json() {
        let cases = [
            ("timeout_ms=5000", json!(5000)),
            ("concurrency=8", json!(8)),
            ("url=https://example.com/", json!("https://example.com/")),
            ("page_id=pg_000001", json!("pg_000001")),
            (
                r#"events=[{"type":"click","x":1,"y":2}]"#,
                json!([{"type":"click","x":1,"y":2}]),
            ),
            ("mode=snapshot", json!("snapshot")),
        ];
        for (arg, expected) in cases {
            let (_, raw) = arg.split_once('=').expect("a pair");
            assert_eq!(coerce(raw), expected, "{arg} parsed wrong");
        }
    }

    // A bare `pg_000001` must NOT become a number, and a URL must not become
    // anything clever. Both are strings; only well-formed JSON is JSON.
    #[test]
    fn ambiguous_scalars_stay_strings() {
        for raw in ["pg_000001", "https://example.com/", "snapshot", "Enter"] {
            let parsed = coerce(raw);
            assert!(parsed.is_string(), "{raw} should stay a string, got {parsed}");
        }
    }
}
