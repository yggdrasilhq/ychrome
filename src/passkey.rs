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
//! Signer --presence outbox-->  the session's VIEW CLIENT     (rpId + account)
//! client --OSC 7717 ; fido2 ; request-->  yggterm GUI        (on the row's PTY)
//! yggterm --native presence dialog-->  user clicks Approve
//! yggterm --POST /fido2/grant (ssh -L)-->  Signer            (request_id)
//! Signer --agent fido2-assert-->  ychrome-vault agent        (mints UserPresence, signs)
//! Signer --assertion-->  shim  --PublicKeyCredential-->  page
//! ```
//!
//! ## ⛔ WHY THE REQUEST IS QUEUED AND NOT WRITTEN TO STDOUT
//!
//! The GUI routes a `fido2 ; request` by the STREAM it arrives on, so the OSC
//! has to be written to the owning session's PTY. The signer does not hold that
//! PTY. It lives in the HOST DAEMON, one process per host serving every
//! session's control endpoint, and a daemon's stdout is `/dev/null` — so a
//! `write!(stdout, ...)` here published the ceremony into nothing. No dialog was
//! ever raised, the ceremony parked on the condvar for the full
//! [`CEREMONY_TIMEOUT`], and the page reported a generic failure that read, to
//! the user and to the next reader of this code, as a broken button.
//!
//! The process that DOES hold the session's PTY is its view client, the
//! foreground `ychrome` whose stdout is the row. So the signer queues the OSC
//! and the client drains it on its tick and writes it to its OWN stdout. The
//! byte sequence and yggterm's parser are unchanged; only the fd it is written
//! to is now the one the GUI reads.
//!
//! ⭐ **And a queue nobody drains is the same silence with extra steps.** A
//! session is presence-reachable only while a client has drained it recently
//! ([`PRESENCE_STALE`]); otherwise a ceremony is refused AT ONCE, naming the
//! reason, instead of parking two minutes on a dialog that can never appear.
//! That is the same skew honesty the daemon's routing already practises: an
//! endpoint is capable once it has been SEEN to be, never because it should be.
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
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// How long a `/fido2/get` blocks for the user to approve before giving up. A
/// ceremony the user ignores must not pin a control-server thread forever.
const CEREMONY_TIMEOUT: Duration = Duration::from_secs(120);

/// How recently a view client must have drained the presence outbox for this
/// session to count as reachable.
///
/// The client drains on its own surface tick, far faster than this; the window
/// is sized against the DAEMON's session expiry, not against the tick, so a
/// client that died a moment ago is not still treated as a place a dialog could
/// appear. Generous enough that a stalled tick does not refuse a real ceremony,
/// short enough that a closed surface stops pretending.
const PRESENCE_STALE: Duration = Duration::from_secs(15);

/// What the GUI dialog delivered for a pending ceremony.
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
///
/// It carries what it is FOR — the site, the kind, and the accounts on offer —
/// because the native presence dialog is not the only place a human can answer
/// one. The vault pane offers the same ceremony, and a pane that had to guess
/// which site was asking would be guessing about consent.
struct Ceremony {
    outcome: Option<Outcome>,
    rp_id: String,
    kind: String,
    /// `[{credential_id, label}]`, exactly as the presence OSC carries them.
    /// Labels and public ids only: this is offered to a schema.
    accounts: Vec<Value>,
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
    /// routes the OSC by the STREAM it arrived on, not this field, which is
    /// exactly why the OSC cannot be written from here (see the module docs).
    session: String,
    pending: Mutex<HashMap<String, Ceremony>>,
    cvar: Condvar,
    /// Presence-request OSCs waiting for this session's view client to publish
    /// on the row's PTY. Written by a ceremony, drained by the client's tick.
    outbox: Mutex<Vec<String>>,
    /// When a view client last drained [`Signer::outbox`]. `None` means no
    /// client has ever spoken the op — either none is attached, or the attached
    /// one predates the presence channel and would drop the request in silence.
    last_drain: Mutex<Option<Instant>>,
    /// How the last ceremony ended, for the pane to report.
    ///
    /// ⭐ **A registration that says nothing is a registration you cannot
    /// trust.** `fido2-create` mints and stores a passkey correctly and the
    /// browser then went silent, so the only evidence a user had that their new
    /// passkey existed was that nothing had visibly failed. One outcome, not a
    /// log: the question this answers is "did the thing I just did work", and
    /// it stops being asked within a minute.
    last_outcome: Mutex<Option<Completed>>,
}

/// How a ceremony ended, and when — [`PRESENCE_STALE`]'s sibling for the
/// reporting side.
struct Completed {
    kind: String,
    rp_id: String,
    label: String,
    at: Instant,
}

/// How long a finished ceremony is still worth reporting in the pane. Long
/// enough to survive walking back to the sidebar, short enough that it is never
/// mistaken for the state of a LATER sign-in.
const OUTCOME_FRESH: Duration = Duration::from_secs(120);

impl Signer {
    pub fn new(port: u16, session: String) -> Arc<Self> {
        Arc::new(Signer {
            token: hex_token(32),
            port,
            session,
            pending: Mutex::new(HashMap::new()),
            cvar: Condvar::new(),
            outbox: Mutex::new(Vec::new()),
            last_drain: Mutex::new(None),
            last_outcome: Mutex::new(None),
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

    /// Take every queued presence OSC, and record that a client asked.
    ///
    /// The DRAIN is what makes this session presence-reachable — not the
    /// client's registration, and not the fact that a surface is open. Only a
    /// client that speaks this op can put the sequence on the PTY, so only a
    /// client that has spoken it is evidence that a dialog can be raised. An
    /// empty drain still counts: it is the liveness signal, and it is the
    /// common case.
    pub fn drain_presence(&self) -> Vec<String> {
        *self.last_drain.lock().unwrap() = Some(Instant::now());
        std::mem::take(&mut *self.outbox.lock().unwrap())
    }

    /// Can a presence dialog actually be raised for this session right now?
    pub fn presence_reachable(&self) -> bool {
        matches!(*self.last_drain.lock().unwrap(), Some(at) if at.elapsed() <= PRESENCE_STALE)
    }

    /// Queue one presence request for the view client to publish. See
    /// [`fido2_request_osc`] for the payload and the module docs for why this
    /// is a queue rather than a write.
    fn publish_presence_request(
        &self,
        request_id: &str,
        rp_id: &str,
        accounts: &[Value],
        kind: &str,
        origin: &str,
    ) {
        let osc = fido2_request_osc(&self.session, request_id, rp_id, accounts, kind, origin);
        self.outbox.lock().unwrap().push(osc);
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
            Err(GetError::NoPresenceChannel) => (503, json!({ "error": NO_PRESENCE_CHANNEL })),
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

        // The bytes the RP will re-hash: whatever we sign, we return verbatim.
        // Independent of which account — computed once.
        let client_data_json = format!(
            r#"{{"type":"webauthn.get","challenge":{},"origin":{},"crossOrigin":false}}"#,
            json_string(challenge),
            json_string(origin),
        );
        let client_data_hash = Sha256::digest(client_data_json.as_bytes());

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
        if !self.presence_reachable() {
            return Err(GetError::NoPresenceChannel);
        }
        let request_id = hex_token(16);
        self.register(&request_id, rp_id, "get", &accounts);
        self.publish_presence_request(&request_id, rp_id, &accounts, "get", origin);
        let outcome = self.wait_for_outcome(&request_id);

        let (user_verified, chosen_id) = match outcome {
            Some(Outcome::Granted {
                user_verified,
                credential_id,
            }) => (user_verified, credential_id),
            Some(Outcome::Denied) => return Err(GetError::Denied),
            None => return Err(GetError::TimedOut),
        };

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

        self.note_outcome("get", rp_id, &account_label(candidate));
        Ok(json!({
            "credentialId": credential_id_rp,
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
            Err(GetError::TimedOut) => {
                (408, json!({ "error": "the user did not respond in time" }))
            }
            Err(GetError::NoPresenceChannel) => (503, json!({ "error": NO_PRESENCE_CHANNEL })),
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
        if !self.presence_reachable() {
            return Err(GetError::NoPresenceChannel);
        }
        self.register(&request_id, rp_id, "create", &accounts);
        self.publish_presence_request(&request_id, rp_id, &accounts, "create", origin);
        let user_verified = match self.wait_for_outcome(&request_id) {
            Some(Outcome::Granted { user_verified, .. }) => user_verified,
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

        self.note_outcome("create", rp_id, label);
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

    fn register(&self, request_id: &str, rp_id: &str, kind: &str, accounts: &[Value]) {
        self.pending.lock().unwrap().insert(
            request_id.to_string(),
            Ceremony {
                outcome: None,
                rp_id: rp_id.to_string(),
                kind: kind.to_string(),
                accounts: accounts.to_vec(),
            },
        );
    }

    /// Record how a ceremony ended. Called on the paths that finish one, so a
    /// user who approved something has somewhere to see that it worked.
    fn note_outcome(&self, kind: &str, rp_id: &str, label: &str) {
        *self.last_outcome.lock().unwrap() = Some(Completed {
            kind: kind.to_string(),
            rp_id: rp_id.to_string(),
            label: label.to_string(),
            at: Instant::now(),
        });
    }

    /// The last ceremony's result while it is still worth reporting, as
    /// `{kind, rp_id, label}`. `None` once it has aged out.
    pub fn recent_outcome(&self) -> Option<Value> {
        let outcome = self.last_outcome.lock().unwrap();
        let outcome = outcome.as_ref().filter(|done| done.at.elapsed() <= OUTCOME_FRESH)?;
        Some(json!({
            "kind": outcome.kind,
            "rp_id": outcome.rp_id,
            "label": outcome.label,
        }))
    }

    /// Start a ceremony with no thread parked on it — TEST BUILDS ONLY.
    ///
    /// It does BOTH halves a real ceremony does, register and publish, because
    /// they are one act: a ceremony registered but never published is a state
    /// production cannot reach, and a test that could reach it would be
    /// rehearsing a shape nobody ships.
    ///
    /// `register` and `publish_presence_request` stay private because in
    /// production a registered ceremony always has a `/fido2/*` thread parked
    /// on it; one registered without a waiter would sit in the pane offering a
    /// sign-in that answers nobody.
    #[cfg(test)]
    pub(crate) fn park_ceremony_for_test(
        &self,
        request_id: &str,
        rp_id: &str,
        kind: &str,
        accounts: &[Value],
    ) {
        self.register(request_id, rp_id, kind, accounts);
        self.publish_presence_request(request_id, rp_id, accounts, kind, "https://example.com");
    }

    /// Stamp an outcome without running a ceremony — TEST BUILDS ONLY, for the
    /// pane's report of what just happened.
    #[cfg(test)]
    pub(crate) fn note_outcome_for_test(&self, kind: &str, rp_id: &str, label: &str) {
        self.note_outcome(kind, rp_id, label);
    }

    /// Ceremonies still waiting on a human, for the vault pane to offer.
    ///
    /// ⭐ **Why the pane offers them at all, when a native dialog exists.** The
    /// dialog is one window and one moment: dismiss it, or miss it behind
    /// another window, and the ceremony is still parked but nothing on screen
    /// says so. The pane is where the user goes when a sign-in did not work, so
    /// it is where the unanswered question belongs.
    ///
    /// Secret-free, like the OSC it mirrors: the `request_id` IS the credential
    /// that authenticates a grant, so it travels only to the GUI-gated pane
    /// route, never into a page-reachable one.
    pub fn pending_ceremonies(&self) -> Vec<Value> {
        let pending = self.pending.lock().unwrap();
        let mut open: Vec<Value> = pending
            .iter()
            .filter(|(_, ceremony)| ceremony.outcome.is_none())
            .map(|(request_id, ceremony)| {
                json!({
                    "request_id": request_id,
                    "rp_id": ceremony.rp_id,
                    "kind": ceremony.kind,
                    "accounts": ceremony.accounts,
                })
            })
            .collect();
        // A HashMap iterates in an arbitrary order, and a pane that reshuffles
        // its own buttons between two renders is one a user cannot click
        // reliably. Order by the id, which is stable for the ceremony's life.
        open.sort_by(|a, b| a["request_id"].as_str().cmp(&b["request_id"].as_str()));
        open
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
    /// No view client is draining this session's presence outbox, so the
    /// approval dialog cannot be raised and no grant can ever arrive. Refused
    /// at once rather than parked for [`CEREMONY_TIMEOUT`]: a two-minute wait
    /// followed by a generic failure is indistinguishable from a broken
    /// button, which is how this defect survived a full session undiagnosed.
    NoPresenceChannel,
    Bad(String),
}

/// What the page is told when the presence channel is down. ONE owner, because
/// `get` and `create` must not describe the same fault two ways — and because
/// this string is the only diagnosis a user ever sees for it.
const NO_PRESENCE_CHANNEL: &str = "this browser cannot ask you to approve the passkey: \
     no ychrome view client is publishing presence requests for this session. \
     Reopen the page in a ychrome web surface.";

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
///
/// Returns the sequence rather than writing it: the process that must write it
/// is the session's view client, not this one. See the module docs.
fn fido2_request_osc(
    session: &str,
    request_id: &str,
    rp_id: &str,
    accounts: &[Value],
    kind: &str,
    origin: &str,
) -> String {
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
    format!("\u{1b}]7717;fido2;request;{encoded}\u{7}")
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
fn shim_js(port: u16, token: &str) -> String {
    // The shim reaches the signer through yggterm's `yggterm-appctl://` bridge,
    // NOT `http://127.0.0.1:{port}` directly: WebKitGTK blocks an https page from
    // fetching http-loopback (mixed content). yggterm registers the scheme as
    // secure and proxies it to this app's control endpoint. The port is unused in
    // the page (the GUI knows which signer to route to); the token still gates.
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


    /// Rust source with `//` comments stripped.
    ///
    /// ⛔ **A source guard that reads comments is not a guard.** Three written
    /// in this repo were vacuous and one was satisfied by the explanatory
    /// comment sitting directly above the call it was meant to police — the
    /// prose describing the bug kept the test green while the bug was present.
    /// Every assertion below runs on this, never on the raw file.
    ///
    /// It also stops at `#[cfg(test)]`. The guard is about what the SIGNER
    /// does, and a test's own assertion message naming the forbidden call is
    /// not the signer making it — the first spelling of this failed on its own
    /// error string, which is a guard measuring the wrong file.
    fn code_only(source: &str) -> String {
        source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(source)
            .lines()
            .map(|line| match line.find("//") {
                Some(at) => &line[..at],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The guard's own guard: prove `code_only` actually removes the thing that
    /// fooled the earlier attempts, so the assertions below rest on something
    /// that has been seen to work.
    #[test]
    fn code_only_drops_a_comment_that_would_satisfy_a_guard() {
        let source = "// let mut stdout = std::io::stdout().lock();\nlet x = 1;";
        assert!(!code_only(source).contains("stdout"));
        assert!(code_only(source).contains("let x = 1;"));
        // And it stops at the test module, so a test's own prose is out of scope.
        assert!(!code_only("let x = 1;\n#[cfg(test)]\nmod tests { stdout }").contains("stdout"));
    }

    /// ⛔ THE REGRESSION THIS FILE EXISTS TO PREVENT. The signer runs inside the
    /// host daemon, whose stdout is `/dev/null`; a presence request written
    /// there reaches nobody and the ceremony parks for two minutes. The request
    /// is queued for the session's view client instead, and this module must not
    /// touch stdout at all.
    #[test]
    fn the_signer_never_writes_the_presence_request_to_stdout() {
        let code = code_only(include_str!("passkey.rs"));
        assert!(
            !code.contains("stdout"),
            "passkey.rs must not touch stdout: the signer lives in the daemon, \
             whose stdout is /dev/null, so a request written there is a ceremony \
             nobody can ever approve"
        );
    }

    /// A queued ceremony must actually be handed to whoever drains it, exactly
    /// once, as the OSC yggterm parses.
    #[test]
    fn a_presence_request_is_queued_for_the_client_and_drained_once() {
        let signer = Signer::new(1234, "sess-7".into());
        assert!(signer.drain_presence().is_empty());

        signer.publish_presence_request(
            "req-9",
            "example.com",
            &[json!({ "credential_id": "cred-a", "label": "someone@example.com" })],
            "get",
            "https://example.com",
        );
        let drained = signer.drain_presence();
        assert_eq!(drained.len(), 1);
        assert!(drained[0].starts_with("\u{1b}]7717;fido2;request;"));
        assert!(drained[0].ends_with('\u{7}'));
        // Drained means taken: a re-drain must not replay a ceremony the user
        // has already been asked about.
        assert!(signer.drain_presence().is_empty());
    }

    /// The payload carries what the dialog needs and nothing the page could use.
    #[test]
    fn the_presence_payload_names_the_accounts_and_carries_no_secret() {
        let osc = fido2_request_osc(
            "sess-7",
            "req-9",
            "example.com",
            &[json!({ "credential_id": "cred-a", "label": "someone@example.com" })],
            "get",
            "https://example.com",
        );
        let encoded = osc
            .trim_start_matches("\u{1b}]7717;fido2;request;")
            .trim_end_matches('\u{7}');
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("the OSC payload is base64");
        let payload: Value = serde_json::from_slice(&decoded).expect("the payload is JSON");
        assert_eq!(payload["request_id"], "req-9");
        assert_eq!(payload["rp_id"], "example.com");
        assert_eq!(payload["kind"], "get");
        assert_eq!(payload["account"], "someone@example.com");
        assert_eq!(payload["accounts"][0]["credential_id"], "cred-a");
        // Never the challenge, never a key.
        let raw = String::from_utf8(decoded).unwrap();
        assert!(!raw.contains("challenge") && !raw.contains("private"));
    }

    /// ⭐ SKEW HONESTY. A session nobody drains cannot raise a dialog, so a
    /// ceremony there is refused AT ONCE and named — never parked for
    /// `CEREMONY_TIMEOUT` and then reported as a generic failure, which is what
    /// a broken button looks like to a user and to the next reader of this code.
    #[test]
    fn a_session_with_no_draining_client_is_not_presence_reachable() {
        let signer = Signer::new(1234, "sess-7".into());
        assert!(
            !signer.presence_reachable(),
            "a signer nobody has drained must not claim it can raise a dialog"
        );
        signer.drain_presence();
        assert!(
            signer.presence_reachable(),
            "a client that drained is the evidence a dialog can be raised"
        );
    }

    /// And the refusal must be REACHED, not merely available: `create` and `get`
    /// both check before registering a ceremony. A `create` on an unreachable
    /// session answers 503 with the shared wording rather than blocking.
    #[test]
    fn create_refuses_immediately_when_no_client_is_draining() {
        let signer = Signer::new(1234, "sess-7".into());
        let started = Instant::now();
        let (status, body) = signer.handle_create(&json!({
            "origin": "https://example.com",
            "rp": { "id": "example.com", "name": "Example" },
            "challenge": "Y2hhbGxlbmdl",
            "user": { "id": "dXNlcg", "name": "someone", "displayName": "Someone" },
        }));
        assert_eq!(status, 503);
        assert_eq!(body["error"], NO_PRESENCE_CHANNEL);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the refusal must be immediate, not a park on the ceremony timeout"
        );
        // Refused before it was registered: nothing is left parked to be woken.
        assert!(signer.pending.lock().unwrap().is_empty());
    }

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
        signer.register("req-1", "example.com", "get", &[]);

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
            Some(Outcome::Granted { user_verified: true, credential_id: Some(id) }) if id == "cred-b"
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
