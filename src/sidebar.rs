//! ychrome's SIDEBAR CONTRIBUTION: the vault pane, owned by ychrome.
//!
//! yggterm used to hardcode a `RightPanelMode::Vault` — app chrome living in the
//! platform, which is the anti-pattern the libyggterm contract exists to
//! prevent. Instead ychrome *declares* the pane over `OSC 7717 ; sidebar` and
//! serves its content from a loopback control endpoint on the host ychrome runs
//! on. yggterm draws generic widgets and knows nothing about vaults.
//!
//! ```text
//! ychrome  --OSC 7717 sidebar;declare-->  yggterm GUI   (control url + pane buttons)
//! yggterm  --GET  <control>/pane/vault->  ychrome       (schema; no secrets)
//! yggterm  --POST <control>/action----->  ychrome       (schema? toast? eval?)
//! ```
//!
//! **The vault never crosses the OSC.** A 1100-row item list would not fit on a
//! PTY, and a secret must never sit in a declaration. The GUI fetches the schema
//! itself, and a credential reaches the page only as an `eval` script the GUI
//! injects into the surface — the app computes, the GUI injects.
//!
//! State is host-resident: the unlocked vault lives in this host's
//! `ychrome-vault` agent, which over ssh is the REMOTE host, not the GUI's.

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
        bail!("{}", if stderr.is_empty() { "ychrome-vault failed" } else { stderr });
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
}

impl PaneState {
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
            uri: host.map(|host| format!("https://{host}")).unwrap_or_default(),
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

/// Bind the control endpoint and serve it on a background thread.
pub fn spawn(profile: &str) -> Result<Sidebar> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding sidebar control server")?;
    let port = listener.local_addr()?.port();
    let control_url = format!("http://127.0.0.1:{port}");
    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(PaneState::new(profile)));

    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            for incoming in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                match incoming {
                    Ok(stream) => handle_conn(stream, &state),
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
pub fn emit_declare(session: &str, control: &str, policy_version: &str) {
    let payload = json!({
        "session": session,
        "control": control,
        "policy_version": policy_version,
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

fn handle_conn(stream: TcpStream, state: &Arc<Mutex<PaneState>>) {
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

    // Drain headers; capture Content-Length so a POST body can be read.
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some(value) = header
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().to_string())
        {
            content_length = value.parse().unwrap_or(0);
        }
    }

    match (method, path) {
        ("GET", p) if p == format!("/pane/{VAULT_PANE}") => {
            let host = query_value(query, "host");
            let schema = {
                let mut state = state.lock().unwrap();
                // Opening the pane straight onto the Add tab must seed the draft
                // too, not only arriving there via the tab action.
                if state.tab == "add" {
                    state.seed_add_draft(host.as_deref());
                }
                vault_schema(&state, host.as_deref())
            };
            respond_json(stream, 200, &schema);
        }
        ("GET", p) if p == format!("/pane/{SETTINGS_PANE}") => {
            let profile = state.lock().unwrap().profile.clone();
            respond_json(stream, 200, &settings_schema(&profile));
        }
        // The EFFECTIVE web-content policy for the profile this ychrome is
        // running: every enable/disable decision already made. yggterm applies
        // it to the webview and persists nothing but WebKit's compiled cache.
        // No `?host=` — unlike a pane schema, this is not about the open page.
        ("GET", "/policy") => {
            let profile = state.lock().unwrap().profile.clone();
            respond_json(stream, 200, &crate::webpolicy::policy(&profile).to_json());
        }
        ("POST", "/action") => {
            let mut body = vec![0u8; content_length];
            if content_length > 0 && reader.read_exact(&mut body).is_err() {
                respond_json(stream, 400, &json!({ "toast": "bad request" }));
                return;
            }
            let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let reply = run_action(state, &request);
            respond_json(stream, 200, &reply);
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
        404 => "Not Found",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
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
            widgets.push(json!({"kind": "label", "muted": true, "text": format!("Vault state: {other}.")}));
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
            if query.is_empty() && let Some(host) = host.filter(|host| !host.is_empty()) {
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
        widgets.push(json!({"kind": "section", "text": format!("Reused passwords ({})", reused.len())}));
        for group in reused.iter().take(MAX_REPORT_ROWS) {
            let labels: Vec<&str> = group.as_array().map(|group| {
                group.iter().filter_map(Value::as_str).collect()
            }).unwrap_or_default();
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
        widgets.push(json!({"kind": "section", "text": format!("Weak passwords ({})", weak.len())}));
        let shown: Vec<&str> = weak.iter().filter_map(Value::as_str).take(MAX_REPORT_ROWS).collect();
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

fn run_action(state: &Arc<Mutex<PaneState>>, request: &Value) -> Value {
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
                merge(reschema(state, host.as_deref()), json!({ "toast": format!("Synced {count} items.") }))
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
                Err(error) => merge(reschema(state, host.as_deref()), json!({ "toast": error.to_string() })),
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
                merge(reschema(state, host.as_deref()), json!({ "toast": "Vault locked." }))
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

fn reschema(state: &Arc<Mutex<PaneState>>, host: Option<&str>) -> Value {
    let state = state.lock().unwrap();
    json!({ "schema": vault_schema(&state, host) })
}

// ---------------------------------------------------------------------------
// The settings pane: ad blocking + userscripts, owned by THIS host.
// ---------------------------------------------------------------------------

/// Toggle ids double as action ids. A userscript's action carries its stem after
/// the prefix, so one arm handles however many scripts the host has.
const USERSCRIPT_ACTION_PREFIX: &str = "userscript:";

/// Read this host's policy files and draw the pane. The I/O lives here so
/// [`settings_schema_from`] stays pure and testable without touching the user's
/// real config — the same split the vault pane uses.
fn settings_schema(profile: &str) -> Value {
    settings_schema_from(profile, &crate::webpolicy::state(profile))
}

fn settings_schema_from(profile: &str, state: &crate::webpolicy::PolicyState) -> Value {
    let mut widgets = vec![json!({"kind": "section", "text": "Ad blocking"})];

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

    widgets.push(json!({"kind": "section", "text": "Userscripts"}));
    if state.userscripts.is_empty() {
        widgets.push(json!({
            "kind": "label",
            "muted": true,
            "text": "None installed. Drop *.js into ~/.yggterm/web-userscripts/ (all profiles) \
                     or this profile's userscripts/ dir, on the host ychrome runs on.",
        }));
    }
    for (stem, enabled) in &state.userscripts {
        widgets.push(json!({
            "kind": "toggle",
            "id": format!("{USERSCRIPT_ACTION_PREFIX}{stem}"),
            "action": format!("{USERSCRIPT_ACTION_PREFIX}{stem}"),
            "label": stem,
            "value": enabled,
        }));
    }

    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "Userscript changes apply when the surface reloads. An adblock RULESET change \
                 needs a yggterm restart — WebKit compiles the filter once per GUI process.",
    }));
    widgets.push(json!({
        "kind": "button",
        "id": "reload-surface",
        "action": "reload-surface",
        "label": "Reload surface now",
        "primary": true,
    }));

    json!({ "title": "ychrome", "widgets": widgets })
}

/// A settings click. Every mutation lands on THIS host's disk, then the pane
/// re-reads it — the files are the source of truth, so the toggle can never
/// disagree with what `/policy` will serve next.
fn run_settings_action(state: &Arc<Mutex<PaneState>>, request: &Value) -> Value {
    let action = request["action"].as_str().unwrap_or_default();
    // A toggle posts its checkbox state as `values.value` ("true"/"false").
    let on = request["values"]["value"].as_str() == Some("true");
    let profile = state.lock().unwrap().profile.clone();

    let outcome = match action {
        "adblock-enabled" => crate::webpolicy::set_adblock_enabled(on),
        "adblock-profile" => crate::webpolicy::set_adblock_profile_disabled(&profile, !on),
        // Reloading is a page action, so it rides the `eval` channel the vault's
        // fill already uses. No new GUI capability.
        "reload-surface" => {
            return json!({
                "schema": settings_schema(&profile),
                "eval": "location.reload()",
                "toast": "Reloading the surface.",
            });
        }
        script if script.starts_with(USERSCRIPT_ACTION_PREFIX) => {
            crate::webpolicy::set_userscript_enabled(
                script.trim_start_matches(USERSCRIPT_ACTION_PREFIX),
                on,
            )
        }
        other => return json!({ "toast": format!("unknown action {other:?}") }),
    };

    // Redraw from disk either way: a failed rename must snap the toggle back to
    // what the file system actually says, not leave it showing the click.
    let schema = settings_schema(&profile);
    match outcome {
        Ok(()) => json!({
            "schema": schema,
            "toast": "Saved. Reload the surface to apply.",
        }),
        Err(error) => json!({ "schema": schema, "toast": error.to_string() }),
    }
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
        assert_eq!(reply["eval"], "location.reload()");
        assert_eq!(reply["schema"]["title"], "ychrome");
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
        assert!(reply["schema"].is_null(), "an unknown action redrew the pane");
        assert!(
            reply["toast"].as_str().unwrap_or_default().contains("unknown"),
            "expected an unknown-action toast, got {reply:?}"
        );
    }

    // Reloading is a page action, so it must ride the existing `eval` channel
    // rather than asking yggterm for a new capability.
    #[test]
    fn reloading_the_surface_rides_the_eval_channel() {
        let state = Arc::new(Mutex::new(PaneState::new("default")));
        let reply = run_settings_action(
            &state,
            &json!({"pane": SETTINGS_PANE, "action": "reload-surface", "values": {}}),
        );
        assert!(reply["eval"].is_string());
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

    // The per-profile override must name the jar it governs, or the user cannot
    // tell which identity they just turned ad blocking off for.
    #[test]
    fn the_settings_schema_names_the_running_profile() {
        let schema = settings_schema_from("work", &policy_state(true, &[]));
        assert_eq!(schema["title"], "ychrome");
        assert!(
            schema.to_string().contains("work"),
            "profile missing from {schema}"
        );
    }

    // With no ruleset on this host there is nothing to toggle: say so, rather
    // than offering a switch that governs nothing.
    #[test]
    fn a_host_with_no_ruleset_offers_no_adblock_toggle() {
        let schema = settings_schema_from("work", &policy_state(false, &[]));
        let widgets = schema["widgets"].as_array().expect("widgets");
        assert!(
            !widgets.iter().any(|w| w["id"] == "adblock-enabled"),
            "offered an adblock toggle with no ruleset installed"
        );
    }

    // A userscript's action carries its stem, so one arm serves every script the
    // host has — and the toggle reflects the `.js.disabled` rename.
    #[test]
    fn each_userscript_gets_a_toggle_keyed_by_its_stem() {
        let schema = settings_schema_from(
            "work",
            &policy_state(true, &[("sponsorblock", true), ("darkmode", false)]),
        );
        let widgets = schema["widgets"].as_array().expect("widgets");
        let sponsor = widgets
            .iter()
            .find(|w| w["id"] == "userscript:sponsorblock")
            .expect("sponsorblock toggle");
        assert_eq!(sponsor["value"], true);
        assert_eq!(sponsor["action"], "userscript:sponsorblock");
        let dark = widgets
            .iter()
            .find(|w| w["id"] == "userscript:darkmode")
            .expect("darkmode toggle");
        assert_eq!(dark["value"], false);
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
        assert!(!wire.contains("GEZDGNBVGY3TQOJQ"), "totp secret leaked into a row");
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
        assert!(!script.contains("\"; alert(1); //\";"), "escaped out of the literal");
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
        assert_eq!(password["secret"], true, "the password field must be masked");
        assert_eq!(password["value"], "", "a schema must never carry a secret");

        // Notes is offered, seeded from the draft, and not a secret.
        let notes = widgets
            .iter()
            .find(|widget| widget["id"] == "add_notes")
            .expect("the Add tab has a notes field");
        assert_ne!(notes["secret"], true, "notes are not a secret");

        // Seeded from the page the user is looking at.
        let named = |id: &str| {
            widgets
                .iter()
                .find(|widget| widget["id"] == id)
                .unwrap()["value"]
                .clone()
        };
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
        absorb_draft(&mut state, &json!({"add_notes": "recovery codes in 1Password"}));
        assert_eq!(state.add.notes, "recovery codes in 1Password");
        let schema = unlocked_schema(
            &PaneState { tab: "add".to_string(), add: AddDraft { notes: "hi".to_string(), ..AddDraft::default() }, ..PaneState::default() },
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
        assert!(wire.contains("restart_agent"), "no remedy offered for a stale agent");
        assert!(wire.contains("re-locks"), "the cost of restarting must be stated");
        // Still an unlock form: restarting lands the user right back here.
        assert!(wire.contains("unlock_password"));

        // A healthy agent gets no banner and no button.
        let fresh = json!({"state": "locked", "email": "you@example.com", "agent_stale": false});
        assert!(!locked_schema(&fresh).to_string().contains("restart_agent"));
        // Absent field (an older `status`) is treated as healthy, not stale.
        assert!(!locked_schema(&json!({"state": "locked"})).to_string().contains("restart_agent"));

        // Tools tab surfaces it too, for a vault that went stale while unlocked.
        let tools = PaneState { tab: "tools".to_string(), ..PaneState::default() };
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

        absorb_draft(&mut state, &json!({ "value": "fill", "host": "github.com" }));
        assert_eq!(state.add.name, "github.com", "an absent field wiped the draft");
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
        assert_eq!(state.generate_length, MAX_GENERATE_LENGTH, "a half-typed number wiped the setting");
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
        let clean = json!(watchtower_widgets(&json!({"scanned": 9, "reused": [], "weak": []})))
            .to_string();
        assert!(clean.contains("No reused or weak passwords"));
        assert!(!clean.contains("Reused passwords ("));
    }

    #[test]
    fn query_values_are_percent_decoded() {
        assert_eq!(query_value("host=example.com", "host").as_deref(), Some("example.com"));
        assert_eq!(query_value("a=1&host=a%2Eb", "host").as_deref(), Some("a.b"));
        assert_eq!(query_value("a=1", "host"), None);
    }
}
