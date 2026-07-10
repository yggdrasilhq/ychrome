//! ychrome's WebAuthn passkey signer — the browser half of the ceremony.
//!
//! WebKitGTK has no WebAuthn, so ychrome answers `navigator.credentials.get()`
//! (and `.create()`) from the vault, exactly as the Chrome Bitwarden extension
//! does. The signing crypto and the consent type live in `ychrome-vault`
//! (`fido2.rs`, `Vault::fido2_assert`, proven by KATs); THIS module is the
//! browser-side orchestration that turns a page ceremony into a vault signature
//! with a real human in the loop.
//!
//! ```text
//! page  --navigator.credentials.get()-->  shim (our userscript)
//! shim  --POST /fido2/get (SOCKS-loopback, bearer token)-->  Signer (this file)
//! Signer --OSC 7717 ; fido2 ; request-->  yggterm GUI       (rpId + account)
//! yggterm --native presence dialog-->  user clicks Approve
//! yggterm --POST /fido2/grant (ssh -L)-->  Signer            (request_id)
//! Signer --agent fido2-assert-->  ychrome-vault agent        (mints UserPresence, signs)
//! Signer --assertion-->  shim  --PublicKeyCredential-->  page
//! ```
//!
//! **Where consent lives.** The `UserPresence` that authorizes a signature is
//! minted in the `ychrome-vault` agent — but only when THIS module calls its
//! `fido2-assert` op, which it does exclusively after the GUI dialog's grant for
//! that exact `request_id` arrives. The page can only *trigger* a ceremony (it
//! cannot reach the grant channel: the `request_id` is never exposed to it, and
//! `/fido2/grant` is a GUI→app call over `ssh -L`, not page-reachable over
//! SOCKS). An agent's `dom-eval` cannot forge the dialog's Approve (`isTrusted`).
//! On a single-uid host the socket cannot distinguish the GUI from another
//! same-uid process, exactly as the vault's existing `get` op (which already
//! returns a plaintext password) cannot — so passkeys are no weaker than the
//! rest of the vault, and the human-facing gate is the dialog.
//!
//! **A secret never crosses into yggterm.** The OSC carries only rpId + a
//! display label. The private key is decrypted, used once and zeroized inside
//! the agent; the assertion (public bytes) is what reaches the page.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// How long a `/fido2/get` blocks for the user to approve before giving up. A
/// ceremony the user ignores must not pin a control-server thread forever.
const CEREMONY_TIMEOUT: Duration = Duration::from_secs(120);

/// What the GUI dialog delivered for a pending ceremony.
enum Outcome {
    Granted { user_verified: bool },
    Denied,
}

/// One in-flight ceremony, awaiting the GUI grant. The `/fido2/get` thread
/// parks on the [`Signer`] condvar until `outcome` is set by `/fido2/grant` or
/// `/fido2/deny` (or the timeout fires and the entry is swept).
#[derive(Default)]
struct Ceremony {
    outcome: Option<Outcome>,
}

/// The browser-side passkey signer. One per surface control server.
pub struct Signer {
    /// Bearer token the shim presents on every `/fido2/*` request. Same-uid
    /// processes could reach the loopback port; the token stops a random one
    /// from summoning a presence dialog (a phishing/annoyance vector). It is NOT
    /// a cross-page secret — every page in the profile gets the shim, and
    /// cross-page safety is the origin↔rpId check, not the token.
    pub token: String,
    /// The control-server port the page fetches: `127.0.0.1:<port>`, reached
    /// over the surface's SOCKS-loopback (remote) or plain loopback (local).
    port: u16,
    /// The emitting session's `YGGTERM_SESSION_ID`. Diagnostic only — the GUI
    /// routes the OSC by the STREAM it arrived on, not this field.
    session: String,
    pending: Mutex<HashMap<String, Ceremony>>,
    cvar: Condvar,
}

impl Signer {
    pub fn new(port: u16, session: String) -> Arc<Self> {
        Arc::new(Signer {
            token: hex_token(32),
            port,
            session,
            pending: Mutex::new(HashMap::new()),
            cvar: Condvar::new(),
        })
    }

    /// The `navigator.credentials` shim, ready to serve as a userscript, with
    /// the control port and bearer token baked in. Prepended to the profile's
    /// userscripts so it injects at document-start in every surface.
    pub fn shim_userscript(&self) -> String {
        shim_js(self.port, &self.token)
    }

    /// Bearer-token check for every `/fido2/*` route. A request without the
    /// exact token is refused before it can touch the vault or the GUI.
    pub fn authorized(&self, header_token: Option<&str>) -> bool {
        header_token == Some(self.token.as_str())
    }

    /// `POST /fido2/get` — a `navigator.credentials.get()` ceremony. Blocks up
    /// to [`CEREMONY_TIMEOUT`] for the GUI grant, then signs. Returns the HTTP
    /// status and the JSON body the shim turns into a `PublicKeyCredential`.
    pub fn handle_get(&self, body: &Value) -> (u16, Value) {
        match self.try_get(body) {
            Ok(response) => (200, response),
            Err(GetError::NoCredential) => (
                404,
                json!({ "error": "no passkey in this vault answers that request" }),
            ),
            Err(GetError::Denied) => (403, json!({ "error": "the user declined" })),
            Err(GetError::TimedOut) => (
                408,
                json!({ "error": "the user did not respond in time" }),
            ),
            Err(GetError::Bad(message)) => (400, json!({ "error": message })),
        }
    }

    fn try_get(&self, body: &Value) -> Result<Value, GetError> {
        let rp_id = body
            .get("rpId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| GetError::Bad("get needs an rpId".into()))?;
        let origin = body
            .get("origin")
            .and_then(Value::as_str)
            .ok_or_else(|| GetError::Bad("get needs the page origin".into()))?;
        // The page cannot forge `window.location.origin`; still, re-check that
        // the rpId is a registrable-domain suffix of it, so a page can only ask
        // for its own site's passkeys. The RP re-checks the rpIdHash anyway.
        if !rp_id_matches_origin(rp_id, origin) {
            return Err(GetError::Bad(format!(
                "rpId {rp_id:?} is not valid for origin {origin:?}"
            )));
        }
        let challenge = body
            .get("challenge")
            .and_then(Value::as_str)
            .ok_or_else(|| GetError::Bad("get needs a challenge".into()))?;
        let allow: Vec<String> = body
            .get("allowCredentialIds")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        // Which stored passkey answers this — secret-free, from the agent.
        let resolved = agent_request(&json!({
            "op": "fido2-resolve",
            "rp_id": rp_id,
            "allow_credential_ids": allow,
        }))
        .map_err(|error| GetError::Bad(error.to_string()))?;
        let candidate = resolved["matches"]
            .as_array()
            .and_then(|matches| matches.first())
            .cloned()
            .ok_or(GetError::NoCredential)?;
        let item_id = candidate["item_id"].as_str().unwrap_or_default().to_string();
        let credential_id = candidate["credential_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let user_handle = candidate["user_handle"].as_str().map(str::to_string);

        // The bytes the RP will re-hash: whatever we sign, we return verbatim.
        let client_data_json = format!(
            r#"{{"type":"webauthn.get","challenge":{},"origin":{},"crossOrigin":false}}"#,
            json_string(challenge),
            json_string(origin),
        );
        let client_data_hash = Sha256::digest(client_data_json.as_bytes());

        // Ask the human. Emit the OSC, then park until the GUI answers.
        let request_id = hex_token(16);
        let label = account_label(&candidate);
        self.register(&request_id);
        emit_fido2_request(&self.session, &request_id, rp_id, &label, "get", origin);
        let outcome = self.wait_for_outcome(&request_id);

        let user_verified = match outcome {
            Some(Outcome::Granted { user_verified }) => user_verified,
            Some(Outcome::Denied) => return Err(GetError::Denied),
            None => return Err(GetError::TimedOut),
        };

        // Consent in hand: the agent mints UserPresence and signs.
        let assertion = agent_request(&json!({
            "op": "fido2-assert",
            "item_id": item_id,
            "credential_id": credential_id,
            "rp_id": rp_id,
            "client_data_hash_b64": b64url(&client_data_hash),
            "user_verified": user_verified,
        }))
        .map_err(|error| GetError::Bad(error.to_string()))?;

        Ok(json!({
            "credentialId": credential_id,
            "clientDataJSON": b64url(client_data_json.as_bytes()),
            "authenticatorData": assertion["authenticator_data_b64"],
            "signature": assertion["signature_b64"],
            "userHandle": user_handle,
        }))
    }

    /// `POST /fido2/create` — a `navigator.credentials.create()` ceremony. Same
    /// consent flow as `get`, then a vault WRITE: the agent mints and stores the
    /// keypair and returns the public material this assembles into an attestation.
    pub fn handle_create(&self, body: &Value) -> (u16, Value) {
        match self.try_create(body) {
            Ok(response) => (200, response),
            Err(GetError::Denied) => (403, json!({ "error": "the user declined" })),
            Err(GetError::TimedOut) => (408, json!({ "error": "the user did not respond in time" })),
            Err(GetError::Bad(message)) => (400, json!({ "error": message })),
            // create() has no "no credential" case; fold it into a 400.
            Err(GetError::NoCredential) => (400, json!({ "error": "invalid create request" })),
        }
    }

    fn try_create(&self, body: &Value) -> Result<Value, GetError> {
        let origin = body
            .get("origin")
            .and_then(Value::as_str)
            .ok_or_else(|| GetError::Bad("create needs the page origin".into()))?;
        let rp_id = body
            .get("rp")
            .and_then(|rp| rp.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| GetError::Bad("create needs an rp.id".into()))?;
        if !rp_id_matches_origin(rp_id, origin) {
            return Err(GetError::Bad(format!(
                "rp.id {rp_id:?} is not valid for origin {origin:?}"
            )));
        }
        let challenge = body
            .get("challenge")
            .and_then(Value::as_str)
            .ok_or_else(|| GetError::Bad("create needs a challenge".into()))?;
        let user = body
            .get("user")
            .ok_or_else(|| GetError::Bad("create needs a user".into()))?;
        let user_id = user.get("id").and_then(Value::as_str).unwrap_or_default();
        let user_name = user.get("name").and_then(Value::as_str).unwrap_or_default();
        let display_name = user
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let rp_name = body
            .get("rp")
            .and_then(|rp| rp.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(rp_id);

        let client_data_json = format!(
            r#"{{"type":"webauthn.create","challenge":{},"origin":{},"crossOrigin":false}}"#,
            json_string(challenge),
            json_string(origin),
        );

        // Ask the human — a registration is a presence ceremony too.
        let request_id = hex_token(16);
        let label = if display_name.is_empty() { user_name } else { display_name };
        self.register(&request_id);
        emit_fido2_request(&self.session, &request_id, rp_id, label, "create", origin);
        let user_verified = match self.wait_for_outcome(&request_id) {
            Some(Outcome::Granted { user_verified }) => user_verified,
            Some(Outcome::Denied) => return Err(GetError::Denied),
            None => return Err(GetError::TimedOut),
        };

        // Consent in hand: the agent generates + stores the keypair, returns the
        // public material (the private key never leaves the agent process).
        let created = agent_request(&json!({
            "op": "fido2-create",
            "rp_id": rp_id,
            "rp_name": rp_name,
            "user_id_b64": user_id,
            "user_name": user_name,
            "user_display_name": display_name,
        }))
        .map_err(|error| GetError::Bad(error.to_string()))?;

        let credential_id = created["credential_id_b64"].as_str().unwrap_or_default();
        let cose = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(created["cose_public_key_b64"].as_str().unwrap_or_default())
            .map_err(|_| GetError::Bad("agent returned a malformed public key".into()))?;
        let credential_id_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(credential_id)
            .map_err(|_| GetError::Bad("agent returned a malformed credential id".into()))?;

        let authenticator_data =
            attested_authenticator_data(rp_id, &credential_id_bytes, &cose, user_verified);
        let attestation_object = none_attestation_object(&authenticator_data);

        Ok(json!({
            "credentialId": credential_id,
            "clientDataJSON": b64url(client_data_json.as_bytes()),
            "attestationObject": b64url(&attestation_object),
        }))
    }

    /// `POST /fido2/grant` — the GUI dialog approved. Wakes the parked ceremony.
    /// Reached only over the GUI's `ssh -L` forward, never from the page.
    pub fn handle_grant(&self, body: &Value) -> (u16, Value) {
        let user_verified = body
            .get("user_verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.resolve_ceremony(body, Outcome::Granted { user_verified })
    }

    /// `POST /fido2/deny` — the GUI dialog was declined or dismissed.
    pub fn handle_deny(&self, body: &Value) -> (u16, Value) {
        self.resolve_ceremony(body, Outcome::Denied)
    }

    fn resolve_ceremony(&self, body: &Value, outcome: Outcome) -> (u16, Value) {
        let Some(request_id) = body.get("request_id").and_then(Value::as_str) else {
            return (400, json!({ "error": "grant needs a request_id" }));
        };
        let mut pending = self.pending.lock().unwrap();
        match pending.get_mut(request_id) {
            Some(ceremony) if ceremony.outcome.is_none() => {
                ceremony.outcome = Some(outcome);
                self.cvar.notify_all();
                (200, json!({ "ok": true }))
            }
            // Unknown or already-answered: idempotent, not an error the GUI acts
            // on. A double-click on Approve must not 500.
            _ => (200, json!({ "ok": true, "already": true })),
        }
    }

    fn register(&self, request_id: &str) {
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.to_string(), Ceremony::default());
    }

    /// Park until the ceremony has an outcome or the timeout fires, then consume
    /// the entry (so a late grant cannot replay it).
    fn wait_for_outcome(&self, request_id: &str) -> Option<Outcome> {
        let mut pending = self.pending.lock().unwrap();
        let deadline = std::time::Instant::now() + CEREMONY_TIMEOUT;
        loop {
            match pending.get(request_id) {
                Some(ceremony) if ceremony.outcome.is_some() => {
                    return pending.remove(request_id).and_then(|c| c.outcome);
                }
                Some(_) => {}
                None => return None,
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                pending.remove(request_id);
                return None;
            }
            let (guard, timed_out) = self.cvar.wait_timeout(pending, remaining).unwrap();
            pending = guard;
            if timed_out.timed_out() {
                pending.remove(request_id);
                return None;
            }
        }
    }
}

/// Why a `get()` could not complete.
enum GetError {
    /// No stored passkey answers the request (wrong RP, or the allow-list names
    /// nothing we hold). The shim reports `NotAllowedError` to the page.
    NoCredential,
    Denied,
    TimedOut,
    Bad(String),
}

/// The label the presence dialog shows for an account: the passkey's userName,
/// else the item name, else the RP name — whatever names the human's account.
fn account_label(candidate: &Value) -> String {
    for key in ["user_display_name", "user_name", "item_name", "rp_name"] {
        if let Some(value) = candidate.get(key).and_then(Value::as_str)
            && !value.is_empty()
        {
            return value.to_string();
        }
    }
    "this account".to_string()
}

/// WebAuthn's rpId rule, minus the public-suffix subtlety: the rpId must equal
/// the origin's host or be a parent domain of it. The RP's own rpIdHash check is
/// the backstop, so this is a cheap early refusal, not the security boundary.
fn rp_id_matches_origin(rp_id: &str, origin: &str) -> bool {
    let Some(host) = origin_host(origin) else {
        return false;
    };
    host == rp_id || host.ends_with(&format!(".{rp_id}"))
}

/// The host of an `https://host[:port]` origin. Only https is a valid WebAuthn
/// origin (bar localhost, which we still accept for testing).
fn origin_host(origin: &str) -> Option<String> {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    let host = rest.split('/').next().unwrap_or(rest);
    let host = host.split(':').next().unwrap_or(host);
    (!host.is_empty()).then(|| host.to_string())
}

/// `OSC 7717 ; fido2 ; request ; <base64 json>`. Carries only rpId + a label —
/// never a challenge secret, never a key. The GUI shows a presence dialog and,
/// on approval, POSTs `/fido2/grant` back to this control endpoint.
fn emit_fido2_request(session: &str, request_id: &str, rp_id: &str, account: &str, kind: &str, origin: &str) {
    let payload = json!({
        "session": session,
        "request_id": request_id,
        "rp_id": rp_id,
        "account": account,
        "kind": kind,
        "origin": origin,
    });
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\u{1b}]7717;fido2;request;{encoded}\u{7}");
    let _ = stdout.flush();
}

/// Send one request to this host's `ychrome-vault` agent and return its reply.
///
/// The browser speaks the agent's unix socket directly rather than shelling out
/// to a CLI verb, deliberately: there is NO `ychrome-vault fido2-assert`
/// subcommand a script could run, so the only path to a signature is this
/// module, behind the GUI dialog. Same newline-delimited-JSON framing the CLI
/// uses; a locked or absent agent surfaces as an error the shim reports.
fn agent_request(request: &Value) -> Result<Value> {
    let socket = agent_socket_path()?;
    let stream = UnixStream::connect(&socket).with_context(|| {
        format!(
            "no vault agent on {} — unlock with `ychrome-vault unlock` on this host",
            socket.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{request}")?;
    writer.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: Value =
        serde_json::from_str(line.trim()).context("vault agent sent a malformed response")?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(response);
    }
    bail!(
        "{}",
        response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("vault agent refused the request")
    );
}

/// `~/.yggterm/vault/agent.sock` — the `ychrome-vault` agent's socket, at the
/// CLI's default `--dir`. Host-resident: this is the host ychrome runs on.
fn agent_socket_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("vault")
        .join("agent.sock"))
}

/// A hex token of `bytes` random bytes, from the OS CSPRNG. Used for the bearer
/// token and per-ceremony request ids — both must be unguessable.
fn hex_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    // /dev/urandom is the CSPRNG on the Linux hosts ychrome runs on. A short
    // read is impossible for a handful of bytes; treat any failure as fatal
    // rather than emit a predictable token.
    let mut file = std::fs::File::open("/dev/urandom").expect("open /dev/urandom");
    file.read_exact(&mut buf).expect("read /dev/urandom");
    buf.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// WebAuthn `authenticatorData` for a registration: rpIdHash ‖ flags ‖
/// signCount(0) ‖ attestedCredentialData, where attestedCredentialData is
/// aaguid(16 zeros) ‖ credIdLen(be16) ‖ credId ‖ COSE public key. Flags set
/// UP (present), AT (attested-credential-data included) and, when the user was
/// verified, UV.
fn attested_authenticator_data(
    rp_id: &str,
    credential_id: &[u8],
    cose_public_key: &[u8],
    user_verified: bool,
) -> Vec<u8> {
    const FLAG_UP: u8 = 0x01;
    const FLAG_UV: u8 = 0x04;
    const FLAG_AT: u8 = 0x40;

    let mut data = Vec::new();
    data.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
    let mut flags = FLAG_UP | FLAG_AT;
    if user_verified {
        flags |= FLAG_UV;
    }
    data.push(flags);
    data.extend_from_slice(&0u32.to_be_bytes()); // signCount — counter-less, like Bitwarden
    data.extend_from_slice(&[0u8; 16]); // aaguid: all-zero (a software authenticator)
    data.extend_from_slice(&(credential_id.len() as u16).to_be_bytes());
    data.extend_from_slice(credential_id);
    data.extend_from_slice(cose_public_key);
    data
}

/// The CBOR attestation object with the `"none"` format:
/// `{"fmt": "none", "attStmt": {}, "authData": <bytes>}`. Keys are in canonical
/// (length-then-byte) order. `authData` is short enough for a one-byte length.
fn none_attestation_object(authenticator_data: &[u8]) -> Vec<u8> {
    let mut cbor = vec![0xa3]; // map(3)
    // "fmt": "none"
    cbor.extend_from_slice(&[0x63, b'f', b'm', b't']);
    cbor.extend_from_slice(&[0x64, b'n', b'o', b'n', b'e']);
    // "attStmt": {}
    cbor.extend_from_slice(&[0x67, b'a', b't', b't', b'S', b't', b'm', b't']);
    cbor.push(0xa0); // map(0)
    // "authData": bstr(len)
    cbor.extend_from_slice(&[0x68, b'a', b'u', b't', b'h', b'D', b'a', b't', b'a']);
    cbor_byte_string(&mut cbor, authenticator_data);
    cbor
}

/// Append a CBOR byte string header + bytes, choosing the minimal length form.
/// authData with a P-256 key is ~150 bytes, so the 1-byte and 2-byte forms are
/// the only ones that occur — but handle all four for correctness.
fn cbor_byte_string(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = bytes.len();
    if len < 24 {
        out.push(0x40 | len as u8);
    } else if len < 0x100 {
        out.extend_from_slice(&[0x58, len as u8]);
    } else if len < 0x10000 {
        out.push(0x59);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x5a);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(bytes);
}

/// base64url without padding — the WebAuthn wire encoding.
fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// JSON-encode a string with its quotes — for hand-building clientDataJSON,
/// whose exact bytes are what the authenticator signs and the RP re-hashes.
fn json_string(value: &str) -> String {
    Value::String(value.to_string()).to_string()
}

/// The `navigator.credentials` shim, served as a document-start userscript. It
/// intercepts `get()`/`create()`, forwards to the signer over loopback, and
/// rebuilds a `PublicKeyCredential` from the response. `PORT`/`TOKEN` are baked
/// in per surface. Kept as one self-contained IIFE so it needs nothing else.
fn shim_js(port: u16, token: &str) -> String {
    format!(
        r#"(function () {{
  'use strict';
  var ENDPOINT = 'http://127.0.0.1:{port}';
  var TOKEN = '{token}';

  function b64urlToBuf(s) {{
    s = s.replace(/-/g, '+').replace(/_/g, '/');
    while (s.length % 4) s += '=';
    var bin = atob(s);
    var arr = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
    return arr.buffer;
  }}
  function bufToB64url(buf) {{
    var bytes = new Uint8Array(buf);
    var s = '';
    for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }}
  function post(path, body) {{
    return fetch(ENDPOINT + path, {{
      method: 'POST',
      headers: {{ 'Content-Type': 'application/json', 'X-Ychrome-Fido2': TOKEN }},
      body: JSON.stringify(body),
    }}).then(function (r) {{
      return r.json().then(function (j) {{ return {{ ok: r.ok, body: j }}; }});
    }});
  }}

  // A PublicKeyCredential the page can hand to the RP. Only the fields RPs read.
  function makeCredential(id, response, isAssertion) {{
    var rawId = b64urlToBuf(id);
    return {{
      id: id,
      rawId: rawId,
      type: 'public-key',
      authenticatorAttachment: 'platform',
      response: response,
      getClientExtensionResults: function () {{ return {{}}; }},
      toJSON: function () {{ return {{ id: id, type: 'public-key' }}; }},
    }};
  }}

  var native = navigator.credentials;
  var shim = Object.create(native || {{}});

  shim.get = function (options) {{
    if (!options || !options.publicKey) {{
      return native && native.get ? native.get(options) : Promise.reject(new Error('no publicKey'));
    }}
    var pk = options.publicKey;
    var allow = (pk.allowCredentials || []).map(function (c) {{ return bufToB64url(c.id); }});
    return post('/fido2/get', {{
      rpId: pk.rpId || location.hostname,
      origin: location.origin,
      challenge: bufToB64url(pk.challenge),
      allowCredentialIds: allow,
      userVerification: pk.userVerification || 'preferred',
    }}).then(function (res) {{
      if (!res.ok) throw new DOMException(res.body.error || 'passkey get failed', 'NotAllowedError');
      var b = res.body;
      var response = {{
        clientDataJSON: b64urlToBuf(b.clientDataJSON),
        authenticatorData: b64urlToBuf(b.authenticatorData),
        signature: b64urlToBuf(b.signature),
        userHandle: b.userHandle ? b64urlToBuf(b.userHandle) : null,
      }};
      return makeCredential(b.credentialId, response, true);
    }});
  }};

  shim.create = function (options) {{
    if (!options || !options.publicKey) {{
      return native && native.create ? native.create(options) : Promise.reject(new Error('no publicKey'));
    }}
    var pk = options.publicKey;
    var excl = (pk.excludeCredentials || []).map(function (c) {{ return bufToB64url(c.id); }});
    return post('/fido2/create', {{
      origin: location.origin,
      rp: {{ id: (pk.rp && pk.rp.id) || location.hostname, name: (pk.rp && pk.rp.name) || '' }},
      user: pk.user ? {{
        id: bufToB64url(pk.user.id),
        name: pk.user.name || '',
        displayName: pk.user.displayName || '',
      }} : null,
      challenge: bufToB64url(pk.challenge),
      excludeCredentialIds: excl,
    }}).then(function (res) {{
      if (!res.ok) throw new DOMException(res.body.error || 'passkey create failed', 'NotAllowedError');
      var b = res.body;
      var response = {{
        clientDataJSON: b64urlToBuf(b.clientDataJSON),
        attestationObject: b64urlToBuf(b.attestationObject),
        getTransports: function () {{ return ['internal']; }},
      }};
      return makeCredential(b.credentialId, response, false);
    }});
  }};

  try {{
    Object.defineProperty(navigator, 'credentials', {{ value: shim, configurable: true }});
  }} catch (e) {{ /* some engines freeze navigator; the assignment below still helps */ }}
  window.PublicKeyCredential = window.PublicKeyCredential || function () {{}};
  window.PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable =
    function () {{ return Promise.resolve(true); }};
  window.PublicKeyCredential.isConditionalMediationAvailable =
    function () {{ return Promise.resolve(false); }};
}})();
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rp_id_must_be_a_suffix_of_the_origin_host() {
        assert!(rp_id_matches_origin("github.com", "https://github.com"));
        assert!(rp_id_matches_origin("github.com", "https://sub.github.com"));
        assert!(rp_id_matches_origin("github.com", "https://github.com:443/x"));
        // A page cannot claim a parent it is not under, nor an unrelated RP.
        assert!(!rp_id_matches_origin("github.com", "https://evil.com"));
        assert!(!rp_id_matches_origin("github.com", "https://notgithub.com"));
        assert!(!rp_id_matches_origin("github.com", "https://github.com.evil.com"));
    }

    #[test]
    fn origin_host_strips_scheme_and_port() {
        assert_eq!(origin_host("https://example.com").as_deref(), Some("example.com"));
        assert_eq!(origin_host("https://example.com:8443/a/b").as_deref(), Some("example.com"));
        assert_eq!(origin_host("about:blank"), None);
    }

    #[test]
    fn a_grant_wakes_a_parked_ceremony_and_is_consumed() {
        let signer = Signer::new(1234, "sess".into());
        signer.register("req-1");

        // Grant for a live ceremony succeeds and is idempotent on repeat.
        let (status, _) = signer.handle_grant(&json!({ "request_id": "req-1", "user_verified": true }));
        assert_eq!(status, 200);
        // The outcome is now set; a second grant is a no-op, never a 500.
        let (status, body) = signer.handle_grant(&json!({ "request_id": "req-1" }));
        assert_eq!(status, 200);
        assert_eq!(body["already"], true);

        // The parked side consumes it exactly once.
        assert!(matches!(
            signer.wait_for_outcome("req-1"),
            Some(Outcome::Granted { user_verified: true })
        ));
        // Consumed: a later look finds nothing.
        assert!(signer.wait_for_outcome("req-1").is_none());
    }

    #[test]
    fn the_attestation_object_is_well_formed_cbor_none() {
        // A 77-byte COSE key (what generate_credential emits) and a 16-byte cred.
        let cose = vec![0xAA; 77];
        let cred_id = vec![0x11; 16];
        let auth = attested_authenticator_data("example.com", &cred_id, &cose, true);

        // rpIdHash ‖ flags(UP|UV|AT=0x45) ‖ signCount(0) ‖ aaguid(16) ‖
        // credLen(be16=16) ‖ cred(16) ‖ cose(77) = 32+1+4+16+2+16+77 = 148.
        assert_eq!(auth.len(), 148);
        assert_eq!(&auth[0..32], Sha256::digest(b"example.com").as_slice());
        assert_eq!(auth[32], 0x45);
        assert_eq!(&auth[33..37], &[0, 0, 0, 0]);
        assert_eq!(&auth[37..53], &[0u8; 16]); // aaguid
        assert_eq!(&auth[53..55], &[0x00, 0x10]); // credIdLen = 16, big-endian

        let obj = none_attestation_object(&auth);
        // map(3), then "fmt":"none", "attStmt":{}, "authData": bstr(148).
        assert_eq!(obj[0], 0xa3);
        // authData is 148 bytes → 0x58 <len> form; find it near the tail.
        assert!(obj.windows(2).any(|w| w == [0x58, 148]));
        // The whole authData rides at the end verbatim.
        assert!(obj.ends_with(&auth));
    }

    #[test]
    fn the_token_gates_every_route() {
        let signer = Signer::new(1234, "sess".into());
        assert!(signer.authorized(Some(&signer.token)));
        assert!(!signer.authorized(Some("wrong")));
        assert!(!signer.authorized(None));
    }

    #[test]
    fn the_shim_bakes_in_the_port_and_token_and_overrides_get() {
        let signer = Signer::new(54321, "sess".into());
        let js = signer.shim_userscript();
        assert!(js.contains("http://127.0.0.1:54321"));
        assert!(js.contains(&signer.token));
        assert!(js.contains("shim.get = function"));
        assert!(js.contains("/fido2/get"));
        // The private key never appears; only public wire fields are handled.
        assert!(!js.contains("keyValue") && !js.contains("private"));
    }
}
