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
//! that exact `request_id` arrives. The strong, enforced boundary is against the
//! WEB threat: a page can only *trigger* a ceremony. It cannot reach the grant
//! channel — the `request_id` is 128 bits of CSPRNG never exposed to it, and
//! `/fido2/grant` is a GUI→app call over `ssh -L`, not page-reachable over the
//! surface's SOCKS egress. So a malicious site can make a dialog appear but can
//! never answer it.
//!
//! Against a same-uid process on the host the boundary is the vault's usual one:
//! the socket cannot distinguish the GUI from another same-uid process, exactly
//! as the `get` op (which already returns a plaintext password) cannot, so
//! passkeys are no weaker than the rest of the vault. The human-facing gate is
//! the GUI dialog; a grant requires a deliberate operator action at the GUI,
//! never a silent socket call.
//!
//! **A secret never crosses into yggterm.** The OSC carries only rpId + a
//! display label. The private key is decrypted, used once and zeroized inside
//! the agent; the assertion (public bytes) is what reaches the page.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// How long a `/fido2/get` blocks for the user to approve before giving up. A
/// ceremony the user ignores must not pin a control-server thread forever.
const CEREMONY_TIMEOUT: Duration = Duration::from_secs(120);

/// What the GUI dialog delivered for a pending ceremony.
#[derive(Clone)]
enum Outcome {
    Granted {
        user_verified: bool,
        /// Which passkey the user chose, when the site offered several accounts.
        /// `None` for a single-account ceremony or a `create()` — the caller
        /// falls back to the only/first candidate.
        credential_id: Option<String>,
    },
    Denied,
}

/// One in-flight ceremony, awaiting the GUI grant. The `/fido2/get` thread
/// parks on the [`Signer`] condvar until `outcome` is set by `/fido2/grant` or
/// `/fido2/deny` (or the timeout fires and the entry is swept).
#[derive(Default)]
struct Ceremony {
    outcome: Option<Outcome>,
    /// Recorded at register time so the ctl `fido2 list` can answer "who is
    /// asking" without the GUI dialog.
    rp_id: String,
    origin: String,
    ceremony: String,
    accounts: Vec<Value>,
    registered_at_ms: Option<u64>,
    /// The validated request (and, for a get, the resolved credential
    /// candidates) stashed at begin time, so a NON-blocking poller can finish
    /// the ceremony later. The scheme-handler transport cannot park: its
    /// callback runs on the engine's main loop, and blocking there froze every
    /// ctl call for the length of a ceremony — measured 2026-08-28.
    context: Option<Value>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Every live [`Signer`], weakly held so a retiring session's server drops out
/// on its own. THE AGENTIC DOOR: the ctl plane (`ychrome ctl fido2 …`) walks
/// this registry to list pending ceremonies and to grant/deny them, which is
/// how a headless daemon — or a session whose presence dialog cannot reach a
/// human — still completes a WebAuthn login. Without it a headless ceremony
/// parked forever: the OSC emission has no GUI stream to arrive on, nothing
/// could ever answer it, and the login simply timed out.
fn live_signers() -> &'static Mutex<Vec<std::sync::Weak<Signer>>> {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<Mutex<Vec<std::sync::Weak<Signer>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn register_signer(signer: &Arc<Signer>) {
    live_signers()
        .lock()
        .unwrap()
        .push(Arc::downgrade(signer));
}

pub(crate) fn for_each_live_signer(mut visit: impl FnMut(&Signer)) {
    let mut registry = live_signers().lock().unwrap();
    registry.retain(|weak| match weak.upgrade() {
        Some(signer) => {
            visit(&signer);
            true
        }
        None => false,
    });
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
        let signer = Arc::new(Signer {
            token: hex_token(32),
            port,
            session,
            pending: Mutex::new(HashMap::new()),
            cvar: Condvar::new(),
        });
        register_signer(&signer);
        signer
    }

    /// One entry of the ctl `fido2 list` answer: everything an agent needs to
    /// decide and grant — the request id, who is asking, which accounts
    /// match, and how long the ceremony has been parked.
    pub fn pending_summary(&self) -> Vec<Value> {
        let pending = self.pending.lock().unwrap();
        pending
            .iter()
            .filter(|(_, ceremony)| ceremony.outcome.is_none())
            .map(|(request_id, ceremony)| {
                json!({
                    "request_id": request_id,
                    "session": self.session,
                    "rp_id": ceremony.rp_id,
                    "origin": ceremony.origin,
                    "ceremony": ceremony.ceremony,
                    "accounts": ceremony.accounts,
                    "age_ms": ceremony
                        .registered_at_ms
                        .map(|at| now_ms().saturating_sub(at))
                        .unwrap_or(0),
                })
            })
            .collect()
    }

    /// The ctl-plane grant: the SAME resolution the GUI dialog's HTTP grant
    /// takes, reached without a GUI.
    pub fn ctl_grant(&self, body: &Value) -> (u16, Value) {
        self.resolve_ceremony(
            body,
            Outcome::Granted {
                user_verified: body
                    .get("user_verified")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                credential_id: body
                    .get("credential_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string),
            },
        )
    }

    /// The ctl-plane deny.
    pub fn ctl_deny(&self, body: &Value) -> (u16, Value) {
        self.resolve_ceremony(body, Outcome::Denied)
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
            Err(GetError::TimedOut) => {
                (408, json!({ "error": "the user did not respond in time" }))
            }
            Err(GetError::Bad(message)) => (400, json!({ "error": message })),
        }
    }

    /// Validate, resolve matching passkeys, and REGISTER the ceremony — but do
    /// not wait. The scheme-handler transport calls this from the engine's main
    /// loop, which must never park; the outcome is collected by [`Self::poll`].
    pub(crate) fn begin_get(&self, body: &Value) -> Result<(String, Vec<Value>), GetError> {
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

        // Which stored passkeys answer this — secret-free, from the agent. ALL
        // of them: a site where the user has several accounts (github, google)
        // offers a choice, exactly as the Bitwarden extension does.
        let resolved = agent_request(&json!({
            "op": "fido2-resolve",
            "rp_id": rp_id,
            "allow_credential_ids": allow,
        }))
        .map_err(|error| GetError::Bad(error.to_string()))?;
        let matches: Vec<Value> = resolved["matches"].as_array().cloned().unwrap_or_default();
        if matches.is_empty() {
            return Err(GetError::NoCredential);
        }

        // Ask the human, offering every matched account. One entry ⇒ the dialog
        // is a plain Approve; several ⇒ a picker. Labels only — no key.
        let accounts: Vec<Value> = matches
            .iter()
            .map(|candidate| {
                json!({
                    "credential_id": candidate["credential_id"].as_str().unwrap_or_default(),
                    "label": account_label(candidate),
                })
            })
            .collect();
        let request_id = hex_token(16);
        self.register_ceremony(
            &request_id,
            rp_id,
            origin,
            "get",
            &accounts,
            json!({ "body": body.clone(), "matches": matches }),
        );
        emit_fido2_request(&self.session, &request_id, rp_id, &accounts, "get", origin);
        Ok((request_id, accounts))
    }

    /// Finish a parked get from the context stored at begin time.
    fn finish_get(&self, ceremony: &Ceremony, outcome: Outcome) -> Result<Value, GetError> {
        let context = ceremony
            .context
            .clone()
            .ok_or_else(|| GetError::Bad("ceremony lost its begin context".into()))?;
        let body = &context["body"];
        let matches: Vec<Value> = context["matches"].as_array().cloned().unwrap_or_default();
        let rp_id = &ceremony.rp_id;
        let origin = &ceremony.origin;

        let (user_verified, chosen_id) = match outcome {
            Outcome::Granted {
                user_verified,
                credential_id,
            } => (user_verified, credential_id),
            Outcome::Denied => return Err(GetError::Denied),
        };

        let challenge = body.get("challenge").and_then(Value::as_str).unwrap_or_default();
        // The bytes the RP will re-hash: whatever we sign, we return verbatim.
        let client_data_json = format!(
            r#"{{"type":"webauthn.get","challenge":{},"origin":{},"crossOrigin":false}}"#,
            json_string(challenge),
            json_string(origin),
        );
        let client_data_hash = Sha256::digest(client_data_json.as_bytes());

        // The account the user chose (or the only one). A chosen id the resolver
        // did not return is refused rather than silently signing another account.
        let candidate = match &chosen_id {
            Some(id) => matches
                .iter()
                .find(|c| c["credential_id"].as_str() == Some(id.as_str()))
                .ok_or_else(|| {
                    GetError::Bad("chosen passkey is not among the offered accounts".into())
                })?,
            None => &matches[0],
        };
        let item_id = candidate["item_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let credential_id = candidate["credential_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // What the RP must receive is base64url of the credential id BYTES, NOT
        // the vault's UUID spelling. Returning the UUID makes `rawId` decode to
        // garbage and the RP rejects an otherwise valid assertion.
        let credential_id_rp = candidate["credential_id_b64url"]
            .as_str()
            .filter(|id| !id.is_empty())
            .unwrap_or(credential_id.as_str())
            .to_string();
        let user_handle = candidate["user_handle"].as_str().map(str::to_string);

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
            "credentialId": credential_id_rp,
            "clientDataJSON": b64url(client_data_json.as_bytes()),
            "authenticatorData": assertion["authenticator_data_b64"],
            "signature": assertion["signature_b64"],
            "userHandle": user_handle,
        }))
    }

    /// The blocking form the sidebar HTTP route uses: a server thread per
    /// ceremony is fine there — the scheme handler's main-loop thread is what
    /// may never park.
    fn try_get(&self, body: &Value) -> Result<Value, GetError> {
        let (request_id, _) = self.begin_get(body)?;
        let Some((outcome, ceremony)) = self.wait_for_outcome(&request_id) else {
            return Err(GetError::TimedOut);
        };
        self.finish_get(&ceremony, outcome)
    }

    /// `POST /fido2/create` — a `navigator.credentials.create()` ceremony. Same
    /// consent flow as `get`, then a vault WRITE: the agent mints and stores the
    /// keypair and returns the public material this assembles into an attestation.
    pub fn handle_create(&self, body: &Value) -> (u16, Value) {
        match self.try_create(body) {
            Ok(response) => (200, response),
            Err(GetError::Denied) => (403, json!({ "error": "the user declined" })),
            Err(GetError::TimedOut) => {
                (408, json!({ "error": "the user did not respond in time" }))
            }
            Err(GetError::Bad(message)) => (400, json!({ "error": message })),
            // create() has no "no credential" case; fold it into a 400.
            Err(GetError::NoCredential) => (400, json!({ "error": "invalid create request" })),
        }
    }

    /// The non-blocking create begin — see [`Self::begin_get`].
    pub(crate) fn begin_create(&self, body: &Value) -> Result<(String, Vec<Value>), GetError> {
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
        let user_name = user.get("name").and_then(Value::as_str).unwrap_or_default();
        let display_name = user
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_default();

        // Ask the human — a registration is a presence ceremony too. One account
        // (the one being created), so the dialog is a plain Approve, never a
        // picker; a chosen credential_id in the grant is ignored here.
        let request_id = hex_token(16);
        let label = if display_name.is_empty() {
            user_name
        } else {
            display_name
        };
        let accounts = vec![json!({ "label": label })];
        self.register_ceremony(
            &request_id,
            rp_id,
            origin,
            "create",
            &accounts,
            json!({ "body": body.clone() }),
        );
        emit_fido2_request(
            &self.session,
            &request_id,
            rp_id,
            &accounts,
            "create",
            origin,
        );
        Ok((request_id, accounts))
    }

    fn finish_create(&self, ceremony: &Ceremony, outcome: Outcome) -> Result<Value, GetError> {
        let context = ceremony
            .context
            .clone()
            .ok_or_else(|| GetError::Bad("ceremony lost its begin context".into()))?;
        let body = &context["body"];
        let rp_id = &ceremony.rp_id;
        let origin = &ceremony.origin;

        let user_verified = match outcome {
            Outcome::Granted { user_verified, .. } => user_verified,
            Outcome::Denied => return Err(GetError::Denied),
        };

        let challenge = body.get("challenge").and_then(Value::as_str).unwrap_or_default();
        let client_data_json = format!(
            r#"{{"type":"webauthn.create","challenge":{},"origin":{},"crossOrigin":false}}"#,
            json_string(challenge),
            json_string(origin),
        );
        let user = body.get("user").cloned().unwrap_or(Value::Null);
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

    /// The blocking form the sidebar HTTP route uses — see [`Self::try_get`].
    fn try_create(&self, body: &Value) -> Result<Value, GetError> {
        let (request_id, _) = self.begin_create(body)?;
        let Some((outcome, ceremony)) = self.wait_for_outcome(&request_id) else {
            return Err(GetError::TimedOut);
        };
        self.finish_create(&ceremony, outcome)
    }

    /// NON-BLOCKING outcome collection for the scheme-handler transport. `None`
    /// = still parked (ask again); `Some` = finished (and the entry consumed —
    /// a late grant cannot replay it). Entries nobody resolved within the
    /// ceremony timeout are swept here, since nothing else ever waits on them.
    pub fn poll(&self, request_id: &str) -> Option<Result<Value, GetError>> {
        let mut pending = self.pending.lock().unwrap();
        let deadline_ms = CEREMONY_TIMEOUT.as_millis() as u64;
        let expired: Vec<String> = pending
            .iter()
            .filter(|(_, c)| {
                c.outcome.is_none()
                    && now_ms().saturating_sub(c.registered_at_ms.unwrap_or(0)) > deadline_ms
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            pending.remove(&key);
        }
        let ceremony = pending.get(request_id)?;
        if ceremony.outcome.is_none() {
            return None;
        }
        let (_, ceremony) = pending.remove_entry(request_id)?;
        let outcome = ceremony.outcome.clone()?;
        match ceremony.ceremony.as_str() {
            "get" => Some(self.finish_get(&ceremony, outcome)),
            "create" => Some(self.finish_create(&ceremony, outcome)),
            _ => Some(Err(GetError::Bad("unknown ceremony kind".into()))),
        }
    }

    /// `POST /fido2/grant` — the GUI dialog approved. Wakes the parked ceremony.
    /// Reached only over the GUI's `ssh -L` forward, never from the page.
    pub fn handle_grant(&self, body: &Value) -> (u16, Value) {
        let user_verified = body
            .get("user_verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // The account the user picked, when the site offered several. Absent for
        // a single-account ceremony (the dialog is a plain Approve).
        let credential_id = body
            .get("credential_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        self.resolve_ceremony(
            body,
            Outcome::Granted {
                user_verified,
                credential_id,
            },
        )
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

    fn register_ceremony(
        &self,
        request_id: &str,
        rp_id: &str,
        origin: &str,
        ceremony: &str,
        accounts: &[Value],
        context: Value,
    ) {
        self.pending.lock().unwrap().insert(
            request_id.to_string(),
            Ceremony {
                rp_id: rp_id.to_string(),
                origin: origin.to_string(),
                ceremony: ceremony.to_string(),
                accounts: accounts.to_vec(),
                registered_at_ms: Some(now_ms()),
                outcome: None,
                context: Some(context),
            },
        );
    }

    /// Park until the ceremony has an outcome or the timeout fires, then consume
    /// the entry (so a late grant cannot replay it). Returns the ceremony too —
    /// the finisher needs the context stashed at begin time.
    fn wait_for_outcome(&self, request_id: &str) -> Option<(Outcome, Ceremony)> {
        let mut pending = self.pending.lock().unwrap();
        let deadline = std::time::Instant::now() + CEREMONY_TIMEOUT;
        loop {
            match pending.get(request_id) {
                Some(ceremony) if ceremony.outcome.is_some() => {
                    return pending
                        .remove(request_id)
                        .and_then(|c| c.outcome.clone().map(|o| (o, c)));
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
pub(crate) enum GetError {
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

/// `OSC 7717 ; fido2 ; request ; <base64 json>`. Carries rpId + the matched
/// accounts (each `{credential_id, label}`) — never a challenge secret, never a
/// key. The GUI shows a presence dialog (a picker when several accounts match)
/// and, on the user's choice, POSTs `/fido2/grant {request_id, credential_id}`
/// back to this control endpoint. `account` is kept as the first label so an
/// older yggterm that reads only that still names an account.
fn emit_fido2_request(
    session: &str,
    request_id: &str,
    rp_id: &str,
    accounts: &[Value],
    kind: &str,
    origin: &str,
) {
    let first_label = accounts
        .first()
        .and_then(|a| a["label"].as_str())
        .unwrap_or("this account");
    let payload = json!({
        "session": session,
        "request_id": request_id,
        "rp_id": rp_id,
        "account": first_label,
        "accounts": accounts,
        "kind": kind,
        "origin": origin,
    });
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\u{1b}]7717;fido2;request;{encoded}\u{7}");
    let _ = stdout.flush();
}

/// Send one request to this host's `ychrome-vault` agent and return its reply,
/// through the shared crypto-free transport in [`ychrome_vault_proto`].
///
/// The browser speaks the agent's unix socket directly rather than shelling out
/// to a CLI verb, deliberately: there is NO `ychrome-vault fido2-assert`
/// subcommand a script could run, so the only path to a signature is this
/// module, behind the GUI dialog. A WebAuthn ceremony has a human in the loop,
/// so it uses a shorter read budget than a full `sync`; a locked or absent agent
/// surfaces as an error the shim reports.
fn agent_request(request: &Value) -> Result<Value> {
    let dir = ychrome_vault_proto::default_dir()?;
    ychrome_vault_proto::request_with_timeout(&dir, request, Duration::from_secs(30))
}

/// A hex token of `bytes` random bytes, from the OS CSPRNG. Used for the bearer
/// token, the per-ceremony request ids, and the sidebar's GUI control token —
/// all three must be unguessable, and one minter is one place to get it right.
pub(crate) fn hex_token(bytes: usize) -> String {
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
/// The page served at `yggterm-appctl://signer/helper` — the scheme-origin
/// resident the shim talks to. See [`shim_js`] for why this page exists: the
/// scheme handler answers SAME-ORIGIN fetches and TOP-LEVEL navigations, and
/// this page is the one thing at that origin, so every ceremony is a
/// postMessage from the shim and a same-origin fetch from here.
///
/// It holds no secrets: the signer's bearer token travels per-request and the
/// handler gates every fido2 route on it, so a rogue page that opens the
/// helper gets exactly nothing it could not get by talking to the handler
/// directly.
pub const HELPER_DOC: &str = r#"<!doctype html>
<html><body>
<script>
(function () {
  'use strict';
  var ENDPOINT = 'yggterm-appctl://signer';
  var IDLE_MS = 300000;  // close when nothing has asked for five minutes

  var closer = setTimeout(function () { window.close(); }, IDLE_MS);

  window.addEventListener('message', function (event) {
    var d = event.data;
    if (!d || typeof d !== 'object' || d.__yfido2 !== 1 || d.kind !== 'request') return;
    if (typeof d.path !== 'string' || d.path.indexOf('/fido2/') !== 0) return;
    clearTimeout(closer);
    closer = setTimeout(function () { window.close(); }, IDLE_MS);
    function relay(ok, body) {
      // Reply ONLY to the asker, only at the origin the asker spoke from.
      if (event.source) {
        event.source.postMessage(
          { __yfido2: 1, kind: 'reply', id: d.id, ok: ok, body: body },
          event.origin);
      }
    }
    // BEGIN first; while the outcome is pending, POLL. The scheme handler
    // must never park — it runs on the engine's main loop, and a parked
    // ceremony there froze the whole daemon (measured 2026-08-28).
    var base = ENDPOINT + d.path
      + '?id=' + encodeURIComponent(String(d.id))
      + '&token=' + encodeURIComponent(String(d.token || ''))
      + '&origin=' + encodeURIComponent(String(d.origin || ''))
      + '&payload=' + encodeURIComponent(JSON.stringify(d.payload || null));
    function parse(t) { try { return JSON.parse(t); } catch (e) { return { error: 'bad reply from signer' }; } }
    // Every handler reply is an ENVELOPE: {ok, body} — and `pending` rides
    // INSIDE the body, so a bare `j.pending` never fires and a parked ceremony
    // gets mistaken for a final answer.
    fetch(base, { method: 'POST', body: '{}' })
      .then(function (r) { return r.text(); })
      .then(function (t) {
        var env = parse(t);
        var body = env && env.body !== undefined ? env.body : env;
        if (env && env.ok && body && body.pending) {
          var pollUrl = ENDPOINT + '/fido2/poll?request_id=' +
            encodeURIComponent(String(body.request_id || '')) +
            '&token=' + encodeURIComponent(String(d.token || ''));
          var tries = 0;
          var iv = setInterval(function () {
            tries += 1;
            if (tries > 220) {  // ~132s, just past the signer's own timeout
              clearInterval(iv);
              relay(false, { error: 'the user did not respond in time' });
              return;
            }
            fetch(pollUrl, { method: 'POST', body: '{}' })
              .then(function (r2) { return r2.text(); })
              .then(function (t2) {
                var env2 = parse(t2);
                var b2 = env2 && env2.body !== undefined ? env2.body : env2;
                if (env2 && env2.ok && b2 && b2.pending) return;  // keep asking
                clearInterval(iv);
                if (env2 && env2.ok) relay(true, b2);
                else relay(false, b2 || { error: 'signer error' });
              })
              .catch(function () { /* transient; keep asking */ });
          }, 600);
          return;
        }
        if (env && env.ok) relay(true, body);
        else relay(false, body || { error: 'signer error' });
      })
      .catch(function (e) { relay(false, { error: String(e) }); });
  });

  // Tell our opener we can take requests (it queues until this arrives).
  window.__yfido2HelperArmed = true;
  if (window.opener) {
    window.opener.postMessage({ __yfido2: 1, kind: 'ready' }, '*');
  }
})();
</script>
</body></html>
"#;

fn shim_js(port: u16, token: &str) -> String {
    // The shim reaches the signer through yggterm's `yggterm-appctl://` bridge,
    // NOT `http://127.0.0.1:{port}` directly: WebKitGTK blocks an https page from
    // fetching http-loopback (mixed content). The port is unused in the page
    // (the GUI knows which signer to route to); the token still gates.
    //
    // ⛔ THE TRANSPORT IS A HELPER PAGE AT THE SCHEME ORIGIN, NOT fetch() FROM
    // THE SITE PAGE. Measured on webkit 2.52.6 (2026-08-28): the scheme
    // handler serves TOP-LEVEL navigations and SAME-ORIGIN fetches only — an
    // https page's own fetch/XHR to the scheme dies `TypeError: Load failed`
    // at the network layer (handler never consulted, journal-proven zero hits
    // ever), and a subframe navigation is rebased to `about:blank`. But a page
    // AT the scheme origin fetches the scheme freely. So the shim opens the
    // helper once (a top-level open — served), and ceremonies travel
    // shim → helper by postMessage, helper → signer by same-origin fetch.
    let _ = port;
    format!(
        r#"(function () {{
  'use strict';
  var ENDPOINT = 'yggterm-appctl://signer';
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
  var PENDING = {{}};
  var SEQ = 0;
  var TIMER_MS = 130000;  // just past the signer's own 120s ceremony timeout
  var HELPER = null;      // the yggterm-appctl://signer helper window
  var HELPER_READY = false;
  var QUEUE = [];

  // The transport, and why it is shaped like this (measured on webkit 2.52.6,
  // 2026-08-28): the scheme handler serves TOP-LEVEL navigations and
  // SAME-ORIGIN fetches only — an https page's own fetch/XHR to the scheme
  // dies `TypeError: Load failed` at the network layer (handler never
  // consulted, journal-proven), and a subframe navigation is rebased to
  // `about:blank`. But a page AT the scheme origin can fetch the scheme
  // freely. So: this shim opens the helper page once (a top-level open, which
  // the handler serves), and every ceremony is a postMessage to the helper
  // and a same-origin fetch inside it.

  function helperMessage(event) {{
    var data = event.data;
    if (!data || typeof data !== 'object' || data.__yfido2 !== 1) return;
    if (data.kind === 'ready') {{
      HELPER_READY = true;
      var q = QUEUE; QUEUE = [];
      for (var i = 0; i < q.length; i++) dispatch(q[i]);
      return;
    }}
    if (data.kind !== 'reply') return;
    // A reply binds to the helper window WE opened, and to a live pending id.
    if (!HELPER || event.source !== HELPER) return;
    var p = PENDING[data.id];
    if (!p) return;
    delete PENDING[data.id];
    clearTimeout(p.timer);
    p.resolve(data.ok ? {{ ok: true, body: data.body }}
                      : {{ ok: false, body: data.body || {{ error: 'signer error' }} }});
  }}
  window.addEventListener('message', helperMessage);
  window.addEventListener('message', function (event) {{
    window.__yfido2Got = (window.__yfido2Got || 0) + 1;
    window.__yfido2LastOrigin = String(event.origin || '');
  }});

  function dispatch(req) {{
    var sent = false;
    try {{
      if (HELPER && HELPER_READY && !HELPER.closed) {{
        HELPER.postMessage(req, 'yggterm-appctl://signer');
        sent = true;
      }}
    }} catch (e) {{ sent = false; }}
    if (!sent) QUEUE.push(req);
  }}

  function ensureHelper() {{
    if (HELPER && !HELPER.closed) return HELPER;
    HELPER_READY = false;
    HELPER = window.open('yggterm-appctl://signer/helper', 'yggterm-fido2-helper');
    return HELPER;
  }}

  function post(path, body) {{
    return new Promise(function (resolve) {{
      var id = ++SEQ;
      PENDING[id] = {{ resolve: resolve }};
      PENDING[id].timer = setTimeout(function () {{
        delete PENDING[id];
        resolve({{ ok: false, body: {{ error: 'passkey signer timed out' }} }});
      }}, TIMER_MS);
      var req = {{
        __yfido2: 1, kind: 'request', id: id, path: path,
        token: TOKEN, origin: location.origin, payload: body,
      }};
      if (ensureHelper()) dispatch(req);
      else resolve({{ ok: false, body: {{ error: 'passkey helper could not be opened' }} }});
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
  // ⛔ CONDITIONAL MEDIATION IS OURS TO ANSWER (the human-UX directive,
  // 2026-08-28). Sites that offer passkey autofill probe this first and
  // call get({{mediation:'conditional'}}) on input focus; answering false is
  // how a browser says "no passkey autofill here", which is exactly the
  // report we were getting. A conditional get parks the same ceremony as
  // any other — the vault pane dialog is the autofill surface.
  window.PublicKeyCredential.isConditionalMediationAvailable =
    function () {{ return Promise.resolve(true); }};
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
        assert!(rp_id_matches_origin(
            "github.com",
            "https://github.com:443/x"
        ));
        // A page cannot claim a parent it is not under, nor an unrelated RP.
        assert!(!rp_id_matches_origin("github.com", "https://evil.com"));
        assert!(!rp_id_matches_origin("github.com", "https://notgithub.com"));
        assert!(!rp_id_matches_origin(
            "github.com",
            "https://github.com.evil.com"
        ));
    }

    #[test]
    fn origin_host_strips_scheme_and_port() {
        assert_eq!(
            origin_host("https://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            origin_host("https://example.com:8443/a/b").as_deref(),
            Some("example.com")
        );
        assert_eq!(origin_host("about:blank"), None);
    }

    #[test]
    fn a_grant_wakes_a_parked_ceremony_and_is_consumed() {
        let signer = Signer::new(1234, "sess".into());
        signer.register_ceremony(
            "req-1",
            "example.com",
            "https://example.com",
            "get",
            &[],
            json!(null),
        );

        // Grant for a live ceremony succeeds, carrying the chosen account, and
        // is idempotent on repeat.
        let (status, _) = signer.handle_grant(
            &json!({ "request_id": "req-1", "user_verified": true, "credential_id": "cred-b" }),
        );
        assert_eq!(status, 200);
        // The outcome is now set; a second grant is a no-op, never a 500.
        let (status, body) = signer.handle_grant(&json!({ "request_id": "req-1" }));
        assert_eq!(status, 200);
        assert_eq!(body["already"], true);

        // The parked side consumes it exactly once, with the picked account.
        assert!(matches!(
            signer.wait_for_outcome("req-1"),
            Some((Outcome::Granted { user_verified: true, credential_id: Some(id) }, _)) if id == "cred-b"
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
    fn the_shim_uses_the_appctl_bridge_and_the_token_and_overrides_get() {
        let signer = Signer::new(54321, "sess".into());
        let js = signer.shim_userscript();
        // The bridge scheme, NOT a raw http-loopback URL (mixed content).
        assert!(js.contains("yggterm-appctl://signer"));
        assert!(!js.contains("http://127.0.0.1"));
        assert!(js.contains(&signer.token));
        assert!(js.contains("shim.get = function"));
        assert!(js.contains("/fido2/get"));
        // The private key never appears; only public wire fields are handled.
        assert!(!js.contains("keyValue") && !js.contains("private"));
    }
}
