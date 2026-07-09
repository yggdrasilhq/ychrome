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
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// The pane id ychrome declares. yggterm only ever echoes it back.
const VAULT_PANE: &str = "vault";
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
    let output = Command::new("ychrome-vault")
        .args(args)
        .output()
        .context("run ychrome-vault (is it installed on this host?)")?;
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

/// What the pane is currently showing. Host-resident, like everything else the
/// app owns: yggterm holds no vault state, not even which tab is selected.
struct PaneState {
    tab: String,
    query: String,
}

impl Default for PaneState {
    fn default() -> Self {
        PaneState {
            tab: "fill".to_string(),
            query: String::new(),
        }
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
pub fn spawn() -> Result<Sidebar> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding sidebar control server")?;
    let port = listener.local_addr()?.port();
    let control_url = format!("http://127.0.0.1:{port}");
    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(PaneState::default()));

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

/// `OSC 7717 ; sidebar ; <action> ; <base64 json>`. Carries the control endpoint
/// and the pane buttons — never a schema, never a secret.
pub fn emit_declare(session: &str, control: &str) {
    let payload = json!({
        "session": session,
        "control": control,
        "panes": [{
            "id": VAULT_PANE,
            "icon": "🔑",
            "title": "Vault (fill logins from Bitwarden)",
        }],
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
                let state = state.lock().unwrap();
                vault_schema(&state, host.as_deref())
            };
            respond_json(stream, 200, &schema);
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

/// Build the pane. NO SECRET is ever placed in a schema — only names, usernames
/// and the booleans saying a password or TOTP secret exists.
fn vault_schema(state: &PaneState, host: Option<&str>) -> Value {
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
            widgets.push(json!({"kind": "text-input", "id": "add_name", "label": "Name", "placeholder": "example.com"}));
            widgets.push(json!({"kind": "text-input", "id": "add_user", "label": "Username", "placeholder": "you@example.com"}));
            widgets.push(json!({"kind": "text-input", "id": "add_uri", "label": "URI", "placeholder": "https://example.com"}));
            widgets.push(json!({"kind": "text-input", "id": "add_folder", "label": "Folder (optional)"}));
            widgets.push(json!({
                "kind": "label", "muted": true,
                "text": "The password is generated on this host and stored straight into the vault. It never crosses the terminal or the GUI.",
            }));
            widgets.push(json!({
                "kind": "button", "id": "add", "action": "add", "primary": true,
                "label": "Add with a generated password",
            }));
        }
        "tools" => {
            widgets.push(json!({"kind": "section", "text": "Vault"}));
            match vault_cli_json(&["status"]) {
                Ok(status) => {
                    let state_label = status["state"].as_str().unwrap_or("unknown");
                    let items = status["item_count"].as_u64().unwrap_or(0);
                    widgets.push(json!({
                        "kind": "label", "muted": true,
                        "text": format!("{state_label} · {items} items"),
                    }));
                }
                Err(error) => widgets.push(json!({
                    "kind": "label", "muted": true, "text": error.to_string(),
                })),
            }
            widgets.push(json!({"kind": "button", "id": "sync", "action": "sync", "label": "Re-sync from the server"}));
            widgets.push(json!({"kind": "button", "id": "lock", "action": "lock", "label": "Lock the vault"}));
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

fn run_action(state: &Arc<Mutex<PaneState>>, request: &Value) -> Value {
    let action = request["action"].as_str().unwrap_or_default();
    let values = &request["values"];
    let value = values["value"].as_str().unwrap_or_default().to_string();
    let host = values["host"].as_str().map(str::to_string);

    match action {
        "tab" => {
            {
                let mut state = state.lock().unwrap();
                state.tab = value;
                // A tab switch abandons the search: the query belonged to the
                // list the user just left.
                state.query.clear();
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
        "sync" => match vault_cli_json(&["sync"]) {
            Ok(reply) => {
                let count = reply["item_count"].as_u64().unwrap_or(0);
                merge(reschema(state, host.as_deref()), json!({ "toast": format!("Synced {count} items.") }))
            }
            Err(error) => json!({ "toast": error.to_string() }),
        },
        "lock" => match vault_cli_json(&["lock"]) {
            Ok(_) => merge(reschema(state, host.as_deref()), json!({ "toast": "Vault locked." })),
            Err(error) => json!({ "toast": error.to_string() }),
        },
        "add" => {
            let name = values["add_name"].as_str().unwrap_or_default();
            if name.trim().is_empty() {
                return json!({ "toast": "An item needs a name." });
            }
            let user = values["add_user"].as_str().unwrap_or_default();
            let uri = values["add_uri"].as_str().unwrap_or_default();
            let folder = values["add_folder"].as_str().unwrap_or_default();
            // `--generate` rolls the password on this host and stores it
            // encrypted. It is never echoed back into a schema: a schema is not
            // a place for a secret.
            let mut args = vec!["add", name.trim()];
            if !user.is_empty() {
                args.push(user);
            }
            if !uri.is_empty() {
                args.extend(["--uri", uri]);
            }
            if !folder.is_empty() {
                args.extend(["--folder", folder]);
            }
            args.push("--generate");
            match vault_cli(&args) {
                Ok(_) => merge(
                    reschema(state, host.as_deref()),
                    json!({ "toast": format!("Added {name} with a generated password.") }),
                ),
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

    #[test]
    fn query_values_are_percent_decoded() {
        assert_eq!(query_value("host=example.com", "host").as_deref(), Some("example.com"));
        assert_eq!(query_value("a=1&host=a%2Eb", "host").as_deref(), Some("a.b"));
        assert_eq!(query_value("a=1", "host"), None);
    }
}
