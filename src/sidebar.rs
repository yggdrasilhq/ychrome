//! ychrome's SIDEBAR CONTRIBUTION: the vault and settings panes, owned by ychrome.
//!
//! yggterm used to hardcode a `RightPanelMode::Vault` and a `::AppSidebar` — app
//! chrome living in the platform, which is the anti-pattern the libyggterm
//! contract exists to prevent. Instead ychrome *declares* both panes over
//! `OSC 7717 ; sidebar` and serves their content from a loopback control endpoint
//! on the host ychrome runs on. yggterm draws generic widgets and knows nothing
//! about vaults or ad blocking.
//!
//! ```text
//! ychrome  --OSC 7717 sidebar;declare-->  yggterm GUI  (control url, panes, policy stamp)
//! yggterm  --GET  <control>/pane/<id>-->  ychrome      (schema; no secrets)
//! yggterm  --GET  <control>/policy---->   ychrome      (adblock rules + userscripts)
//! yggterm  --POST <control>/action---->   ychrome      (schema? toast? eval? reload_surface?)
//! ```
//!
//! **The vault never crosses the OSC.** A 1100-row item list would not fit on a
//! PTY, and a secret must never sit in a declaration. The GUI fetches the schema
//! itself, and a credential reaches the page only as an `eval` script the GUI
//! injects into the surface — the app computes, the GUI injects.
//!
//! State is host-resident: the unlocked vault lives in this host's
//! `ychrome-vault` agent, and the web-content policy in this host's
//! `~/.yggterm/web-adblock` + `web-userscripts` — which over ssh is the REMOTE
//! host, not the GUI's. See [`crate::webpolicy`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use ychrome_vault_proto::CIPHER_TYPE_CARD;

/// The pane ids ychrome declares. yggterm only ever echoes them back.
const VAULT_PANE: &str = "vault";
/// ychrome's own settings: ad blocking and userscripts, both owned by the host
/// ychrome runs on. yggterm used to hardcode this as `RightPanelMode::AppSidebar`.
const SETTINGS_PANE: &str = "settings";
/// Rows the pane shows before the user narrows with the search box. The vault
/// has ~1100 items; rendering them all would make the panel unusable and the
/// schema enormous.
const MAX_ROWS: usize = 80;
/// Separates an item's name from its username in a row id. `\x1f` (unit
/// separator) cannot occur in either — a vault name may contain tabs and
/// newlines, so a printable separator would be ambiguous.
const ROW_SEP: char = '\u{1f}';

/// This host's vault directory (`~/.yggterm/vault`), where the agent's socket
/// and secret-free config live.
fn vault_dir() -> Result<std::path::PathBuf> {
    ychrome_vault_proto::default_dir()
}

/// Send one op to this host's vault agent and return its reply.
///
/// The browser speaks the agent's unix socket DIRECTLY through the crypto-free
/// [`ychrome_vault_proto`] — it no longer spawns the `ychrome-vault` CLI per
/// operation. The agent is host-resident and caches the unlocked vault, so a
/// read is cheap and keyless once the user has unlocked. The workspace still
/// keeps the crypto in `ychrome-vault`; only the wire (this op) is shared.
fn vault_op(op: Value) -> Result<Value> {
    with_readable_error(ychrome_vault_proto::request(&vault_dir()?, &op))
}

/// As [`vault_op`], but starts an agent first if none is listening — the
/// `unlock` path, where the spawn is the point.
fn vault_op_autostart(op: Value) -> Result<Value> {
    with_readable_error(ychrome_vault_proto::request_autostart(&vault_dir()?, &op))
}

/// The lock/staleness status the pane gates on — a running agent is
/// authoritative, else the secret-free config answers (see
/// [`ychrome_vault_proto::status`]).
fn vault_status() -> Result<Value> {
    ychrome_vault_proto::status(&vault_dir()?)
}

/// Said ONCE per process, not once per `/policy` fetch — the GUI refetches
/// whenever the policy stamp moves, and a line that repeats forever is a line
/// nobody reads (the same reasoning as the daemon's staleness notice).
///
/// ⚠ THIS LINE IS NOT THE USER-FACING SURFACE, AND ON 2026-08-01 IT REACHED
/// NOBODY. ychrome's stderr is the PTY it was launched in; it is not in
/// `~/.yggterm`, not in the GUI's `app-launch-logs`, and not anywhere an
/// operator greps. The user met the failure at a Forgejo 2FA prompt as "Your
/// browser does not currently support WebAuthn" while this line existed and had
/// nowhere to land. [`passkey_shim_widgets`] is the surface that answers that;
/// this stays for the log-reading case and is deliberately secondary.
fn announce_vault_agent_predates_passkey_scoping() {
    static ANNOUNCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ANNOUNCED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    eprintln!(
        "ychrome: the running vault agent predates the `passkey-hosts` op, so the \
         WebAuthn shim is installed on NO site and passkey logins will not work."
    );
    eprintln!("ychrome: hand the agent over to the current binary:  ychrome-vault handover");
}

/// Whether the WebAuthn shim is installable, and when it is not, WHY.
///
/// ONE owner for a fact two surfaces need: `/policy` turns it into match
/// patterns, and the vault pane turns it into something the user can read. They
/// must never derive it separately — a browser that silently disables passkeys
/// while the pane reports a healthy vault is exactly the 2026-08-01 failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PasskeyShimState {
    /// Installable, scoped to these rpIds (never empty).
    ScopedTo(Vec<String>),
    /// The vault is readable and holds no passkey at all. Correct and common:
    /// every page keeps a pristine `navigator`.
    NoStoredPasskeys,
    /// Locked, or no agent. A ceremony needs an unlocked agent anyway, so no
    /// shim is the honest state — but the user must be told, because the PAGE
    /// will blame their browser.
    VaultUnavailable(String),
    /// ⛔ THE ONE THAT LOOKS HEALTHY AND IS NOT. The agent is running and
    /// unlocked and simply does not know the op this browser needs, so passkeys
    /// are off on EVERY site while `status` reports a perfectly good vault.
    ///
    /// Measured on guihost 2026-08-01: `status` answered `state: unlocked`,
    /// `agent_stale: false`, 1116 items — and the same socket answered
    /// `unknown op "passkey-hosts"`. `agent_stale` compares the agent against
    /// the INSTALLED `ychrome-vault`, and both were the same six-day-old
    /// binary, so it is structurally incapable of catching this. Only asking
    /// for the op finds it.
    AgentPredatesBrowser,
}

/// Ask the vault where the shim may be installed. THE ONE call site of the
/// `passkey-hosts` probe (a lock in this file's tests pins that).
fn passkey_shim_state() -> PasskeyShimState {
    const RP_ID_PROBE_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

    let Ok(dir) = vault_dir() else {
        return PasskeyShimState::VaultUnavailable("no vault directory on this host".into());
    };
    let reply = match ychrome_vault_proto::request_with_timeout(
        &dir,
        &json!({ "op": "passkey-hosts" }),
        RP_ID_PROBE_BUDGET,
    ) {
        Ok(reply) => reply,
        Err(error) => {
            // The proto crate rewrites `unknown op` into a sentence naming the
            // cause; that rewrite is the only way to tell "too old" from
            // "locked", and the two need opposite remedies.
            if error.to_string().contains("predates this binary") {
                announce_vault_agent_predates_passkey_scoping();
                return PasskeyShimState::AgentPredatesBrowser;
            }
            return PasskeyShimState::VaultUnavailable(error.to_string());
        }
    };
    let rp_ids: Vec<String> = reply["rp_ids"]
        .as_array()
        .map(|hosts| {
            hosts
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if rp_ids.is_empty() {
        return PasskeyShimState::NoStoredPasskeys;
    }
    PasskeyShimState::ScopedTo(rp_ids)
}

/// What the vault pane says about the shim. Empty when there is nothing the
/// user needs to act on.
///
/// ⛔ THE PAGE CANNOT TELL THEM. A site whose ceremony finds no
/// `navigator.credentials` reports "your browser does not support WebAuthn",
/// which is true of the engine and useless as a remedy. This pane is already on
/// screen when that happens, and it is the only surface that knows why.
///
/// Secret-free: a count of hosts, never the host list. Which sites you hold
/// passkeys for is the user's business and does not belong in a schema that
/// crosses the OSC channel.
fn passkey_shim_widgets(state: &PasskeyShimState) -> Vec<Value> {
    match state {
        // Working, and silence is right: a banner on every render for a healthy
        // subsystem is a banner nobody reads when it finally matters.
        PasskeyShimState::ScopedTo(_) | PasskeyShimState::NoStoredPasskeys => Vec::new(),
        PasskeyShimState::AgentPredatesBrowser => vec![
            json!({"kind": "section", "text": "Passkeys are off"}),
            json!({
                "kind": "label", "muted": true,
                "text": "This host's vault agent is older than this browser and does not \
                         answer the request that decides where passkeys work, so passkey \
                         sign-in is disabled on every site. A site will tell you your \
                         browser does not support WebAuthn; this is the real reason. \
                         Install the current ychrome-vault on this host, then hand the \
                         agent over — it keeps the vault unlocked.",
            }),
            json!({
                "kind": "button", "id": "hand_over_agent", "action": "hand_over_agent",
                "primary": true, "label": "Hand the agent over (keeps the unlock)",
            }),
        ],
        PasskeyShimState::VaultUnavailable(reason) => vec![
            json!({"kind": "section", "text": "Passkeys are off"}),
            json!({
                "kind": "label", "muted": true,
                "text": format!(
                    "Passkey sign-in needs a reachable, unlocked vault on this host, and \
                     the shim is decided when a web surface OPENS. {reason}. Unlock the \
                     vault, then open the page in a new web surface.",
                ),
            }),
        ],
    }
}

/// A WebKit match pattern covering an rpId and its subdomains.
///
/// WebAuthn scopes a credential to the rpId and any subdomain of it (a passkey
/// for `example.com` is usable on `login.example.com`), so the pattern has to
/// admit both. Two patterns rather than one because `*://*.example.com/*` does
/// NOT match the bare `example.com` in WebKit's grammar.
pub(crate) fn rp_id_match_patterns(rp_id: &str) -> Vec<String> {
    let host = rp_id.trim().trim_matches('.').to_ascii_lowercase();
    // A pattern built from a host containing a separator would silently widen
    // the scope — `*://*./*` matches everything. Refuse rather than over-admit.
    if host.is_empty() || host.contains(['/', ':', '*', ' ', '?', '#']) {
        return Vec::new();
    }
    vec![format!("*://{host}/*"), format!("*://*.{host}/*")]
}

/// The passkey shim, installed ONLY for the hosts this vault holds a passkey
/// for. Empty means install nothing, and empty is the common, correct answer.
///
/// ⛔ A LOCKED OR UNREACHABLE VAULT INSTALLS NOTHING, AND THAT IS NOT A
/// REGRESSION. A ceremony needs an unlocked agent regardless, so a shim
/// installed over a locked vault could only ever fail the ceremony it advertised
/// — while still telling every page it touches that this browser has a platform
/// authenticator. Silence is both honest and strictly safer.
///
/// ⛔ THIS DOES NOT TOUCH THE USER-PRESENCE INVARIANT. Scoping decides WHERE
/// `navigator.credentials` is patched, nothing more. Every ceremony still goes
/// through `/fido2/*`, still needs the per-page bearer token, and still blocks
/// on an explicit GUI grant. An agent cannot approve its own ceremony, before
/// this change or after it.
///
/// ⛔ TIGHTLY BOUNDED, BECAUSE `/policy` IS ON A PATH THAT ALREADY FAILS BADLY.
/// A surface whose policy fetch fails is created UNBLOCKED and runs without
/// adblock or userscripts for its whole life (`yggterm/docs/web-surfaces.md`),
/// so making this route wait on another process is how a slow vault agent turns
/// into an unprotected browser. Measured while adding it: the default read
/// budget pushed `tests/daemon_staleness.rs` from 8.5 s to 23-30 s and made its
/// control-endpoint reads time out. The agent is host-local and answers in
/// microseconds when it is there at all, so anything slower is a failure, and a
/// failure means no shim.
fn passkey_shim_scripts(state: &ControlState) -> Vec<crate::userscript::Userscript> {
    // ⛔ ONE FAILURE HERE IS NOT LIKE THE OTHERS, AND IT MUST NOT BE SILENT. A
    // locked or absent vault installing no shim is correct — a ceremony needs an
    // unlocked agent anyway. But an agent that simply PREDATES this op is
    // unlocked and working, and answering it with silence turns passkeys off
    // everywhere while looking exactly like the healthy case. That is the
    // deploy-ordering hazard of this change, it reached the user on 2026-08-01,
    // and the remedy is [`passkey_shim_widgets`] in the pane — a stderr line
    // reached nobody.
    let patterns: Vec<String> = match passkey_shim_state() {
        PasskeyShimState::ScopedTo(rp_ids) => rp_ids
            .iter()
            .flat_map(|rp_id| rp_id_match_patterns(rp_id))
            .collect(),
        PasskeyShimState::NoStoredPasskeys
        | PasskeyShimState::VaultUnavailable(_)
        | PasskeyShimState::AgentPredatesBrowser => Vec::new(),
    };
    if patterns.is_empty() {
        return Vec::new();
    }
    // MAIN world, not the isolated default every user script gets: the shim
    // exists to be called BY THE PAGE (`navigator.credentials`), and a patch
    // installed in an isolated world is invisible from the page that needs it.
    let mut script =
        crate::userscript::Userscript::new(state.signer.shim_userscript()).in_main_world();
    script.matches = patterns;
    vec![script]
}

/// Re-word the one failure the user can act on. The agent replies "the vault is
/// locked"; point them at the fix, exactly as the old CLI shell-out did.
fn with_readable_error(result: Result<Value>) -> Result<Value> {
    result.map_err(|error| {
        if error.to_string().contains("locked") {
            anyhow::anyhow!("vault locked: run `ychrome-vault unlock` on this host")
        } else {
            error
        }
    })
}

/// A vault op field: the string when non-empty, JSON `null` otherwise — the same
/// present-or-null shape the `ychrome-vault` CLI sent, so the agent sees no
/// difference between this path and a shell invocation.
fn opt_field(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        Value::String(value.to_string())
    }
}

/// Default length of a generated password, mirroring `ychrome-vault generate`.
const DEFAULT_GENERATE_LENGTH: i64 = 20;
const MIN_GENERATE_LENGTH: i64 = 8;
const MAX_GENERATE_LENGTH: i64 = 128;

/// The Add tab's draft. It lives HERE, not in the GUI: yggterm's copy of a
/// pane's field values is only the user's edits since the last schema, and the
/// app re-declares them on every render.
///
/// The password is deliberately absent. It reaches this process as one action's
/// `values.add_password`, goes straight to `ychrome-vault add`'s stdin, and is
/// dropped — it is never stored, never echoed into a schema.
#[derive(Default)]
struct AddDraft {
    name: String,
    user: String,
    uri: String,
    folder: String,
    notes: String,
    /// The page host this draft was seeded from, so re-entering the tab on the
    /// same site does not clobber what the user typed, and browsing to a new
    /// site does re-seed.
    seeded_host: Option<String>,
}

/// What the pane is currently showing. Host-resident, like everything else the
/// app owns: yggterm holds no vault state, not even which tab is selected.
pub(crate) struct PaneState {
    /// The profile this ychrome is running. The settings pane needs it to show
    /// the per-profile adblock override, and `/policy` needs it to decide which
    /// userscripts apply.
    profile: String,
    tab: String,
    query: String,
    add: AddDraft,
    generate_length: i64,
    generate_no_symbols: bool,
    /// The last watchtower scan. Labels only — the report type cannot carry a
    /// password (see `ychrome_vault::watchtower`).
    watchtower: Option<Value>,
}

impl Default for PaneState {
    fn default() -> Self {
        PaneState {
            profile: "default".to_string(),
            tab: "fill".to_string(),
            query: String::new(),
            add: AddDraft::default(),
            generate_length: DEFAULT_GENERATE_LENGTH,
            generate_no_symbols: false,
            watchtower: None,
        }
    }
}

impl PaneState {
    fn new(profile: &str) -> Self {
        PaneState {
            profile: profile.to_string(),
            ..PaneState::default()
        }
    }

    /// Seed the Add draft from the page the user is looking at, once per host.
    /// The old hardcoded pane only offered this as a placeholder; naming the
    /// item after the host is what makes fill-matching find it later.
    fn seed_add_draft(&mut self, host: Option<&str>) {
        let host = host.filter(|host| !host.is_empty());
        if self.add.seeded_host.as_deref() == host {
            return;
        }
        self.add = AddDraft {
            name: host.unwrap_or_default().to_string(),
            uri: host
                .map(|host| format!("https://{host}"))
                .unwrap_or_default(),
            seeded_host: host.map(str::to_string),
            ..AddDraft::default()
        };
    }
}

/// The control endpoint's per-session state: the pane draft (behind a lock,
/// mutated by actions) and the passkey signer (its own internal locks). The
/// host daemon owns one of these per registered session — the per-invocation
/// control server is gone; the daemon serves every session's endpoint from one
/// process (see [`crate::daemon`]).
pub(crate) struct ControlState {
    pub(crate) pane: Mutex<PaneState>,
    pub(crate) signer: Arc<crate::passkey::Signer>,
    /// The GUI's bearer token for this session's control endpoint, presented as
    /// `X-Ychrome-Control` on every [`RouteAccess::GuiOnly`] route.
    ///
    /// **Why a second token when the signer already has one.** The signer's
    /// token is baked into the shim userscript, so every PAGE in the profile
    /// holds it by construction — reusing it here would gate nothing. This one
    /// travels the other way: the daemon mints it, the register reply hands it
    /// to the client, and the client puts it in the `sidebar ; declare` OSC,
    /// which reaches the GUI over the PTY stream. A page cannot read a PTY, and
    /// the GUI's bridge (`yggterm-appctl://`) forwards only the signer's header,
    /// so a page has no path to this value. Same provenance as the passkey
    /// `request_id` that authenticates `/fido2/grant`: an OSC-delivered secret.
    ///
    /// The boundary this enforces is the WEB one, exactly as in
    /// [`crate::passkey`]: a same-uid process on this host can read the daemon's
    /// memory or the vault socket anyway, so it was never the threat model.
    pub(crate) control_token: String,
}

impl ControlState {
    /// `session` is the emitting `YGGTERM_SESSION_ID` (the routing `env_id`),
    /// carried in the passkey OSC for diagnostics — the GUI routes by stream.
    /// `port` is the daemon's per-session control listener port (the passkey
    /// shim ignores it; it reaches the signer through the `yggterm-appctl://`
    /// bridge, so the port need not be page-reachable).
    pub(crate) fn new(profile: &str, session: &str, port: u16) -> ControlState {
        ControlState {
            pane: Mutex::new(PaneState::new(profile)),
            signer: crate::passkey::Signer::new(port, session.to_string()),
            control_token: crate::passkey::hex_token(32),
        }
    }

    /// Does this request carry the GUI's control token? THE ONE OWNER of that
    /// question — the gate below and the daemon's `/ping` drain both ask it here
    /// rather than re-comparing the field, because two comparisons of one secret
    /// are two places to get it wrong and only one of them would be under test.
    pub(crate) fn gui_authorized(&self, req: &ParsedRequest) -> bool {
        req.control_token.as_deref() == Some(self.control_token.as_str())
    }
}

/// Who may call a control route — the ONE table both the gate and the CORS
/// headers read, so "is this route page-reachable" cannot be answered two ways.
///
/// The default is [`RouteAccess::GuiOnly`]: a route added later is gated unless
/// it says otherwise. That is the whole point of naming this — the hole this
/// enum closes existed because `POST /action` was simply never considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteAccess {
    /// Read-only, no secrets, no state change: `GET /policy`, `GET /zoom`,
    /// `GET /ping`. **These stay open deliberately.** A policy body is the
    /// profile's adblock ruleset + userscripts — the very content the GUI is
    /// about to inject into the page, so the page can already see all of it;
    /// the zoom map is a host→percent table; `/ping` is liveness stamps. None
    /// of the three is a capability or a secret, and keeping them open is what
    /// lets an OLDER GUI keep its rail alive and its surfaces ad-blocked across
    /// a mixed-version deploy. (`/ping`'s command DRAIN is a different fact and
    /// is gated separately — see [`crate::daemon`]'s `ping_reply`.)
    Open,
    /// Page-origin by design: the signer routes the shim calls
    /// (`POST /fido2/get`, `POST /fido2/create`). Gated by the signer's OWN
    /// bearer token, which every page holds; the cross-page boundary is the
    /// origin↔rpId check. These are the only routes that get a CORS wildcard.
    PageSigner,
    /// GUI dialog → app, authenticated by the unguessable per-ceremony
    /// `request_id` (`POST /fido2/grant`, `POST /fido2/deny`). Not page
    /// reachable, so no CORS — but NOT control-token gated either: the
    /// request_id already is the credential and `/fido2` is deliberately
    /// unchanged by this gate.
    Ceremony,
    /// Everything else: `POST /action` (which can unlock the vault, fill a
    /// credential into the page, disable ad blocking, delete a userscript) and
    /// `GET /pane/<id>` (which lists vault item names and seeds the Add draft).
    /// Only the GUI may call these, proven by the control token.
    GuiOnly,
}

/// Classify a route. Path-first, because a CORS preflight arrives as `OPTIONS`
/// on the path it is asking about and must be classified identically.
pub(crate) fn route_access(path: &str) -> RouteAccess {
    match path {
        "/policy" | "/zoom" | "/ping" => RouteAccess::Open,
        "/fido2/get" | "/fido2/create" => RouteAccess::PageSigner,
        "/fido2/grant" | "/fido2/deny" => RouteAccess::Ceremony,
        _ => RouteAccess::GuiOnly,
    }
}

/// Must this request prove it is the GUI? [`route_access`] answers for the PATH;
/// this adds the METHOD, and the difference is load-bearing.
///
/// [`RouteAccess::Open`]'s entire justification is that the three paths are
/// **reads** — a policy body, a zoom map, liveness stamps. The classification is
/// path-keyed (a preflight must classify identically), so on its own it says
/// nothing about `POST /zoom`. Today no such arm exists in [`dispatch`] and the
/// method falls through to a 404, which is why this was never a live hole; but
/// the enum's promise — *"a route added later is gated unless it says
/// otherwise"* — covered only a new PATH, and the day someone adds
/// `("POST", "/zoom")` to persist a zoom level it would be page-callable with no
/// gate and no failing test. A non-GET on an open path is not the thing that was
/// opened, so it is gated like anything else.
fn requires_gui_token(method: &str, path: &str) -> bool {
    match route_access(path) {
        RouteAccess::Open => method != "GET",
        RouteAccess::GuiOnly => true,
        // Both authenticate themselves: the signer's bearer token on the page
        // routes, the per-ceremony `request_id` on grant/deny. `/fido2` is
        // deliberately untouched by this gate.
        RouteAccess::PageSigner | RouteAccess::Ceremony => false,
    }
}

/// A refused request, as a value: what to journal and what to answer. Pure, so
/// the audit line can be asserted without a filesystem.
pub(crate) struct Refusal {
    pub(crate) event: &'static str,
    pub(crate) data: Value,
    pub(crate) body: Value,
}

/// Can the client that registered this session deliver the GUI's token at all?
///
/// The token has exactly one courier: the `sidebar ; declare` OSC the session's
/// own `ychrome` CLI writes to its PTY. A CLI built before the gate existed
/// (2026-07-28) emits a declare with no `control_token` field, so the GUI holds
/// nothing to present and **every GUI-only route 403s for the life of that
/// process** — while `/policy`, `/zoom` and `/ping` keep answering, so ad
/// blocking and userscripts look perfectly healthy. Neither a daemon handover
/// nor a GUI restart changes it; only cycling that CLI does.
///
/// This is not inferred. The client ASSERTS it in the same `register` round trip
/// that mints the token (`declares_control_token`), which is the only place the
/// fact can be known first-hand: an old binary cannot claim a capability whose
/// name it has never heard, and nothing else has to guess its vintage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenCourier {
    /// The registered client declares the token. A refusal here is about THIS
    /// request (a page, or a GUI holding an older generation's token), not about
    /// the session being permanently undrivable.
    Live,
    /// The registered client never claimed the capability. Carries its pid for
    /// the JOURNAL only — see [`gui_only_refusal`] on why the body stays
    /// pid-free.
    Absent { client_pid: i32 },
    /// **Deliberately not consulted.** A CORS preflight is a PAGE asking whether
    /// it may drive a GUI-only route cross-origin, and the answer is no whatever
    /// the session's client vintage is. Telling that page which vintage runs on
    /// the host would be a fact about the machine handed to the exact caller
    /// this gate exists to refuse, so the preflight asks a different question and
    /// gets a page-facing answer.
    NotAsked,
}

/// The refusal for a GUI-only route reached without the control token. Names
/// the route, the reason, and — this is the part that took a live incident to
/// learn — **what the reader can do about it**.
///
/// `presented` is what the caller sent, and only its PRESENCE is recorded — a
/// refusal is exactly the moment an attacker's guess would be written down, and
/// an audit line that echoes the credential it is protecting (into a file that
/// outlives the request, and that a support paste would carry) is worse than no
/// audit line at all.
///
/// The BODY is page-reachable (that is the whole reason this gate exists), so it
/// carries prose and no host facts; the client pid rides the journal line, which
/// a page cannot read.
pub(crate) fn gui_only_refusal(
    method: &str,
    path: &str,
    presented: Option<&str>,
    courier: TokenCourier,
) -> Refusal {
    // Three distinct failures wore one message until 2026-07-31, and the one
    // that actually happens — a pre-gate CLI that can never deliver the token —
    // was the one the message did not describe. The user saw "control endpoint
    // returned 403" and had nothing to act on.
    let (reason, remedy) = match (courier, presented.is_some()) {
        (TokenCourier::Absent { .. }, _) => (
            "the ychrome CLI serving this session predates the control-token gate, so it \
             declares no token and this route can never answer",
            "restarting the daemon does not fix it and neither does restarting the GUI: \
             press Ctrl+C in that session's terminal and run ychrome again. \
             `ychrome status` marks every session in this state.",
        ),
        (TokenCourier::Live, true) => (
            "the X-Ychrome-Control token did not match this session",
            "the caller is holding a token from an earlier daemon generation; the session's \
             CLI re-declares the current one on its next heartbeat (~4s). If it persists, \
             that CLI is no longer registering, so restart it.",
        ),
        (TokenCourier::Live | TokenCourier::NotAsked, false) => (
            "no X-Ychrome-Control token was presented",
            "this endpoint is reachable from a page through the yggterm-appctl bridge, so \
             mutating routes require the control token ychrome declares to the GUI over \
             OSC 7717. A page cannot hold it, and a pre-gate GUI does not send it.",
        ),
        (TokenCourier::NotAsked, true) => (
            "the X-Ychrome-Control token did not match this session",
            "this endpoint is reachable from a page through the yggterm-appctl bridge, so \
             mutating routes require the control token ychrome declares to the GUI over \
             OSC 7717.",
        ),
    };
    let mut data = json!({
        "method": method,
        "path": path,
        "reason": reason,
        "token_presented": presented.is_some(),
        "token_courier": match courier {
            TokenCourier::Live => "live",
            TokenCourier::Absent { .. } => "absent",
            TokenCourier::NotAsked => "not_asked",
        },
    });
    if let TokenCourier::Absent { client_pid } = courier {
        data["client_pid"] = json!(client_pid);
    }
    Refusal {
        event: "control_refused",
        data,
        body: json!({
            "error": format!("forbidden: {path} is GUI-only and {reason}. {remedy}"),
            "route": path,
            // A machine-readable handle on WHICH of the three it was, so the GUI
            // (or an agent) can act without parsing prose.
            "cause": match courier {
                TokenCourier::Absent { .. } => "client_predates_control_token",
                _ if presented.is_some() => "token_mismatch",
                _ => "token_absent",
            },
        }),
    }
}

/// How long one distinct refusal stays coalesced.
const REFUSAL_COALESCE_WINDOW: Duration = Duration::from_secs(60);

/// How many distinct refusals the ledger tracks before it folds the rest
/// together. The KEY contains the attacker-chosen path, so an unbounded ledger
/// is both a memory leak and a way to get one journal line per made-up path —
/// i.e. the flood back again, wearing a different hat.
const REFUSAL_LEDGER_MAX: usize = 64;

/// One run of identical refusals: when it started, and how many have been
/// swallowed since the last line was written.
#[derive(Debug)]
pub(crate) struct RefusalRun {
    first_at: Instant,
    suppressed: u64,
}

/// Should this refusal be WRITTEN, and if so how many repeats did it stand in
/// for? `None` means it was counted and not written.
///
/// **Why refusals are rationed at all.** A refusal is page-driven: a page in an
/// ychrome surface can loop `fetch('yggterm-appctl://x/action', {method:'POST'})`
/// forever, and one appended line per attempt is an unbounded write to
/// `journal.jsonl` in the user's home — a disk-fill the audit trail hands the
/// attacker, and a way to drown the very sighting the trail exists to keep. The
/// `command_drain_refused` line next door already reasons this way ("say it once
/// per withheld batch, not once per 4s heartbeat"); this is the same rule for the
/// path an attacker actually controls.
///
/// **What is never rationed:** the FIRST of any distinct refusal is always
/// written, immediately. Coalescing may cost us the repetition count's precision;
/// it may never cost us the sighting.
pub(crate) fn note_refusal(
    ledger: &mut HashMap<String, RefusalRun>,
    key: &str,
    now: Instant,
    window: Duration,
) -> Option<u64> {
    // Which run does this refusal count under? Resolved BEFORE touching the
    // ledger: a new key on a full ledger is folded into the overflow run, so a
    // caller varying the path cannot mint a fresh unrationed slot per request.
    //
    // Resolved here rather than by recursing into the overflow key, which is how
    // this was first written and is an infinite recursion rather than a bound —
    // the recursive call finds the ledger just as full and the overflow key just
    // as absent. Its own lock caught it as a stack overflow (2026-07-30).
    let key = if ledger.contains_key(key) || ledger.len() < REFUSAL_LEDGER_MAX {
        key
    } else {
        OVERFLOW_REFUSAL_KEY
    };
    if let Some(run) = ledger.get_mut(key) {
        if now.duration_since(run.first_at) < window {
            run.suppressed += 1;
            return None;
        }
        // The window closed: write one line that accounts for everything the
        // window swallowed, and start a fresh run.
        let suppressed = run.suppressed;
        run.first_at = now;
        run.suppressed = 0;
        return Some(suppressed);
    }
    // The overflow run is admitted even though it takes the map one past the cap:
    // a bound that cannot record the fact that it was hit is not a bound.
    ledger.insert(
        key.to_string(),
        RefusalRun {
            first_at: now,
            suppressed: 0,
        },
    );
    Some(0)
}

/// The key every refusal past [`REFUSAL_LEDGER_MAX`] is counted under.
pub(crate) const OVERFLOW_REFUSAL_KEY: &str = "<ledger overflow: many distinct routes refused>";

fn refusal_ledger() -> &'static Mutex<HashMap<String, RefusalRun>> {
    static LEDGER: OnceLock<Mutex<HashMap<String, RefusalRun>>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Write a refusal to the audit journal, rationed by [`note_refusal`]. The
/// suppressed count rides the line that does get written, so a flood is visible
/// as a number rather than as a million lines.
pub(crate) fn journal_refusal(refusal: &Refusal) {
    let key = format!(
        "{}|{}|{}",
        refusal.data["method"], refusal.data["path"], refusal.data["token_presented"]
    );
    let mut ledger = refusal_ledger().lock().unwrap();
    let Some(suppressed) = note_refusal(&mut ledger, &key, Instant::now(), REFUSAL_COALESCE_WINDOW)
    else {
        return;
    };
    drop(ledger);
    let mut data = refusal.data.clone();
    if suppressed > 0 {
        data["suppressed_repeats"] = json!(suppressed);
        data["suppressed_window_secs"] = json!(REFUSAL_COALESCE_WINDOW.as_secs());
    }
    crate::daemon::journal(refusal.event, data);
}

/// A control-endpoint HTTP request, parsed off the wire (request line, headers,
/// body). Returned by [`read_request`] so the daemon's connection handler can
/// dispatch it — and, for a `/ping`, drain the session's command queue itself.
pub(crate) struct ParsedRequest {
    pub(crate) method: String,
    /// Path without the query, e.g. `/pane/vault` or `/ping`.
    pub(crate) path: String,
    /// The raw query string (no leading `?`).
    pub(crate) query: String,
    /// The `X-Ychrome-Fido2` bearer token, if the request carried one.
    pub(crate) fido2_token: Option<String>,
    /// The `X-Ychrome-Control` bearer token, if the request carried one — the
    /// GUI's credential for [`RouteAccess::GuiOnly`] routes. Kept apart from
    /// `fido2_token` on purpose: the two are different secrets with different
    /// provenance, and a page holds the fido2 one.
    pub(crate) control_token: Option<String>,
    /// The POST body parsed as JSON (`Value::Null` for a bodyless GET).
    pub(crate) body: Value,
}

/// Read one HTTP request off a control-endpoint connection. `None` on an IO
/// error or a truncated body. A `/fido2/get` blocks up to two minutes awaiting
/// the presence dialog, so the daemon serves each connection on its own thread.
///
/// Generic over the transport, not tied to TCP. The per-session control
/// endpoint is a loopback `TcpStream` (the surface's userscripts have to reach
/// it from inside a page), but the agent engine mounts `/engine/*` on the
/// daemon's UNIX socket per `docs/agent-engine.md` §3, and that is the same
/// HTTP/1.1 wire over a different pipe. One parser for both: a second copy
/// would be a second place for the fido2/control header gates to drift.
///
/// `&TcpStream` and `&UnixStream` both implement `Read`, so callers pass a
/// borrow and keep the stream for the response.
pub(crate) fn read_request(stream: impl Read) -> Option<ParsedRequest> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let request_target = parts.next().unwrap_or("/");
    let (path, query) = request_target
        .split_once('?')
        .unwrap_or((request_target, ""));
    let path = path.to_string();
    let query = query.to_string();

    // Drain headers; capture Content-Length so a POST body can be read, the
    // passkey bearer token so a `/fido2/*` route can gate on it, and the GUI's
    // control token so a mutating route can.
    let mut content_length = 0usize;
    let mut fido2_token: Option<String> = None;
    let mut control_token: Option<String> = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("x-ychrome-fido2") {
                fido2_token = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("x-ychrome-control") {
                control_token = Some(value.to_string());
            }
        }
    }

    let body = if content_length > 0 {
        let mut raw = vec![0u8; content_length];
        reader.read_exact(&mut raw).ok()?;
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    Some(ParsedRequest {
        method,
        path,
        query,
        fido2_token,
        control_token,
        body,
    })
}

/// `OSC 7717 ; sidebar ; <action> ; <base64 json>`. Carries the control endpoint,
/// the pane buttons, and a stamp over this host's web-content policy — never a
/// schema, never a ruleset, never a secret.
///
/// `policy_version` is what makes the ~4s re-declare cheap: yggterm refetches
/// `<control>/policy` only when the stamp moves. See [`crate::webpolicy`].
/// `zoom_version` is the same trick for per-site zoom (`<control>/zoom`, see
/// [`crate::webzoom`]). `app_name` is the display name yggterm shows on the main
/// zoom control ("Ychrome Global Zoom") — the app names itself, yggterm never
/// hardcodes it.
///
/// `control_token` is the ONE exception to "never a secret in a declaration",
/// and it is deliberate: the OSC stream is precisely the channel a page cannot
/// read, which is what makes it the right courier for the GUI's credential.
/// Without it the GUI could not prove it is the GUI, and `POST /action` — vault
/// unlock, credential fill, ad blocking off — would stay callable by any page
/// in the surface. See [`ControlState::control_token`].
pub fn emit_declare(
    session: &str,
    control: &str,
    control_token: &str,
    policy_version: &str,
    zoom_version: &str,
) {
    let payload = declare_payload(
        session,
        control,
        control_token,
        policy_version,
        zoom_version,
    );
    emit_osc("declare", &payload.to_string());
}

/// The declare's payload, split out from the write so the contract it carries
/// can be asserted without a terminal.
fn declare_payload(
    session: &str,
    control: &str,
    control_token: &str,
    policy_version: &str,
    zoom_version: &str,
) -> Value {
    json!({
        "session": session,
        "control_token": control_token,
        // The routing identity the GUI stamps on this contribution (Phase 5):
        // a host daemon targets commands at `env_id`, the GUI reverses it to the
        // session path. It IS `YGGTERM_SESSION_ID` — the same value as `session`
        // — named separately because `session` is diagnostic (the GUI routes the
        // OSC by stream) while `env_id` is load-bearing for routing.
        "env_id": session,
        "control": control,
        "app_name": "Ychrome",
        "policy_version": policy_version,
        "zoom_version": zoom_version,
        "panes": [
            {
                "id": VAULT_PANE,
                // U+FE0E VARIATION SELECTOR-15 forces TEXT presentation, so the key
                // renders as a monochrome glyph that sits with yggterm's other chrome
                // (▦ ⧉ ⚙) instead of a colour emoji. Without it WebKitGTK picks the
                // emoji font and the button looks pasted on.
                "icon": "🔑\u{fe0e}",
                "title": "Vault (fill logins from Bitwarden)",
            },
            {
                "id": SETTINGS_PANE,
                "icon": "⚙\u{fe0e}",
                "title": "ychrome settings (ad blocking, userscripts)",
            },
        ],
    })
}

pub fn emit_close(session: &str) {
    emit_osc("close", &json!({ "session": session }).to_string());
}

fn emit_osc(action: &str, payload: &str) {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\u{1b}]7717;sidebar;{action};{encoded}\u{7}");
    let _ = stdout.flush();
}

/// Route one parsed control request against a session's [`ControlState`] and
/// return `(status, json)`. The `OPTIONS` preflight and the `/ping` command
/// envelope are handled by the daemon's connection loop BEFORE this — the
/// former needs no state, the latter reads the session's command queue, which
/// lives on the daemon, not here. Everything else (panes, policy, zoom,
/// appearance, actions, the WebAuthn signer) is the app's, and answers here.
///
/// `courier` is the session's registration fact, not this request's: it is what
/// turns a bare 403 into a refusal that names the cause and the remedy. See
/// [`TokenCourier`].
pub(crate) fn dispatch(
    state: &ControlState,
    req: &ParsedRequest,
    courier: TokenCourier,
) -> (u16, Value) {
    // THE GATE. Nothing below this line may run for a GUI-only route reached
    // without the GUI's token — the next arms unlock the vault, fill a
    // credential into the page and rewrite the profile's content policy, and the
    // control port is page-reachable through yggterm's `yggterm-appctl://`
    // bridge. Before 2026-07-27 `POST /action` had no gate at all.
    if requires_gui_token(&req.method, &req.path) && !state.gui_authorized(req) {
        let refusal = gui_only_refusal(
            &req.method,
            &req.path,
            req.control_token.as_deref(),
            courier,
        );
        journal_refusal(&refusal);
        return (403, refusal.body);
    }
    let query = req.query.as_str();
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p == format!("/pane/{VAULT_PANE}") => {
            let host = query_value(query, "host");
            let mut pane = state.pane.lock().unwrap();
            // Opening the pane straight onto the Add tab must seed the draft
            // too, not only arriving there via the tab action.
            if pane.tab == "add" {
                pane.seed_add_draft(host.as_deref());
            }
            (200, vault_schema(&pane, host.as_deref()))
        }
        ("GET", p) if p == format!("/pane/{SETTINGS_PANE}") => {
            let profile = state.pane.lock().unwrap().profile.clone();
            let page = PageContext::from_query(query);
            (200, settings_schema(&profile, &page))
        }
        // The per-site zoom overrides for this host. yggterm applies the entry
        // for the current page's host on navigation and falls back to its global
        // "Ychrome Global Zoom" — the GUI does the matching, ychrome owns the map.
        ("GET", "/zoom") => (200, crate::webzoom::to_json()),
        // CAMERA AND MICROPHONE, per origin. yggterm owns the engine mechanics
        // (the `permission-request` gate and the native prompt); this endpoint is
        // the MEMORY, and it is the only one — see [`crate::webmedia`].
        //
        // With an `origin`, this answers one live `getUserMedia()` ask and the
        // GUI acts on the verdict. Without one, it is the whole map, which is
        // what the settings pane's review-and-revoke list renders.
        //
        // GUI-only (the default in `route_access`): a page must never be able to
        // read — let alone write — what the human decided about its camera.
        ("GET", "/media-permission") => (200, media_permission_query(query)),
        ("POST", "/media-permission") => {
            if req.body.is_null() {
                return (400, json!({ "error": "bad request" }));
            }
            media_permission_write(&req.body)
        }
        // The EFFECTIVE web-content policy for the profile this ychrome is
        // running: every enable/disable decision already made, PLUS the passkey
        // shim prepended (document-start, so `navigator.credentials` is patched
        // before the page can call it). yggterm applies it to the webview.
        ("GET", "/policy") => {
            let profile = state.pane.lock().unwrap().profile.clone();
            let mut policy = crate::webpolicy::policy(&profile);
            // ⛔ THE SHIM IS SCOPED TO THE HOSTS A PASSKEY ACTUALLY EXISTS FOR.
            //
            // It used to be installed on EVERY page, unconditionally. On a page
            // it touches, `window.PublicKeyCredential` becomes DEFINED and
            // `isUserVerifyingPlatformAuthenticatorAvailable()` answers true —
            // on an engine (WebKitGTK 2.52.5) that has no WebAuthn at all, where
            // both read `undefined` untouched. Claiming a platform authenticator
            // this browser cannot have is an anomaly no real GNOME Web shows,
            // and a bot check reads it. `all_frames: false` did not save it: an
            // interstitial managed challenge is served as the TOP-FRAME document
            // at the site's own URL, so it runs in exactly the patched world.
            //
            // Making the shim "look native" cannot work — the engine genuinely
            // lacks WebAuthn, so ANY presence of these APIs is the anomaly. Only
            // absence is native, and per-origin installation is how you get it.
            //
            // ⚠ This is an agent-socket round trip, so it must never move onto
            // the `policy_version` path — that stamp is recomputed on the ~4 s
            // heartbeat and must stay free of socket IO.
            for script in passkey_shim_scripts(state) {
                policy.prepend(script);
            }
            (200, policy.to_json())
        }
        ("POST", "/action") => {
            if req.body.is_null() {
                return (400, json!({ "toast": "bad request" }));
            }
            (200, run_action(&state.pane, &req.body))
        }
        // The WebAuthn signer routes. `/fido2/get` and `/fido2/create` come from
        // the PAGE (over SOCKS-loopback) and are bearer-token-gated, so a random
        // local process cannot summon a presence dialog. `/fido2/grant` and
        // `/fido2/deny` come from the GUI dialog (over `ssh -L`) and are
        // authenticated instead by the unguessable per-ceremony `request_id`,
        // which only the app (who emitted it) and the GUI (who received the OSC)
        // know — the GUI never sees the page's token.
        ("POST", p) if p.starts_with("/fido2/") => {
            let page_route = p == "/fido2/get" || p == "/fido2/create";
            if page_route && !state.signer.authorized(req.fido2_token.as_deref()) {
                return (401, json!({ "error": "unauthorized" }));
            }
            if req.body.is_null() {
                return (400, json!({ "error": "bad request" }));
            }
            match p {
                "/fido2/get" => state.signer.handle_get(&req.body),
                "/fido2/create" => state.signer.handle_create(&req.body),
                "/fido2/grant" => state.signer.handle_grant(&req.body),
                "/fido2/deny" => state.signer.handle_deny(&req.body),
                _ => (404, json!({ "error": "unknown fido2 route" })),
            }
        }
        _ => (404, json!({})),
    }
}

/// `GET /media-permission` — either the verdict for ONE live ask, or the whole
/// remembered map.
///
/// The verdict form always answers with a decision word; there is no error shape
/// the GUI could misread as a grant. An origin nothing can be keyed to (a
/// `file://` page, a blank tab) reports `ask`, so the human still decides and
/// nothing is written down for it.
fn media_permission_query(query: &str) -> Value {
    let Some(origin) = query_value(query, "origin").filter(|origin| !origin.is_empty()) else {
        return crate::webmedia::to_json();
    };
    let audio = query_value(query, "audio").as_deref() == Some("1");
    let video = query_value(query, "video").as_deref() == Some("1");
    let sites = crate::webmedia::sites();
    let decisions = crate::webmedia::decisions_for(&sites, &origin);
    json!({
        "origin": crate::webmedia::normalize_origin(&origin),
        "decision": crate::webmedia::verdict(&sites, &origin, audio, video).as_str(),
        "camera": decisions.camera.as_str(),
        "microphone": decisions.microphone.as_str(),
    })
}

/// `POST /media-permission` — remember what the human said.
///
/// `{"origin": "https://…", "camera": "allow"|"deny"|"ask", "microphone": …}`.
/// Either device key may be omitted; an omitted key is left alone, so "the user
/// allowed the microphone" does not silently clear a camera block. `ask` forgets.
///
/// A page the GUI could not reduce to an origin is refused rather than stored
/// under a key that would never match again.
fn media_permission_write(body: &Value) -> (u16, Value) {
    let Some(origin) = body["origin"].as_str().filter(|origin| !origin.is_empty()) else {
        return (
            400,
            json!({ "error": "media permission write needs an origin" }),
        );
    };
    if crate::webmedia::normalize_origin(origin).is_none() {
        return (
            400,
            json!({ "error": format!("no origin to remember a decision for: {origin}") }),
        );
    }
    for (key, device) in [
        ("camera", crate::webmedia::Device::Camera),
        ("microphone", crate::webmedia::Device::Microphone),
    ] {
        let Some(raw) = body[key].as_str() else {
            continue;
        };
        let decision = crate::webmedia::Decision::from_str(raw);
        if let Err(error) = crate::webmedia::set(origin, device, decision) {
            return (500, json!({ "error": error.to_string() }));
        }
    }
    let sites = crate::webmedia::sites();
    let decisions = crate::webmedia::decisions_for(&sites, origin);
    (
        200,
        json!({
            "ok": true,
            "origin": crate::webmedia::normalize_origin(origin),
            "camera": decisions.camera.as_str(),
            "microphone": decisions.microphone.as_str(),
        }),
    )
}

pub(crate) fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Open a streaming NDJSON response: headers now, body written by the caller.
///
/// No `Content-Length`, because the whole point is that the length is not known
/// when the first line goes out — `Connection: close` is what ends it. This is
/// what `/engine/batch` needs so a 300-page crawl reports each page as it
/// finishes instead of after the last one.
pub(crate) fn respond_ndjson_head(stream: &mut impl Write) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\
                Cache-Control: no-store\r\nConnection: close\r\n\r\n";
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.flush();
}

/// Answer with raw bytes and an explicit content type. `/engine/shot` returns
/// `image/png` per docs/agent-engine.md §4: a screenshot is bytes the HTTP
/// layer can carry natively, and base64 inside JSON would be a second encoding
/// of it.
///
/// `extra_headers` is already-formatted `Name: value\r\n` lines, or empty. It
/// exists for `/engine/shot`, whose body must stay pure PNG bytes (`--out`
/// writes it straight to a file) while the capture still has to be able to say
/// WHAT it captured. Each line is written verbatim, so a caller building one
/// owes it a value with no CR or LF in it — `engine::api::shot_meta_header` is
/// the only builder and it enforces that by serialising compact JSON.
pub(crate) fn respond_bytes(
    mut stream: impl Write,
    status: u16,
    content_type: &str,
    extra_headers: &str,
    body: &[u8],
) {
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\n\
         Content-Length: {len}\r\nCache-Control: no-store\r\n{extra_headers}\
         Connection: close\r\n\r\n",
        len = body.len(),
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

pub(crate) fn respond_json(stream: impl Write, status: u16, body: &Value, path: &str) {
    write_json(stream, status, body, cors_headers(route_access(path)));
}

fn write_json(mut stream: impl Write, status: u16, body: &Value, cors: &str) {
    let body = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nCache-Control: no-store\r\nConnection: close\r\n\
         {cors}\r\n{body}",
        len = body.len(),
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// CORS headers for a route, by its [`RouteAccess`] class.
///
/// The signer's page routes are fetched by a userscript running in the RP's page
/// — a cross-origin request (webauthn.io → `127.0.0.1:<port>`) that WebKit
/// refuses without these, and the custom `X-Ychrome-Fido2` header forces a
/// preflight `OPTIONS`. The wildcard is safe THERE: the shim sends no
/// credentials and the real boundary is the signer's bearer token plus the
/// origin↔rpId check.
///
/// Everywhere else the answer is NO CORS AT ALL. It used to be `*` on every
/// response, which told any page in the surface that reading `/action`'s and
/// `/pane/<id>`'s replies was fine. The token gate is what actually stops the
/// call, but advertising a wildcard on a GUI-only route is a standing invitation
/// and it is withdrawn here. `X-Ychrome-Control` is deliberately NOT in
/// `Allow-Headers`: no page is ever meant to send it.
fn cors_headers(access: RouteAccess) -> &'static str {
    match access {
        RouteAccess::PageSigner => {
            "Access-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Methods: POST, OPTIONS\r\n\
             Access-Control-Allow-Headers: Content-Type, X-Ychrome-Fido2\r\n\
             Access-Control-Max-Age: 600\r\n"
        }
        RouteAccess::Open | RouteAccess::Ceremony | RouteAccess::GuiOnly => "",
    }
}

/// Answer a CORS preflight. For a signer page route: 204 + the CORS headers,
/// without which the browser never sends the real `/fido2/*` POST. For anything
/// else: the same 403 the real request would get, named — a preflight for
/// `/action` is a page asking whether it may drive the settings pane, and the
/// honest answer is no, said out loud rather than by silent omission.
pub(crate) fn respond_preflight(mut stream: impl Write, path: &str) {
    let access = route_access(path);
    if access != RouteAccess::PageSigner {
        if access == RouteAccess::GuiOnly {
            // `NotAsked`: a preflight is a page-origin question, so the answer
            // must not describe the host's own client. See [`TokenCourier`].
            let refusal = gui_only_refusal("OPTIONS", path, None, TokenCourier::NotAsked);
            // Rationed like every other refusal: a preflight is as cheap for a
            // page to loop as the real request is.
            journal_refusal(&refusal);
            write_json(stream, 403, &refusal.body, "");
            return;
        }
        // Open and Ceremony routes: no page is meant to reach them from a page
        // origin either, but they are not capabilities — refuse the preflight
        // without an audit line, so a stray probe cannot flood the journal.
        let body = json!({
            "error": format!(
                "forbidden: {path} is not a page route; only /fido2/get and \
                 /fido2/create answer a cross-origin preflight"
            ),
            "route": path,
        });
        write_json(stream, 403, &body, "");
        return;
    }
    let response = format!(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n{cors}\r\n",
        cors = cors_headers(access),
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// A row's stable handle: `name \x1f username`. The vault's item id would be
/// tidier, but the agent resolves by (name, user) — the same pair `get` and
/// `totp` take — so no new agent op (and no re-unlock) is needed.
fn row_id(name: &str, user: &str) -> String {
    format!("{name}{ROW_SEP}{user}")
}

fn split_row_id(value: &str) -> (String, String) {
    match value.split_once(ROW_SEP) {
        Some((name, user)) => (name.to_string(), user.to_string()),
        None => (value.to_string(), String::new()),
    }
}

fn item_row(item: &Value) -> Value {
    let name = item["name"].as_str().unwrap_or_default();
    let user = item["username"].as_str().unwrap_or_default();
    let folder = item["folder"].as_str().unwrap_or_default();
    let subtitle = match (user.is_empty(), folder.is_empty()) {
        (false, false) => format!("{user} · {folder}"),
        (false, true) => user.to_string(),
        (true, false) => folder.to_string(),
        (true, true) => String::new(),
    };
    // A card carries no password at all, so the login fill would fail on it —
    // the agent's `get` resolves the password before it looks at anything else
    // and refuses. Cards get their own injector instead of a button that cannot
    // work. The type comes from the agent's secret-free metadata; the number
    // does not (see `card-fill` in `run_vault_action`).
    let is_card = item["item_type"].as_u64() == Some(u64::from(CIPHER_TYPE_CARD));
    let mut actions = vec![if is_card {
        json!({
            "action": "card-fill",
            "label": "▤",
            "title": "Fill this card into the page",
        })
    } else {
        json!({
            "action": "fill",
            "label": "⧉",
            "title": "Fill this login into the page",
        })
    }];
    // rbw's `list` could not say whether an item had an authenticator secret,
    // so the old pane drew the button on every row. Ours knows.
    if item["has_totp"].as_bool().unwrap_or(false) {
        actions.push(json!({
            "action": "totp",
            "label": "⏱",
            "title": "Fill the authenticator code into the page",
        }));
    }
    json!({
        "kind": "list-row",
        "id": row_id(name, user),
        "title": name,
        "subtitle": subtitle,
        "actions": actions,
    })
}

/// Rows of a watchtower report rendered before it is truncated. A vault this
/// size can have dozens of reuse groups; the panel is 300px wide.
const MAX_REPORT_ROWS: usize = 30;

/// The unlock screen, shown in place of the tabs whenever the vault is not
/// unlocked. The master password is a `secret` field: it carries what the user
/// types UP to this process on the `unlock` action and is declared back empty,
/// so it never rides a schema down. The unlock itself runs `ychrome-vault
/// unlock` on THIS host, reading the password from stdin — the same path the
/// user would take at a shell, now without leaving the sidebar.
/// The agent outlives the binary: install a new `ychrome-vault` and the running
/// agent keeps serving the OLD code, so ops added since it started answer
/// `unknown op`. `status` reports this as `agent_stale`.
///
/// Retiring the agent DROPS the cached keys, so the vault re-locks and the user
/// must unlock again. That is why this is a button and not something the pane
/// does behind their back — and why the button lives right next to the unlock
/// form, which is where the flow lands.
fn stale_agent_widgets(status: &Value) -> Vec<Value> {
    if !status["agent_stale"].as_bool().unwrap_or(false) {
        return Vec::new();
    }
    vec![
        json!({
            "kind": "label", "muted": true,
            "text": "This host's vault agent is older than the installed ychrome-vault, so newer features are unavailable. Restarting it re-locks the vault.",
        }),
        json!({
            "kind": "button", "id": "restart_agent", "action": "restart_agent",
            "label": "Restart agent (re-locks)",
        }),
    ]
}

fn locked_schema(status: &Value) -> Value {
    let state = status["state"].as_str().unwrap_or("unknown");
    let mut widgets = vec![];
    match state {
        "locked" => {
            widgets.push(json!({"kind": "section", "text": "Unlock the vault"}));
            if let Some(email) = status["email"].as_str().filter(|email| !email.is_empty()) {
                widgets.push(json!({"kind": "label", "muted": true, "text": email}));
            }
            widgets.push(json!({
                // `action` fires on Enter: typing a master password and reaching
                // for the mouse is not how anyone unlocks a vault.
                "kind": "text-input", "id": "unlock_password", "label": "Master password",
                "placeholder": "Master password", "secret": true, "value": "",
                "action": "unlock",
            }));
            widgets.push(json!({
                "kind": "button", "id": "unlock", "action": "unlock", "primary": true,
                "label": "Unlock",
            }));
            widgets.push(json!({
                "kind": "label", "muted": true,
                "text": "Your password unlocks the vault on this host and is not stored. It never crosses the terminal or the GUI.",
            }));
            widgets.extend(stale_agent_widgets(status));
        }
        "not_configured" => {
            widgets.push(json!({"kind": "section", "text": "Vault not set up"}));
            widgets.push(json!({
                "kind": "label", "muted": true,
                "text": "No vault is configured on this host. Run `ychrome-vault configure --server <url> --email <you>` here, then unlock.",
            }));
        }
        other => {
            widgets.push(json!({"kind": "section", "text": "Vault"}));
            widgets.push(
                json!({"kind": "label", "muted": true, "text": format!("Vault state: {other}.")}),
            );
        }
    }
    json!({ "title": "Vault", "widgets": widgets })
}

/// The pane, with lock state resolved. A locked vault shows an unlock form, not
/// the item list; `status` is the SSOT for it (a cheap agent round-trip). An
/// error here (agent unreachable) surfaces the reason rather than a broken tab.
///
/// The I/O lives here so [`unlocked_schema`] stays pure and testable without an
/// agent — a test must never touch the user's real vault.
fn vault_schema(state: &PaneState, host: Option<&str>) -> Value {
    // ONE `status` call per schema. It is the SSOT for lock state AND agent
    // staleness, so both branches read the same answer — the Tools tab used to
    // fetch it a second time and could disagree with the gate above it.
    match vault_status() {
        Ok(status) if status["state"].as_str() == Some("unlocked") => {
            unlocked_schema(state, host, &status)
        }
        Ok(status) => locked_schema(&status),
        Err(error) => json!({
            "title": "Vault",
            "widgets": [
                {"kind": "section", "text": "Vault"},
                {"kind": "label", "muted": true, "text": error.to_string()},
            ],
        }),
    }
}

/// Build the unlocked pane. NO SECRET is ever placed in a schema — only names,
/// usernames and the booleans saying a password or TOTP secret exists. The Add
/// tab's password field is declared EMPTY every time: it carries what the user
/// types up to this process on an action, and nothing ever comes back down.
fn unlocked_schema(state: &PaneState, host: Option<&str>, status: &Value) -> Value {
    let mut widgets = vec![json!({
        "kind": "tabs",
        "id": "tab",
        "action": "tab",
        "active": state.tab,
        "tabs": [
            {"id": "fill", "label": "Fill"},
            {"id": "add", "label": "Add"},
            {"id": "tools", "label": "Tools"},
        ],
    })];

    // ⛔ ABOVE THE TABS, ON EVERY TAB, BECAUSE IT IS NOT A TAB'S PROBLEM. A
    // browser whose passkeys are off is off for every site, and the user
    // arrives at this pane from a login page that just blamed their browser.
    // Silent when the shim is fine, which is nearly always.
    widgets.extend(passkey_shim_widgets(&passkey_shim_state()));

    match state.tab.as_str() {
        "add" => {
            widgets.push(json!({"kind": "section", "text": "Add a login"}));
            widgets.push(json!({"kind": "text-input", "id": "add_name", "label": "Name", "placeholder": "example.com", "value": state.add.name}));
            widgets.push(json!({"kind": "text-input", "id": "add_user", "label": "Username", "placeholder": "you@example.com", "value": state.add.user}));
            widgets.push(json!({"kind": "text-input", "id": "add_uri", "label": "URI", "placeholder": "https://example.com", "value": state.add.uri}));
            widgets.push(json!({"kind": "text-input", "id": "add_folder", "label": "Folder (optional)", "value": state.add.folder}));
            widgets.push(json!({
                "kind": "text-input", "id": "add_notes", "label": "Notes (optional)",
                "placeholder": "Anything to remember", "value": state.add.notes,
                "multiline": true, "rows": 10,
            }));
            widgets.push(json!({
                "kind": "text-input", "id": "add_password", "label": "Password",
                "placeholder": "Leave empty to generate one", "secret": true, "value": "",
            }));
            widgets.push(json!({"kind": "section", "text": "Generator"}));
            widgets.push(json!({
                "kind": "number-input", "id": "generate_length", "label": "Length",
                "value": state.generate_length,
                "min": MIN_GENERATE_LENGTH, "max": MAX_GENERATE_LENGTH,
            }));
            widgets.push(json!({
                "kind": "toggle", "id": "generate_no_symbols", "label": "No symbols",
                "value": state.generate_no_symbols,
            }));
            widgets.push(json!({
                "kind": "label", "muted": true,
                "text": "An empty password is rolled on this host with the settings above and stored straight into the vault. It never crosses the terminal or the GUI. Name the entry after the site's host so fill matching finds it.",
            }));
            widgets.push(json!({
                "kind": "button", "id": "add", "action": "add", "primary": true,
                "label": "Save to vault",
            }));
        }
        "tools" => {
            widgets.push(json!({"kind": "section", "text": "Vault"}));
            let state_label = status["state"].as_str().unwrap_or("unknown");
            let items = status["item_count"].as_u64().unwrap_or(0);
            widgets.push(json!({
                "kind": "label", "muted": true,
                "text": format!("{state_label} · {items} items"),
            }));
            widgets.extend(stale_agent_widgets(status));
            widgets.push(json!({"kind": "button", "id": "sync", "action": "sync", "label": "Re-sync from the server"}));
            widgets.push(json!({"kind": "button", "id": "lock", "action": "lock", "label": "Lock the vault"}));

            widgets.push(json!({"kind": "section", "text": "Watchtower"}));
            widgets.push(json!({
                "kind": "label", "muted": true,
                "text": "Finds logins that share a password, and passwords that are short or single-class. The scan runs inside the vault agent; only entry names come back.",
            }));
            widgets.push(json!({
                "kind": "button", "id": "watchtower", "action": "watchtower",
                "label": if state.watchtower.is_some() { "Scan again" } else { "Run watchtower scan" },
            }));
            if let Some(report) = &state.watchtower {
                widgets.extend(watchtower_widgets(report));
            }
        }
        _ => {
            widgets.push(json!({
                "kind": "search-box", "id": "query", "action": "search",
                "placeholder": "Search vault…", "value": state.query,
            }));
            let query = state.query.trim();
            if query.is_empty()
                && let Some(host) = host.filter(|host| !host.is_empty())
            {
                widgets.push(json!({"kind": "section", "text": format!("For {host}")}));
                match vault_op(json!({"op": "suggest", "host": host})) {
                    Ok(reply) => {
                        let items = reply["items"].as_array().cloned().unwrap_or_default();
                        if items.is_empty() {
                            widgets.push(json!({
                                "kind": "label", "muted": true,
                                "text": "No entries match this site — search or pick from all items.",
                            }));
                        } else {
                            widgets.extend(items.iter().map(item_row));
                        }
                    }
                    Err(error) => widgets.push(json!({
                        "kind": "label", "muted": true, "text": error.to_string(),
                    })),
                }
            }

            widgets.push(json!({
                "kind": "section",
                "text": if query.is_empty() { "All items".to_string() } else { format!("Matching “{query}”") },
            }));
            match vault_op(json!({"op": "list", "query": opt_field(query), "trashed": false})) {
                Ok(reply) => {
                    let items = reply["items"].as_array().cloned().unwrap_or_default();
                    let total = items.len();
                    widgets.extend(items.iter().take(MAX_ROWS).map(item_row));
                    if total > MAX_ROWS {
                        widgets.push(json!({
                            "kind": "label", "muted": true,
                            "text": format!("Showing {MAX_ROWS} of {total} — search to narrow."),
                        }));
                    }
                    if total == 0 {
                        widgets.push(json!({"kind": "label", "muted": true, "text": "No items."}));
                    }
                }
                Err(error) => widgets.push(json!({
                    "kind": "label", "muted": true, "text": error.to_string(),
                })),
            }
        }
    }

    json!({ "title": "Vault", "widgets": widgets })
}

/// Render a watchtower report. The report carries labels only, so this cannot
/// leak a password however it is written.
fn watchtower_widgets(report: &Value) -> Vec<Value> {
    let scanned = report["scanned"].as_u64().unwrap_or(0);
    let reused = report["reused"].as_array().cloned().unwrap_or_default();
    let weak = report["weak"].as_array().cloned().unwrap_or_default();
    let mut widgets = vec![json!({
        "kind": "label", "muted": true,
        "text": format!(
            "Scanned {scanned} logins: {} reused-password groups, {} weak.",
            reused.len(), weak.len(),
        ),
    })];

    if !reused.is_empty() {
        widgets.push(
            json!({"kind": "section", "text": format!("Reused passwords ({})", reused.len())}),
        );
        for group in reused.iter().take(MAX_REPORT_ROWS) {
            let labels: Vec<&str> = group
                .as_array()
                .map(|group| group.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            widgets.push(json!({
                "kind": "label",
                "text": format!("Shared by {} logins", labels.len()),
            }));
            widgets.push(json!({"kind": "label", "muted": true, "text": labels.join(" · ")}));
        }
        if reused.len() > MAX_REPORT_ROWS {
            widgets.push(json!({
                "kind": "label", "muted": true,
                "text": format!("Showing {MAX_REPORT_ROWS} groups of {}.", reused.len()),
            }));
        }
    }

    if !weak.is_empty() {
        widgets
            .push(json!({"kind": "section", "text": format!("Weak passwords ({})", weak.len())}));
        let shown: Vec<&str> = weak
            .iter()
            .filter_map(Value::as_str)
            .take(MAX_REPORT_ROWS)
            .collect();
        widgets.push(json!({"kind": "label", "muted": true, "text": shown.join(" · ")}));
        if weak.len() > MAX_REPORT_ROWS {
            widgets.push(json!({
                "kind": "label", "muted": true,
                "text": format!("Showing {MAX_REPORT_ROWS} of {}.", weak.len()),
            }));
        }
    }

    if reused.is_empty() && weak.is_empty() && scanned > 0 {
        widgets.push(json!({
            "kind": "label", "muted": true,
            "text": "No reused or weak passwords. Nothing to do.",
        }));
    }
    widgets
}

/// Fold the GUI's draft edits back into the app's state.
///
/// yggterm's copy of a pane's values is only what the user has typed since the
/// last schema; this process owns them. A field the current schema does not
/// declare is simply absent, which is why every read is conditional — a `tab`
/// action fired from the Fill tab must not blank the Add draft.
fn absorb_draft(state: &mut PaneState, values: &Value) {
    let text = |key: &str| values[key].as_str().map(str::to_string);
    if let Some(name) = text("add_name") {
        state.add.name = name;
    }
    if let Some(user) = text("add_user") {
        state.add.user = user;
    }
    if let Some(uri) = text("add_uri") {
        state.add.uri = uri;
    }
    if let Some(folder) = text("add_folder") {
        state.add.folder = folder;
    }
    if let Some(notes) = text("add_notes") {
        state.add.notes = notes;
    }
    if let Some(length) = values["generate_length"].as_str() {
        // An empty or half-typed number box must not wipe the setting.
        if let Ok(length) = length.parse::<i64>() {
            state.generate_length = length.clamp(MIN_GENERATE_LENGTH, MAX_GENERATE_LENGTH);
        }
    }
    if let Some(no_symbols) = values["generate_no_symbols"].as_str() {
        state.generate_no_symbols = no_symbols == "true";
    }
}

fn run_action(state: &Mutex<PaneState>, request: &Value) -> Value {
    // Which pane the click came from. The two panes have disjoint action names,
    // but they return DIFFERENT schemas — routing on the pane id is what stops a
    // settings toggle from redrawing the rail as the vault.
    if request["pane"].as_str() == Some(SETTINGS_PANE) {
        return run_settings_action(state, request);
    }
    let action = request["action"].as_str().unwrap_or_default();
    let values = &request["values"];
    let value = values["value"].as_str().unwrap_or_default().to_string();
    let host = values["host"].as_str().map(str::to_string);
    absorb_draft(&mut state.lock().unwrap(), values);

    match action {
        "tab" => {
            {
                let mut state = state.lock().unwrap();
                state.tab = value;
                // A tab switch abandons the search: the query belonged to the
                // list the user just left.
                state.query.clear();
                if state.tab == "add" {
                    state.seed_add_draft(host.as_deref());
                }
            }
            reschema(state, host.as_deref())
        }
        "search" => {
            {
                let mut state = state.lock().unwrap();
                state.query = values["query"].as_str().unwrap_or_default().to_string();
            }
            reschema(state, host.as_deref())
        }
        "watchtower" => match vault_op(json!({"op": "watchtower"})) {
            Ok(report) => {
                let (reused, weak) = (
                    report["reused"].as_array().map_or(0, Vec::len),
                    report["weak"].as_array().map_or(0, Vec::len),
                );
                state.lock().unwrap().watchtower = Some(report);
                merge(
                    reschema(state, host.as_deref()),
                    json!({ "toast": format!("Watchtower: {reused} reused-password groups, {weak} weak.") }),
                )
            }
            Err(error) => json!({ "toast": error.to_string() }),
        },
        "sync" => match vault_op(json!({"op": "sync"})) {
            Ok(reply) => {
                let count = reply["item_count"].as_u64().unwrap_or(0);
                merge(
                    reschema(state, host.as_deref()),
                    json!({ "toast": format!("Synced {count} items.") }),
                )
            }
            Err(error) => json!({ "toast": error.to_string() }),
        },
        "unlock" => {
            // The master password reaches `ychrome-vault unlock` on stdin and is
            // used for this one call — never stored in PaneState, never echoed
            // back. On success the vault is open and reschema falls through to the
            // tabs; on failure it stays on the unlock form with the field cleared.
            let password = values["unlock_password"].as_str().unwrap_or_default();
            if password.is_empty() {
                return json!({ "toast": "Enter your master password." });
            }
            match vault_op_autostart(json!({"op": "unlock", "password": password})) {
                Ok(reply) => {
                    let count = reply["item_count"].as_u64().unwrap_or(0);
                    merge(
                        reschema(state, host.as_deref()),
                        json!({ "toast": format!("Vault unlocked — {count} items.") }),
                    )
                }
                Err(error) => merge(
                    reschema(state, host.as_deref()),
                    json!({ "toast": error.to_string() }),
                ),
            }
        }
        // The CHEAP remedy for an agent older than this browser: exec the
        // installed binary in place, same pid, same socket, vault still
        // unlocked. `restart_agent` below is the fallback that costs a master
        // password, and it is deliberately the second offer.
        //
        // ⚠ It execs the INSTALLED `ychrome-vault`, which the agent chooses —
        // never this process. If that binary is itself old, the handover is
        // refused ("it is the binary already running") rather than looping, and
        // the operator has to install the new one first. Verification lives in
        // `ychrome_vault_proto::handover`, one owner shared with the CLI.
        "hand_over_agent" => {
            match vault_dir().and_then(|dir| ychrome_vault_proto::handover(&dir)) {
                Ok(reply) if reply["handed_over"].as_bool().unwrap_or(false) => merge(
                    reschema(state, host.as_deref()),
                    json!({ "toast": "Agent handed over — passkeys are on again, in web surfaces opened from now on." }),
                ),
                // Accepted but unproven is a FAILURE here, not a partial success:
                // the second round trip found the old binary still answering.
                Ok(_) => json!({
                    "toast": "The agent is still running its old binary. Install the current \
                              ychrome-vault on this host, then try again.",
                }),
                Err(error) => json!({ "toast": error.to_string() }),
            }
        }
        "restart_agent" => match vault_dir().and_then(|dir| ychrome_vault_proto::stop(&dir)) {
            Ok(_) => {
                // The agent held the keys, so the vault is locked now and the old
                // scan is meaningless. Reschema lands on the unlock form.
                state.lock().unwrap().watchtower = None;
                merge(
                    reschema(state, host.as_deref()),
                    json!({ "toast": "Agent restarted — unlock the vault to continue." }),
                )
            }
            Err(error) => json!({ "toast": error.to_string() }),
        },
        "lock" => match vault_op(json!({"op": "lock"})) {
            Ok(_) => {
                // A locked vault's scan is stale and unrepeatable; do not keep
                // showing which of the user's logins share a password.
                state.lock().unwrap().watchtower = None;
                merge(
                    reschema(state, host.as_deref()),
                    json!({ "toast": "Vault locked." }),
                )
            }
            Err(error) => json!({ "toast": error.to_string() }),
        },
        "add" => {
            // The draft was absorbed above, so it is this process's copy that
            // is authoritative — and it survives a failed save.
            let (name, user, uri, folder, notes, length, no_symbols) = {
                let state = state.lock().unwrap();
                (
                    state.add.name.trim().to_string(),
                    state.add.user.trim().to_string(),
                    state.add.uri.trim().to_string(),
                    state.add.folder.trim().to_string(),
                    state.add.notes.trim().to_string(),
                    state.generate_length,
                    state.generate_no_symbols,
                )
            };
            if name.is_empty() {
                return json!({ "toast": "An item needs a name." });
            }
            // The typed password is used for this call and dropped. An empty one
            // means generate: rolled on this host, stored encrypted, and never
            // echoed back — a schema is not a place for a secret. The op carries
            // present-or-null fields, exactly the shape the CLI sent the agent.
            let password = values["add_password"].as_str().unwrap_or_default();
            let op = json!({
                "op": "add",
                "name": name,
                "user": opt_field(&user),
                "uri": opt_field(&uri),
                "folder": opt_field(&folder),
                "notes": opt_field(&notes),
                "totp": Value::Null,
                "password": opt_field(password),
                "generate": password.is_empty(),
                "length": length,
                "symbols": !no_symbols,
            });
            match vault_op(op) {
                Ok(_) => {
                    let how = if password.is_empty() {
                        "a generated password"
                    } else {
                        "the password you typed"
                    };
                    {
                        // The item exists now: clear the draft so the tab is
                        // ready for the next one rather than re-adding this.
                        let mut state = state.lock().unwrap();
                        state.add = AddDraft::default();
                        state.seed_add_draft(host.as_deref());
                    }
                    merge(
                        reschema(state, host.as_deref()),
                        json!({ "toast": format!("Added {name} with {how}.") }),
                    )
                }
                Err(error) => json!({ "toast": error.to_string() }),
            }
        }
        "fill" => {
            let (name, user) = split_row_id(&value);
            match vault_op(json!({"op": "get", "name": name, "user": opt_field(&user)})) {
                // The password comes off the host's agent, goes straight into the
                // eval script, and is dropped. It never enters a schema, the OSC
                // stream, or yggterm's state.
                Ok(reply) => {
                    let password = reply["entry"]["password"].as_str().unwrap_or_default();
                    json!({
                        "eval": fill_script(&user, password),
                        "toast": format!("Filled {name}."),
                    })
                }
                Err(error) => json!({ "toast": error.to_string() }),
            }
        }
        // A card's number and CVV reach the page the same way a password does
        // and by the SAME rule: computed on this host, embedded in the eval
        // script, dropped. What makes a card stricter is that it has no CLI
        // verb at all — `ychrome-vault card` prints metadata only, because a PAN
        // in a scrollback or an agent transcript is durable and, unlike a
        // password, cannot be rotated on demand. The toast names the item and
        // the script's return value names FIELDS, never values.
        "card-fill" => {
            let (name, user) = split_row_id(&value);
            // `host` and `client` are for the agent's audit line and nothing
            // else — no decision is taken on either. They travel because the
            // vault's only durable record of a card fill is that line, and a
            // line that cannot say WHERE the number went is half a record.
            match vault_op(json!({
                "op": "card-secret",
                "name": name,
                "user": opt_field(&user),
                "host": host,
                "client": "ychrome sidebar",
            })) {
                Ok(reply) => {
                    let field = |key: &str| reply[key].as_str().unwrap_or_default();
                    json!({
                        "eval": card_fill_script(
                            field("number"),
                            field("code"),
                            field("exp_month"),
                            field("exp_year"),
                            field("cardholder"),
                        ),
                        "toast": format!("Filled {name}."),
                    })
                }
                Err(error) => json!({ "toast": error.to_string() }),
            }
        }
        "totp" => {
            let (name, user) = split_row_id(&value);
            match vault_op(json!({"op": "totp", "name": name, "user": opt_field(&user)})) {
                Ok(reply) => {
                    let code = reply["code"].as_str().unwrap_or_default();
                    json!({
                        "eval": totp_script(code),
                        "toast": format!("Filled {name}'s authenticator code."),
                    })
                }
                Err(error) => json!({ "toast": error.to_string() }),
            }
        }
        _ => json!({ "toast": format!("unknown action {action:?}") }),
    }
}

fn reschema(state: &Mutex<PaneState>, host: Option<&str>) -> Value {
    let state = state.lock().unwrap();
    json!({ "schema": vault_schema(&state, host) })
}

// ---------------------------------------------------------------------------
// The settings pane: ad blocking + userscripts, owned by THIS host.
// ---------------------------------------------------------------------------

/// Toggle ids double as action ids. A userscript's action carries its stem after
/// the prefix, so one arm handles however many scripts the host has.
const USERSCRIPT_ACTION_PREFIX: &str = "userscript:";
/// Delete a userscript (the list-row's trash action).
const USERSCRIPT_DELETE_PREFIX: &str = "userscript-delete:";
/// Install a bundled extension by its catalog stem (the "Add an extension" list).
const INSTALL_ACTION_PREFIX: &str = "install:";
/// Set one SponsorBlock category's behaviour: `sponsorblock:<category>:<behaviour>`.
/// ⚠ It must not be a prefix of `USERSCRIPT_ACTION_PREFIX` or vice versa — the
/// dispatch is a `starts_with` chain, and `sponsorblock:` deliberately does not
/// collide with `userscript:sponsorblock`, which is the on/off toggle.
const SPONSORBLOCK_ACTION_PREFIX: &str = "sponsorblock:";

/// The per-site zoom controls' action ids.
const ZOOM_IN_ACTION: &str = "zoom-in";
const ZOOM_OUT_ACTION: &str = "zoom-out";
const ZOOM_RESET_ACTION: &str = "zoom-reset";
/// Vertical tabs and "continue where you left off". Both are yggterm's prefs —
/// it owns the tabs, the tab tree and the chrome that draws them — so the pane
/// only VIEWS them (from the injected page context) and asks the GUI to change
/// them (`surface_prefs` on the reply). ychrome stores neither.
const VERTICAL_TABS_ACTION: &str = "tabs-vertical";
const RESTORE_TABS_ACTION: &str = "tabs-restore";
/// Pick the browser-wide identity. Carries the preset id after the prefix.
const USER_AGENT_ACTION_PREFIX: &str = "user-agent:";
/// Pick the identity for the LIVE PAGE'S HOST only. Carries the preset id after
/// the prefix. Separate from [`USER_AGENT_ACTION_PREFIX`] because they write
/// different keys of the same config and a shared prefix would make the
/// narrower, safer one reachable by a typo in the broader one.
const SITE_IDENTITY_ACTION_PREFIX: &str = "site-identity:";
/// Drop this site's identity override; it falls back to the browser default.
const SITE_IDENTITY_RESET_ACTION: &str = "site-identity-reset";
/// Change ONE device's remembered decision for ONE origin. The tail is
/// `<origin>:<device>:<decision>` and is parsed FROM THE RIGHT, because an
/// origin legitimately contains colons (`https://example.com:8443`).
const MEDIA_PERMISSION_ACTION_PREFIX: &str = "media-permission:";
/// Forget everything remembered for one origin — the pane's Revoke. Checked
/// BEFORE [`MEDIA_PERMISSION_ACTION_PREFIX`], which is a prefix of it.
const MEDIA_PERMISSION_FORGET_PREFIX: &str = "media-permission-forget:";

/// What the GUI reports about the live surface, on the schema GET (as query
/// params) and on every action (as `values`). All non-secret, and none of it is
/// something ychrome could know: the surface is the GUI's.
///
/// `vertical_tabs` and `restore_tabs` are yggterm's own web-surface preferences,
/// injected so the browser's settings pane can hold the browser's settings
/// without either side keeping a second copy of the truth.
#[derive(Debug, Clone, Default)]
struct PageContext {
    host: Option<String>,
    zoom: Option<f64>,
    secure: Option<bool>,
    vertical_tabs: bool,
    restore_tabs: bool,
}

impl PageContext {
    fn from_query(query: &str) -> Self {
        PageContext {
            host: query_value(query, "host").filter(|host| !host.is_empty()),
            zoom: query_value(query, "zoom").and_then(|text| text.parse::<f64>().ok()),
            secure: query_value(query, "secure").map(|text| text == "true"),
            vertical_tabs: query_value(query, "vertical_tabs").as_deref() == Some("true"),
            restore_tabs: query_value(query, "restore_tabs").as_deref() == Some("true"),
        }
    }

    fn from_values(values: &Value) -> Self {
        PageContext {
            host: values["host"]
                .as_str()
                .filter(|host| !host.is_empty())
                .map(ToOwned::to_owned),
            zoom: read_zoom(&values["zoom"]),
            secure: read_bool(&values["secure"]),
            vertical_tabs: read_bool(&values["vertical_tabs"]).unwrap_or(false),
            restore_tabs: read_bool(&values["restore_tabs"]).unwrap_or(false),
        }
    }

    fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }
}

/// The browsing-mode section: where the tabs live, and what happens to them on
/// the next visit. The toggles the user asked for, in the browser's own settings
/// pane rather than buried in the tab strip.
fn browsing_widgets(page: &PageContext) -> Vec<Value> {
    vec![
        json!({"kind": "section", "text": "Tabs"}),
        json!({
            "kind": "toggle",
            "id": "tabs-vertical",
            "action": VERTICAL_TABS_ACTION,
            "label": "Vertical tabs",
            "value": page.vertical_tabs,
        }),
        json!({
            "kind": "label",
            "muted": true,
            "text": "Tabs move out of the page into a sidebar tree with folders you can \
                     make, rename and drag tabs into. Classic tabs put them back in a strip \
                     at the top, where folders cannot be drawn.",
        }),
        json!({
            "kind": "toggle",
            "id": "tabs-restore",
            "action": RESTORE_TABS_ACTION,
            "label": "Continue tabs from last time",
            "value": page.restore_tabs,
        }),
        json!({
            "kind": "label",
            "muted": true,
            "text": "Off: each visit starts fresh, and the loose tabs from last time are \
                     purged. Folders and the tabs filed in them are saved either way.",
        }),
    ]
}

/// The per-site browser identity row, drawn in "This site".
///
/// This is where an identity override BELONGS: a badly-coded portal that gates
/// on the UA string is one site, and making the whole browser lie for it is what
/// puts an inconsistent fingerprint in front of every OTHER site's bot check.
/// The default row is "WebKit (this engine)", which sets nothing.
fn current_site_identity_widgets(
    host: Option<&str>,
    sites: &std::collections::BTreeMap<String, crate::useragent::Preset>,
    global: crate::useragent::Preset,
) -> Vec<Value> {
    let Some(host) = host.filter(|host| !host.is_empty()) else {
        return Vec::new();
    };
    let effective = crate::useragent::preset_for_host(sites, global, host);
    let overridden = crate::sitehost::lookup(sites, host).is_some();
    let subtitle = if overridden {
        format!("{} · this site", effective.label())
    } else {
        format!("{} · browser default", effective.label())
    };
    // One "Use" per OTHER preset, plus Reset once an override exists — the same
    // vocabulary as the zoom row directly above it.
    let mut actions: Vec<Value> = crate::useragent::Preset::ALL
        .iter()
        .filter(|preset| **preset != effective)
        .map(|preset| {
            json!({
                "action": format!("{SITE_IDENTITY_ACTION_PREFIX}{}", preset.id()),
                "label": preset.label(),
                "title": format!("Identify as {} on {host} only", preset.label()),
            })
        })
        .collect();
    if overridden {
        actions.push(json!({
            "action": SITE_IDENTITY_RESET_ACTION,
            "label": "Reset",
            "title": "Use the browser default identity here",
        }));
    }
    vec![
        json!({
            "kind": "list-row",
            "id": "site-identity",
            "title": format!("Identity on {host}"),
            "subtitle": subtitle,
            "actions": actions,
        }),
        json!({
            "kind": "label",
            "muted": true,
            "text": crate::useragent::OVERRIDE_WARNING,
        }),
    ]
}

/// Read one `media-permission:<origin>:<device>:<decision>` action.
///
/// ⛔ Parsed FROM THE RIGHT. An origin carries colons of its own — the scheme's,
/// and a non-default port's (`https://example.com:8443`) — so a left-to-right
/// split would cut the origin in half and write the decision under a key that
/// can never match the page again. That failure is silent: the pane would say
/// "blocked" and the site would keep being allowed.
///
/// Pure, so the parse can be proven against a colon-bearing origin without a
/// disk write. `None` for an unrecognised device; an unrecognised DECISION is
/// [`crate::webmedia::Decision::Ask`], never a grant.
fn parse_media_permission_action(
    action: &str,
) -> Option<(String, crate::webmedia::Device, crate::webmedia::Decision)> {
    let tail = action.strip_prefix(MEDIA_PERMISSION_ACTION_PREFIX)?;
    let mut parts = tail.rsplitn(3, ':');
    let decision = parts.next().unwrap_or_default();
    let device = parts.next().unwrap_or_default();
    let origin = parts.next()?;
    Some((
        origin.to_string(),
        crate::webmedia::Device::from_str(device)?,
        crate::webmedia::Decision::from_str(decision),
    ))
}

/// The camera/microphone section: every origin that has been answered once, and
/// the way back out of each answer.
///
/// Review and revoke is the whole job here. Notice what is deliberately ABSENT:
/// there is no way to GRANT a device from this pane. A capture grant is only
/// ever created at the moment a page actually asked, in yggterm's prompt, with
/// the site named — inventing a second door here would mean a grant could exist
/// without anyone having seen which page it was for.
///
/// `Ask` is absence, so an origin with nothing remembered has no row at all;
/// the list is exactly the set of decisions the user could want to take back.
fn media_permission_widgets(
    sites: &std::collections::BTreeMap<String, crate::webmedia::SiteDecisions>,
) -> Vec<Value> {
    use crate::webmedia::{Decision, Device};
    let mut widgets = vec![json!({"kind": "section", "text": "Camera & microphone"})];
    if sites.is_empty() {
        widgets.push(json!({
            "kind": "label",
            "muted": true,
            "text": "No site has been given the camera or the microphone. yggterm asks the \
                     first time a page tries, and remembers only what you tell it to.",
        }));
        return widgets;
    }
    for (origin, decisions) in sites {
        let word = |decision: Decision| match decision {
            Decision::Allow => "allowed",
            Decision::Deny => "blocked",
            Decision::Ask => "asks",
        };
        // ⛔ ONE action, and it must stay one. The rail's `list-row` gives the
        // actions all the width they ask for and lets the TITLE take what is
        // left, under `overflow:hidden; white-space:nowrap`. Measured live in the
        // rail (2026-08-01): with a "Block camera" + "Block microphone" +
        // "Revoke" row, the title element came back **0 px wide** — the origin,
        // the one fact this whole section exists to show, painted nothing at all,
        // and the user was left with three buttons and no idea which site they
        // acted on. On a security-review surface that is worse than no row.
        //
        // The tri-state is still fully reachable without more buttons here:
        // ALLOW and DENY are both made at the prompt (where the site is named
        // and the page actually asked), and Revoke is the way back to ASK.
        let actions = vec![json!({
            "action": format!("{MEDIA_PERMISSION_FORGET_PREFIX}{origin}"),
            "label": "Revoke",
            "title": format!("Forget every camera and microphone decision for {origin}"),
        })];
        widgets.push(json!({
            "kind": "list-row",
            "id": format!("media-permission-{origin}"),
            "title": origin,
            "subtitle": format!(
                "Camera {} · mic {}",
                word(decisions.camera),
                word(decisions.microphone),
            ),
            "actions": actions,
        }));
    }
    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "Revoking takes effect on the next request: the page is asked to be asked \
                 about again, and a stream it already holds is not retroactively cut.",
    }));
    widgets
}

/// The browser-wide identity. The default is the engine's OWN UA, which is the
/// coherent one: a bot check that scores consistency (Cloudflare's managed
/// challenge) passes a browser whose UA, JS environment and TLS all agree, and
/// challenges one whose UA claims a platform the page contradicts. Presented as
/// a row per preset rather than a free-text field: the failure mode of a
/// hand-typed UA is a site quietly serving you the wrong code.
fn user_agent_widgets() -> Vec<Value> {
    let current = crate::useragent::preset();
    let mut widgets = vec![json!({"kind": "section", "text": "Browser identity"})];
    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "This is the identity every site gets. Prefer a per-site override in \
                 “This site” above — a browser-wide spoof puts the same inconsistency in \
                 front of every login you have.",
    }));
    for preset in crate::useragent::Preset::ALL {
        let selected = preset == current;
        // "This is the one in use" is SELECTION, not a status dot — yggterm
        // tints the selected row, which is the vocabulary every other row list
        // already speaks. It used to be a literal `●` glued onto the title,
        // which painted in the row's text colour (a black dot in the light
        // theme) and shifted the label one character right.
        widgets.push(json!({
            "kind": "list-row",
            "id": format!("ua-{}", preset.id()),
            "title": preset.label(),
            "selected": selected,
            "subtitle": preset.description(),
            "actions": if selected {
                json!([])
            } else {
                json!([{
                    "action": format!("{USER_AGENT_ACTION_PREFIX}{}", preset.id()),
                    "label": "Use",
                    "title": format!("Identify as {}", preset.label()),
                }])
            },
        }));
    }
    widgets
}

/// The "This site" zoom row. ychrome owns the per-site override; the row shows a
/// real number either way — the stored override when custom, else the GUI's
/// reported live (global) zoom. `−`/`+` step the override from whatever is on
/// screen now, and `Reset` clears it back to the global.
fn current_site_zoom_widgets(
    host: Option<&str>,
    live_zoom: Option<f64>,
    zoom_sites: &std::collections::BTreeMap<String, f64>,
) -> Vec<Value> {
    let Some(host) = host.filter(|host| !host.is_empty()) else {
        return vec![json!({
            "kind": "label",
            "muted": true,
            "text": "Open a site in this surface to set its zoom.",
        })];
    };
    let override_pct = crate::webzoom::zoom_for_host(zoom_sites, host);
    // The number to show: the stored override when the site is custom, else the
    // live global the GUI reported. ychrome does not know yggterm's global, so
    // with neither we say so plainly rather than invent a number.
    let subtitle = match (override_pct, live_zoom) {
        (Some(pct), _) => format!("{}% · this site", pct as i64),
        (None, Some(global)) => format!("{}% · global default", global as i64),
        (None, None) => "Using the global zoom".to_string(),
    };
    let mut actions = vec![
        json!({ "action": ZOOM_OUT_ACTION, "label": "−", "title": "Zoom out" }),
        json!({ "action": ZOOM_IN_ACTION, "label": "+", "title": "Zoom in" }),
    ];
    // Reset only means something once there is an override to clear.
    if override_pct.is_some() {
        actions.insert(
            1,
            json!({ "action": ZOOM_RESET_ACTION, "label": "Reset", "title": "Use the global zoom" }),
        );
    }
    vec![json!({
        "kind": "list-row",
        "id": "site-zoom",
        "title": host,
        "subtitle": subtitle,
        "actions": actions,
    })]
}

/// Read this host's policy files and draw the pane. The I/O lives here so
/// [`settings_schema_from`] stays pure and testable without touching the user's
/// real config — the same split the vault pane uses.
///
/// Everything the GUI knows about the live surface arrives in [`PageContext`]:
/// the page's host, its live zoom, its HTTPS state, and yggterm's two
/// web-surface prefs. ychrome owns the per-site zoom OVERRIDE; it never knows
/// yggterm's global, so `page.zoom` is how the "This site" row shows a real
/// number when a site is on the global.
fn settings_schema(profile: &str, page: &PageContext) -> Value {
    settings_schema_from(
        profile,
        page,
        &crate::webzoom::sites(),
        &crate::webpolicy::state(profile),
        &crate::webmedia::sites(),
    )
}

fn settings_schema_from(
    profile: &str,
    page: &PageContext,
    zoom_sites: &std::collections::BTreeMap<String, f64>,
    state: &crate::webpolicy::PolicyState,
    media_sites: &std::collections::BTreeMap<String, crate::webmedia::SiteDecisions>,
) -> Value {
    let (host, live_zoom, secure) = (page.host(), page.zoom, page.secure);
    let mut widgets = vec![json!({"kind": "section", "text": "This site"})];
    widgets.extend(current_site_zoom_widgets(host, live_zoom, zoom_sites));
    widgets.extend(current_site_security_widgets(host, secure));
    // The per-site identity override, beside the per-site zoom: both are "this
    // one site behaves differently", both keyed by host, both matched by
    // `sitehost`. A user looking for either finds them in one place.
    widgets.extend(current_site_identity_widgets(
        host,
        &crate::useragent::sites(),
        crate::useragent::preset(),
    ));

    // Tabs first among the browser-wide settings: it is the one that changes what
    // the window looks like.
    widgets.extend(browsing_widgets(page));

    widgets.push(json!({"kind": "section", "text": "Ad blocking"}));
    if state.adblock_rules_present {
        widgets.push(json!({
            "kind": "toggle",
            "id": "adblock-enabled",
            "action": "adblock-enabled",
            "label": format!("Block ads & trackers ({} rules)", state.adblock_rule_count),
            "value": state.adblock_enabled,
        }));
        widgets.push(json!({
            "kind": "toggle",
            "id": "adblock-profile",
            "action": "adblock-profile",
            "label": format!("Enabled for “{profile}”"),
            "value": !state.adblock_profile_disabled,
        }));
    } else {
        widgets.push(json!({
            "kind": "label",
            "muted": true,
            "text": "No ruleset installed. ychrome installs the bundled one at launch, so \
                     this means the write failed — check that ~/.yggterm/web-adblock/ is \
                     writable, or run `ychrome adblock update` on this host.",
        }));
    }

    // Hardware capture, beside ad blocking: both are "what may a page do to me",
    // and this is the one the user comes looking for when they want a grant back.
    widgets.extend(media_permission_widgets(media_sites));

    // SponsorBlock is a userscript, but a flagship one, so it gets its own named
    // section with a friendly toggle — pulled out of the generic list below.
    widgets.extend(sponsorblock_widgets(state));

    // Everything EXCEPT sponsorblock: one list-row each, with Enable/Disable and
    // a Delete (the "toggle + trash icon" the design calls for).
    widgets.push(json!({"kind": "section", "text": "Userscripts"}));
    let managed: Vec<&crate::webpolicy::UserscriptStatus> = state
        .userscripts
        .iter()
        .filter(|script| script.stem != crate::extensions::SPONSORBLOCK_STEM)
        .collect();
    if managed.is_empty() {
        widgets.push(json!({
            "kind": "label",
            "muted": true,
            "text": "None installed. Add one below, or drop *.js into \
                     ~/.yggterm/web-userscripts/ on the host ychrome runs on.",
        }));
    }
    for script in managed {
        widgets.push(userscript_row(
            &script.stem,
            script.enabled,
            // A refusal outranks a note: a refused script is not running at
            // all, which is the more urgent of the two things to say.
            script.refusal.as_deref().or(script.note.as_deref()),
        ));
    }

    // The catalog, filtered to what is not already installed. "Installed" is read
    // from the SAME `state` snapshot the rest of the pane draws from — one source
    // of truth per render, so the catalog can never disagree with the list above
    // it. Omit the whole section when there is nothing left to add.
    let installed: std::collections::HashSet<&str> = state
        .userscripts
        .iter()
        .map(|script| script.stem.as_str())
        .collect();
    let installable: Vec<&crate::extensions::Extension> = crate::extensions::catalog()
        .iter()
        .filter(|ext| !installed.contains(ext.stem))
        .collect();
    if !installable.is_empty() {
        widgets.push(json!({"kind": "section", "text": "Add an extension"}));
        for ext in installable {
            widgets.push(json!({
                "kind": "list-row",
                "id": format!("catalog-{}", ext.stem),
                "title": ext.name,
                "subtitle": ext.description,
                "actions": [
                    {
                        "action": format!("{INSTALL_ACTION_PREFIX}{}", ext.stem),
                        "label": "Install",
                        "title": format!("Install {}", ext.name),
                    }
                ],
            }));
        }
    }

    widgets.extend(user_agent_widgets());

    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "Userscript and identity changes apply when the surface reloads. An adblock \
                 RULESET change needs a yggterm restart — WebKit compiles the filter once per \
                 GUI process.",
    }));
    widgets.push(json!({
        "kind": "button",
        "id": "reload-surface",
        "action": "reload-surface",
        "label": "Reload surface now",
        "primary": true,
    }));

    json!({ "title": "YChrome Settings", "widgets": widgets })
}

/// The connection line for "This site". Honest and narrow: HTTPS vs not, which is
/// what the GUI can tell us. Full certificate detail (issuer, expiry) would need
/// WebKit's TLS certificate, a capability yggterm does not expose yet. When the
/// GUI reports nothing (older GUI, or no site), the line is simply omitted.
fn current_site_security_widgets(host: Option<&str>, secure: Option<bool>) -> Vec<Value> {
    let Some(host) = host.filter(|host| !host.is_empty()) else {
        return Vec::new();
    };
    match secure {
        Some(true) => vec![json!({
            "kind": "label",
            "text": format!("🔒 Secure connection to {host} (HTTPS)"),
        })],
        Some(false) => vec![json!({
            "kind": "label",
            "muted": true,
            "text": format!("⚠ Not secure — {host} loaded over HTTP."),
        })],
        None => Vec::new(),
    }
}

/// The SponsorBlock section. Installed ⇒ a friendly toggle (its state is the
/// `sponsorblock.js` vs `.js.disabled` rename, exactly like any userscript),
/// then one row per category so the user can say what each one should do.
/// Not installed ⇒ nothing here; it appears under "Add an extension" instead.
///
/// The category rows are drawn from `crate::sponsorblock`, which is the one
/// owner of the catalogue, the defaults and the stored choices. The pane
/// re-derives none of it: a category added there appears here with no change,
/// and the buttons it offers are that category's own `options`.
fn sponsorblock_widgets(state: &crate::webpolicy::PolicyState) -> Vec<Value> {
    let installed = state
        .userscripts
        .iter()
        .find(|script| script.stem == crate::extensions::SPONSORBLOCK_STEM);
    let Some(script) = installed else {
        return Vec::new();
    };
    let mut widgets = vec![
        json!({"kind": "section", "text": "SponsorBlock"}),
        json!({
            "kind": "toggle",
            "id": format!("{USERSCRIPT_ACTION_PREFIX}{}", script.stem),
            "action": format!("{USERSCRIPT_ACTION_PREFIX}{}", script.stem),
            "label": "Skip YouTube sponsor segments",
            // The toggle stays honest to the FILENAME (that is what it flips);
            // a gate refusal — impossible for the bundled header, but a user
            // can edit the file — is surfaced right under it.
            "value": script.enabled,
        }),
    ];
    if let Some(refusal) = &script.refusal {
        widgets.push(json!({ "kind": "label", "muted": true, "text": refusal }));
    }
    // The per-category rows only when the script is on: offering a choice that
    // nothing will act on is worse than offering none.
    if !script.enabled {
        return widgets;
    }
    for (category, behaviour) in crate::sponsorblock::effective() {
        widgets.push(sponsorblock_category_row(category, behaviour));
    }
    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "Segments come from the community database at sponsor.ajay.app, asked \
                 for by hash prefix so it is never told which video you are watching. \
                 ychrome submits nothing and votes on nothing.",
    }));
    widgets
}

/// The action id for "put `<category>` into `<behaviour>`". One string, parsed
/// back by `run_settings_action` — the row and the handler agree by
/// construction rather than by two matching format strings.
fn sponsorblock_action(category: &str, behaviour: &str) -> String {
    format!("{SPONSORBLOCK_ACTION_PREFIX}{category}:{behaviour}")
}

/// One category as a list-row: what it does now in the subtitle, and a button
/// for each state it is NOT in.
///
/// The current state is deliberately absent from the buttons rather than shown
/// pressed: `list-row` actions render as plain buttons with no selected state,
/// so a row offering "Auto-skip" while already auto-skipping is a control that
/// appears to do nothing when clicked.
fn sponsorblock_category_row(
    category: &'static crate::sponsorblock::Category,
    behaviour: &'static str,
) -> Value {
    let actions: Vec<Value> = category
        .options
        .iter()
        .filter(|option| **option != behaviour)
        .map(|option| {
            json!({
                "action": sponsorblock_action(category.id, option),
                "label": sponsorblock_behaviour_label(option),
                "title": format!(
                    "{}: {}",
                    category.label,
                    sponsorblock_behaviour_title(option),
                ),
            })
        })
        .collect();
    json!({
        "kind": "list-row",
        "id": format!("sponsorblock-{}", category.id),
        "title": category.label,
        "subtitle": format!(
            "{} — {}",
            sponsorblock_behaviour_label(behaviour),
            category.description,
        ),
        "actions": actions,
    })
}

fn sponsorblock_behaviour_label(behaviour: &str) -> &'static str {
    match behaviour {
        crate::sponsorblock::AUTO => "Auto-skip",
        crate::sponsorblock::MANUAL => "Skip button",
        crate::sponsorblock::MUTE => "Mute",
        crate::sponsorblock::SHOW => "Show",
        _ => "Off",
    }
}

fn sponsorblock_behaviour_title(behaviour: &str) -> &'static str {
    match behaviour {
        crate::sponsorblock::AUTO => "seek past it without asking",
        crate::sponsorblock::MANUAL => "offer a skip button while it plays",
        crate::sponsorblock::MUTE => "mute it rather than seek past it",
        crate::sponsorblock::SHOW => "mark it on the seek bar",
        _ => "ignore it entirely",
    }
}

/// One managed userscript as a list-row: its on/off state in the subtitle, an
/// Enable/Disable action, and a Delete. Keyed by stem so Dioxus never patches one
/// script's row into another's (identity, not index — the pane's hard-won rule).
///
/// A gate-refused script is on disk and enabled BY FILENAME, but injected
/// nowhere — its subtitle carries the refusal (naming each offending line
/// verbatim) instead of the lie "Enabled". The actions stay: refusal is a
/// verdict on the header, not a lock-out, and Disable/Delete are exactly what
/// a user may want to do with a script that runs nowhere.
fn userscript_row(stem: &str, enabled: bool, notice: Option<&str>) -> Value {
    let toggle_label = if enabled { "Disable" } else { "Enable" };
    let subtitle = match notice {
        Some(notice) => notice.to_string(),
        None if enabled => "Enabled".to_string(),
        None => "Disabled".to_string(),
    };
    json!({
        "kind": "list-row",
        "id": format!("script-{stem}"),
        "title": stem,
        "subtitle": subtitle,
        "actions": [
            {
                "action": format!("{USERSCRIPT_ACTION_PREFIX}{stem}"),
                "label": toggle_label,
                "title": format!("{toggle_label} {stem}"),
            },
            {
                "action": format!("{USERSCRIPT_DELETE_PREFIX}{stem}"),
                "label": "Delete",
                "title": format!("Delete {stem}"),
            }
        ],
    })
}

/// A settings click. Every mutation lands on THIS host's disk, then the pane
/// re-reads it — the files are the source of truth, so the toggle can never
/// disagree with what `/policy` will serve next.
fn run_settings_action(state: &Mutex<PaneState>, request: &Value) -> Value {
    let action = request["action"].as_str().unwrap_or_default();
    // Everything the GUI knows about the live surface: host, zoom, HTTPS, and its
    // own web-surface prefs.
    let page = PageContext::from_values(&request["values"]);
    let profile = state.lock().unwrap().profile.clone();
    let redraw = |extra: Value| merge(json!({ "schema": settings_schema(&profile, &page) }), extra);

    // Per-site zoom lands FIRST: it needs the host and reports back with a fresh
    // schema plus `refetch_zoom` so the GUI re-reads `/zoom` and re-applies the
    // override to the live page without waiting for the ~4s heartbeat.
    if matches!(action, ZOOM_IN_ACTION | ZOOM_OUT_ACTION | ZOOM_RESET_ACTION) {
        return run_zoom_action(&profile, action, &page);
    }

    // A toggle widget posts its new state as `values.value`; a list-row button
    // posts none. A `userscript:`/adblock arm reads it, defaulting to the FLIP of
    // the current state so the row's Enable/Disable button works.
    let posted = request["values"]["value"].as_str();

    // The two prefs yggterm owns. ychrome writes nothing: it echoes the requested
    // state back in the schema (so the switch lands under the finger) and asks the
    // GUI to apply it. The next schema GET reads the truth back out of the page
    // context, so a refused change would correct itself rather than lie.
    if matches!(action, VERTICAL_TABS_ACTION | RESTORE_TABS_ACTION) {
        let want = posted == Some("true");
        let mut next = page.clone();
        let patch = if action == VERTICAL_TABS_ACTION {
            next.vertical_tabs = want;
            json!({ "vertical_tabs": want })
        } else {
            next.restore_tabs = want;
            json!({ "restore_tabs": want })
        };
        return json!({
            "schema": settings_schema(&profile, &next),
            "surface_prefs": patch,
        });
    }

    // Camera/microphone. Forget is checked FIRST: `media-permission-forget:` and
    // `media-permission:` share a stem, and a prefix match on the shorter one
    // would swallow the Revoke button whole.
    if let Some(origin) = action.strip_prefix(MEDIA_PERMISSION_FORGET_PREFIX) {
        return match crate::webmedia::forget(origin) {
            Ok(()) => redraw(json!({
                "toast": format!("{origin} will be asked about again."),
            })),
            Err(error) => json!({ "toast": error.to_string() }),
        };
    }
    if action.starts_with(MEDIA_PERMISSION_ACTION_PREFIX) {
        let Some((origin, device, decision)) = parse_media_permission_action(action) else {
            return json!({ "toast": "Unknown device." });
        };
        return match crate::webmedia::set(&origin, device, decision) {
            Ok(()) => redraw(json!({
                "toast": format!(
                    "{} on {origin}: {}.",
                    device.label(),
                    match decision {
                        crate::webmedia::Decision::Allow => "allowed",
                        crate::webmedia::Decision::Deny => "blocked",
                        crate::webmedia::Decision::Ask => "will be asked about again",
                    },
                ),
            })),
            Err(error) => json!({ "toast": error.to_string() }),
        };
    }

    // The per-site identity, checked BEFORE the browser-wide one so the narrower
    // action can never be swallowed by a prefix match on the broader.
    if action == SITE_IDENTITY_RESET_ACTION || action.starts_with(SITE_IDENTITY_ACTION_PREFIX) {
        let Some(host) = page.host() else {
            return json!({ "toast": "No site is open to set an identity for." });
        };
        let wanted = action.strip_prefix(SITE_IDENTITY_ACTION_PREFIX);
        let outcome = crate::useragent::set_site(host, wanted);
        let toast = match wanted {
            // ⚠ Say the cost at the moment of the choice, not in a doc nobody
            // reads. A spoof that breaks a fingerprint-gated login fails as a
            // challenge that never clears, which reads like the SITE being
            // broken — so the user has to be told it was this switch.
            Some(_) => format!(
                "Identity for {host} changed. Reloading. {}",
                crate::useragent::OVERRIDE_WARNING
            ),
            None => format!("{host} is back on the browser default identity. Reloading."),
        };
        return match outcome {
            Ok(()) => redraw(json!({ "reload_surface": true, "toast": toast })),
            Err(error) => redraw(json!({ "toast": error.to_string() })),
        };
    }

    if let Some(preset) = action.strip_prefix(USER_AGENT_ACTION_PREFIX) {
        return match crate::useragent::set_preset(preset) {
            // The UA is fixed when the webview is CREATED, so an in-page reload
            // would keep the old identity. `reload_surface` destroys and recreates
            // it (refetching /policy first), which is the only thing that can
            // change what the browser says it is.
            Ok(()) => redraw(json!({
                "reload_surface": true,
                "toast": "Browser identity changed. Reloading the surface.",
            })),
            Err(error) => redraw(json!({ "toast": error.to_string() })),
        };
    }

    let outcome = match action {
        "adblock-enabled" => crate::webpolicy::set_adblock_enabled(posted == Some("true")),
        "adblock-profile" => {
            crate::webpolicy::set_adblock_profile_disabled(&profile, posted != Some("true"))
        }
        // `reload_surface`, NOT `eval: "location.reload()"`. A content filter and
        // its userscripts are attached to the WEBVIEW at creation, so reloading
        // the document leaves both exactly as they were — turning ad blocking off
        // and reloading in-page would appear to do nothing. Only the GUI can
        // destroy and recreate the surface, and it refetches `/policy` first.
        "reload-surface" => {
            return redraw(json!({
                "reload_surface": true,
                "toast": "Reloading the surface with the current policy.",
            }));
        }
        script if script.starts_with(USERSCRIPT_DELETE_PREFIX) => {
            let stem = script.trim_start_matches(USERSCRIPT_DELETE_PREFIX);
            crate::webpolicy::delete_userscript(stem)
        }
        // `sponsorblock:<category>:<behaviour>`. Checked BEFORE the userscript
        // arm even though the two prefixes cannot collide, so the ordering says
        // out loud which one owns the string.
        category if category.starts_with(SPONSORBLOCK_ACTION_PREFIX) => {
            let rest = category.trim_start_matches(SPONSORBLOCK_ACTION_PREFIX);
            match rest.split_once(':') {
                Some((id, behaviour)) => crate::sponsorblock::set_behaviour(id, behaviour),
                None => Err(anyhow::anyhow!(
                    "malformed SponsorBlock action {category:?} \
                     (want sponsorblock:<category>:<behaviour>)"
                )),
            }
        }
        install if install.starts_with(INSTALL_ACTION_PREFIX) => {
            let stem = install.trim_start_matches(INSTALL_ACTION_PREFIX);
            match crate::extensions::find(stem) {
                Some(ext) => crate::webpolicy::install_userscript(ext.stem, ext.body),
                None => Err(anyhow::anyhow!("no bundled extension named {stem:?}")),
            }
        }
        script if script.starts_with(USERSCRIPT_ACTION_PREFIX) => {
            let stem = script.trim_start_matches(USERSCRIPT_ACTION_PREFIX);
            // Toggle widget → its posted state; list-row button → flip current.
            let enable = match posted {
                Some("true") => true,
                Some("false") => false,
                _ => !crate::webpolicy::userscript_enabled(stem).unwrap_or(false),
            };
            crate::webpolicy::set_userscript_enabled(stem, enable)
        }
        other => return json!({ "toast": format!("unknown action {other:?}") }),
    };

    // Redraw from disk either way: a failed rename must snap the toggle back to
    // what the file system actually says, not leave it showing the click.
    match outcome {
        Ok(()) => redraw(json!({ "toast": "Saved. Reload the surface to apply." })),
        Err(error) => redraw(json!({ "toast": error.to_string() })),
    }
}

/// A per-site zoom click. `−`/`+` step the override from the live effective zoom
/// the GUI reported; `Reset` clears it. The reply asks the GUI to re-read `/zoom`
/// so the change reaches the live page at once.
fn run_zoom_action(profile: &str, action: &str, page: &PageContext) -> Value {
    let Some(host) = page.host() else {
        return json!({ "toast": "No site is open to zoom." });
    };
    let base = page.zoom.unwrap_or(100.0);
    let outcome = match action {
        ZOOM_IN_ACTION => crate::webzoom::set(host, Some(base + crate::webzoom::ZOOM_STEP)),
        ZOOM_OUT_ACTION => crate::webzoom::set(host, Some(base - crate::webzoom::ZOOM_STEP)),
        ZOOM_RESET_ACTION => crate::webzoom::set(host, None),
        _ => return json!({ "toast": "unknown zoom action" }),
    };
    // Redraw: for a step the override now exists and the row shows it exactly;
    // for a reset it is gone, so pass no live zoom and the row reads "global".
    let mut next = page.clone();
    if action == ZOOM_RESET_ACTION {
        next.zoom = None;
    }
    let schema = settings_schema(profile, &next);
    match outcome {
        Ok(()) => json!({ "schema": schema, "refetch_zoom": true }),
        Err(error) => json!({ "schema": schema, "toast": error.to_string() }),
    }
}

/// The live zoom the GUI reports, tolerant of a number or a stringified number
/// (action values arrive as strings; a query param is text too).
fn read_zoom(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

/// A bool the GUI reports, tolerant of a real bool or the strings "true"/"false".
fn read_bool(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| match value.as_str() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    })
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

/// A JS string literal. The secret is embedded in the script the GUI injects
/// into the surface — that is the whole point of `eval`: the app computes the
/// credential host-side, and the GUI only injects it. It never lands in
/// yggterm's state, a schema, or the OSC stream.
fn js_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '<' => out.push_str("\\u003c"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Set a field the way a real user would: assign, then fire `input` and
/// `change`, or a framework-controlled field silently reverts on the next
/// render.
const SET_FIELD: &str = r#"
function ychromeSet(el, value) {
  if (!el) return false;
  const proto = Object.getPrototypeOf(el);
  const setter = Object.getOwnPropertyDescriptor(proto, 'value');
  if (setter && setter.set) { setter.set.call(el, value); } else { el.value = value; }
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return true;
}
"#;

fn fill_script(username: &str, password: &str) -> String {
    format!(
        r#"(function() {{
{SET_FIELD}
  const pw = document.querySelector('input[type=password]:not([disabled])');
  let user = null;
  if (pw) {{
    const form = pw.form || document;
    const candidates = Array.from(form.querySelectorAll('input'));
    const pwIndex = candidates.indexOf(pw);
    user = candidates.slice(0, pwIndex < 0 ? candidates.length : pwIndex).reverse().find((el) =>
      ['text', 'email', 'tel', ''].includes((el.type || '').toLowerCase()) && !el.disabled);
  }}
  if (!user) {{
    user = document.querySelector('input[autocomplete=username], input[name*=user i], input[type=email]');
  }}
  const filledUser = {username} ? ychromeSet(user, {username}) : false;
  const filledPw = ychromeSet(pw, {password});
  if (pw) {{ pw.focus(); }}
  return filledPw ? 'filled' : (filledUser ? 'user-only' : 'no-fields');
}})()"#,
        username = js_string(username),
        password = js_string(password),
    )
}

/// Type a card into a payment form. The autocomplete tokens are the WHATWG
/// names every payment form is supposed to carry; the name/id heuristics behind
/// them catch the ones that do not.
///
/// The script returns the list of FIELD NAMES it filled, never a value — that
/// return string is what lands in the GUI's action log, and a PAN must not.
fn card_fill_script(
    number: &str,
    code: &str,
    exp_month: &str,
    exp_year: &str,
    cardholder: &str,
) -> String {
    format!(
        r#"(function() {{
{SET_FIELD}
  const pick = (...sel) => sel.map((s) => document.querySelector(s)).find((el) => el && !el.disabled) || null;
  const filled = [];
  const put = (label, el, value) => {{
    if (value && ychromeSet(el, value)) {{ filled.push(label); }}
  }};
  put('cc-number', pick('input[autocomplete="cc-number"]', 'input[name*=cardnumber i]',
    'input[name*=card_number i]', 'input[id*=cardnumber i]', 'input[name*=cardno i]'), {number});
  put('cc-csc', pick('input[autocomplete="cc-csc"]', 'input[name*=cvv i]', 'input[name*=cvc i]',
    'input[name*=csc i]', 'input[name*=securitycode i]', 'input[id*=cvv i]'), {code});
  put('cc-exp-month', pick('[autocomplete="cc-exp-month"]', 'select[name*=expmonth i]',
    'input[name*=expmonth i]', '[name*=exp_month i]'), {exp_month});
  put('cc-exp-year', pick('[autocomplete="cc-exp-year"]', 'select[name*=expyear i]',
    'input[name*=expyear i]', '[name*=exp_year i]'), {exp_year});
  put('cc-name', pick('input[autocomplete="cc-name"]', 'input[name*=cardholder i]',
    'input[name*=nameoncard i]'), {cardholder});
  return filled.length ? filled.join(',') : 'no-card-fields';
}})()"#,
        number = js_string(number),
        code = js_string(code),
        exp_month = js_string(exp_month),
        exp_year = js_string(exp_year),
        cardholder = js_string(cardholder),
    )
}

fn totp_script(code: &str) -> String {
    format!(
        r#"(function() {{
{SET_FIELD}
  const otp = document.querySelector(
    'input[autocomplete="one-time-code"], input[name*=otp i], input[name*=totp i], input[id*=otp i], input[name*=code i]');
  if (!otp) return 'no-otp-field';
  ychromeSet(otp, {code});
  otp.focus();
  return 'filled';
}})()"#,
        code = js_string(code),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The passkey shim is the ONE script ychrome injects that must live in the
    // page's own world: it replaces `navigator.credentials` for the PAGE to
    // call, and a replacement made in an isolated world is invisible from the
    // page. Every other script defaults to isolated, so the `/policy` route has
    // to say `in_main_world()` out loud — and it has to go through `prepend`, or
    // it lands in the legacy array only and a new GUI drops it.
    //
    // Source-anchored on the route arm, not the file: a `.in_main_world()`
    // anywhere else must not satisfy this.
    #[test]
    fn the_policy_route_puts_the_passkey_shim_first_and_in_the_page_world() {
        let source = include_str!("sidebar.rs");
        let arm = source
            .split("(\"GET\", \"/policy\") => {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n        }").next())
            .expect("the /policy route arm is present");
        assert!(
            arm.contains("policy.prepend("),
            "the shim must go through `Policy::prepend`, which owns BOTH wire \
             shapes; splicing it into the JSON afterwards puts it in the legacy \
             array only",
        );
        assert!(
            arm.contains("passkey_shim_scripts(state)"),
            "the /policy route must source the shim from its one owner, which is \
             also where the per-origin scoping lives",
        );
        // The main-world requirement did not go away when the shim moved into
        // `passkey_shim_scripts` for scoping — it moved with it. Anchored on
        // that function for the same reason this test was anchored on the route
        // arm: an `.in_main_world()` anywhere else must not satisfy it.
        let builder = source
            .split("fn passkey_shim_scripts(state: &ControlState) -> Vec<crate::userscript::Userscript> {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("passkey_shim_scripts is present");
        assert!(
            builder.contains(".in_main_world()"),
            "the passkey shim was left on the isolated default, where its patch \
             to `navigator.credentials` is invisible to every page that calls it",
        );
    }

    // The active User-Agent preset is marked by SELECTION, the row vocabulary
    // yggterm already draws — never by a glyph smuggled into the title. The
    // `●` prefix painted in the row's text colour (black in the light theme)
    // and shoved the label one character right, and it only existed because
    // the schema had no way to say "this row is the current one".
    #[test]
    fn the_active_user_agent_row_is_marked_by_selection_not_a_glyph_in_its_title() {
        let widgets = user_agent_widgets();
        let rows: Vec<&Value> = widgets.iter().filter(|w| w["kind"] == "list-row").collect();
        assert!(
            rows.len() > 1,
            "there is more than one preset to choose from"
        );
        for row in &rows {
            let title = row["title"].as_str().expect("every row has a title");
            assert!(
                !title.contains('\u{25cf}'),
                "status must not live in the title: {title:?}"
            );
        }
        let selected: Vec<&&Value> = rows
            .iter()
            .filter(|row| row["selected"] == json!(true))
            .collect();
        assert_eq!(
            selected.len(),
            1,
            "exactly one preset is in use, and the row says so"
        );
        assert_eq!(
            selected[0]["title"].as_str(),
            Some(crate::useragent::preset().label()),
            "the selected row is the preset actually in use"
        );
    }

    // An action is routed by the pane it came from, not by its name. Without
    // this, a settings click would be answered with the VAULT's schema and the
    // rail would redraw as the wrong pane.
    #[test]
    fn a_settings_action_is_routed_to_the_settings_pane() {
        let state = Arc::new(Mutex::new(PaneState::new("personal")));
        let reply = run_action(
            &state,
            &json!({"pane": SETTINGS_PANE, "action": "reload-surface", "values": {}}),
        );
        assert_eq!(reply["reload_surface"], true);
        assert_eq!(reply["schema"]["title"], "YChrome Settings");
    }

    // An unknown settings action must not touch the disk or fall through to the
    // vault's arms (where "sync" et al. would happily run).
    #[test]
    fn an_unknown_settings_action_only_toasts() {
        let state = Arc::new(Mutex::new(PaneState::new("personal")));
        let reply = run_action(
            &state,
            &json!({"pane": SETTINGS_PANE, "action": "sync", "values": {}}),
        );
        assert!(
            reply["schema"].is_null(),
            "an unknown action redrew the pane"
        );
        assert!(
            reply["toast"]
                .as_str()
                .unwrap_or_default()
                .contains("unknown"),
            "expected an unknown-action toast, got {reply:?}"
        );
    }

    // A policy change needs the WEBVIEW recreated, not the document reloaded: a
    // content filter and its userscripts are attached at creation, so
    // `location.reload()` would leave ad blocking exactly as it was. Asking for
    // an in-page reload here is a silent no-op the user reads as a broken toggle.
    #[test]
    fn reloading_the_surface_asks_the_gui_to_recreate_it() {
        let state = Arc::new(Mutex::new(PaneState::new("default")));
        let reply = run_settings_action(
            &state,
            &json!({"pane": SETTINGS_PANE, "action": "reload-surface", "values": {}}),
        );
        assert_eq!(reply["reload_surface"], true);
        assert!(
            reply["eval"].is_null(),
            "an in-page reload cannot detach a content filter"
        );
    }

    fn policy_state(rules: bool, userscripts: &[(&str, bool)]) -> crate::webpolicy::PolicyState {
        crate::webpolicy::PolicyState {
            adblock_rules_present: rules,
            adblock_rule_count: 42,
            adblock_enabled: true,
            adblock_profile_disabled: false,
            userscripts: userscripts
                .iter()
                .map(|(stem, on)| crate::webpolicy::UserscriptStatus {
                    stem: stem.to_string(),
                    enabled: *on,
                    refusal: None,
                    note: None,
                })
                .collect(),
        }
    }

    fn no_zoom() -> std::collections::BTreeMap<String, f64> {
        std::collections::BTreeMap::new()
    }

    /// No origin has been answered about the camera or the microphone. The
    /// DEFAULT state of a fresh profile, and the one every pre-existing settings
    /// test means when it says nothing about capture.
    fn no_media() -> std::collections::BTreeMap<String, crate::webmedia::SiteDecisions> {
        std::collections::BTreeMap::new()
    }

    /// ⛔ THE parse lock for the capture pane. An origin with a NON-DEFAULT PORT
    /// carries three colons of its own, so a left-to-right split writes the
    /// decision under `https` and the user's Block silently does nothing while
    /// the pane reports success. Proven here rather than in a live click,
    /// because that failure looks like a working button.
    #[test]
    fn a_capture_action_is_read_from_the_right_so_a_ported_origin_survives() {
        use crate::webmedia::{Decision, Device};
        assert_eq!(
            parse_media_permission_action("media-permission:http://127.0.0.1:8099:camera:deny"),
            Some((
                "http://127.0.0.1:8099".to_string(),
                Device::Camera,
                Decision::Deny
            )),
        );
        assert_eq!(
            parse_media_permission_action("media-permission:https://example.com:microphone:ask"),
            Some((
                "https://example.com".to_string(),
                Device::Microphone,
                Decision::Ask
            )),
        );
        // Unknown device: refused, not guessed.
        assert_eq!(
            parse_media_permission_action("media-permission:https://a.test:speaker:allow"),
            None,
        );
        // An unknown DECISION word degrades to Ask, never to a grant.
        assert_eq!(
            parse_media_permission_action("media-permission:https://a.test:camera:yes")
                .map(|parsed| parsed.2),
            Some(Decision::Ask),
        );
        // A different action is not this one.
        assert_eq!(parse_media_permission_action("zoom-in"), None);
    }

    /// Revoke and set share a stem, and the shorter one is a prefix of the
    /// longer. If the dispatcher ever checks `media-permission:` first, Revoke is
    /// swallowed and parses as an origin of `forget` — so the ORDER is the lock.
    #[test]
    fn revoke_is_not_swallowed_by_the_set_action_prefix() {
        assert!(
            MEDIA_PERMISSION_FORGET_PREFIX
                .starts_with(MEDIA_PERMISSION_ACTION_PREFIX.trim_end_matches(':'))
        );
        let revoke = format!("{MEDIA_PERMISSION_FORGET_PREFIX}https://example.com");
        assert!(
            revoke
                .strip_prefix(MEDIA_PERMISSION_FORGET_PREFIX)
                .is_some()
        );
        let body = include_str!("sidebar.rs");
        let forget_at = body
            .find("if let Some(origin) = action.strip_prefix(MEDIA_PERMISSION_FORGET_PREFIX)")
            .expect("the Revoke arm is gone from run_settings_action");
        let set_at = body
            .find("if action.starts_with(MEDIA_PERMISSION_ACTION_PREFIX)")
            .expect("the set arm is gone from run_settings_action");
        assert!(
            forget_at < set_at,
            "the set arm is checked before Revoke; `media-permission:` is a prefix \
             of `media-permission-forget:`, so every Revoke click would be read as \
             a set with a nonsense origin",
        );
    }

    /// The pane REVIEWS and REVOKES, and — deliberately — offers no way to grant.
    #[test]
    fn the_settings_pane_lists_every_remembered_origin_with_a_way_out() {
        use crate::webmedia::{Decision, SiteDecisions};
        let media: std::collections::BTreeMap<String, SiteDecisions> = [
            (
                "https://meet.example.com".to_string(),
                SiteDecisions {
                    camera: Decision::Allow,
                    microphone: Decision::Allow,
                },
            ),
            (
                "https://ads.example.net".to_string(),
                SiteDecisions {
                    camera: Decision::Deny,
                    microphone: Decision::Ask,
                },
            ),
        ]
        .into_iter()
        .collect();
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(true, &[]),
            &media,
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        let row = |id: &str| {
            widgets
                .iter()
                .find(|widget| widget["id"] == id)
                .unwrap_or_else(|| panic!("no {id} row in {schema}"))
        };
        let granted = row("media-permission-https://meet.example.com");
        assert_eq!(granted["title"], "https://meet.example.com");
        let subtitle = granted["subtitle"].as_str().expect("subtitle");
        assert!(
            subtitle.contains("Camera allowed") && subtitle.contains("mic allowed"),
            "the row does not say what is remembered: {subtitle}",
        );
        let blocked = row("media-permission-https://ads.example.net");
        let blocked_subtitle = blocked["subtitle"].as_str().expect("subtitle");
        assert!(
            blocked_subtitle.contains("Camera blocked") && blocked_subtitle.contains("mic asks"),
            "a blocked camera and an untouched mic must read differently: \
             {blocked_subtitle}",
        );
        // ⛔ EXACTLY ONE action per row, and it is Revoke. Measured live in the
        // rail (2026-08-01): a three-action row squeezed the TITLE element to
        // **0 px** under `overflow:hidden; white-space:nowrap`, so the origin —
        // the one fact this section exists to show — painted nothing and the
        // user could not tell which site the buttons belonged to. Every extra
        // button here is width taken from the site name.
        for (id, row_value) in [
            ("https://meet.example.com", &granted),
            ("https://ads.example.net", &blocked),
        ] {
            let actions = row_value["actions"].as_array().expect("actions");
            assert_eq!(
                actions.len(),
                1,
                "{id}: {} actions on a capture row — the rail gives the actions \
                 their width first and the title takes what is left, so this \
                 blanks the origin",
                actions.len(),
            );
            assert_eq!(actions[0]["label"], "Revoke");
        }
        // ⛔ Nothing in this pane hands out a device.
        let text = schema.to_string();
        assert!(
            !text.contains(":camera:allow") && !text.contains(":microphone:allow"),
            "the settings pane offers a GRANT action; a capture grant may only be \
             created at the moment a page asked, in yggterm's prompt",
        );
    }

    #[test]
    fn the_settings_pane_says_so_when_nothing_is_remembered() {
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(true, &[]),
            &no_media(),
        );
        let text = schema.to_string();
        assert!(
            text.contains("Camera & microphone"),
            "the capture section is missing entirely",
        );
        assert!(
            text.contains("No site has been given the camera or the microphone"),
            "the empty state does not tell the user what the default is",
        );
    }

    /// `GET /media-permission` answers ONE live ask with a decision word, and
    /// with no origin it is the whole map the pane renders.
    #[test]
    fn the_control_endpoint_answers_a_live_ask_and_serves_the_whole_map() {
        // No origin: the map shape, whatever is on this host's disk.
        let all = media_permission_query("");
        assert!(
            all.get("sites").is_some(),
            "the map form lost its `sites` key"
        );
        // With an origin: always a decision word, never an error shape the GUI
        // could misread. An origin nothing was remembered for asks.
        let one = media_permission_query("origin=https%3A%2F%2Fnobody.invalid&audio=1&video=1");
        assert_eq!(one["decision"], "ask");
        assert_eq!(one["camera"], "ask");
        assert_eq!(one["microphone"], "ask");
        assert_eq!(one["origin"], "https://nobody.invalid");
        // A page with no origin to key a decision to still asks.
        let none = media_permission_query("origin=about%3Ablank&audio=1&video=0");
        assert_eq!(none["decision"], "ask");
        assert_eq!(none["origin"], Value::Null);
        // A write with no usable origin is REFUSED rather than stored under a key
        // that could never match again.
        let (status, _) = media_permission_write(&json!({
            "origin": "about:blank", "camera": "allow"
        }));
        assert_eq!(status, 400);
        let (status, _) = media_permission_write(&json!({ "camera": "allow" }));
        assert_eq!(status, 400);
    }

    /// ⛔ A page must never be able to read or write what the human decided about
    /// its camera. The control port is page-reachable through yggterm's
    /// `yggterm-appctl://` bridge, so this route living outside the token gate
    /// would put the whole capture memory on the web.
    #[test]
    fn the_capture_memory_is_gui_only() {
        assert!(requires_gui_token("GET", "/media-permission"));
        assert!(requires_gui_token("POST", "/media-permission"));
        assert!(matches!(
            route_access("/media-permission"),
            RouteAccess::GuiOnly
        ));
    }

    // The "This site" row shows the override number and a Reset when a site is
    // custom; on the global it shows the GUI's reported number and no Reset.
    #[test]
    fn the_zoom_row_reflects_override_vs_global() {
        let sites: std::collections::BTreeMap<String, f64> =
            [("youtube.com".to_string(), 130.0)].into_iter().collect();

        let custom = current_site_zoom_widgets(Some("www.youtube.com"), Some(130.0), &sites);
        let row = &custom[0];
        assert_eq!(row["kind"], "list-row");
        assert!(
            row["subtitle"]
                .as_str()
                .unwrap()
                .contains("130% · this site")
        );
        let actions: Vec<&str> = row["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["action"].as_str().unwrap())
            .collect();
        assert_eq!(
            actions,
            [ZOOM_OUT_ACTION, ZOOM_RESET_ACTION, ZOOM_IN_ACTION]
        );

        let global = current_site_zoom_widgets(Some("example.com"), Some(110.0), &sites);
        let row = &global[0];
        assert!(row["subtitle"].as_str().unwrap().contains("110% · global"));
        let actions: Vec<&str> = row["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["action"].as_str().unwrap())
            .collect();
        assert_eq!(
            actions,
            [ZOOM_OUT_ACTION, ZOOM_IN_ACTION],
            "no Reset on the global"
        );
    }

    // No site open ⇒ a hint, never a zoom row that would act on nothing.
    #[test]
    fn the_zoom_row_needs_a_site() {
        let widgets = current_site_zoom_widgets(None, None, &no_zoom());
        assert_eq!(widgets[0]["kind"], "label");
        assert!(widgets[0]["text"].as_str().unwrap().contains("Open a site"));
    }

    // The per-profile override must name the jar it governs, or the user cannot
    // tell which identity they just turned ad blocking off for.
    #[test]
    fn the_settings_schema_names_the_running_profile() {
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(true, &[]),
            &no_media(),
        );
        assert_eq!(schema["title"], "YChrome Settings");
        assert!(
            schema.to_string().contains("work"),
            "profile missing from {schema}"
        );
    }

    // The tab toggles are a VIEW of yggterm's prefs: they render what the GUI
    // injected, never a copy ychrome keeps.
    #[test]
    fn the_tab_toggles_render_the_prefs_the_gui_reported() {
        let page = PageContext {
            vertical_tabs: true,
            restore_tabs: false,
            ..PageContext::default()
        };
        let schema = settings_schema_from(
            "work",
            &page,
            &no_zoom(),
            &policy_state(true, &[]),
            &no_media(),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        let toggle = |id: &str| {
            widgets
                .iter()
                .find(|widget| widget["id"] == id)
                .unwrap_or_else(|| panic!("no {id} toggle in {schema}"))
        };
        assert_eq!(toggle("tabs-vertical")["value"], true);
        assert_eq!(toggle("tabs-vertical")["action"], VERTICAL_TABS_ACTION);
        assert_eq!(toggle("tabs-restore")["value"], false);
        assert_eq!(toggle("tabs-restore")["action"], RESTORE_TABS_ACTION);
    }

    // Flipping a tab toggle writes nothing here: it asks the GUI (which owns the
    // tabs) via `surface_prefs`, and echoes the requested state so the switch
    // lands under the user's finger instead of snapping back for a heartbeat.
    #[test]
    fn a_tab_toggle_asks_the_gui_and_echoes_the_new_state() {
        let state = Mutex::new(PaneState::new("work"));
        let reply = run_settings_action(
            &state,
            &json!({
                "pane": SETTINGS_PANE,
                "action": VERTICAL_TABS_ACTION,
                "values": { "value": "true", "vertical_tabs": false, "restore_tabs": false },
            }),
        );
        assert_eq!(reply["surface_prefs"]["vertical_tabs"], true);
        assert!(
            reply["surface_prefs"].get("restore_tabs").is_none(),
            "an untouched pref must be absent, not sent as false: {reply}"
        );
        let widgets = reply["schema"]["widgets"].as_array().expect("widgets");
        let vertical = widgets
            .iter()
            .find(|widget| widget["id"] == "tabs-vertical")
            .expect("vertical toggle");
        assert_eq!(
            vertical["value"], true,
            "the schema must echo the new state"
        );
    }

    // The identity picker marks the live preset and offers "Use" on the others.
    #[test]
    fn the_identity_rows_offer_every_preset_but_the_current_one() {
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(true, &[]),
            &no_media(),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        for preset in crate::useragent::Preset::ALL {
            let row = widgets
                .iter()
                .find(|widget| widget["id"] == format!("ua-{}", preset.id()))
                .unwrap_or_else(|| panic!("no row for {}", preset.id()));
            let actions = row["actions"].as_array().expect("actions");
            if preset == crate::useragent::preset() {
                assert!(actions.is_empty(), "the live identity offered a Use button");
            } else {
                assert_eq!(
                    actions[0]["action"],
                    format!("{USER_AGENT_ACTION_PREFIX}{}", preset.id())
                );
            }
        }
    }

    // With no ruleset on this host there is nothing to toggle: say so, rather
    // than offering a switch that governs nothing.
    #[test]
    fn a_host_with_no_ruleset_offers_no_adblock_toggle() {
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(false, &[]),
            &no_media(),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        assert!(
            !widgets.iter().any(|w| w["id"] == "adblock-enabled"),
            "offered an adblock toggle with no ruleset installed"
        );
    }

    // Every catalogued category gets a row, and the row offers exactly the
    // states it is NOT in. Asserted structurally against
    // `crate::sponsorblock`'s catalogue rather than against a hand-written list
    // of eleven ids, so a category added there is covered the day it lands and
    // a pane that quietly drops one goes red.
    #[test]
    fn every_sponsorblock_category_gets_a_row_offering_the_states_it_is_not_in() {
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(true, &[("sponsorblock", true)]),
            &no_media(),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        let live: std::collections::HashMap<&str, &str> = crate::sponsorblock::effective()
            .into_iter()
            .map(|(category, behaviour)| (category.id, behaviour))
            .collect();
        for category in crate::sponsorblock::catalog() {
            let row = widgets
                .iter()
                .find(|w| w["id"] == format!("sponsorblock-{}", category.id))
                .unwrap_or_else(|| panic!("no settings row for {}", category.id));
            assert_eq!(row["kind"], "list-row");
            assert_eq!(row["title"], category.label);
            let current = live[category.id];
            let offered: Vec<&str> = row["actions"]
                .as_array()
                .expect("actions")
                .iter()
                .map(|a| a["action"].as_str().expect("action id"))
                .collect();
            let expected: Vec<String> = category
                .options
                .iter()
                .filter(|option| **option != current)
                .map(|option| format!("sponsorblock:{}:{option}", category.id))
                .collect();
            assert_eq!(
                offered, expected,
                "{} offers the wrong states (it is currently {current})",
                category.id
            );
            assert!(
                !offered.contains(&format!("sponsorblock:{}:{current}", category.id).as_str()),
                "{} offers a button for the state it is already in",
                category.id
            );
        }
    }

    // The action id the row emits must be the one the dispatcher parses. These
    // are two different pieces of code agreeing on one string, which is exactly
    // where a format-string edit on one side ships a dead button.
    #[test]
    fn a_category_action_id_round_trips_through_the_dispatchers_parser() {
        for category in crate::sponsorblock::catalog() {
            for option in category.options {
                let action = sponsorblock_action(category.id, option);
                let rest = action
                    .strip_prefix(SPONSORBLOCK_ACTION_PREFIX)
                    .unwrap_or_else(|| panic!("{action} lost its prefix"));
                let (id, behaviour) = rest
                    .split_once(':')
                    .unwrap_or_else(|| panic!("{action} has no behaviour half"));
                assert_eq!(id, category.id);
                assert_eq!(behaviour, *option);
                assert!(
                    crate::sponsorblock::find(id).is_some(),
                    "{action} names a category the catalogue does not have"
                );
            }
        }
        // The on/off toggle's id must not be eaten by the category arm.
        assert!(
            !format!("{USERSCRIPT_ACTION_PREFIX}sponsorblock")
                .starts_with(SPONSORBLOCK_ACTION_PREFIX),
            "the SponsorBlock on/off toggle would be parsed as a category action"
        );
    }

    // A disabled script offers no category rows: a control that nothing will
    // act on is worse than no control.
    #[test]
    fn a_disabled_sponsorblock_offers_no_category_rows() {
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(true, &[("sponsorblock", false)]),
            &no_media(),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        assert!(
            !widgets.iter().any(|w| {
                w["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("sponsorblock-"))
            }),
            "category rows drawn for a script that is switched off"
        );
    }

    // SponsorBlock gets its own named toggle; a plain userscript becomes a
    // list-row with Enable/Disable + Delete actions, keyed by stem.
    #[test]
    fn sponsorblock_is_promoted_and_other_scripts_get_delete_rows() {
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(true, &[("sponsorblock", true), ("darkmode", false)]),
            &no_media(),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        // SponsorBlock: its own toggle, friendly label, NOT in the generic list.
        let sponsor = widgets
            .iter()
            .find(|w| w["id"] == "userscript:sponsorblock")
            .expect("sponsorblock toggle");
        assert_eq!(sponsor["kind"], "toggle");
        assert_eq!(sponsor["value"], true);
        assert!(widgets.iter().any(|w| w["text"] == "SponsorBlock"));
        // darkmode: a managed list-row with a toggle action and a delete action.
        let dark = widgets
            .iter()
            .find(|w| w["id"] == "script-darkmode")
            .expect("darkmode row");
        assert_eq!(dark["kind"], "list-row");
        assert_eq!(dark["subtitle"], "Disabled");
        let actions: Vec<&str> = dark["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["action"].as_str().unwrap())
            .collect();
        assert_eq!(
            actions,
            ["userscript:darkmode", "userscript-delete:darkmode"]
        );
        // sponsorblock must NOT also appear as a managed script row.
        assert!(
            !widgets.iter().any(|w| w["id"] == "script-sponsorblock"),
            "sponsorblock leaked into the generic userscripts list"
        );
    }

    // A script the promotion gate refuses is on disk and "enabled" by
    // filename, but injected NOWHERE. The pane must say that — a user who
    // never reads the daemon's stderr must still learn why their script does
    // nothing, and which header lines to fix.
    #[test]
    fn a_refused_script_is_not_presented_as_enabled() {
        let mut state = policy_state(true, &[("broken", true)]);
        state.userscripts[0].refusal =
            Some("Refused — not injected: @exclude https://*.youtube.com/embed/*".to_string());
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &state,
            &no_media(),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        let row = widgets
            .iter()
            .find(|w| w["id"] == "script-broken")
            .expect("broken row");
        let subtitle = row["subtitle"].as_str().expect("subtitle");
        assert_ne!(
            subtitle, "Enabled",
            "a refused script was presented as running"
        );
        assert!(
            subtitle.contains("Refused"),
            "the refusal state is invisible in the pane: {subtitle}"
        );
        assert!(
            subtitle.contains("@exclude https://*.youtube.com/embed/*"),
            "the refusal must name the offending line verbatim: {subtitle}"
        );
        // The row keeps its controls: refusal is a verdict, not a lock-out.
        let actions: Vec<&str> = row["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["action"].as_str().unwrap())
            .collect();
        assert_eq!(actions, ["userscript:broken", "userscript-delete:broken"]);
    }

    // The catalog shows only what is NOT installed, judged against the SAME state
    // snapshot the pane draws from. sponsorblock is installed here, so it is
    // absent from "Add an extension"; unblock-select is not, so it is offered.
    #[test]
    fn the_catalog_offers_only_uninstalled_extensions() {
        let schema = settings_schema_from(
            "work",
            &PageContext::default(),
            &no_zoom(),
            &policy_state(true, &[("sponsorblock", true)]),
            &no_media(),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        assert!(
            !widgets.iter().any(|w| w["id"] == "catalog-sponsorblock"),
            "an installed extension was still offered in the catalog"
        );
        let unblock = widgets
            .iter()
            .find(|w| w["id"] == "catalog-unblock-select")
            .expect("unblock-select should be offered when not installed");
        assert_eq!(unblock["actions"][0]["action"], "install:unblock-select");
    }

    // The security line is honest and omitted when unknown: HTTPS -> a lock,
    // HTTP -> a warning, None (older GUI) -> nothing.
    #[test]
    fn the_security_line_reflects_https_or_is_omitted() {
        assert!(current_site_security_widgets(None, Some(true)).is_empty());
        assert!(current_site_security_widgets(Some("x.com"), None).is_empty());
        let secure = current_site_security_widgets(Some("x.com"), Some(true));
        assert!(secure[0]["text"].as_str().unwrap().contains("Secure"));
        let insecure = current_site_security_widgets(Some("x.com"), Some(false));
        assert!(insecure[0]["text"].as_str().unwrap().contains("Not secure"));
    }

    // A row id must survive names that contain the characters a vault really
    // holds — this user's vault has names with tabs and newlines.
    #[test]
    fn row_id_round_trips_awkward_names() {
        for (name, user) in [
            ("github.com", "octocat"),
            ("weird\tname\nwith breaks", "a@b.c"),
            ("no user", ""),
            ("has=equals&and?q", "u"),
        ] {
            let (back_name, back_user) = split_row_id(&row_id(name, user));
            assert_eq!((back_name.as_str(), back_user.as_str()), (name, user));
        }
    }

    // A row is built from the agent's SECRET-FREE item metadata, and carries
    // none of it onward. (`vault_schema` itself is not unit-testable without a
    // live agent — it would read the user's real vault, which a test must never
    // do; the no-secret guarantee is enforced here, at the only place an item
    // becomes a widget.)
    #[test]
    fn item_row_carries_no_secret() {
        let item = json!({
            "name": "github.com",
            "username": "octocat",
            "folder": "Work",
            "has_password": true,
            "has_totp": true,
            // Even if the agent ever handed these over, a row must not echo them.
            "password": "hunter2",
            "totp_secret": "GEZDGNBVGY3TQOJQ",
        });
        let row = item_row(&item);
        let wire = row.to_string();
        assert!(!wire.contains("hunter2"), "password leaked into a row");
        assert!(
            !wire.contains("GEZDGNBVGY3TQOJQ"),
            "totp secret leaked into a row"
        );
        assert_eq!(row["title"], "github.com");
        assert_eq!(row["subtitle"], "octocat · Work");
        // ⏱ only where a secret actually exists — `rbw list` could not say.
        let actions: Vec<&str> = row["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["action"].as_str().unwrap())
            .collect();
        assert_eq!(actions, ["fill", "totp"]);

        let plain = item_row(&json!({"name": "n", "username": "", "has_totp": false}));
        let actions = plain["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1, "no authenticator secret, no ⏱ button");
        assert_eq!(plain["subtitle"], "");
    }

    // The secret is embedded in the eval script (that is the design), but it
    // must be escaped so it cannot break out of the string literal.
    #[test]
    fn fill_script_escapes_a_hostile_password() {
        let script = fill_script("a\"b", "p\"; alert(1); //");
        assert!(script.contains(r#""a\"b""#));
        assert!(script.contains(r#""p\"; alert(1); //""#));
        assert!(
            !script.contains("\"; alert(1); //\";"),
            "escaped out of the literal"
        );
    }

    // A card cipher has no password, so the login fill would refuse it before it
    // reached the page ("has no password"). The row must offer the injector that
    // CAN work, and must still carry no secret — the type is metadata, the
    // number never travels with it.
    #[test]
    fn a_card_row_offers_the_card_injector_and_still_carries_no_secret() {
        let card = item_row(&json!({
            "name": "HDFC Regalia",
            "username": "",
            "folder": "Cards",
            "item_type": 3,
            "has_password": false,
            "has_totp": false,
            // Even if the agent ever handed these over, a row must not echo them.
            "number": "4111111111114242",
            "code": "737",
        }));
        let actions: Vec<&str> = card["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["action"].as_str().unwrap())
            .collect();
        assert_eq!(actions, ["card-fill"], "a card cannot use the login fill");
        let wire = card.to_string();
        assert!(!wire.contains("4111111111114242"), "PAN leaked: {wire}");
        assert!(!wire.contains("737"), "CVV leaked: {wire}");

        // A login is untouched by any of this.
        let login = item_row(&json!({
            "name": "github.com", "username": "octocat", "item_type": 1, "has_totp": true,
        }));
        let actions: Vec<&str> = login["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["action"].as_str().unwrap())
            .collect();
        assert_eq!(actions, ["fill", "totp"]);
    }

    // The PAN rides the eval script (that is the design — the app computes, the
    // GUI injects), so it must be escaped like any other injected secret. What
    // the script RETURNS is the other half: the GUI logs that string, so it may
    // name fields and never values.
    #[test]
    fn card_fill_script_escapes_the_pan_and_returns_field_names_only() {
        let script = card_fill_script("4111\"; alert(1); //", "737", "11", "2029", "A \"Q\" K");
        assert!(script.contains(r#""4111\"; alert(1); //""#));
        assert!(
            !script.contains("\"; alert(1); //\";"),
            "escaped out of the literal"
        );
        assert!(script.contains(r#""A \"Q\" K""#));
        // The return value is built from these labels alone.
        for label in [
            "cc-number",
            "cc-csc",
            "cc-exp-month",
            "cc-exp-year",
            "cc-name",
        ] {
            assert!(script.contains(label), "missing {label}");
        }
        assert!(
            script.contains("filled.join(',')"),
            "the result must be the field names it filled"
        );
    }

    // `<` is escaped so an injected value can never open a tag if the script
    // is ever placed in an HTML context.
    #[test]
    fn js_string_escapes_control_characters_and_angle_brackets() {
        assert_eq!(js_string("a\nb"), r#""a\nb""#);
        assert_eq!(js_string("</script>"), r#""\u003c/script>""#);
    }

    // The Add tab is buildable without an agent (it shells out to nothing), so
    // the pane's central promise is testable: a schema never carries a secret.
    #[test]
    fn add_tab_schema_never_declares_a_password() {
        let mut state = PaneState {
            tab: "add".to_string(),
            ..PaneState::default()
        };
        state.seed_add_draft(Some("github.com"));
        // `unlocked_schema`, not `vault_schema`: the latter shells out to
        // `ychrome-vault status`, which a test must never do.
        let schema = unlocked_schema(&state, Some("github.com"), &json!({"state": "unlocked"}));
        let widgets = schema["widgets"].as_array().unwrap();

        let password = widgets
            .iter()
            .find(|widget| widget["id"] == "add_password")
            .expect("the Add tab has a password field");
        assert_eq!(
            password["secret"], true,
            "the password field must be masked"
        );
        assert_eq!(password["value"], "", "a schema must never carry a secret");

        // Notes is offered, seeded from the draft, and not a secret.
        let notes = widgets
            .iter()
            .find(|widget| widget["id"] == "add_notes")
            .expect("the Add tab has a notes field");
        assert_ne!(notes["secret"], true, "notes are not a secret");

        // Seeded from the page the user is looking at.
        let named =
            |id: &str| widgets.iter().find(|widget| widget["id"] == id).unwrap()["value"].clone();
        assert_eq!(named("add_name"), "github.com");
        assert_eq!(named("add_uri"), "https://github.com");
        // The generator knobs round-trip through the schema.
        assert_eq!(named("generate_length"), DEFAULT_GENERATE_LENGTH);
        assert_eq!(named("generate_no_symbols"), false);
    }

    // The draft is seeded once per host: re-entering the tab must not clobber
    // what the user typed, and browsing elsewhere must re-seed.
    #[test]
    fn add_draft_is_seeded_once_per_host() {
        let mut state = PaneState::default();
        state.seed_add_draft(Some("github.com"));
        state.add.user = "octocat".to_string();

        state.seed_add_draft(Some("github.com"));
        assert_eq!(state.add.user, "octocat", "re-seeding clobbered the draft");

        state.seed_add_draft(Some("gitlab.com"));
        assert_eq!(state.add.name, "gitlab.com");
        assert_eq!(state.add.user, "", "a new site starts a new draft");

        // No host (a page with no host, or no surface): nothing to seed from.
        let mut blank = PaneState::default();
        blank.seed_add_draft(None);
        assert_eq!(blank.add.name, "");
        assert_eq!(blank.add.uri, "");
    }

    // A locked vault shows an unlock form in place of the tabs, and the master
    // password field is a masked, declared-empty secret — never carried in the
    // schema. `locked_schema` is pure, so this needs no agent.
    #[test]
    fn locked_schema_offers_a_masked_unlock_field() {
        let schema = locked_schema(&json!({"state": "locked", "email": "you@example.com"}));
        let widgets = schema["widgets"].as_array().unwrap();
        // No tabs: a locked vault is an unlock prompt, not a browser.
        assert!(!widgets.iter().any(|w| w["kind"] == "tabs"));
        let field = widgets
            .iter()
            .find(|w| w["id"] == "unlock_password")
            .expect("locked pane has a master-password field");
        assert_eq!(field["secret"], true, "the master password must be masked");
        assert_eq!(field["value"], "", "a schema must never carry a secret");
        // Enter in the field unlocks, without reaching for the button.
        assert_eq!(field["action"], "unlock");
        assert!(widgets.iter().any(|w| w["action"] == "unlock"));
        // The account is shown for context; the password never is.
        assert!(json!(widgets).to_string().contains("you@example.com"));

        // A host with no vault gives instructions, not an unlock field.
        let unconfigured = locked_schema(&json!({"state": "not_configured"}));
        let wire = unconfigured.to_string();
        assert!(!wire.contains("unlock_password"));
        assert!(wire.contains("configure"));
    }

    // The Add tab carries a notes draft up to the app; absorb_draft folds it in.
    #[test]
    fn add_notes_round_trips_through_the_draft() {
        let mut state = PaneState::default();
        absorb_draft(
            &mut state,
            &json!({"add_notes": "recovery codes in 1Password"}),
        );
        assert_eq!(state.add.notes, "recovery codes in 1Password");
        let schema = unlocked_schema(
            &PaneState {
                tab: "add".to_string(),
                add: AddDraft {
                    notes: "hi".to_string(),
                    ..AddDraft::default()
                },
                ..PaneState::default()
            },
            None,
            &json!({"state": "unlocked"}),
        );
        let notes = schema["widgets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"] == "add_notes")
            .expect("notes field present");
        assert_eq!(notes["value"], "hi");
    }

    // The agent outlives the binary. When `status` says so, the pane must SAY so
    // and offer the remedy — otherwise the user meets `unknown op` in a toast and
    // has to go to a terminal, which the sidebar-unlock work exists to avoid.
    #[test]
    fn a_stale_agent_is_surfaced_with_a_restart_button() {
        let stale = json!({"state": "locked", "email": "you@example.com", "agent_stale": true});
        let wire = locked_schema(&stale).to_string();
        assert!(
            wire.contains("restart_agent"),
            "no remedy offered for a stale agent"
        );
        assert!(
            wire.contains("re-locks"),
            "the cost of restarting must be stated"
        );
        // Still an unlock form: restarting lands the user right back here.
        assert!(wire.contains("unlock_password"));

        // A healthy agent gets no banner and no button.
        let fresh = json!({"state": "locked", "email": "you@example.com", "agent_stale": false});
        assert!(!locked_schema(&fresh).to_string().contains("restart_agent"));
        // Absent field (an older `status`) is treated as healthy, not stale.
        assert!(
            !locked_schema(&json!({"state": "locked"}))
                .to_string()
                .contains("restart_agent")
        );

        // Tools tab surfaces it too, for a vault that went stale while unlocked.
        let tools = PaneState {
            tab: "tools".to_string(),
            ..PaneState::default()
        };
        let unlocked_stale = json!({"state": "unlocked", "item_count": 1107, "agent_stale": true});
        let wire = unlocked_schema(&tools, None, &unlocked_stale).to_string();
        assert!(wire.contains("restart_agent"));
        assert!(wire.contains("1107 items"));
    }

    // yggterm posts only the values its CURRENT schema declares. A `tab` action
    // fired from the Fill tab carries no `add_*` keys, and must not blank them.
    #[test]
    fn absorb_draft_ignores_fields_the_schema_did_not_declare() {
        let mut state = PaneState::default();
        state.add.name = "github.com".to_string();
        state.generate_length = 32;
        state.generate_no_symbols = true;

        absorb_draft(
            &mut state,
            &json!({ "value": "fill", "host": "github.com" }),
        );
        assert_eq!(
            state.add.name, "github.com",
            "an absent field wiped the draft"
        );
        assert_eq!(state.generate_length, 32);
        assert!(state.generate_no_symbols);

        // Present fields are adopted; the number box is clamped and a half-typed
        // value leaves the setting alone.
        absorb_draft(
            &mut state,
            &json!({"add_name": "gitlab.com", "generate_length": "9999", "generate_no_symbols": "false"}),
        );
        assert_eq!(state.add.name, "gitlab.com");
        assert_eq!(state.generate_length, MAX_GENERATE_LENGTH);
        assert!(!state.generate_no_symbols);

        absorb_draft(&mut state, &json!({"generate_length": ""}));
        assert_eq!(
            state.generate_length, MAX_GENERATE_LENGTH,
            "a half-typed number wiped the setting"
        );
    }

    // The report the agent returns carries labels only. Rendering it cannot
    // invent a secret, but the widgets must still show what the user needs.
    #[test]
    fn watchtower_widgets_report_labels_only() {
        let widgets = watchtower_widgets(&json!({
            "scanned": 4,
            "reused": [["a (x)", "b (y)"]],
            "weak": ["c (z)"],
        }));
        let wire = json!(widgets).to_string();
        assert!(wire.contains("Scanned 4 logins: 1 reused-password groups, 1 weak."));
        assert!(wire.contains("Shared by 2 logins"));
        assert!(wire.contains("a (x) · b (y)"));
        assert!(wire.contains("Weak passwords (1)"));
        assert!(wire.contains("c (z)"));

        // A clean vault says so rather than rendering two empty headings.
        let clean = json!(watchtower_widgets(
            &json!({"scanned": 9, "reused": [], "weak": []})
        ))
        .to_string();
        assert!(clean.contains("No reused or weak passwords"));
        assert!(!clean.contains("Reused passwords ("));
    }

    #[test]
    fn query_values_are_percent_decoded() {
        assert_eq!(
            query_value("host=example.com", "host").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            query_value("a=1&host=a%2Eb", "host").as_deref(),
            Some("a.b")
        );
        assert_eq!(query_value("a=1", "host"), None);
    }

    // -----------------------------------------------------------------------
    // THE CONTROL-TOKEN GATE
    //
    // The control endpoint is page-reachable: yggterm registers a
    // `yggterm-appctl://` scheme in every surface's web context and proxies it
    // to THIS server, so a `fetch('yggterm-appctl://x/action', {method:'POST'})`
    // from any page in the profile used to land on `run_action` with no
    // credential at all. That reaches vault unlock, `fill` (which returns an
    // `eval` the GUI injects into the page — a credential handed to the
    // attacker), ad blocking off and userscript deletion. These locks hold the
    // gate that closed it.
    // -----------------------------------------------------------------------

    fn control_state() -> ControlState {
        ControlState::new("default", "sess-1", 41234)
    }

    /// Every gate lock below is about the REQUEST — which credential it carried
    /// and which route it aimed at — so they all drive a session whose client
    /// does declare the token. The courier's own arms are locked separately in
    /// `a_refusal_names_which_of_the_three_failures_it_is`.
    fn dispatch_live(state: &ControlState, req: &ParsedRequest) -> (u16, Value) {
        dispatch(state, req, TokenCourier::Live)
    }

    /// A request as a page would make it: whatever the page can set, and
    /// nothing it cannot. The fido2 token is deliberately fillable — every page
    /// in the profile holds it, baked into the shim userscript — which is
    /// exactly why it cannot be the credential for a GUI-only route.
    fn page_request(method: &str, path: &str, body: Value) -> ParsedRequest {
        ParsedRequest {
            method: method.to_string(),
            path: path.to_string(),
            query: String::new(),
            fido2_token: None,
            control_token: None,
            body,
        }
    }

    fn gui_request(state: &ControlState, method: &str, path: &str, body: Value) -> ParsedRequest {
        ParsedRequest {
            control_token: Some(state.control_token.clone()),
            ..page_request(method, path, body)
        }
    }

    #[test]
    fn an_untokened_action_post_is_refused_and_the_refusal_names_the_route() {
        let state = control_state();
        let (status, body) = dispatch_live(
            &state,
            &page_request(
                "POST",
                "/action",
                json!({"pane": SETTINGS_PANE, "action": "reload-surface", "values": {}}),
            ),
        );
        assert_eq!(
            status, 403,
            "an untokened /action must be refused: {body:?}"
        );
        assert_eq!(body["route"], "/action");
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("GUI-only") && error.contains("no X-Ychrome-Control token"),
            "the refusal must name itself, got {error:?}"
        );
        assert!(
            body["reload_surface"].is_null(),
            "a refused action must not have run: {body:?}"
        );
    }

    /// A page holds the SIGNER's token (it is in its own userscript). Presenting
    /// it must buy nothing on a GUI-only route — the whole reason the control
    /// token is a second, differently-sourced secret.
    #[test]
    fn the_signers_token_does_not_open_the_gui_only_routes() {
        let state = control_state();
        let stolen = ParsedRequest {
            fido2_token: Some(state.signer.token.clone()),
            control_token: Some(state.signer.token.clone()),
            ..page_request(
                "POST",
                "/action",
                json!({"pane": SETTINGS_PANE, "action": "x"}),
            )
        };
        let (status, body) = dispatch_live(&state, &stolen);
        assert_eq!(
            status, 403,
            "the shim's token must not gate /action: {body:?}"
        );
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("did not match this session"),
            "a WRONG token must be named as wrong, got {body:?}"
        );
    }

    /// The legitimate caller: the GUI's settings pane, presenting the token the
    /// declare handed it. This must drive the REAL dispatch, not a stub — the
    /// reply is `run_settings_action`'s own `reload_surface`.
    #[test]
    fn the_guis_tokened_action_reaches_the_real_dispatch() {
        let state = control_state();
        let (status, body) = dispatch_live(
            &state,
            &gui_request(
                &state,
                "POST",
                "/action",
                json!({"pane": SETTINGS_PANE, "action": "reload-surface", "values": {}}),
            ),
        );
        assert_eq!(status, 200, "the pane's tokened action must work: {body:?}");
        assert_eq!(
            body["reload_surface"], true,
            "the reply must be the settings pane's own, got {body:?}"
        );
    }

    /// `GET /pane/<id>` is a read, but it lists the user's vault item names and
    /// seeds the Add draft — page-reachable disclosure plus a mutation. Gated
    /// with the same token, and the GUI's fetch still answers.
    #[test]
    fn the_pane_schema_is_gui_only_but_still_answers_the_gui() {
        let state = control_state();
        let (status, _) =
            dispatch_live(&state, &page_request("GET", "/pane/settings", Value::Null));
        assert_eq!(status, 403, "an untokened pane fetch must be refused");
        let (status, body) = dispatch_live(
            &state,
            &gui_request(&state, "GET", "/pane/settings", Value::Null),
        );
        assert_eq!(status, 200);
        assert!(
            !body["widgets"].is_null(),
            "the GUI's pane fetch must still return a schema, got {body:?}"
        );
    }

    /// The reads that stay open, on purpose: a policy body is the very content
    /// the GUI is about to inject into the page, the zoom map is a host→percent
    /// table, and both must keep answering an OLDER GUI that has no token at
    /// all — otherwise a mixed-version deploy silently drops ad blocking.
    #[test]
    fn policy_and_zoom_reads_stay_open_to_an_untokened_caller() {
        let state = control_state();
        for path in ["/policy", "/zoom"] {
            let (status, body) = dispatch_live(&state, &page_request("GET", path, Value::Null));
            assert_eq!(status, 200, "{path} must stay open, got {status} {body:?}");
        }
        assert_eq!(route_access("/policy"), RouteAccess::Open);
        assert_eq!(route_access("/zoom"), RouteAccess::Open);
        assert_eq!(route_access("/ping"), RouteAccess::Open);
    }

    /// `/fido2` is UNCHANGED by this gate: the page routes keep answering on the
    /// signer's own bearer token (and keep 401ing without it), and the grant
    /// routes keep authenticating on the per-ceremony request_id.
    #[test]
    fn the_gate_leaves_fido2_alone() {
        let state = control_state();
        let unauthorized = dispatch_live(
            &state,
            &page_request("POST", "/fido2/get", json!({"rpId": "example.com"})),
        );
        assert_eq!(
            unauthorized.0, 401,
            "a shim call with no signer token is the signer's 401, not the gate's 403: \
             {unauthorized:?}"
        );
        let shimmed = ParsedRequest {
            fido2_token: Some(state.signer.token.clone()),
            ..page_request("POST", "/fido2/get", Value::Null)
        };
        let (status, _) = dispatch_live(&state, &shimmed);
        assert_eq!(
            status, 400,
            "a signer-tokened page route must reach the signer (400 = its own bad-request), \
             not be refused by the control gate"
        );
        let grant = page_request("POST", "/fido2/deny", json!({"request_id": "nope"}));
        assert_ne!(
            dispatch_live(&state, &grant).0,
            403,
            "grant/deny authenticate on the request_id and must not need the control token"
        );
        assert_eq!(route_access("/fido2/get"), RouteAccess::PageSigner);
        assert_eq!(route_access("/fido2/grant"), RouteAccess::Ceremony);
    }

    /// The gate fails CLOSED. The hole existed because nobody classified
    /// `/action` when it was added; a route added tomorrow is GUI-only until
    /// someone deliberately writes it into the open list.
    #[test]
    fn an_unclassified_route_is_gui_only_by_default() {
        assert_eq!(
            route_access("/some-route-added-later"),
            RouteAccess::GuiOnly
        );
        assert_eq!(route_access("/action"), RouteAccess::GuiOnly);
        assert_eq!(route_access("/pane/vault"), RouteAccess::GuiOnly);
        let state = control_state();
        assert_eq!(
            dispatch_live(
                &state,
                &page_request("POST", "/some-route-added-later", json!({}))
            )
            .0,
            403,
            "an unknown route must be refused, not 404'd, for an untokened caller"
        );
    }

    /// A refusal is page-driven, so the audit line it writes is page-driven too.
    /// Unrationed, a page looping `POST /action` appends to `journal.jsonl`
    /// forever: a disk-fill in the user's home, and a way to drown the sighting
    /// the journal exists to preserve.
    ///
    /// The contract: the FIRST of any distinct refusal is written immediately
    /// (coalescing may never cost us the sighting), repeats inside the window are
    /// counted instead of written, and the count rides the next line that IS
    /// written so a flood reads as a number.
    #[test]
    fn a_flood_of_refusals_is_counted_rather_than_written_but_the_first_is_never_lost() {
        let mut ledger = HashMap::new();
        let t0 = Instant::now();
        let window = Duration::from_secs(60);

        assert_eq!(
            note_refusal(&mut ledger, "POST|/action|false", t0, window),
            Some(0),
            "the first sighting of a refusal must ALWAYS be written, at once"
        );
        for i in 1..500 {
            assert_eq!(
                note_refusal(
                    &mut ledger,
                    "POST|/action|false",
                    t0 + Duration::from_millis(i),
                    window
                ),
                None,
                "a repeat inside the window must be counted, not written"
            );
        }
        assert_eq!(
            note_refusal(
                &mut ledger,
                "POST|/action|false",
                t0 + Duration::from_secs(61),
                window
            ),
            Some(499),
            "the line that reopens the window must account for everything the \
             window swallowed"
        );
        assert_eq!(
            note_refusal(
                &mut ledger,
                "POST|/action|false",
                t0 + Duration::from_secs(62),
                window
            ),
            None,
            "and the new window coalesces again from there"
        );

        // A DIFFERENT refusal is a different sighting and is written at once —
        // rationing is per-refusal, not a global throttle that a first attack
        // could hide a second one behind.
        assert_eq!(
            note_refusal(&mut ledger, "GET|/pane/vault|false", t0, window),
            Some(0)
        );
    }

    /// The ledger's key carries the attacker-chosen path, so the ledger itself
    /// must be bounded — otherwise varying the path mints a fresh unrationed slot
    /// per request and the flood is back, as both unbounded memory and one
    /// written line per made-up route.
    #[test]
    fn varying_the_path_cannot_mint_unlimited_unrationed_journal_slots() {
        let mut ledger = HashMap::new();
        let t0 = Instant::now();
        let window = Duration::from_secs(60);

        for i in 0..REFUSAL_LEDGER_MAX {
            assert_eq!(
                note_refusal(&mut ledger, &format!("POST|/made-up-{i}|false"), t0, window),
                Some(0),
                "a genuinely distinct route is a distinct sighting while there is room"
            );
        }
        assert_eq!(ledger.len(), REFUSAL_LEDGER_MAX);

        // Past capacity the overflow run absorbs everything: ONE new key, one
        // written line, and then counting.
        assert_eq!(
            note_refusal(&mut ledger, "POST|/made-up-overflow-1|false", t0, window),
            Some(0),
            "the overflow run announces itself once"
        );
        for i in 2..1000 {
            assert_eq!(
                note_refusal(
                    &mut ledger,
                    &format!("POST|/made-up-overflow-{i}|false"),
                    t0,
                    window
                ),
                None,
                "every further made-up route is counted under the overflow run"
            );
        }
        assert_eq!(
            ledger.len(),
            REFUSAL_LEDGER_MAX + 1,
            "the ledger must not grow past its cap plus the single overflow run — \
             it is fed by an attacker-chosen string"
        );
        assert!(
            ledger.contains_key(OVERFLOW_REFUSAL_KEY),
            "the overflow must be accounted under a named key, so the journal says \
             'many distinct routes refused' rather than going quiet"
        );
    }

    /// The open list opens a READ, not a path. `/policy` and `/zoom` are Open
    /// because a GET of them is a read with no secret in it — so a POST to the
    /// same path is gated, and stays gated the day someone adds a
    /// `("POST", "/zoom")` arm to persist a zoom level.
    ///
    /// This is the other axis of the fail-closed promise. `route_access` is
    /// path-keyed (a preflight has to classify identically), so before this lock
    /// the enum's guarantee covered a new PATH only, and a new METHOD on an
    /// already-open path would have been page-callable with every test green.
    #[test]
    fn a_write_to_an_open_path_is_gated_even_though_the_path_is_open() {
        let state = control_state();
        for path in ["/policy", "/zoom", "/ping"] {
            assert_eq!(
                route_access(path),
                RouteAccess::Open,
                "{path} is still an open path — this lock is about the METHOD"
            );
            assert!(
                !requires_gui_token("GET", path),
                "the GET these paths exist for must stay open: {path}"
            );
            for method in ["POST", "PUT", "DELETE", "PATCH"] {
                assert!(
                    requires_gui_token(method, path),
                    "{method} {path} is a write on an open path and must need the \
                     GUI's token"
                );
                assert_eq!(
                    dispatch_live(&state, &page_request(method, path, json!({}))).0,
                    403,
                    "{method} {path} must be refused for an untokened caller, not \
                     fall through to a 404 that a new dispatch arm would turn into \
                     a page-callable mutation"
                );
            }
        }

        // The GUI itself is never blocked by this: it presents the token.
        assert_eq!(
            dispatch_live(&state, &gui_request(&state, "POST", "/zoom", json!({}))).0,
            404,
            "a tokened write reaches the dispatch table (404 = no such arm today), \
             so the gate is refusing the CALLER, not the method"
        );

        // And /fido2 keeps authenticating itself — the method axis must not drag
        // the signer's POST routes into the control gate.
        assert!(!requires_gui_token("POST", "/fido2/get"));
        assert!(!requires_gui_token("POST", "/fido2/grant"));
    }

    /// A refused request must leave an honest line, and that line must not leak
    /// the credential it is protecting.
    #[test]
    fn a_refusal_is_journalled_and_carries_no_token() {
        let presented = "s3cr3t-guess-the-attacker-sent";
        let refusal = gui_only_refusal("POST", "/action", Some(presented), TokenCourier::Live);
        assert_eq!(refusal.event, "control_refused");
        assert_eq!(refusal.data["path"], "/action");
        assert_eq!(refusal.data["method"], "POST");
        assert_eq!(refusal.data["token_presented"], true);
        assert!(
            refusal.data["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("did not match")
        );
        let wire = refusal.data.to_string();
        let state = control_state();
        assert!(
            !wire.contains(presented)
                && !wire.contains(&state.control_token)
                && !wire.contains(&state.signer.token),
            "an audit line must never carry a token — not ours, and not the one \
             the caller guessed: {wire}"
        );

        // ANCHOR: the refusal arm must actually WRITE the line. The value above
        // is only honest if dispatch journals it.
        let source = include_str!("sidebar.rs");
        let gate = source
            .split("pub(crate) fn dispatch(")
            .nth(1)
            .and_then(|rest| rest.split("let query = req.query.as_str();").next())
            .expect("the gate sits at the top of dispatch");
        assert!(
            gate.contains("journal_refusal(&refusal)"),
            "a refused control request must be journalled, not silently dropped — \
             and through `journal_refusal`, which rations a page-driven flood \
             rather than appending a line per attempt"
        );
        assert!(
            !gate.contains("crate::daemon::journal("),
            "the gate writes straight to the journal again, bypassing the \
             rationing: a page looping this route can fill the disk"
        );
        // ANCHOR: and the gate must ask the METHOD-aware predicate. Asking
        // `route_access` alone here is the hole `requires_gui_token` exists to
        // close, and it would leave every other lock in this file green.
        assert!(
            gate.contains("requires_gui_token(&req.method, &req.path)"),
            "the gate must classify on method AND path — a write to an open path \
             is not the read that path was opened for"
        );
        assert!(
            !gate.contains("route_access(&req.path) == RouteAccess::GuiOnly"),
            "the gate is back to a path-only classification, so a `POST /zoom` arm \
             added later would be page-callable"
        );
    }

    /// THE LIVE BUG, 2026-07-31: the user's vault and settings panes rendered
    /// one line, `control endpoint returned 403`, and there was nothing in it to
    /// act on. Three different failures wore that one message, and the one that
    /// was actually happening — a `ychrome` CLI older than the gate, which can
    /// never deliver the token however new the daemon and the GUI are — was the
    /// one the message did not describe. A refusal that cannot be acted on is
    /// only half a refusal.
    #[test]
    fn a_refusal_names_which_of_the_three_failures_it_is() {
        let pre_gate = gui_only_refusal(
            "GET",
            "/pane/vault",
            None,
            TokenCourier::Absent { client_pid: 4242 },
        );
        let error = pre_gate.body["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("predates the control-token gate"),
            "the cause must be named: {error}"
        );
        assert!(
            error.contains("Ctrl+C") && error.contains("run ychrome again"),
            "the REMEDY must be named, and it is the only one that works: {error}"
        );
        assert!(
            error.contains("restarting the daemon does not fix it"),
            "the remedy people reach for first must be ruled out, or they will \
             restart the daemon six times: {error}"
        );
        assert_eq!(pre_gate.body["cause"], "client_predates_control_token");
        assert_eq!(pre_gate.data["client_pid"], 4242);

        // The body is PAGE-REACHABLE — that is the whole reason this gate
        // exists — so the pid rides the journal line and nothing else.
        assert!(
            !pre_gate.body.to_string().contains("4242"),
            "a page must not learn host facts from a refusal: {}",
            pre_gate.body
        );

        // A live courier and a wrong token is a transient, not a dead session:
        // the client re-declares the current token within ~4s. Telling that
        // reader to restart their browser would be wrong advice.
        let mismatch = gui_only_refusal("POST", "/action", Some("old"), TokenCourier::Live);
        let error = mismatch.body["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("earlier daemon generation") && error.contains("~4s"),
            "a stale token is self-healing and must say so: {error}"
        );
        assert_eq!(mismatch.body["cause"], "token_mismatch");
        assert!(mismatch.data["client_pid"].is_null());

        // And a page reaching a GUI-only route gets the page-facing answer.
        let from_page = gui_only_refusal("POST", "/action", None, TokenCourier::Live);
        assert_eq!(from_page.body["cause"], "token_absent");
        assert!(
            from_page.body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("yggterm-appctl bridge")
        );
    }

    /// A CORS preflight is a PAGE asking to drive a GUI-only route
    /// cross-origin. It must be refused whatever the session's client vintage
    /// is, and — the part worth a lock — it must not answer with a fact about
    /// the host's own CLI, which would hand the caller this gate exists to
    /// refuse a piece of reconnaissance.
    #[test]
    fn a_preflight_never_reports_the_hosts_client_vintage() {
        let refusal = gui_only_refusal("OPTIONS", "/action", None, TokenCourier::NotAsked);
        let error = refusal.body["error"].as_str().unwrap_or_default();
        assert!(
            !error.contains("predates") && !error.contains("Ctrl+C"),
            "a preflight answer must describe the ROUTE, not the host: {error}"
        );
        assert_eq!(refusal.data["token_courier"], "not_asked");

        // ANCHOR: and the preflight responder must keep asking with `NotAsked`.
        // Passing the session's real courier here is a one-word edit that no
        // other lock in this file would notice.
        let source = include_str!("sidebar.rs");
        let body = source
            .split("pub(crate) fn respond_preflight(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("respond_preflight body present");
        assert!(
            body.contains("TokenCourier::NotAsked"),
            "the preflight must not consult the session's courier"
        );
    }

    /// CORS stops advertising `*` on anything but the signer's page routes. The
    /// wildcard never let a call through by itself — the gate does that — but a
    /// GUI-only route telling every origin "read my replies" is an invitation
    /// that no longer stands.
    #[test]
    fn only_the_signers_page_routes_reflect_a_cors_wildcard() {
        assert!(cors_headers(RouteAccess::PageSigner).contains("Access-Control-Allow-Origin: *"));
        for access in [
            RouteAccess::Open,
            RouteAccess::Ceremony,
            RouteAccess::GuiOnly,
        ] {
            assert!(
                !cors_headers(access).contains("Access-Control-Allow-Origin"),
                "{access:?} must not reflect an origin"
            );
        }
        assert!(
            !cors_headers(RouteAccess::PageSigner).contains("X-Ychrome-Control"),
            "no page is ever meant to send the control header, so it is not allow-listed"
        );
    }

    /// The GUI's credential travels on the declare — the one channel a page
    /// cannot read. Without this the GUI has no way to prove it is the GUI.
    #[test]
    fn the_declare_carries_the_control_token() {
        let payload = declare_payload("sess-1", "http://127.0.0.1:41234", "tok-abc", "p1", "z1");
        assert_eq!(payload["control_token"], "tok-abc");
        assert_eq!(payload["control"], "http://127.0.0.1:41234");
    }

    /// Each session's endpoint gets its OWN token: one surface's page must not
    /// be able to drive another surface's pane even if it somehow learned a
    /// token, and the signer's token is a different secret again.
    #[test]
    fn every_session_mints_its_own_control_token() {
        let a = control_state();
        let b = control_state();
        assert_ne!(a.control_token, b.control_token);
        assert_ne!(a.control_token, a.signer.token);
        assert_eq!(a.control_token.len(), 64, "32 CSPRNG bytes, hex");
    }

    // The agent engine mounts /engine/* on the daemon's UNIX socket
    // (docs/agent-engine.md §3 amendment), so the HTTP plumbing had to stop
    // being TCP-only. This is the lock: the SAME parser and the SAME responder,
    // driven end to end over a UnixStream pair, with the header gates intact.
    // If someone re-types either signature back to TcpStream, this fails to
    // compile rather than failing quietly at Phase B.
    #[test]
    fn the_control_http_plumbing_speaks_unix_sockets_too() {
        use std::os::unix::net::UnixStream;

        let (client, server) = UnixStream::pair().expect("socketpair");
        let body = br#"{"url":"https://example.com/"}"#;
        let request = format!(
            "POST /engine/open?profile=research HTTP/1.1\r\nContent-Type: application/json\r\n\
             X-Ychrome-Control: deadbeef\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        {
            let mut client = &client;
            client.write_all(request.as_bytes()).unwrap();
            client.write_all(body).unwrap();
            client.flush().unwrap();
        }

        let parsed = read_request(&server).expect("a unix-socket request parses");
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/engine/open");
        assert_eq!(parsed.query, "profile=research");
        assert_eq!(parsed.body["url"], "https://example.com/");
        // The header gates are transport-independent: an engine route reached
        // over the socket must still be able to see the control token.
        assert_eq!(parsed.control_token.as_deref(), Some("deadbeef"));
        assert!(parsed.fido2_token.is_none());

        respond_json(&server, 200, &json!({ "page_id": "pg_1" }), &parsed.path);
        drop(server);

        let mut raw = String::new();
        BufReader::new(&client).read_to_string(&mut raw).unwrap();
        assert!(raw.starts_with("HTTP/1.1 200 OK\r\n"), "got {raw:?}");
        assert!(raw.contains("Content-Type: application/json"));
        assert!(raw.ends_with(r#"{"page_id":"pg_1"}"#), "got {raw:?}");
        // An engine route is not a signer page route, so it gets no CORS
        // wildcard just because it now shares the parser with one.
        assert!(
            !raw.contains("Access-Control-Allow-Origin"),
            "an /engine/* reply must not advertise CORS: {raw:?}"
        );
    }

    // ── THE PASSKEY SHIM'S SCOPE ────────────────────────────────────────────
    // Installed on every page, the shim DEFINES window.PublicKeyCredential and
    // answers isUserVerifyingPlatformAuthenticatorAvailable() true — on an
    // engine that has no WebAuthn at all, where both read undefined. A bot
    // check reads that mismatch, and an interstitial managed challenge is
    // served as the TOP-FRAME document at the site's own URL, so all_frames:
    // false never protected it.

    // WebAuthn scopes a credential to the rpId AND its subdomains, so both must
    // match. They are two patterns because WebKit's `*://*.example.com/*` does
    // NOT admit the bare `example.com`.
    #[test]
    fn an_rp_id_matches_itself_and_its_subdomains() {
        let patterns = rp_id_match_patterns("example.com");
        assert!(
            patterns.contains(&"*://example.com/*".to_string()),
            "the bare rpId must match: {patterns:?}"
        );
        assert!(
            patterns.contains(&"*://*.example.com/*".to_string()),
            "a passkey for example.com is usable on login.example.com: {patterns:?}"
        );
    }

    // ⛔ A PATTERN IS A SCOPE. Anything that could widen one to every site must
    // produce NO pattern rather than a permissive one — `*://*./*` is every page
    // on the web, which is the exact state this whole change exists to end.
    #[test]
    fn a_malformed_rp_id_yields_no_pattern_rather_than_a_wide_one() {
        for bad in [
            "",
            "   ",
            "*",
            "evil.com/*",
            "https://evil.com",
            "a b",
            "x?y",
            "h#f",
            "host:443",
        ] {
            assert!(
                rp_id_match_patterns(bad).is_empty(),
                "{bad:?} must produce no pattern at all, never a widened one"
            );
        }
        // …and a well-formed one still works after all that refusing.
        assert_eq!(rp_id_match_patterns("github.com").len(), 2);
    }

    // The shim must never be installed unscoped. An empty `matches` means EVERY
    // URL (see `Userscript::matches`), so a shim with no patterns is precisely
    // the old bug wearing the new code's clothes.
    #[test]
    fn the_shim_is_never_installed_with_an_empty_match_list() {
        let source = include_str!("sidebar.rs");
        let body = source
            .split("fn passkey_shim_scripts(state: &ControlState) -> Vec<crate::userscript::Userscript> {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("passkey_shim_scripts body present");
        assert!(
            body.contains("if patterns.is_empty()") && body.contains("return Vec::new()"),
            "no rpIds must mean NO shim: an empty matches list means every URL, \
             which is the unscoped shim this change removes"
        );
        assert!(
            body.contains("script.matches = patterns"),
            "the shim must carry its match patterns"
        );
    }

    // ⛔⛔ THE USER-PRESENCE INVARIANT IS NOT A SCOPING QUESTION. Scoping decides
    // WHERE navigator.credentials is patched. Every ceremony still goes through
    // the token-gated /fido2/* routes and still blocks on an explicit GUI grant.
    // An agent must never be able to approve its own ceremony.
    #[test]
    fn scoping_the_shim_does_not_touch_the_presence_gate() {
        let source = include_str!("sidebar.rs");
        assert!(
            source.contains(
                r#"if page_route && !state.signer.authorized(req.fido2_token.as_deref())"#
            ),
            "the page-facing /fido2 routes must still be bearer-token gated"
        );
        for route in ["/fido2/get", "/fido2/create", "/fido2/grant", "/fido2/deny"] {
            assert!(source.contains(route), "{route} must still exist");
        }
    }

    // ⛔ THE FAILURE THAT REACHED THE USER ON 2026-08-01. A vault agent older
    // than this browser answers `status` perfectly — unlocked, 1116 items,
    // `agent_stale: false`, because the agent and the INSTALLED ychrome-vault
    // were the same binary — while answering `unknown op "passkey-hosts"` on the
    // same socket. Passkeys were off on every site, the pane said nothing, and
    // the page said "your browser does not support WebAuthn".
    #[test]
    fn an_agent_older_than_this_browser_is_reported_in_the_pane() {
        let widgets = passkey_shim_widgets(&PasskeyShimState::AgentPredatesBrowser);
        assert!(
            !widgets.is_empty(),
            "this state must never be silent in the pane"
        );
        let text = serde_json::to_string(&widgets).unwrap();
        assert!(
            text.contains("WebAuthn"),
            "the pane must connect itself to the words the PAGE showed the user: {text}"
        );
        assert!(
            widgets
                .iter()
                .any(|widget| widget["action"] == "hand_over_agent"),
            "the remedy that keeps the unlock must be one click: {text}"
        );
    }

    // A working shim is SILENT. A banner rendered on every schema for a healthy
    // subsystem is a banner nobody reads when it finally matters.
    #[test]
    fn a_healthy_shim_says_nothing() {
        for state in [
            PasskeyShimState::ScopedTo(vec!["example.com".into()]),
            PasskeyShimState::NoStoredPasskeys,
        ] {
            assert!(
                passkey_shim_widgets(&state).is_empty(),
                "{state:?} must be silent"
            );
        }
    }

    // ⛔ THE PANE MUST NOT LEAK WHICH SITES YOU HOLD PASSKEYS FOR. The schema
    // crosses the OSC channel to the GUI; an rpId list is the user's business.
    #[test]
    fn the_pane_never_names_the_hosts_a_passkey_exists_for() {
        let state = PasskeyShimState::ScopedTo(vec!["bank.example".into()]);
        let text = serde_json::to_string(&passkey_shim_widgets(&state)).unwrap();
        assert!(
            !text.contains("bank.example"),
            "an rpId reached the schema: {text}"
        );
    }

    // A locked or unreachable vault is a DIFFERENT story from an agent that is
    // too old, and the two need opposite remedies: unlock versus hand over.
    #[test]
    fn an_unavailable_vault_tells_the_user_to_unlock_and_reopen() {
        let widgets = passkey_shim_widgets(&PasskeyShimState::VaultUnavailable(
            "the vault is locked".into(),
        ));
        let text = serde_json::to_string(&widgets).unwrap();
        assert!(text.contains("Unlock"), "{text}");
        // The shim is chosen when a surface OPENS, so unlocking alone is not
        // enough and saying only "unlock" would be a stale answer.
        assert!(text.contains("new web surface"), "{text}");
        assert!(
            !widgets
                .iter()
                .any(|widget| widget["action"] == "hand_over_agent"),
            "handing the agent over does not unlock a vault: {text}"
        );
    }

    // ⛔ ONE OWNER FOR THE DECISION. `/policy` decides where to install the shim
    // and the pane explains why it did not; if they probed separately, a browser
    // could disable passkeys while the pane reported everything fine — which is
    // exactly what happened before this existed.
    #[test]
    fn the_shim_decision_and_the_pane_read_the_same_owner() {
        let production = include_str!("sidebar.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("source before the test module");
        assert_eq!(
            production.matches("fn passkey_shim_state()").count(),
            1,
            "there must be exactly one owner of the shim decision"
        );
        for caller in ["fn passkey_shim_scripts(", "fn unlocked_schema("] {
            let body = production
                .split(caller)
                .nth(1)
                .and_then(|suffix| suffix.split("\n}").next())
                .unwrap_or_else(|| panic!("{caller} is present"));
            assert!(
                body.contains("passkey_shim_state()"),
                "{caller} must read the shared owner rather than probing on its own"
            );
        }
    }

    // ⛔ THE POLICY STAMP WAS BLIND TO THE SHIM, WHICH MADE THE FAILURE
    // PERMANENT. Measured on guihost: `sidebar_contribution/policy` recorded
    // `userscripts: 6` then `userscripts: 5` under ONE unchanged
    // `policy_version` (`ebc219f7d40ddc53`), and the GUI refetches only when
    // that stamp moves — so no surface could ever recover the shim.
    #[test]
    fn the_policy_stamp_moves_when_the_vault_agent_is_replaced() {
        let production = include_str!("webpolicy.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("source before the test module");
        let body = production
            .split("pub fn policy_version(profile: &str) -> String {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("policy_version is present");
        assert!(
            body.contains("passkey_shim_stamp()"),
            "the stamp must cover the vault facts that decide the shim's scope, \
             or a handed-over agent never reaches an open surface"
        );
        let stamp = production
            .split("fn passkey_shim_stamp() -> String {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("passkey_shim_stamp is present");
        // STAT-ONLY: this runs on the ~4s re-declare, where a socket round trip
        // was already measured to wreck the surface tests.
        assert!(
            !stamp.contains("request") && !stamp.contains("passkey-hosts"),
            "the stamp runs on the heartbeat and must never do socket IO"
        );
        assert!(
            stamp.contains("pid_path") && stamp.contains("installed_vault_exe_stamp"),
            "a handover rewrites agent.pid and an install moves the binary's mtime; \
             those two stats are what make a recovered agent reach a surface"
        );
    }

    // The probe is a unix-socket round trip. On the ~4s heartbeat path that
    // would be a per-beat socket call, which the bug entry rules out explicitly.
    #[test]
    fn the_rp_id_probe_stays_off_the_heartbeat_path() {
        // PRODUCTION CODE ONLY, and the needle is assembled at runtime, or this
        // test's own prose would satisfy the search it is performing.
        let production = include_str!("sidebar.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("source before the test module");
        let probe = format!("\"op\": \"passkey-{}\"", "hosts");
        assert_eq!(
            production.matches(probe.as_str()).count(),
            1,
            "the rpId probe is a unix-socket round trip and must have exactly \
             ONE call site (`passkey_shim_scripts`); a second one is how it \
             reaches a hot path"
        );
        // The ~4s heartbeat is `emit_declare`/`declare_payload`; the `/policy`
        // route is refetched only when the stamp MOVES. Neither declare function
        // may reach the probe, directly or through the shim builder.
        for name in ["pub fn emit_declare(", "fn declare_payload("] {
            let body = production
                .split(name)
                .nth(1)
                .and_then(|suffix| suffix.split("\n}").next())
                .unwrap_or_else(|| panic!("{name} is present"));
            assert!(
                !body.contains(probe.as_str()) && !body.contains("passkey_shim_scripts"),
                "{name} runs on the ~4s heartbeat and must stay free of socket \
                 IO — the rpId probe belongs on the /policy route, which is \
                 fetched only when the policy stamp moves"
            );
        }
    }
}
