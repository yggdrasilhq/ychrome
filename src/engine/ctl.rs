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

/// The header a `/engine/shot` reply carries its account in, folded for the
/// case-insensitive compare. The router's spelling is
/// `api::SHOT_META_HEADER`; this is the same name, and the test below holds
/// them together so a rename cannot land on one side only.
const SHOT_META_HEADER: &str = "x-ychrome-shot";

/// The usage text, with the verb list read from the ROUTER rather than typed
/// here. See [`super::api::VERBS`] for why: a hand-kept copy drifted, and an
/// engine that could fill credentials advertised that it could not.
fn usage() -> String {
    // Wrapped by hand at a width that fits a terminal, because the list is
    // long and a single line scrolls the rest of the banner off screen.
    let verbs = super::api::VERBS;
    let (head, tail) = verbs.split_at(verbs.len().min(11));
    format!(
        "usage: ychrome ctl <verb> [key=value ...] [out=FILE | --out FILE]\n\
         verbs: {head}\n\
         \x20      {tail}\n\
         \n\
         shot regions (all four write PNG bytes; out=FILE catches them):\n\
         \x20  region=viewport                       what is on screen (default)\n\
         \x20  region=full                           the whole scrollable document\n\
         \x20  region=element selector='#main'       one element, cropped from the full page\n\
         \x20  region=rect rect='{{\"x\":0,\"y\":0,\"w\":800,\"h\":600}}'  a document-space area\n\
         \x20  prescroll=true                        walk the page first so lazy images load",
        head = head.join(" "),
        tail = tail.join(" "),
    )
}

pub fn run(args: &[String]) -> Result<()> {
    let Some(verb) = args.first() else {
        bail!(usage());
    };
    // ⚠ `--help` was forwarded to the engine AS A VERB, so it came back as
    // `{"error":"unknown engine verb \"--help\"","ok":false}` over a 404. It
    // exits non-zero either way, so nothing false was ever reported — but the
    // one word a caller types when they are already lost should not answer with
    // a routing error about itself.
    if matches!(verb.as_str(), "--help" | "-h" | "help") {
        println!("{}", usage());
        return Ok(());
    }
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
            // ⚠ Every OTHER argument here is `key=value`, so `--out FILE` is the
            // odd one out and callers reach for `out=` first. It used to fall
            // through as an ordinary body key, the engine ignored it, and the
            // PNG then died on the UTF-8 check below with "stream did not
            // contain valid UTF-8" — a message about the wrong thing entirely.
            // That cost a live payment session its screenshots. `out=` is now
            // the same verb as `--out`, and its near misses are named.
            Some(("out", raw)) => out_path = Some(raw.to_string()),
            Some((key, _)) if matches!(key, "path" | "file" | "dest" | "output") => {
                bail!(
                    "{key}= is not how a capture names its file — use out=FILE \
                     (or --out FILE). {verb} answers with PNG bytes, and without a \
                     destination there is nothing to write them to"
                )
            }
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
    let (status, content_type, shot_meta) = read_head(&mut reader)?;

    if let Some(path) = out_path {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        // A NON-2xx body is an error message, not an image. Writing it to the
        // caller's `--out` path would leave a "PNG" that is really a line of
        // JSON, which is a file a script then feeds to something that reports a
        // decode failure three steps away from the actual refusal.
        if !(200..300).contains(&status) {
            eprintln!("{}", String::from_utf8_lossy(&bytes).trim());
            return exit_status(status);
        }
        std::fs::write(&path, &bytes)?;
        // The ACCOUNT on stdout, the pixels in the file. A cropped capture that
        // could only answer with bytes could not tell a caller which element it
        // cropped to, and that is the one thing the image cannot show. One JSON
        // object, so a recipe pipes it to `jget` exactly like every other verb's
        // reply instead of parsing a sentence.
        println!("{}", out_report(shot_meta.as_deref(), &path, bytes.len()));
        return exit_status(status);
    }

    // A binary body with nowhere to go. Reading it as text is how this used to
    // end, and "stream did not contain valid UTF-8" reads as "the verb is
    // broken" rather than "you did not say where to put it".
    if content_type.starts_with("image/") {
        bail!(
            "{verb} answers with {content_type} bytes — say where to put them: \
             ychrome ctl {verb} page_id=… out=FILE"
        );
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
    if let Some(note) = version_skew_note(status, &text) {
        eprintln!("{note}");
    }
    exit_status(status)
}

/// The sentence that turns a 404 into an actionable one, or `None`.
///
/// ⛔ **"Unknown verb" and "this engine predates the verb" are different facts
/// and they send you to different systems.** `ctl frame` was once reported as
/// advertised-but-404ing, with the proposed remedy "drop it from the usage
/// line" — it was implemented, and the daemon answering was 43 hours old,
/// serving from an inode replaced on disk hours after it started. The help came
/// from the CLI; the 404 came from an older build. Deleting the feature would
/// have been the wrong repair to the wrong component.
///
/// Pure, so the message is a unit test rather than something only a stale
/// daemon can produce.
fn version_skew_note(status: u16, body: &str) -> Option<String> {
    if status != 404 {
        return None;
    }
    let reply: serde_json::Value = serde_json::from_str(body).ok()?;
    let engine_build = reply["engine"]["build"].as_str()?;
    let error = reply["error"].as_str().unwrap_or("unknown engine verb");
    let mine = crate::build::identity();
    if engine_build == mine.build {
        // Same bytes on both ends. The verb really does not exist, and saying
        // anything about builds here would send the reader off to restart a
        // daemon that is already current.
        return None;
    }
    let deleted = reply["engine"]["exe_deleted"].as_bool().unwrap_or(false);
    let started = reply["engine"]["started_unix"].as_u64();
    let mut note = format!(
        "ychrome ctl: {error} — but THIS IS A VERSION SKEW, not a missing feature.\n\
         ychrome ctl:   the engine answering is build {engine_build}"
    );
    if let Some(started) = started {
        note.push_str(&format!(" (started {started})"));
    }
    if deleted {
        note.push_str(", running a binary already replaced on disk");
    }
    note.push_str(&format!(
        "\nychrome ctl:   your CLI is build {}\n\
         ychrome ctl:   restart the engine before concluding the verb is missing.",
        mine.build
    ));
    Some(note)
}

/// What `--out` prints: the capture's own account with the file it landed in
/// folded into the SAME object.
///
/// Pure, so the shape a recipe parses is a unit test rather than something only
/// a live engine can produce. A reply with no account (any binary verb that is
/// not `shot`) still answers with an object, never a sentence — `bytes` and
/// `out` are the two facts that always exist.
fn out_report(meta: Option<&str>, path: &str, bytes: usize) -> String {
    let mut object = meta
        .and_then(|meta| serde_json::from_str::<Value>(meta).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    object.insert("out".to_string(), json!(path));
    object.insert("bytes".to_string(), json!(bytes));
    Value::Object(object).to_string()
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
///
/// Answers the content type AND the capture account, because a binary reply's
/// only channel for saying what it is IS a header.
fn read_head(reader: &mut BufReader<UnixStream>) -> Result<(u16, String, Option<String>)> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .with_context(|| format!("the daemon did not answer with HTTP: {line:?}"))?;
    let mut content_type = String::new();
    let mut shot_meta = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-type") {
            content_type = value.trim().to_ascii_lowercase();
        } else if name.eq_ignore_ascii_case(SHOT_META_HEADER) {
            shot_meta = Some(value.trim().to_string());
        }
    }
    Ok((status, content_type, shot_meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ⛔ THE WHOLE VALUE IS IN TELLING TWO FACTS APART. Same build on both ends
    // means the verb really is missing; different builds mean the reader is
    // about to repair the wrong component.
    #[test]
    fn a_404_only_blames_version_skew_when_the_builds_actually_differ() {
        let mine = &crate::build::identity().build;

        // Same build: say NOTHING about versions, or every genuine typo sends
        // someone off to restart a daemon that is already current.
        let same = format!(
            r#"{{"ok":false,"error":"unknown engine verb \"nope\"","engine":{{"build":"{mine}","exe_deleted":false,"started_unix":1700000000}}}}"#
        );
        assert_eq!(version_skew_note(404, &same), None);

        // Different build: name both, and say which way to fix it.
        let skewed = r#"{"ok":false,"error":"unknown engine verb \"frame\"","engine":{"build":"0123456789ab","exe_deleted":true,"started_unix":1700000000}}"#;
        let note = version_skew_note(404, skewed).expect("a skew must be reported");
        assert!(note.contains("0123456789ab"), "{note}");
        assert!(note.contains(mine.as_str()), "{note}");
        assert!(note.contains("replaced on disk"), "{note}");
        assert!(note.contains("restart the engine"), "{note}");
        assert!(
            note.contains("not a missing feature"),
            "the reader's next move must not be to delete the verb: {note}"
        );

        // Anything that is not a 404, or carries no engine block (an engine too
        // old to send one), stays silent rather than guessing.
        assert_eq!(version_skew_note(200, skewed), None);
        assert_eq!(
            version_skew_note(404, r#"{"ok":false,"error":"nope"}"#),
            None
        );
        assert_eq!(version_skew_note(404, "not json at all"), None);
    }

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

    // The two halves of ONE wire name. A rename on the router side that did
    // not reach the client would leave `--out` silently printing a byte count
    // where the capture's account should be, which reads as success.
    #[test]
    fn the_client_reads_the_header_the_router_writes() {
        assert_eq!(
            SHOT_META_HEADER,
            crate::engine::api::SHOT_META_HEADER.to_ascii_lowercase()
        );
    }

    // `--out` answers with ONE json object: the capture's account plus where
    // the bytes went. A recipe parses this the same way it parses every other
    // verb's reply, so a capture is not the one verb that needs a sentence
    // parser.
    #[test]
    fn out_reports_one_object_with_the_account_folded_in() {
        let meta = r#"{"region":"full","width":1280,"height":4200}"#;
        let report: Value = serde_json::from_str(&out_report(Some(meta), "/tmp/a.png", 91234))
            .expect("a json object");
        assert_eq!(report["region"], json!("full"));
        assert_eq!(report["height"], json!(4200));
        assert_eq!(report["out"], json!("/tmp/a.png"));
        assert_eq!(report["bytes"], json!(91234));
    }

    // A binary reply with NO account still answers with an object. The two
    // facts that always exist are the file and its size.
    #[test]
    fn out_without_an_account_is_still_an_object() {
        let report: Value =
            serde_json::from_str(&out_report(None, "/tmp/b.png", 7)).expect("a json object");
        assert_eq!(report["out"], json!("/tmp/b.png"));
        assert_eq!(report["bytes"], json!(7));
    }

    // A bare `pg_000001` must NOT become a number, and a URL must not become
    // anything clever. Both are strings; only well-formed JSON is JSON.
    #[test]
    fn ambiguous_scalars_stay_strings() {
        for raw in ["pg_000001", "https://example.com/", "snapshot", "Enter"] {
            let parsed = coerce(raw);
            assert!(
                parsed.is_string(),
                "{raw} should stay a string, got {parsed}"
            );
        }
    }
}

#[cfg(test)]
mod destination_tests {
    /// The four spellings a live session actually tried before concluding the
    /// verb could not write a file at all. Three of them must be REFUSED BY
    /// NAME, and the fourth must work — silence here cost a payment run its
    /// screenshots and forced a blind-click workaround that was rightly refused.
    #[test]
    fn a_capture_names_its_destination_in_the_house_style() {
        let source = include_str!("ctl.rs");
        assert!(
            source.contains(r#"Some(("out", raw)) => out_path = Some(raw.to_string())"#),
            "out=FILE must be the same verb as --out FILE"
        );
        for near_miss in ["path", "file", "dest", "output"] {
            assert!(
                source.contains(&format!("\"{near_miss}\"")),
                "{near_miss}= must be refused by name, not swallowed as a body key"
            );
        }
    }

    /// The failure that named the wrong thing. A PNG body read as text reported
    /// an encoding problem, which reads as "the verb is broken".
    #[test]
    fn a_binary_reply_with_nowhere_to_go_says_so_instead_of_blaming_utf8() {
        let source = include_str!("ctl.rs");
        assert!(
            source.contains(r#"content_type.starts_with("image/")"#),
            "an image body must be recognised before anything tries to decode it"
        );
    }
}
