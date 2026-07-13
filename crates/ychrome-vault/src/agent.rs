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

/// The agent's pid, written beside the socket.
///
/// `stop` is an op like any other, which means an agent old enough not to know
/// it cannot be asked to leave — precisely the agent you most want gone after a
/// rebuild. The pid file is the escape hatch: signal it instead.
fn pid_path(dir: &Path) -> PathBuf {
    dir.join("agent.pid")
}

fn read_pid(dir: &Path) -> Option<i32> {
    std::fs::read_to_string(pid_path(dir))
        .ok()?
        .trim()
        .parse()
        .ok()
}

struct AgentState {
    manager: VaultManager,
    /// Bumped by every op that touches secrets; the idle-lock clock reads it.
    last_activity: Instant,
    dir: PathBuf,
    /// Set by the `stop` op; the connection handler exits once it has replied.
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl AgentState {
    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }
}

/// Identifies the exact binary an agent is running: path plus mtime.
///
/// A vault agent outlives the binary that spawned it, so after a rebuild the
/// old process keeps answering with old code — a `get` works, a newly added op
/// comes back "unknown op", and the confusion is total. Clients compare this
/// stamp against their own and say so.
pub fn exe_stamp() -> String {
    let Ok(path) = std::env::current_exe() else {
        return String::new();
    };
    let mtime = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or(0);
    format!("{}@{mtime}", path.display())
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
        std::fs::remove_file(&socket)
            .with_context(|| format!("removing stale {}", socket.display()))?;
    }
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    std::fs::write(pid_path(dir), std::process::id().to_string())
        .with_context(|| format!("writing {}", pid_path(dir).display()))?;
    std::fs::set_permissions(pid_path(dir), std::fs::Permissions::from_mode(0o600))?;

    let state = Arc::new(Mutex::new(AgentState {
        manager: VaultManager::load(dir),
        last_activity: Instant::now(),
        dir: dir.to_path_buf(),
        stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        // `stop` replies first, then takes the process down — the client must
        // see "stopped" rather than a closed socket.
        let stopping = state
            .lock()
            .map(|state| state.stop.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(false);
        if stopping {
            std::process::exit(0);
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
        // Drop the keys, unlink the socket, and exit once the reply is out.
        // Unlinking here (rather than on the way down) means a client that
        // immediately re-spawns cannot race a socket we are about to remove.
        "stop" => {
            state.manager.lock();
            let _ = std::fs::remove_file(socket_path(&state.dir));
            let _ = std::fs::remove_file(pid_path(&state.dir));
            state.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(json!({ "stopped": true }))
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
            // `--trashed` lists the recoverable soft-deleted items instead of the
            // live ones; the two sets never overlap.
            let trashed = request
                .get("trashed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut items = if trashed {
                vault.trashed_items()
            } else {
                vault.items()
            };
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
                (
                    a.name.to_lowercase(),
                    a.username.clone().unwrap_or_default(),
                )
                    .cmp(&(
                        b.name.to_lowercase(),
                        b.username.clone().unwrap_or_default(),
                    ))
            });
            state.touch();
            Ok(json!({ "items": items }))
        }
        // The whole scan runs HERE, where the ciphers are already decrypted.
        // The sidebar used to ask for all ~1100 passwords over this socket, 25
        // at a time, to do the same arithmetic in the GUI. Only labels come out.
        "watchtower" => {
            let vault = unlocked(&state)?;
            let report = crate::watchtower::analyze(
                vault
                    .items()
                    .into_iter()
                    .filter(|item| item.has_password)
                    .filter_map(|item| {
                        let password = vault.password(&item.id)?;
                        let label = crate::watchtower::label(&item.name, item.username.as_deref());
                        Some((label, zeroize::Zeroizing::new(password)))
                    }),
            );
            state.touch();
            Ok(serde_json::to_value(report)?)
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
        // Notes live only in the raw record, so this is also the read that
        // proves an edit preserved them.
        "notes" => {
            let name = string("name").ok_or_else(|| anyhow!("notes needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let notes = vault
                .notes(&item.id)
                .ok_or_else(|| anyhow!("{} has no notes", item.name))?;
            let name = item.name.clone();
            state.touch();
            Ok(json!({ "notes": notes, "name": name }))
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
        // The item's stored passkeys, metadata only. No private key crosses this
        // socket — that is reserved for a future ceremony op with explicit
        // user consent, never a listing.
        "passkeys" => {
            let name = string("name").ok_or_else(|| anyhow!("passkeys needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let passkeys = vault.passkeys(&item.id);
            state.touch();
            Ok(json!({ "name": item.name, "passkeys": passkeys }))
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
        // Create a login. The plaintext arrives over the 0600 socket, is
        // encrypted under the user key here, and only EncStrings reach the
        // server. A `generate` flag rolls the password locally so it never has
        // to cross a shell's argv.
        "add" => {
            // Same refusal wording the read ops give, rather than the raw
            // VaultError text.
            unlocked(&state)?;
            let name = string("name").ok_or_else(|| anyhow!("add needs a name"))?;
            let generate = request
                .get("generate")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let password = if generate {
                let length = request
                    .get("length")
                    .and_then(Value::as_u64)
                    .unwrap_or(crate::generator::DEFAULT_LENGTH as u64)
                    as usize;
                let symbols = request
                    .get("symbols")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                Some(crate::generator::generate_password(length, symbols).to_string())
            } else {
                string("password")
            };
            // A folder is named by the caller and identified by id on the wire.
            // An unknown name is an error, not a silently-unfiled item.
            let folder_id = match string("folder") {
                Some(folder) => Some(
                    unlocked(&state)?
                        .folder_id(&folder)
                        .ok_or_else(|| anyhow!("no vault folder named {folder:?}"))?,
                ),
                None => None,
            };
            let login = crate::model::NewLogin {
                name: name.clone(),
                username: string("user"),
                password: password.clone(),
                totp: string("totp"),
                uri: string("uri"),
                notes: string("notes"),
                folder_id,
            };
            let id = state
                .manager
                .add_login(&login)
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            // The generated password comes back so the caller can show it once;
            // a caller-supplied one is never echoed.
            Ok(json!({
                "id": id,
                "name": name,
                "generated_password": generate.then_some(password).flatten(),
            }))
        }
        // Patch an existing item. Every field the caller does not name — notes,
        // custom fields, favorite, password history — survives verbatim; see
        // `Vault::edit_body`.
        "edit" => {
            // Refuse on the lock before anything else, so a write op fails on
            // the safety condition rather than on a missing argument.
            unlocked(&state)?;
            let name = string("name").ok_or_else(|| anyhow!("edit needs a name"))?;
            let generate = request
                .get("generate")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let password = if generate {
                let length = request
                    .get("length")
                    .and_then(Value::as_u64)
                    .unwrap_or(crate::generator::DEFAULT_LENGTH as u64)
                    as usize;
                let symbols = request
                    .get("symbols")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                Some(crate::generator::generate_password(length, symbols).to_string())
            } else {
                string("password")
            };
            let folder_id = match string("folder") {
                Some(folder) => Some(
                    unlocked(&state)?
                        .folder_id(&folder)
                        .ok_or_else(|| anyhow!("no vault folder named {folder:?}"))?,
                ),
                None => None,
            };
            let edit = crate::model::CipherEdit {
                name: string("rename"),
                username: string("set_user"),
                password: password.clone(),
                totp: string("totp"),
                uri: string("uri"),
                notes: string("notes"),
                folder_id,
            };
            if edit.is_empty() {
                bail!("edit needs at least one field to change");
            }
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let (id, name) = (item.id.clone(), item.name.clone());
            state
                .manager
                .edit_item(&id, &edit)
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            Ok(json!({
                "id": id,
                "name": name,
                "generated_password": generate.then_some(password).flatten(),
            }))
        }
        // Delete an item. Soft by default: it lands in the vault's trash and any
        // Bitwarden client can restore it. `permanent` destroys it outright —
        // the caller must ask for that explicitly, and there is no undo.
        "rm" => {
            let name = string("name").ok_or_else(|| anyhow!("rm needs a name"))?;
            let permanent = request
                .get("permanent")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let (id, name) = (item.id.clone(), item.name.clone());
            state
                .manager
                .remove_item(&id, permanent)
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            Ok(json!({
                "id": id,
                "name": name,
                "permanent": permanent,
                "trashed": !permanent,
            }))
        }
        // Bring a soft-deleted item back from the trash — the inverse of a soft
        // `rm`. The name is resolved among the TRASHED items, not the live ones,
        // so restoring cannot accidentally touch a live entry that shares a name.
        "restore" => {
            let name = string("name").ok_or_else(|| anyhow!("restore needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.trashed_items();
            let item =
                find_by_name(&items, &name, string("user").as_deref()).map_err(|candidates| {
                    if candidates.is_empty() {
                        anyhow!(
                            "no trashed entry named {name:?} \
                             (only a soft-deleted item can be restored)"
                        )
                    } else {
                        let users: Vec<String> = candidates
                            .iter()
                            .map(|item| item.username.as_deref().unwrap_or("<no user>").to_string())
                            .collect();
                        anyhow!(
                            "{name:?} matches {} trashed accounts — name one: {}",
                            candidates.len(),
                            users.join(", ")
                        )
                    }
                })?;
            let (id, name) = (item.id.clone(), item.name.clone());
            state
                .manager
                .restore_item(&id)
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            Ok(json!({
                "id": id,
                "name": name,
                "restored": true,
            }))
        }
        // Account for every cipher the server sent: how many we can read, and
        // why we cannot read the rest.
        "diagnose" => {
            let vault = unlocked(&state)?;
            Ok(serde_json::to_value(vault.diagnose())?)
        }
        // Resolve a `navigator.credentials.get()` request to the stored passkeys
        // that can answer it — secret-free candidate metadata for the account
        // the presence dialog will name. No private key crosses this socket.
        // Reserved for the browser signer; there is no `ychrome-vault` CLI verb.
        "fido2-resolve" => {
            let rp_id = string("rp_id").ok_or_else(|| anyhow!("fido2-resolve needs an rp_id"))?;
            let allow: Vec<String> = request
                .get("allow_credential_ids")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let vault = unlocked(&state)?;
            let matches = vault.passkeys_for_assertion(&rp_id, &allow);
            state.touch();
            Ok(json!({ "matches": matches }))
        }
        // Sign ONE WebAuthn assertion. This is the only op that mints a
        // `UserPresence`, and it does so by value the moment it is called — so it
        // MUST NOT be reachable except from the browser signer, AFTER the user
        // approved the GUI presence dialog for this exact ceremony. There is
        // deliberately no `ychrome-vault` CLI verb for it, and no way for the
        // page (or a casual script) to reach this socket.
        //
        // The honest boundary: on a single-uid host the socket cannot itself
        // distinguish the browser from another same-uid process, exactly as the
        // `get` op (which already returns a plaintext password) cannot. The
        // strong, enforced gate is against the WEB threat — a page can trigger a
        // ceremony but cannot reach the grant that unblocks this op. It is a pure
        // signer behind the GUI dialog, and no weaker than the vault already is.
        "fido2-assert" => {
            let item_id =
                string("item_id").ok_or_else(|| anyhow!("fido2-assert needs an item_id"))?;
            let rp_id = string("rp_id").ok_or_else(|| anyhow!("fido2-assert needs an rp_id"))?;
            let client_data_hash = b64_standard_or_url(
                &string("client_data_hash_b64")
                    .ok_or_else(|| anyhow!("fido2-assert needs a client_data_hash_b64"))?,
            )
            .ok_or_else(|| anyhow!("client_data_hash_b64 did not base64-decode"))?;
            let user_verified = request
                .get("user_verified")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let vault = unlocked(&state)?;
            let assertion = vault
                .fido2_assert(
                    &item_id,
                    string("credential_id").as_deref(),
                    &rp_id,
                    &client_data_hash,
                    crate::fido2::UserPresence::granted(user_verified),
                )
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            Ok(json!({
                "authenticator_data_b64": b64_url_no_pad(&assertion.authenticator_data),
                "signature_b64": b64_url_no_pad(&assertion.signature),
            }))
        }
        // Register a NEW passkey — a `navigator.credentials.create()`. Mints a
        // P-256 credential, stores it as a login (private key sealed under the
        // user key), and returns the PUBLIC material the browser needs to build
        // the attestation: the credential id and the COSE public key. Like
        // `fido2-assert`, this is a WRITE gated by the browser's GUI presence
        // dialog and reachable only from the signer, never a CLI verb.
        "fido2-create" => {
            unlocked(&state)?;
            let rp_id = string("rp_id").ok_or_else(|| anyhow!("fido2-create needs an rp_id"))?;
            let user_id = b64_standard_or_url(
                &string("user_id_b64")
                    .ok_or_else(|| anyhow!("fido2-create needs a user_id_b64"))?,
            )
            .ok_or_else(|| anyhow!("user_id_b64 did not base64-decode"))?;

            let credential = crate::fido2::generate_credential(&mut rand::rngs::OsRng);
            let rp_name = string("rp_name").unwrap_or_else(|| rp_id.clone());
            let user_name = string("user_name").unwrap_or_default();
            let passkey = crate::model::NewPasskey {
                item_name: rp_name.clone(),
                rp_id: rp_id.clone(),
                rp_name,
                user_name: user_name.clone(),
                user_display_name: string("user_display_name").unwrap_or_else(|| user_name.clone()),
                user_id,
                credential_id: credential.credential_id.clone(),
                pkcs8_der: credential.pkcs8_der.to_vec(),
                account_username: (!user_name.is_empty()).then_some(user_name),
                creation_date: iso8601_now(),
            };
            let id = state
                .manager
                .add_passkey_login(&passkey)
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            Ok(json!({
                "item_id": id,
                "credential_id_b64": b64_url_no_pad(&credential.credential_id),
                "cose_public_key_b64": b64_url_no_pad(&credential.cose_public_key),
            }))
        }
        // Roll a password without touching the vault (the sidebar's generator).
        "generate" => {
            let length = request
                .get("length")
                .and_then(Value::as_u64)
                .unwrap_or(crate::generator::DEFAULT_LENGTH as u64)
                as usize;
            let symbols = request
                .get("symbols")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let password = crate::generator::generate_password(length, symbols);
            Ok(json!({ "password": password.to_string() }))
        }
        other => bail!("unknown op {other:?}"),
    }
}

/// Decode base64, accepting either standard or URL-safe-no-pad — the shim sends
/// URL-safe, but a hand-run probe may paste standard.
fn b64_standard_or_url(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let text = text.trim();
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text))
        .ok()
}

/// WebAuthn wire encoding for binary response fields: base64url without padding.
fn b64_url_no_pad(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `YYYY-MM-DDTHH:MM:SS.000Z` for now, for a new passkey's plaintext
/// `creationDate`. Hand-rolled (no chrono dep) via Howard Hinnant's civil-date
/// algorithm — the vault crate already avoids heavy deps.
fn iso8601_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, sod) = ((secs / 86400) as i64, secs % 86400);
    let (hour, minute, second) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

/// (year, month, day) for a count of days since the Unix epoch. Howard
/// Hinnant's `civil_from_days`, valid for the whole Gregorian range.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
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
            // The username is a POSITIONAL argument (`rbw get NAME USER`
            // parity). The old wording told the user to type `--user`, which
            // clap rejects.
            let users: Vec<String> = candidates
                .iter()
                .map(|item| item.username.as_deref().unwrap_or("<no user>").to_string())
                .collect();
            anyhow!(
                "{name:?} matches {} accounts — name one: {}",
                candidates.len(),
                users.join(", ")
            )
        }
    })
}

pub fn status_json(manager: &VaultManager) -> Value {
    let mut status = match manager.status() {
        VaultStatus::NotConfigured => json!({ "state": "not_configured" }),
        VaultStatus::Locked { email, server_url } => {
            json!({ "state": "locked", "email": email, "server_url": server_url })
        }
        VaultStatus::Unlocked {
            email,
            item_count,
            cipher_count,
        } => json!({
            "state": "unlocked",
            "email": email,
            "item_count": item_count,
            "cipher_count": cipher_count,
            "undecryptable": cipher_count.saturating_sub(item_count),
            "lock_timeout_secs": manager.lock_timeout_secs(),
        }),
    };
    status["version"] = json!(env!("CARGO_PKG_VERSION"));
    status["exe_stamp"] = json!(exe_stamp());
    status
}

/// Is an agent answering on this vault dir's socket?
pub fn is_running(dir: &Path) -> bool {
    UnixStream::connect(socket_path(dir)).is_ok()
}

/// Send one request to a running agent. Does not start one.
pub fn request(dir: &Path, request: &Value) -> Result<Value> {
    let socket = socket_path(dir);
    let stream = UnixStream::connect(&socket).with_context(|| {
        format!(
            "no agent on {} — start one with `ychrome-vault unlock`",
            socket.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{request}")?;
    writer.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: Value = serde_json::from_str(line.trim())
        .with_context(|| format!("agent sent a malformed response: {line:?}"))?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(response);
    }
    let error = response
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("agent refused the request");
    // The agent outlives the binary that spawned it. An op this binary knows
    // but the agent does not means the running agent predates the rebuild —
    // say so, instead of leaving the caller staring at "unknown op". `stop` is
    // exempt: it is the remedy, and `stop()` has its own fallback for an agent
    // too old to perform it.
    let stopping = request.get("op").and_then(Value::as_str) == Some("stop");
    if error.starts_with("unknown op") && !stopping {
        bail!("{error} — the running agent predates this binary; run `ychrome-vault stop-agent`");
    }
    Err(anyhow!(error.to_string()))
}

/// Ask a running agent to drop its keys and exit. Returns false when none ran.
///
/// An agent predating the `stop` op cannot answer the request that would retire
/// it, so fall back to signalling the pid file. An agent predating *that* is
/// unreachable by any means we control, and says so rather than pretending.
pub fn stop(dir: &Path) -> Result<bool> {
    if !is_running(dir) {
        clear_agent_files(dir);
        return Ok(false);
    }
    match request(dir, &json!({"op": "stop"})) {
        Ok(_) => Ok(true),
        Err(error) => {
            let Some(pid) = read_pid(dir) else {
                bail!(
                    "{error}\nit also predates the agent pid file, so it cannot be \
                     retired automatically — run: pkill -f 'ychrome-vault agent'"
                );
            };
            terminate(pid, dir)?;
            clear_agent_files(dir);
            Ok(true)
        }
    }
}

/// SIGTERM, then SIGKILL if it will not go. An agent holds decrypted keys, so
/// "still running" is never an acceptable outcome of `stop`.
fn terminate(pid: i32, dir: &Path) -> Result<()> {
    // SAFETY: `kill` on a pid we wrote ourselves; a stale pid at worst returns
    // ESRCH, which the deadline loop below treats as "already gone".
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if !is_running(dir) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::kill(pid, libc::SIGKILL) };
    std::thread::sleep(Duration::from_millis(100));
    if is_running(dir) {
        bail!("the vault agent (pid {pid}) survived SIGKILL");
    }
    Ok(())
}

/// Socket and pid file left behind by a killed agent.
fn clear_agent_files(dir: &Path) {
    let _ = std::fs::remove_file(socket_path(dir));
    let _ = std::fs::remove_file(pid_path(dir));
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
        let dir = std::env::temp_dir().join(format!(
            "ychrome-vault-agent-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // An unconfigured agent still answers: `status` reports not_configured, and
    // every secret op refuses rather than panicking.
    #[test]
    fn agent_answers_status_and_refuses_secrets_while_locked() {
        let dir = temp_dir("locked");
        let state = test_state(VaultManager::load(&dir), dir.clone());

        let status = dispatch(&json!({"op": "status"}), &state).unwrap();
        assert_eq!(status["state"], "not_configured");

        // The write ops refuse on the LOCK, not on a missing argument — a
        // destructive verb must never get as far as resolving a target.
        for op in [
            "list",
            "get",
            "totp",
            "match",
            "suggest",
            "rm",
            "restore",
            "edit",
            "passkeys",
            "watchtower",
        ] {
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
        use crate::model::{RawCipher, RawFido2Credential, Vault, seal};

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
                fido2: vec![RawFido2Credential {
                    credential_id: enc("cred-abc"),
                    rp_id: enc("github.com"),
                    user_name: enc("octocat"),
                    discoverable: enc("true"),
                    key_value: enc("PRIVATE-KEY-MUST-NOT-LEAK"),
                    ..Default::default()
                }],
                ..Default::default()
            },
            RawCipher {
                id: "gt".to_string(),
                item_type: 1,
                name: enc("ygg.example"),
                username: enc("avikalpa"),
                password: enc("hunter2"),
                ..Default::default()
            },
        ];
        // One soft-deleted item, so `list --trashed` and `restore` have a target.
        // It stays OUT of the live `ciphers` above — the live list must not see it.
        let trashed = vec![RawCipher {
            id: "old".to_string(),
            item_type: 1,
            name: enc("deleted-site.example"),
            username: enc("ghost"),
            password: enc("was-here"),
            ..Default::default()
        }];
        let dir = temp_dir("synthetic");
        let mut manager = VaultManager::load(&dir);
        manager.install_vault_for_test(Vault::new(
            user_key,
            Default::default(),
            ciphers,
            trashed,
            Default::default(),
        ));
        test_state(manager, dir)
    }

    fn test_state(manager: VaultManager, dir: PathBuf) -> Arc<Mutex<AgentState>> {
        Arc::new(Mutex::new(AgentState {
            manager,
            last_activity: Instant::now(),
            dir,
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

        let query = dispatch(&json!({"op": "list", "query": "YGG"}), &state).unwrap();
        assert_eq!(query["items"].as_array().unwrap().len(), 1);

        let got = dispatch(&json!({"op": "get", "name": "github"}), &state).unwrap();
        assert_eq!(got["entry"]["password"], "s3cret!");
        assert_eq!(got["entry"]["username"], "octocat");

        let totp = dispatch(&json!({"op": "totp", "name": "GitHub"}), &state).unwrap();
        assert_eq!(totp["code"].as_str().unwrap().len(), 6);
        assert!(dispatch(&json!({"op": "totp", "name": "ygg.example"}), &state).is_err());

        // Strict rule: the github URI auto-matches its own host...
        let matched = dispatch(&json!({"op": "match", "host": "github.com"}), &state).unwrap();
        assert_eq!(matched["entry"]["password"], "s3cret!");
        // ...but a base-domain entry never auto-fills a subdomain.
        assert!(dispatch(&json!({"op": "match", "host": "chat.ygg.example"}), &state).is_err());
        // Loose rule: the sidebar still suggests it there, secret-free.
        let suggested = dispatch(&json!({"op": "suggest", "host": "chat.ygg.example"}), &state).unwrap();
        let suggested = suggested["items"].as_array().unwrap();
        assert_eq!(suggested.len(), 1);
        assert_eq!(suggested[0]["name"], "ygg.example");
        assert!(suggested[0].get("password").is_none());

        assert!(dispatch(&json!({"op": "get", "name": "nope"}), &state).is_err());
    }

    // Passkeys surface as a badge on the list and a metadata-only op. The
    // private key never crosses the socket, and an item without a passkey
    // reports none rather than erroring.
    #[test]
    fn agent_reports_stored_passkeys_metadata_only() {
        let state = synthetic_state();

        let list = dispatch(&json!({"op": "list"}), &state).unwrap();
        let github = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == "GitHub")
            .unwrap()
            .clone();
        assert_eq!(github["has_passkey"], true);

        let response = dispatch(&json!({"op": "passkeys", "name": "github"}), &state).unwrap();
        let passkeys = response["passkeys"].as_array().unwrap();
        assert_eq!(passkeys.len(), 1);
        assert_eq!(passkeys[0]["rp_id"], "github.com");
        assert_eq!(passkeys[0]["user_name"], "octocat");
        // The whole response, serialized, must not contain the private key.
        assert!(
            !response.to_string().contains("PRIVATE-KEY-MUST-NOT-LEAK"),
            "{response}"
        );

        // An item with no passkey answers with an empty list, not an error.
        let none = dispatch(&json!({"op": "passkeys", "name": "ygg.example"}), &state).unwrap();
        assert!(none["passkeys"].as_array().unwrap().is_empty());
    }

    // The `get()` ceremony over the agent socket, end to end with a REAL P-256
    // key: `fido2-resolve` names the candidate secret-free, then `fido2-assert`
    // returns an assertion that verifies against the credential's public key —
    // exactly what an RP checks. This is the browser signer's whole agent path.
    #[test]
    fn agent_resolves_and_signs_a_real_passkey_assertion() {
        use base64::Engine;
        use p256::ecdsa::signature::Verifier;
        use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
        use p256::pkcs8::EncodePrivateKey;
        use sha2::{Digest, Sha256};

        use crate::crypto::SymmetricKey;
        use crate::model::{RawCipher, RawFido2Credential, Vault, seal};

        // A real credential: fixed scalar so the test is deterministic, exported
        // as the base64 PKCS#8 that a decrypted `keyValue` decodes to.
        let signing = SigningKey::from_bytes(&[0x22u8; 32].into()).unwrap();
        let pkcs8 = signing.to_pkcs8_der().unwrap();
        let key_value_b64 = base64::engine::general_purpose::STANDARD.encode(pkcs8.as_bytes());

        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let enc = |text: &str| Some(seal(&key_bytes, text.as_bytes()));
        let cipher = RawCipher {
            id: "pk".into(),
            item_type: 1,
            name: enc("Cloudflare"),
            fido2: vec![RawFido2Credential {
                credential_id: enc("cred-real"),
                rp_id: enc("dash.cloudflare.com"),
                user_name: enc("avikalpa"),
                counter: enc("0"),
                key_value: enc(&key_value_b64),
                ..Default::default()
            }],
            ..Default::default()
        };
        let dir = temp_dir("fido2-assert");
        let mut manager = VaultManager::load(&dir);
        manager.install_vault_for_test(Vault::new(
            user_key,
            Default::default(),
            vec![cipher],
            vec![],
            Default::default(),
        ));
        let state = test_state(manager, dir.clone());

        // Resolve: the candidate carries the account to show, never the key.
        let resolved = dispatch(
            &json!({"op": "fido2-resolve", "rp_id": "dash.cloudflare.com"}),
            &state,
        )
        .unwrap();
        let matches = resolved["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["item_id"], "pk");
        assert_eq!(matches[0]["credential_id"], "cred-real");
        assert_eq!(matches[0]["user_name"], "avikalpa");
        assert!(!resolved.to_string().contains(&key_value_b64));

        // Assert: a real clientDataHash in, a verifiable assertion out.
        let client_data_hash = Sha256::digest(br#"{"type":"webauthn.get"}"#);
        let cdh_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(client_data_hash);
        let assertion = dispatch(
            &json!({
                "op": "fido2-assert",
                "item_id": "pk",
                "credential_id": "cred-real",
                "rp_id": "dash.cloudflare.com",
                "client_data_hash_b64": cdh_b64,
                "user_verified": true,
            }),
            &state,
        )
        .unwrap();

        let decode = |field: &str| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(assertion[field].as_str().unwrap())
                .unwrap()
        };
        let authenticator_data = decode("authenticator_data_b64");
        let signature = decode("signature_b64");

        // authenticatorData is rpIdHash ‖ flags(UP|UV) ‖ signCount(0).
        assert_eq!(
            &authenticator_data[0..32],
            Sha256::digest(b"dash.cloudflare.com").as_slice()
        );
        assert_eq!(authenticator_data[32], 0b0000_0101);

        // THE proof an RP does: the signature verifies over
        // authenticatorData ‖ clientDataHash against the credential's public key.
        let verifying = VerifyingKey::from(&signing);
        let sig = Signature::from_der(&signature).unwrap();
        let mut signed = authenticator_data.clone();
        signed.extend_from_slice(&client_data_hash);
        verifying
            .verify(&signed, &sig)
            .expect("assertion must verify");

        std::fs::remove_dir_all(&dir).ok();
    }

    // The trash is a second, opt-in list. `restore` resolves names against it —
    // and only it — so a destructive verb's inverse can never touch a live entry.
    #[test]
    fn trash_is_listed_only_on_request_and_restore_resolves_the_trash() {
        let state = synthetic_state();

        // The live list never shows the trashed item...
        let live = dispatch(&json!({"op": "list"}), &state).unwrap();
        let live = live["items"].as_array().unwrap();
        assert_eq!(live.len(), 2);
        assert!(
            live.iter()
                .all(|item| item["name"] != "deleted-site.example")
        );

        // ...but `list --trashed` shows exactly it, secret-free like any list.
        let trashed = dispatch(&json!({"op": "list", "trashed": true}), &state).unwrap();
        let trashed = trashed["items"].as_array().unwrap();
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0]["name"], "deleted-site.example");
        assert!(trashed[0].get("password").is_none());

        // Restoring a LIVE item's name refuses before any network — restore's
        // target space is the trash, never the live list.
        let error = dispatch(&json!({"op": "restore", "name": "GitHub"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no trashed entry named"), "{error}");

        // A name that is in neither list refuses the same way.
        let error = dispatch(&json!({"op": "restore", "name": "nope"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no trashed entry named"), "{error}");
    }

    // An `edit` that names no field to change must not reach the network. The
    // guard runs on an UNLOCKED vault, so it cannot be mistaken for a refusal
    // to open the vault at all.
    #[test]
    fn edit_refuses_a_change_that_changes_nothing() {
        let state = synthetic_state();
        let error = dispatch(&json!({"op": "edit", "name": "github"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("at least one field"), "{error}");

        // An ambiguous or unknown target is rejected before any write, with the
        // same wording the read ops use.
        let error = dispatch(
            &json!({"op": "edit", "name": "nope", "rename": "x"}),
            &state,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("no vault entry named"), "{error}");
        let error = dispatch(&json!({"op": "rm", "name": "nope"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no vault entry named"), "{error}");
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
