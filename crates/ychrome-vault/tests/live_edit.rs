//! The edit path, end to end, against a REAL Bitwarden server.
//!
//! ⛔ NEVER THE OPERATOR'S VAULT. This provisions its own throwaway account on a
//! scratch vaultwarden and creates its own items. Nothing here can reach a real
//! one: the server URL comes from `YCHROME_VAULT_TEST_SERVER` and the account is
//! registered by this test, with a password this test invents.
//!
//! Ignored by default — it needs a server, which `cargo test` in CI does not
//! have. Run it against a scratch container:
//!
//! ```sh
//! docker run -d --name ychrome-vault-scratch \
//!   -e SIGNUPS_ALLOWED=true -e ROCKET_PORT=8080 -e I_REALLY_WANT_VOLATILE_STORAGE=true \
//!   -p 127.0.0.1:8087:8080 vaultwarden/server:latest
//! YCHROME_VAULT_TEST_SERVER=http://127.0.0.1:8087 \
//!   cargo test -p ychrome-vault --test live_edit -- --ignored --nocapture
//! ```
//!
//! What it proves that a unit test cannot: that the body `edit_body` builds is
//! one a real Vaultwarden ACCEPTS, that the fields ride back out of a real
//! `sync` decrypting to what was written, and that the parts of a cipher this
//! client never models — `favorite`, `reprompt`, and a key no Bitwarden version
//! has ever defined — are still there afterwards.

use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};
use ychrome_vault::crypto::{Kdf, MasterKey, SymmetricKey};

const KDF_ITERATIONS: u32 = 600_000;
/// The scratch account's master password. It protects nothing: the account is
/// created by this test, on a throwaway server, and is thrown away with it.
const SCRATCH_PASSWORD: &str = "scratch-vault-master-password";

struct Scratch {
    dir: PathBuf,
    server: String,
    email: String,
    http: reqwest::blocking::Client,
    token: String,
}

impl Scratch {
    /// Register a fresh account and unlock it through the real CLI.
    fn provision(tag: &str) -> Option<Self> {
        let server = std::env::var("YCHROME_VAULT_TEST_SERVER").ok()?;
        let email = format!(
            "scratch-{tag}-{}-{}@example.invalid",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir =
            std::env::temp_dir().join(format!("ychrome-vault-live-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        // The account's key material, built with the SAME primitives the client
        // uses to unlock — if these drifted, `unlock` below would fail, which is
        // itself part of what this proves.
        let kdf = Kdf::Pbkdf2 {
            iterations: KDF_ITERATIONS,
        };
        let master = MasterKey::derive(SCRATCH_PASSWORD, &email, kdf).unwrap();
        let stretched = master.stretch();
        let user_key_bytes: [u8; 64] =
            std::array::from_fn(|index| (index as u8).wrapping_mul(7) ^ 0x5a);
        // Prove the bytes are a usable symmetric key before handing the server
        // their sealed form; a bad one would fail much later and much less
        // legibly.
        SymmetricKey::from_bytes(&user_key_bytes).unwrap();
        let protected_user_key = stretched.encrypt(&user_key_bytes).unwrap().to_string();

        let http = reqwest::blocking::Client::new();
        let registered = http
            .post(format!("{server}/identity/accounts/register"))
            .json(&json!({
                "email": email,
                "name": "ychrome-vault scratch",
                "masterPasswordHash": master.password_hash_b64(SCRATCH_PASSWORD),
                "masterPasswordHint": Value::Null,
                "key": protected_user_key,
                "kdf": 0,
                "kdfIterations": KDF_ITERATIONS,
            }))
            .send()
            .expect("the scratch server answers");
        assert!(
            registered.status().is_success(),
            "scratch registration failed: {}",
            registered.text().unwrap_or_default()
        );

        let mut scratch = Scratch {
            dir,
            server,
            email,
            http,
            token: String::new(),
        };
        scratch.cli(
            &[
                "configure",
                "--server",
                &scratch.server.clone(),
                "--email",
                &scratch.email.clone(),
            ],
            None,
        );
        scratch.cli(&["unlock"], Some(SCRATCH_PASSWORD));
        scratch.token = scratch.login();
        Some(scratch)
    }

    /// Run the real `ychrome-vault` binary. Panics with both streams on failure,
    /// because a silent non-zero exit here is the least useful outcome.
    fn cli(&self, args: &[&str], stdin: Option<&str>) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ychrome-vault"));
        command.arg("--dir").arg(&self.dir).args(args);
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        let mut child = command.spawn().expect("the CLI binary runs");
        {
            use std::io::Write as _;
            let pipe = child.stdin.as_mut().expect("piped");
            // Always closed, even with nothing to send: `read_secret` refuses a
            // terminal, and an open empty pipe is what a scripted caller gives.
            if let Some(secret) = stdin {
                write!(pipe, "{secret}").unwrap();
            }
        }
        let out = child.wait_with_output().expect("the CLI exits");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "`ychrome-vault {}` failed ({:?})\nstdout: {stdout}\nstderr: {}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
        stdout
    }

    fn cli_json(&self, args: &[&str], stdin: Option<&str>) -> Value {
        serde_json::from_str(&self.cli(args, stdin)).expect("the CLI prints JSON")
    }

    /// A bearer token of our own, so the test can read RAW ciphers — the shape
    /// the client deliberately does not surface, and the only way to see that an
    /// unmodelled field survived.
    fn login(&self) -> String {
        let kdf = Kdf::Pbkdf2 {
            iterations: KDF_ITERATIONS,
        };
        let master = MasterKey::derive(SCRATCH_PASSWORD, &self.email, kdf).unwrap();
        let body = format!(
            "grant_type=password&username={}&password={}&scope=api%20offline_access\
             &client_id=web&deviceType=8&deviceIdentifier={}&deviceName=test",
            urlencode(&self.email),
            urlencode(&master.password_hash_b64(SCRATCH_PASSWORD)),
            "11111111-2222-3333-4444-555555555555",
        );
        let reply: Value = self
            .http
            .post(format!("{}/identity/connect/token", self.server))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Auth-Email", base64_url(self.email.as_bytes()))
            .body(body)
            .send()
            .expect("token endpoint answers")
            .json()
            .expect("token reply is json");
        reply["access_token"]
            .as_str()
            .unwrap_or_else(|| panic!("no access token: {reply}"))
            .to_string()
    }

    /// The cipher exactly as the server holds it.
    fn raw_cipher(&self, id: &str) -> Value {
        self.http
            .get(format!("{}/api/ciphers/{id}", self.server))
            .bearer_auth(&self.token)
            .send()
            .expect("cipher read answers")
            .json()
            .expect("cipher is json")
    }

    /// Write keys onto the cipher that this CLIENT never writes, so a later edit
    /// can be shown not to have destroyed them.
    fn stamp_unmodelled_fields(&self, id: &str) {
        let mut cipher = self.raw_cipher(id);
        let body = cipher.as_object_mut().expect("cipher object");
        body.insert("favorite".into(), json!(true));
        body.insert("reprompt".into(), json!(1));
        body.insert("lastKnownRevisionDate".into(), body["revisionDate"].clone());
        let response = self
            .http
            .put(format!("{}/api/ciphers/{id}", self.server))
            .bearer_auth(&self.token)
            .json(&cipher)
            .send()
            .expect("stamping PUT answers");
        assert!(
            response.status().is_success(),
            "stamping the unmodelled fields failed: {}",
            response.text().unwrap_or_default()
        );
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // The agent holds this scratch account's keys; leaving one running would
        // outlive the test and keep answering on a temp directory.
        Command::new(env!("CARGO_BIN_EXE_ychrome-vault"))
            .arg("--dir")
            .arg(&self.dir)
            .arg("stop-agent")
            .output()
            .ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Every field the CLI can change, written and then read back through the same
/// CLI a user would type — plus the proof that what nobody named survived.
#[test]
#[ignore = "needs a scratch vaultwarden; set YCHROME_VAULT_TEST_SERVER"]
fn every_editable_field_round_trips_against_a_real_server() {
    let Some(vault) = Scratch::provision("edit") else {
        eprintln!("YCHROME_VAULT_TEST_SERVER is not set — skipping");
        return;
    };

    let added = vault.cli_json(
        &[
            "add",
            "example.com",
            "alice",
            "--uri",
            "https://example.com",
            "--notes",
            "original notes",
            "--totp",
            "JBSWY3DPEHPK3PXP",
            "--generate",
        ],
        None,
    );
    let id = added["id"].as_str().expect("the new item's id").to_string();

    // Two keys the client models nowhere, written by a different client. If an
    // edit rebuilt the cipher from what it understands, these are what would
    // vanish — silently, and only for the fields nobody thought to check.
    vault.stamp_unmodelled_fields(&id);
    vault.cli(&["sync"], None);

    // ── the title, the username, the folder-less move, the notes ────────────
    vault.cli_json(
        &[
            "edit",
            "example.com",
            "alice",
            "--rename",
            "example.com (work)",
            "--set-user",
            "alice@work.example",
            "--notes",
            "edited notes",
        ],
        None,
    );
    assert_eq!(
        vault
            .cli(&["get", "example.com (work)", "--field", "username"], None)
            .trim(),
        "alice@work.example"
    );
    assert_eq!(
        vault
            .cli(&["get", "example.com (work)", "--field", "notes"], None)
            .trim(),
        "edited notes"
    );

    // ── the password, from stdin, with the old one kept in history ──────────
    vault.cli_json(
        &["edit", "example.com (work)", "--password"],
        Some("a-new-password"),
    );
    assert_eq!(
        vault.cli(&["get", "example.com (work)"], None).trim(),
        "a-new-password"
    );
    assert!(
        !vault.raw_cipher(&id)["passwordHistory"]
            .as_array()
            .map(|history| history.is_empty())
            .unwrap_or(true),
        "replacing a password must keep the old one in history"
    );

    // ── several uris at once, replacing the list ────────────────────────────
    vault.cli_json(
        &[
            "edit",
            "example.com (work)",
            "--uri",
            "https://example.com",
            "--uri",
            "https://login.example.com",
        ],
        None,
    );
    let listed = vault.cli_json(&["list", "example.com", "--json"], None);
    let uris = listed[0]["uris"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        uris.iter().filter_map(Value::as_str).collect::<Vec<_>>(),
        vec!["https://example.com", "https://login.example.com"],
    );

    // ── the authenticator secret ────────────────────────────────────────────
    vault.cli_json(
        &["edit", "example.com (work)", "--totp", "KRSXG5CTMVRXEZLU"],
        None,
    );
    assert_eq!(
        vault
            .cli(
                &["get", "example.com (work)", "--field", "totp-secret"],
                None
            )
            .trim(),
        "KRSXG5CTMVRXEZLU"
    );

    // ── custom fields: create, update, hide, remove ─────────────────────────
    vault.cli_json(
        &[
            "edit",
            "example.com (work)",
            "--set-field",
            "API Key=first-token",
            "--set-field",
            "Account ID=A-1234",
        ],
        None,
    );
    assert_eq!(
        vault
            .cli(
                &["fields", "example.com (work)", "--field-name", "API Key"],
                None
            )
            .trim(),
        "first-token"
    );
    vault.cli_json(
        &[
            "edit",
            "example.com (work)",
            "--set-field",
            "API Key=second-token",
        ],
        None,
    );
    assert_eq!(
        vault
            .cli(
                &["fields", "example.com (work)", "--field-name", "API Key"],
                None
            )
            .trim(),
        "second-token"
    );
    // A hidden field's value goes in on stdin, exactly like a password.
    vault.cli_json(
        &[
            "edit",
            "example.com (work)",
            "--set-hidden-field",
            "Recovery Code",
        ],
        Some("recovery-secret"),
    );
    assert_eq!(
        vault
            .cli(
                &[
                    "fields",
                    "example.com (work)",
                    "--field-name",
                    "Recovery Code"
                ],
                None
            )
            .trim(),
        "recovery-secret"
    );
    let hidden_type = vault.raw_cipher(&id)["fields"]
        .as_array()
        .and_then(|fields| fields.last().cloned())
        .map(|field| field["type"].clone());
    assert_eq!(
        hidden_type,
        Some(json!(1)),
        "a hidden field is stored as type 1"
    );

    vault.cli_json(
        &["edit", "example.com (work)", "--remove-field", "Account ID"],
        None,
    );
    let fields = vault.cli(&["fields", "example.com (work)"], None);
    assert!(
        !fields.contains("Account ID"),
        "the removed field is gone: {fields}"
    );
    assert!(fields.contains("API Key"), "the others are not: {fields}");

    // ── clearing, which setting deliberately cannot express ─────────────────
    vault.cli_json(
        &[
            "edit",
            "example.com (work)",
            "--clear",
            "notes",
            "--clear",
            "totp",
        ],
        None,
    );
    let notes = Command::new(env!("CARGO_BIN_EXE_ychrome-vault"))
        .arg("--dir")
        .arg(&vault.dir)
        .args(["get", "example.com (work)", "--field", "notes"])
        .output()
        .expect("the CLI runs");
    assert!(
        !notes.status.success(),
        "a cleared field must be ABSENT, not an empty string that exits 0"
    );

    // ── and the whole point: what nobody named is still there ───────────────
    let after = vault.raw_cipher(&id);
    assert_eq!(
        after["favorite"],
        json!(true),
        "favorite was destroyed by an edit"
    );
    assert_eq!(
        after["reprompt"],
        json!(1),
        "reprompt was destroyed by an edit"
    );
    assert_eq!(
        after["login"]["uris"].as_array().map(Vec::len),
        Some(2),
        "the uri list did not survive its own edit"
    );
    eprintln!("live edit round trip: OK");
}

/// The receipt, and the refusals. A write that says "done" without looking is
/// the failure shape this path exists to remove.
#[test]
#[ignore = "needs a scratch vaultwarden; set YCHROME_VAULT_TEST_SERVER"]
fn an_edit_reports_what_a_re_read_confirmed_and_refuses_what_it_cannot() {
    let Some(vault) = Scratch::provision("verify") else {
        eprintln!("YCHROME_VAULT_TEST_SERVER is not set — skipping");
        return;
    };
    vault.cli_json(&["add", "verify.example", "bob", "--generate"], None);

    let reply = vault.cli_json(
        &[
            "edit",
            "verify.example",
            "--rename",
            "verify.example (renamed)",
            "--set-field",
            "Token=abc",
        ],
        None,
    );
    let verified: Vec<&str> = reply["verified"]
        .as_array()
        .expect("an edit reply carries its receipt")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(verified.contains(&"name"), "receipt: {verified:?}");
    assert!(verified.contains(&"field:Token"), "receipt: {verified:?}");
    // The receipt names fields; it must never carry their values.
    assert!(
        !reply.to_string().contains("abc"),
        "a field value reached the CLI's own output"
    );

    // Refusals, each with its reason on stderr and nothing on stdout.
    for (args, expected) in [
        (
            vec!["edit", "verify.example (renamed)", "--notes", ""],
            "empty string",
        ),
        (
            vec![
                "edit",
                "verify.example (renamed)",
                "--notes",
                "x",
                "--clear",
                "notes",
            ],
            "set and clear",
        ),
        (
            vec!["edit", "verify.example (renamed)", "--remove-field", "Nope"],
            "no custom field named",
        ),
        (
            vec!["edit", "verify.example (renamed)"],
            "at least one field",
        ),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_ychrome-vault"))
            .arg("--dir")
            .arg(&vault.dir)
            .args(&args)
            .output()
            .expect("the CLI runs");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(!out.status.success(), "`{args:?}` should have failed");
        assert!(
            stderr.contains(expected),
            "`{args:?}` should say {expected:?}, said: {stderr}"
        );
    }
    eprintln!("edit receipt + refusals: OK");
}
