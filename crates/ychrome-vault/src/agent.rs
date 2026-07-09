//! The unlock-caching agent.
//!
//! A vault that re-derives PBKDF2/600000 and re-syncs 1100 ciphers on every
//! `get` is unusable for automation — that, not the crypto, is what `rbw-agent`
//! actually bought us. So: one long-lived process holds the unlocked [`Vault`]
//! in memory; `unlock` happens once; `list`/`get`/`totp` are keyless from then
//! on, until an idle timeout drops it.
//!
//! **Transport is a unix socket, not loopback TCP.** `~/.yggterm/vault/` is
//! created `0700` and the socket `0600`, so reaching it already requires being
//! this uid — no port for another local user to connect to, no token to leak in
//! an argv or an env var. (A same-uid attacker could read any token we might
//! add, so a token would buy nothing here; the filesystem *is* the auth.)
//!
//! Requests and responses are one JSON object per line:
//!
//! ```text
//! {"op":"get","name":"github.com","user":null}
//! {"ok":true,"entry":{"name":"github.com","username":"octocat","password":"…"}}
//! ```
//!
//! Host-resident, like every libyggterm app's state: the agent runs on the
//! machine ychrome runs on, which over ssh is NOT the machine the GUI is on.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::matching::{auto_match_for_host, find_by_name};
use crate::session::{VaultManager, VaultStatus};

/// How long a client waits for a freshly spawned agent to bind its socket.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the idle-lock thread wakes to check the clock.
const LOCK_TICK: Duration = Duration::from_secs(5);

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("agent.sock")
}

struct AgentState {
    manager: VaultManager,
    /// Bumped by every op that touches secrets; the idle-lock clock reads it.
    last_activity: Instant,
}

impl AgentState {
    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }
}

/// Run the agent in the foreground, serving `dir/agent.sock` until killed.
/// Fails fast if another agent already holds the socket.
pub fn serve(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("locking down {}", dir.display()))?;

    let socket = socket_path(dir);
    if socket.exists() {
        if UnixStream::connect(&socket).is_ok() {
            bail!("an agent is already running on {}", socket.display());
        }
        // Bind fails on an existing path; a socket nobody answers on is stale
        // (the agent was killed). Removing it is the only way forward, and it
        // is safe precisely because the connect above proved it is dead.
        std::fs::remove_file(&socket).with_context(|| format!("removing stale {}", socket.display()))?;
    }
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;

    let state = Arc::new(Mutex::new(AgentState {
        manager: VaultManager::load(dir),
        last_activity: Instant::now(),
    }));

    spawn_idle_lock_thread(state.clone());

    eprintln!("ychrome-vault: agent listening on {}", socket.display());
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = state.clone();
                std::thread::spawn(move || serve_connection(stream, &state));
            }
            Err(error) => eprintln!("ychrome-vault: accept failed: {error}"),
        }
    }
    Ok(())
}

/// Drop the unlocked vault once it has gone untouched for `lock_timeout_secs`.
/// A timeout of 0 means "never" — the user opted into an unlock that lasts as
/// long as the process.
fn spawn_idle_lock_thread(state: Arc<Mutex<AgentState>>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(LOCK_TICK);
            let mut state = match state.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            let timeout = state.manager.lock_timeout_secs();
            if timeout == 0 || !state.manager.is_unlocked() {
                continue;
            }
            if state.last_activity.elapsed() >= Duration::from_secs(timeout) {
                state.manager.lock();
                eprintln!("ychrome-vault: idle {timeout}s — vault locked");
            }
        }
    });
}

fn serve_connection(stream: UnixStream, state: &Arc<Mutex<AgentState>>) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(stream);
    let mut writer = write_half;
    for line in reader.lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => dispatch(&request, state),
            Err(error) => Err(anyhow!("malformed request: {error}")),
        };
        let body = match response {
            Ok(mut value) => {
                value["ok"] = json!(true);
                value
            }
            Err(error) => json!({ "ok": false, "error": error.to_string() }),
        };
        if writeln!(writer, "{body}").is_err() || writer.flush().is_err() {
            return;
        }
    }
}

fn dispatch(request: &Value, state: &Arc<Mutex<AgentState>>) -> Result<Value> {
    let op = request
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("request has no op"))?;
    let string = |key: &str| -> Option<String> {
        request
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    };
    let mut state = state.lock().map_err(|_| anyhow!("agent state poisoned"))?;

    match op {
        "ping" => Ok(json!({})),
        "status" => Ok(status_json(&state.manager)),
        "lock" => {
            state.manager.lock();
            Ok(status_json(&state.manager))
        }
        "unlock" => {
            let password = request
                .get("password")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("unlock needs a password"))?;
            let count = state
                .manager
                .unlock(password)
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            Ok(json!({ "item_count": count }))
        }
        "sync" => {
            let count = state
                .manager
                .resync()
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            Ok(json!({ "item_count": count }))
        }
        "list" => {
            let vault = unlocked(&state)?;
            let query = string("query").map(|q| q.to_lowercase());
            let mut items = vault.items();
            if let Some(query) = &query {
                items.retain(|item| {
                    item.name.to_lowercase().contains(query)
                        || item
                            .username
                            .as_deref()
                            .is_some_and(|user| user.to_lowercase().contains(query))
                });
            }
            items.sort_by(|a, b| {
                (a.name.to_lowercase(), a.username.clone().unwrap_or_default())
                    .cmp(&(b.name.to_lowercase(), b.username.clone().unwrap_or_default()))
            });
            state.touch();
            Ok(json!({ "items": items }))
        }
        "get" => {
            let name = string("name").ok_or_else(|| anyhow!("get needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let password = vault
                .password(&item.id)
                .ok_or_else(|| anyhow!("{} has no password", item.name))?;
            let entry = json!({
                "id": item.id,
                "name": item.name,
                "username": item.username,
                "password": password,
            });
            state.touch();
            Ok(json!({ "entry": entry }))
        }
        "totp" => {
            let name = string("name").ok_or_else(|| anyhow!("totp needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let (code, remaining) = vault
                .totp_code(&item.id)
                .ok_or_else(|| anyhow!("{} has no authenticator secret", item.name))?;
            let name = item.name.clone();
            state.touch();
            Ok(json!({ "code": code, "remaining_secs": remaining, "name": name }))
        }
        // The strict host rule: what an auto-fill is allowed to use. Returns
        // the credential outright, because every caller wants it next.
        "match" => {
            let host = string("host").ok_or_else(|| anyhow!("match needs a host"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = auto_match_for_host(&items, &host)
                .ok_or_else(|| anyhow!("no vault entry matches host {host}"))?;
            let password = vault
                .password(&item.id)
                .ok_or_else(|| anyhow!("{} has no password", item.name))?;
            let entry = json!({
                "id": item.id,
                "name": item.name,
                "username": item.username,
                "password": password,
                "has_totp": item.has_totp,
            });
            state.touch();
            Ok(json!({ "entry": entry }))
        }
        // The loose host rule: rows the sidebar floats to the top. Secret-free.
        "suggest" => {
            let host = string("host").ok_or_else(|| anyhow!("suggest needs a host"))?;
            let vault = unlocked(&state)?;
            let items: Vec<_> = vault
                .items()
                .into_iter()
                .filter(|item| crate::matching::item_applies_to_host(item, &host))
                .collect();
            state.touch();
            Ok(json!({ "items": items }))
        }
        other => bail!("unknown op {other:?}"),
    }
}

fn unlocked(state: &AgentState) -> Result<&crate::model::Vault> {
    state
        .manager
        .vault()
        .ok_or_else(|| anyhow!("vault locked: run `ychrome-vault unlock` first"))
}

/// Resolve a name to one item, turning the ambiguous case into an error that
/// names the candidates (so the user knows which `--user` to pass).
fn resolve<'a>(
    items: &'a [crate::model::VaultItem],
    name: &str,
    user: Option<&str>,
) -> Result<&'a crate::model::VaultItem> {
    find_by_name(items, name, user).map_err(|candidates| {
        if candidates.is_empty() {
            anyhow!("no vault entry named {name:?}")
        } else {
            let users: Vec<String> = candidates
                .iter()
                .map(|item| {
                    format!(
                        "{} ({})",
                        item.name,
                        item.username.as_deref().unwrap_or("no user")
                    )
                })
                .collect();
            anyhow!(
                "{name:?} is ambiguous — disambiguate with --user: {}",
                users.join(", ")
            )
        }
    })
}

pub fn status_json(manager: &VaultManager) -> Value {
    match manager.status() {
        VaultStatus::NotConfigured => json!({ "state": "not_configured" }),
        VaultStatus::Locked { email, server_url } => {
            json!({ "state": "locked", "email": email, "server_url": server_url })
        }
        VaultStatus::Unlocked { email, item_count } => json!({
            "state": "unlocked",
            "email": email,
            "item_count": item_count,
            "lock_timeout_secs": manager.lock_timeout_secs(),
        }),
    }
}

/// Is an agent answering on this vault dir's socket?
pub fn is_running(dir: &Path) -> bool {
    UnixStream::connect(socket_path(dir)).is_ok()
}

/// Send one request to a running agent. Does not start one.
pub fn request(dir: &Path, request: &Value) -> Result<Value> {
    let socket = socket_path(dir);
    let stream = UnixStream::connect(&socket)
        .with_context(|| format!("no agent on {} — start one with `ychrome-vault unlock`", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{request}")?;
    writer.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: Value = serde_json::from_str(line.trim())
        .with_context(|| format!("agent sent a malformed response: {line:?}"))?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(response)
    } else {
        Err(anyhow!(
            response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("agent refused the request")
                .to_string()
        ))
    }
}

/// Send one request, starting an agent first if none is listening.
///
/// Only `unlock` uses this. Autostarting for a read op would buy nothing — the
/// fresh agent holds no keys, so `get` would still fail — and would leave a
/// pointless daemon behind; those ops report "no agent, run unlock" instead.
pub fn request_autostart(dir: &Path, req: &Value) -> Result<Value> {
    if !is_running(dir) {
        spawn_agent(dir)?;
    }
    request(dir, req)
}

/// Spawn `ychrome-vault agent` detached from this process group, then wait for
/// it to bind. `process_group(0)` keeps a terminal's Ctrl+C / SIGHUP from
/// reaching the agent when the shell that first needed it goes away.
fn spawn_agent(dir: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt as _;

    let exe = std::env::current_exe().context("locating the ychrome-vault binary")?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("agent")
        .arg("--dir")
        .arg(dir)
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.process_group(0);
    command.spawn().context("spawning the vault agent")?;

    let deadline = Instant::now() + SPAWN_TIMEOUT;
    while Instant::now() < deadline {
        if is_running(dir) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!(
        "the vault agent did not bind {} within {}s",
        socket_path(dir).display(),
        SPAWN_TIMEOUT.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ychrome-vault-agent-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // An unconfigured agent still answers: `status` reports not_configured, and
    // every secret op refuses rather than panicking.
    #[test]
    fn agent_answers_status_and_refuses_secrets_while_locked() {
        let dir = temp_dir("locked");
        let state = Arc::new(Mutex::new(AgentState {
            manager: VaultManager::load(&dir),
            last_activity: Instant::now(),
        }));

        let status = dispatch(&json!({"op": "status"}), &state).unwrap();
        assert_eq!(status["state"], "not_configured");

        for op in ["list", "get", "totp", "match", "suggest"] {
            let error = dispatch(
                &json!({"op": op, "name": "x", "host": "example.com"}),
                &state,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("locked"), "{op}: {error}");
        }
        assert!(dispatch(&json!({"op": "nope"}), &state).is_err());
        assert!(dispatch(&json!({"op": "ping"}), &state).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A genuinely sealed two-item vault: one login on github.com with a TOTP
    /// secret, one on a base domain. No network, no server, no password — the
    /// user key is handed straight in.
    fn synthetic_state() -> Arc<Mutex<AgentState>> {
        use crate::crypto::SymmetricKey;
        use crate::model::{RawCipher, Vault, seal};

        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let enc = |text: &str| Some(seal(&key_bytes, text.as_bytes()));
        let ciphers = vec![
            RawCipher {
                id: "gh".to_string(),
                item_type: 1,
                name: enc("GitHub"),
                username: enc("octocat"),
                password: enc("s3cret!"),
                totp: enc("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"),
                uris: vec![seal(&key_bytes, b"https://github.com/login")],
                ..Default::default()
            },
            RawCipher {
                id: "gt".to_string(),
                item_type: 1,
                name: enc("gour.top"),
                username: enc("avikalpa"),
                password: enc("hunter2"),
                ..Default::default()
            },
        ];
        let dir = temp_dir("synthetic");
        let mut manager = VaultManager::load(&dir);
        manager.install_vault_for_test(Vault::new(user_key, ciphers, Default::default()));
        Arc::new(Mutex::new(AgentState {
            manager,
            last_activity: Instant::now(),
        }))
    }

    // The whole read path an agent or the sidebar uses, over a real sealed
    // vault: metadata carries no secrets, `get`/`totp` decrypt on demand, and
    // the strict/loose host rules land on the right side of the fence.
    #[test]
    fn agent_serves_the_read_path_over_a_sealed_vault() {
        let state = synthetic_state();

        let items = dispatch(&json!({"op": "list"}), &state).unwrap();
        let items = items["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["name"], "GitHub", "sorted by lowercased name");
        assert!(items[0]["has_totp"].as_bool().unwrap());
        // Metadata must never carry the secret itself.
        assert!(items[0].get("password").is_none());

        let query = dispatch(&json!({"op": "list", "query": "GOUR"}), &state).unwrap();
        assert_eq!(query["items"].as_array().unwrap().len(), 1);

        let got = dispatch(&json!({"op": "get", "name": "github"}), &state).unwrap();
        assert_eq!(got["entry"]["password"], "s3cret!");
        assert_eq!(got["entry"]["username"], "octocat");

        let totp = dispatch(&json!({"op": "totp", "name": "GitHub"}), &state).unwrap();
        assert_eq!(totp["code"].as_str().unwrap().len(), 6);
        assert!(dispatch(&json!({"op": "totp", "name": "gour.top"}), &state).is_err());

        // Strict rule: the github URI auto-matches its own host...
        let matched = dispatch(&json!({"op": "match", "host": "github.com"}), &state).unwrap();
        assert_eq!(matched["entry"]["password"], "s3cret!");
        // ...but a base-domain entry never auto-fills a subdomain.
        assert!(dispatch(&json!({"op": "match", "host": "chat.example.com"}), &state).is_err());
        // Loose rule: the sidebar still suggests it there, secret-free.
        let suggested = dispatch(&json!({"op": "suggest", "host": "chat.example.com"}), &state).unwrap();
        let suggested = suggested["items"].as_array().unwrap();
        assert_eq!(suggested.len(), 1);
        assert_eq!(suggested[0]["name"], "gour.top");
        assert!(suggested[0].get("password").is_none());

        assert!(dispatch(&json!({"op": "get", "name": "nope"}), &state).is_err());
    }

    // `lock` must make the cached vault unreachable immediately.
    #[test]
    fn lock_drops_the_cached_vault() {
        let state = synthetic_state();
        assert!(dispatch(&json!({"op": "get", "name": "github"}), &state).is_ok());
        dispatch(&json!({"op": "lock"}), &state).unwrap();
        let error = dispatch(&json!({"op": "get", "name": "github"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("locked"), "{error}");
    }

    // A dead socket file must not wedge the agent forever: serve() detects that
    // nobody answers and rebinds. (Bind on an existing path always fails.)
    #[test]
    fn stale_socket_is_reclaimed() {
        let dir = temp_dir("stale");
        let socket = socket_path(&dir);
        std::fs::write(&socket, b"").unwrap();
        assert!(!is_running(&dir), "a plain file is not a live agent");

        let listener = UnixListener::bind(&socket);
        assert!(listener.is_err(), "bind must refuse an existing path");
        std::fs::remove_file(&socket).unwrap();
        assert!(UnixListener::bind(&socket).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
