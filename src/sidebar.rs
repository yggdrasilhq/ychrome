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

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

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

/// Run the `ychrome-vault` CLI on THIS host and return its stdout.
///
/// The browser deliberately does not link `ychrome-vault`: the workspace keeps
/// the browser build lean (no crypto, no http client), and the CLI is already
/// the one documented interface to the vault — the same one yggterm used before
/// this pane existed. It talks to the host's unlock-caching agent, so a read is
/// cheap and keyless once the user has unlocked.
fn vault_cli(args: &[&str]) -> Result<String> {
    vault_cli_stdin(args, None)
}

/// As [`vault_cli`], but writes `stdin` to the child first. A password reaches
/// `ychrome-vault add` this way and no other: never a flag (it would show up in
/// `ps`), never an environment variable.
fn vault_cli_stdin(args: &[&str], stdin: Option<&str>) -> Result<String> {
    let mut command = Command::new("ychrome-vault");
    command
        .args(args)
        .stdin(match stdin {
            Some(_) => Stdio::piped(),
            // `read_secret` refuses a terminal on stdin, and ychrome's stdin IS
            // the session's PTY — so a no-secret call must not inherit it.
            None => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("run ychrome-vault (is it installed on this host?)")?;
    if let Some(secret) = stdin {
        child
            .stdin
            .take()
            .context("ychrome-vault stdin")?
            .write_all(secret.as_bytes())
            .context("writing the password to ychrome-vault")?;
    }
    let output = child.wait_with_output().context("ychrome-vault failed")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        // The one failure the user can act on gets the wording it deserves.
        if stderr.contains("locked") {
            bail!("vault locked: run `ychrome-vault unlock` on this host");
        }
        bail!(
            "{}",
            if stderr.is_empty() {
                "ychrome-vault failed"
            } else {
                stderr
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn vault_cli_json(args: &[&str]) -> Result<Value> {
    let stdout = vault_cli(args)?;
    serde_json::from_str(&stdout).context("ychrome-vault did not return json")
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
struct PaneState {
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

pub struct Sidebar {
    /// Loopback control endpoint, e.g. `http://127.0.0.1:41234`. Reachable by
    /// the GUI only through an `ssh -L` forward when ychrome runs remotely —
    /// which yggterm sets up, not us.
    pub control_url: String,
    stop: Arc<AtomicBool>,
}

impl Sidebar {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The control server's shared state: the pane draft (behind a lock, mutated by
/// actions) and the passkey signer (its own internal locks). One `Arc` so a
/// per-connection thread can hold both.
struct ServerState {
    pane: Mutex<PaneState>,
    signer: Arc<crate::passkey::Signer>,
}

/// Bind the control endpoint and serve it on a background thread.
///
/// `session` is the emitting `YGGTERM_SESSION_ID`, carried in the passkey OSC
/// for diagnostics (the GUI routes by stream). A connection is served on its own
/// thread: a `/fido2/get` blocks for up to two minutes awaiting the presence
/// dialog, and must not wedge the concurrent `/fido2/grant` the GUI sends back.
pub fn spawn(profile: &str, session: &str) -> Result<Sidebar> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding sidebar control server")?;
    let port = listener.local_addr()?.port();
    let control_url = format!("http://127.0.0.1:{port}");
    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(ServerState {
        pane: Mutex::new(PaneState::new(profile)),
        signer: crate::passkey::Signer::new(port, session.to_string()),
    });

    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            for incoming in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                match incoming {
                    Ok(stream) => {
                        let state = Arc::clone(&state);
                        std::thread::spawn(move || handle_conn(stream, &state));
                    }
                    Err(_) => continue,
                }
            }
        });
    }
    Ok(Sidebar { control_url, stop })
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
pub fn emit_declare(session: &str, control: &str, policy_version: &str, zoom_version: &str) {
    let payload = json!({
        "session": session,
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
    });
    emit_osc("declare", &payload.to_string());
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

fn handle_conn(stream: TcpStream, state: &ServerState) {
    let Ok(peek) = stream.try_clone() else { return };
    let mut reader = BufReader::new(peek);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let request_target = parts.next().unwrap_or("/");
    let (path, query) = request_target
        .split_once('?')
        .unwrap_or((request_target, ""));

    // Drain headers; capture Content-Length so a POST body can be read, and the
    // passkey bearer token so a `/fido2/*` route can gate on it.
    let mut content_length = 0usize;
    let mut fido2_token: Option<String> = None;
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
            }
        }
    }

    // Read a POST body up front (routes that don't need it ignore it).
    let read_body = |reader: &mut BufReader<TcpStream>| -> Option<Value> {
        let mut body = vec![0u8; content_length];
        if content_length > 0 && reader.read_exact(&mut body).is_err() {
            return None;
        }
        Some(serde_json::from_slice(&body).unwrap_or(Value::Null))
    };

    // A cross-origin fetch from an RP's page (the passkey shim) preflights with
    // OPTIONS before the real POST — answer it, whatever the path.
    if method == "OPTIONS" {
        respond_preflight(stream);
        return;
    }

    match (method, path) {
        ("GET", p) if p == format!("/pane/{VAULT_PANE}") => {
            let host = query_value(query, "host");
            let schema = {
                let mut pane = state.pane.lock().unwrap();
                // Opening the pane straight onto the Add tab must seed the draft
                // too, not only arriving there via the tab action.
                if pane.tab == "add" {
                    pane.seed_add_draft(host.as_deref());
                }
                vault_schema(&pane, host.as_deref())
            };
            respond_json(stream, 200, &schema);
        }
        ("GET", p) if p == format!("/pane/{SETTINGS_PANE}") => {
            let profile = state.pane.lock().unwrap().profile.clone();
            let page = PageContext::from_query(query);
            respond_json(stream, 200, &settings_schema(&profile, &page));
        }
        // The per-site zoom overrides for this host. yggterm applies the entry
        // for the current page's host on navigation and falls back to its global
        // "Ychrome Global Zoom" — the GUI does the matching, ychrome owns the map.
        ("GET", "/zoom") => {
            respond_json(stream, 200, &crate::webzoom::to_json());
        }
        // The EFFECTIVE web-content policy for the profile this ychrome is
        // running: every enable/disable decision already made, PLUS the passkey
        // shim prepended (document-start, so `navigator.credentials` is patched
        // before the page can call it). yggterm applies it to the webview.
        ("GET", "/policy") => {
            let profile = state.pane.lock().unwrap().profile.clone();
            let mut policy = crate::webpolicy::policy(&profile).to_json();
            if let Some(scripts) = policy["userscripts"].as_array_mut() {
                scripts.insert(0, json!(state.signer.shim_userscript()));
            }
            respond_json(stream, 200, &policy);
        }
        ("POST", "/action") => {
            let Some(request) = read_body(&mut reader) else {
                respond_json(stream, 400, &json!({ "toast": "bad request" }));
                return;
            };
            let reply = run_action(&state.pane, &request);
            respond_json(stream, 200, &reply);
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
            if page_route && !state.signer.authorized(fido2_token.as_deref()) {
                respond_json(stream, 401, &json!({ "error": "unauthorized" }));
                return;
            }
            let Some(body) = read_body(&mut reader) else {
                respond_json(stream, 400, &json!({ "error": "bad request" }));
                return;
            };
            let (status, reply) = match p {
                "/fido2/get" => state.signer.handle_get(&body),
                "/fido2/create" => state.signer.handle_create(&body),
                "/fido2/grant" => state.signer.handle_grant(&body),
                "/fido2/deny" => state.signer.handle_deny(&body),
                _ => (404, json!({ "error": "unknown fido2 route" })),
            };
            respond_json(stream, status, &reply);
        }
        _ => respond_json(stream, 404, &json!({})),
    }
}

fn query_value(query: &str, key: &str) -> Option<String> {
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

fn respond_json(mut stream: TcpStream, status: u16, body: &Value) {
    let body = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nCache-Control: no-store\r\nConnection: close\r\n\
         {cors}\r\n{body}",
        len = body.len(),
        cors = cors_headers(),
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// CORS headers on EVERY control response.
///
/// The signer's page routes (`/fido2/get`, `/fido2/create`) are fetched by a
/// userscript running in the RP's page — a cross-origin request (webauthn.io →
/// `127.0.0.1:<port>`) that WebKit refuses without these. `*` is safe: CORS is
/// not our security boundary — the bearer token (page routes) and the unguessable
/// request_id (grant routes) are — and the shim never sends credentials, so a
/// wildcard origin is allowed. The custom `X-Ychrome-Fido2` header forces a
/// preflight `OPTIONS`, which [`handle_conn`] answers.
fn cors_headers() -> &'static str {
    "Access-Control-Allow-Origin: *\r\n\
     Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
     Access-Control-Allow-Headers: Content-Type, X-Ychrome-Fido2\r\n\
     Access-Control-Max-Age: 600\r\n"
}

/// Answer a CORS preflight: 204, the CORS headers, no body. Without this the
/// browser never sends the real `/fido2/*` POST.
fn respond_preflight(mut stream: TcpStream) {
    let response = format!(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n{cors}\r\n",
        cors = cors_headers(),
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
    let mut actions = vec![json!({
        "action": "fill",
        "label": "⧉",
        "title": "Fill this login into the page",
    })];
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
    match vault_cli_json(&["status"]) {
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
                match vault_cli_json(&["suggest", host]) {
                    Ok(items) => {
                        let items = items.as_array().cloned().unwrap_or_default();
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
            let list_args: Vec<&str> = if query.is_empty() {
                vec!["list", "--json"]
            } else {
                vec!["list", "--json", query]
            };
            match vault_cli_json(&list_args) {
                Ok(items) => {
                    let items = items.as_array().cloned().unwrap_or_default();
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
        "watchtower" => match vault_cli_json(&["watchtower"]) {
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
        "sync" => match vault_cli_json(&["sync"]) {
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
            match vault_cli_stdin(&["unlock"], Some(password)) {
                Ok(reply) => {
                    let count = serde_json::from_str::<Value>(&reply)
                        .ok()
                        .and_then(|value| value["item_count"].as_u64())
                        .unwrap_or(0);
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
        "restart_agent" => match vault_cli_json(&["stop-agent"]) {
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
        "lock" => match vault_cli_json(&["lock"]) {
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
                    state.generate_length.to_string(),
                    state.generate_no_symbols,
                )
            };
            if name.is_empty() {
                return json!({ "toast": "An item needs a name." });
            }
            // The typed password is used for this call and dropped. An empty one
            // means `--generate`: rolled on this host, stored encrypted, and
            // never echoed back — a schema is not a place for a secret.
            let password = values["add_password"].as_str().unwrap_or_default();
            let mut args = vec!["add", name.as_str()];
            if !user.is_empty() {
                args.push(user.as_str());
            }
            if !uri.is_empty() {
                args.extend(["--uri", uri.as_str()]);
            }
            if !folder.is_empty() {
                args.extend(["--folder", folder.as_str()]);
            }
            if !notes.is_empty() {
                args.extend(["--notes", notes.as_str()]);
            }
            if password.is_empty() {
                args.extend(["--generate", "--length", length.as_str()]);
                if no_symbols {
                    args.push("--no-symbols");
                }
            }
            let stdin = (!password.is_empty()).then_some(password);
            match vault_cli_stdin(&args, stdin) {
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
            let mut args = vec!["get", name.as_str()];
            if !user.is_empty() {
                args.push(user.as_str());
            }
            match vault_cli(&args) {
                // The password is on stdout of a process on THIS host, goes
                // straight into the eval script, and is dropped. It never enters
                // a schema, the OSC stream, or yggterm's state.
                Ok(password) => json!({
                    "eval": fill_script(&user, &password),
                    "toast": format!("Filled {name}."),
                }),
                Err(error) => json!({ "toast": error.to_string() }),
            }
        }
        "totp" => {
            let (name, user) = split_row_id(&value);
            let mut args = vec!["totp", name.as_str()];
            if !user.is_empty() {
                args.push(user.as_str());
            }
            match vault_cli(&args) {
                Ok(code) => json!({
                    "eval": totp_script(&code),
                    "toast": format!("Filled {name}'s authenticator code."),
                }),
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
/// Pick the browser identity. Carries the preset id after the prefix.
const USER_AGENT_ACTION_PREFIX: &str = "user-agent:";

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

/// The browser identity. The default UA a WebKitGTK build sends describes Safari
/// on Linux — a browser that does not exist — and UA-allowlisting edges refuse
/// it outright (claude.ai answers "Request not allowed"). Presented as a row per
/// preset rather than a free-text field: the failure mode of a hand-typed UA is a
/// site quietly serving you the wrong code, which is worse than not offering it.
fn user_agent_widgets() -> Vec<Value> {
    let current = crate::useragent::preset();
    let mut widgets = vec![json!({"kind": "section", "text": "Browser identity"})];
    for preset in crate::useragent::Preset::ALL {
        let selected = preset == current;
        widgets.push(json!({
            "kind": "list-row",
            "id": format!("ua-{}", preset.id()),
            "title": if selected { format!("● {}", preset.label()) } else { preset.label().to_string() },
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
    )
}

fn settings_schema_from(
    profile: &str,
    page: &PageContext,
    zoom_sites: &std::collections::BTreeMap<String, f64>,
    state: &crate::webpolicy::PolicyState,
) -> Value {
    let (host, live_zoom, secure) = (page.host(), page.zoom, page.secure);
    let mut widgets = vec![json!({"kind": "section", "text": "This site"})];
    widgets.extend(current_site_zoom_widgets(host, live_zoom, zoom_sites));
    widgets.extend(current_site_security_widgets(host, secure));

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
            "text": "No ruleset installed (~/.yggterm/web-adblock/rules.json missing on this host).",
        }));
    }

    // SponsorBlock is a userscript, but a flagship one, so it gets its own named
    // section with a friendly toggle — pulled out of the generic list below.
    widgets.extend(sponsorblock_widgets(state));

    // Everything EXCEPT sponsorblock: one list-row each, with Enable/Disable and
    // a Delete (the "toggle + trash icon" the design calls for).
    widgets.push(json!({"kind": "section", "text": "Userscripts"}));
    let managed: Vec<&(String, bool)> = state
        .userscripts
        .iter()
        .filter(|(stem, _)| stem != crate::extensions::SPONSORBLOCK_STEM)
        .collect();
    if managed.is_empty() {
        widgets.push(json!({
            "kind": "label",
            "muted": true,
            "text": "None installed. Add one below, or drop *.js into \
                     ~/.yggterm/web-userscripts/ on the host ychrome runs on.",
        }));
    }
    for (stem, enabled) in managed {
        widgets.push(userscript_row(stem, *enabled));
    }

    // The catalog, filtered to what is not already installed. "Installed" is read
    // from the SAME `state` snapshot the rest of the pane draws from — one source
    // of truth per render, so the catalog can never disagree with the list above
    // it. Omit the whole section when there is nothing left to add.
    let installed: std::collections::HashSet<&str> = state
        .userscripts
        .iter()
        .map(|(stem, _)| stem.as_str())
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
/// `sponsorblock.js` vs `.js.disabled` rename, exactly like any userscript).
/// Not installed ⇒ nothing here; it appears under "Add an extension" instead.
fn sponsorblock_widgets(state: &crate::webpolicy::PolicyState) -> Vec<Value> {
    let installed = state
        .userscripts
        .iter()
        .find(|(stem, _)| stem == crate::extensions::SPONSORBLOCK_STEM);
    let Some((stem, enabled)) = installed else {
        return Vec::new();
    };
    vec![
        json!({"kind": "section", "text": "SponsorBlock"}),
        json!({
            "kind": "toggle",
            "id": format!("{USERSCRIPT_ACTION_PREFIX}{stem}"),
            "action": format!("{USERSCRIPT_ACTION_PREFIX}{stem}"),
            "label": "Skip YouTube sponsor segments",
            "value": enabled,
        }),
    ]
}

/// One managed userscript as a list-row: its on/off state in the subtitle, an
/// Enable/Disable action, and a Delete. Keyed by stem so Dioxus never patches one
/// script's row into another's (identity, not index — the pane's hard-won rule).
fn userscript_row(stem: &str, enabled: bool) -> Value {
    let toggle_label = if enabled { "Disable" } else { "Enable" };
    json!({
        "kind": "list-row",
        "id": format!("script-{stem}"),
        "title": stem,
        "subtitle": if enabled { "Enabled" } else { "Disabled" },
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
                .map(|(stem, on)| (stem.to_string(), *on))
                .collect(),
        }
    }

    fn no_zoom() -> std::collections::BTreeMap<String, f64> {
        std::collections::BTreeMap::new()
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
        let schema = settings_schema_from("work", &page, &no_zoom(), &policy_state(true, &[]));
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
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        assert!(
            !widgets.iter().any(|w| w["id"] == "adblock-enabled"),
            "offered an adblock toggle with no ruleset installed"
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
}
