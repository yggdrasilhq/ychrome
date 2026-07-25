//! What the CLI puts on which stream, and what it exits with.
//!
//! `main.rs` has no unit tests and cannot easily have them — its whole job is
//! stdout, stderr and the exit code, which only exist in a real process. So the
//! real binary is run against a FAKE agent on a temp `--dir`: the reply is
//! whatever this test says it is, no vault is unlocked, and no network or real
//! socket is touched.
//!
//! The property under test is the one a script depends on: **a value that is
//! not there must fail, not print an empty line and succeed.** A stored empty
//! string is a different thing and must still succeed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

/// A fake agent on `dir/agent.sock` that answers every request with the same
/// line. Detached: the listener lives as long as the test binary, which is the
/// simplest thing that cannot race the CLI's connect.
fn fake_agent(dir: &Path, reply: String) {
    let listener = UnixListener::bind(dir.join("agent.sock")).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            // A bare liveness probe connects and closes without asking anything.
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let mut writer = stream;
            writeln!(writer, "{reply}").ok();
            writer.flush().ok();
        }
    });
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run the real `ychrome-vault` against a fake agent that answers `reply`.
fn run(tag: &str, reply: &str, args: &[&str]) -> Run {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "ychrome-vault-cli-test-{tag}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    fake_agent(&dir, reply.to_string());

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ychrome-vault"))
        .arg("--dir")
        .arg(&dir)
        .args(args)
        .output()
        .expect("the CLI binary runs");
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

// THE bug: `get ITEM --field username` on an item that has a password but no
// username printed "\n" and exited 0. `USER=$(...)` captured "" and reported
// success, so a script could not tell "absent" from "empty".
#[test]
fn an_absent_field_fails_instead_of_printing_a_blank_line() {
    let out = run(
        "absent-user",
        r#"{"ok":true,"entry":{"name":"x","username":null,"password":"p"}}"#,
        &["get", "x", "--field", "username"],
    );
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, "", "nothing may reach stdout");
    assert!(out.stderr.contains("has no username"), "{}", out.stderr);
}

// The other half of the same rule: a value the item really stores as an empty
// string is NOT absent. It prints (as an empty line) and succeeds, so the fix
// above cannot be "fail whenever the output would be empty".
#[test]
fn a_stored_empty_string_still_succeeds() {
    let out = run(
        "empty-user",
        r#"{"ok":true,"entry":{"name":"x","username":"","password":"p"}}"#,
        &["get", "x", "--field", "username"],
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, "\n");

    let out = run(
        "real-user",
        r#"{"ok":true,"entry":{"name":"x","username":"octocat","password":"p"}}"#,
        &["get", "x", "--field", "username"],
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, "octocat\n");
    assert_eq!(out.stderr, "");
}

// Every `get` arm reads through the same owner, so a reply missing the key it
// promised fails the same way whichever field was asked for.
#[test]
fn every_get_field_refuses_a_null_value() {
    for (tag, reply, field, wanted) in [
        (
            "null-notes",
            r#"{"ok":true,"notes":null}"#,
            "notes",
            "notes",
        ),
        (
            "null-code",
            r#"{"ok":true,"code":null,"name":"x"}"#,
            "totp",
            "code",
        ),
        (
            "null-secret",
            r#"{"ok":true,"totp_secret":null,"name":"x"}"#,
            "totp-secret",
            // snake_case on the wire, kebab-case in the message the user reads.
            "totp-secret",
        ),
        (
            "null-password",
            r#"{"ok":true,"entry":{"name":"x","username":"u"}}"#,
            "password",
            "password",
        ),
    ] {
        let out = run(tag, reply, &["get", "x", "--field", field]);
        assert_eq!(out.code, Some(1), "{field}: stderr {}", out.stderr);
        assert_eq!(out.stdout, "", "{field} wrote to stdout");
        assert!(
            out.stderr.contains(&format!("has no {wanted}")),
            "{field}: {}",
            out.stderr
        );
    }
}

// A LINKED custom field has no stored value of its own. `fields --field-name`
// printed an empty line and exited 0, which reads to a script as "the field is
// blank" rather than "this field cannot be read".
#[test]
fn a_linked_custom_field_refuses_rather_than_printing_empty() {
    let out = run(
        "linked",
        r#"{"ok":true,"fields":[{"name":"Card","value":null}],"raw_field_count":1}"#,
        &["fields", "x", "--field-name", "Card"],
    );
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, "");
    assert!(out.stderr.contains("linked field"), "{}", out.stderr);

    // A field that really holds a value still prints it, unadorned.
    let out = run(
        "linked-ok",
        r#"{"ok":true,"fields":[{"name":"Card","value":"1234"}],"raw_field_count":1}"#,
        &["fields", "x", "--field-name", "Card"],
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, "1234\n");
}

// The `--field` whitelist has ONE owner now (the `GetField` enum), so the help
// text, the accepted values and the error message cannot disagree the way they
// did: the flag's doc named four fields, the match accepted five, and the error
// named a different four. `totp-secret` was the field that fell through the gap.
#[test]
fn the_field_whitelist_names_every_field_it_accepts() {
    let accepted = run(
        "totp-secret",
        r#"{"ok":true,"totp_secret":"deadbeef","name":"x"}"#,
        &["get", "x", "--field", "totp-secret"],
    );
    assert_eq!(accepted.code, Some(0), "stderr: {}", accepted.stderr);
    assert_eq!(accepted.stdout, "deadbeef\n");

    let rejected = run("bogus", r#"{"ok":true}"#, &["get", "x", "--field", "bogus"]);
    assert_ne!(rejected.code, Some(0));
    assert_eq!(rejected.stdout, "");
    for field in ["password", "username", "totp", "totp-secret", "notes"] {
        assert!(
            rejected.stderr.contains(field),
            "the refusal must name {field}: {}",
            rejected.stderr
        );
    }
}

// The agent's own refusals were already on stderr with a non-zero exit — a
// project note claimed otherwise. Pin it, so nobody "fixes" errors onto stdout
// and silently breaks every `$(ychrome-vault get ...)` in a script.
#[test]
fn an_agent_error_stays_on_stderr_with_a_non_zero_exit() {
    let out = run(
        "agent-error",
        r#"{"ok":false,"error":"HDFC Card has no password"}"#,
        &["get", "x"],
    );
    assert_eq!(out.code, Some(1));
    assert_eq!(out.stdout, "", "an error must never reach stdout");
    assert!(out.stderr.contains("has no password"), "{}", out.stderr);
}
