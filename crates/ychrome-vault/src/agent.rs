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

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::matching::{auto_match_for_host, find_by_name};
use crate::session::{SessionMaterial, VaultManager, VaultStatus};

use ychrome_vault_proto::pid_path;
/// The agent's client transport — connect/send/read, autostart, stop, and the
/// socket path — lives in the crypto-free `ychrome-vault-proto` crate so the
/// browser can speak this same wire without linking the crypto below. Re-exported
/// so `agent::request` and friends keep their call sites (the CLI here, the
/// sidebar in the browser).
pub use ychrome_vault_proto::{is_running, request, request_autostart, socket_path, stop};

/// How often the idle-lock thread wakes to check the clock.
const LOCK_TICK: Duration = Duration::from_secs(5);

struct AgentState {
    manager: VaultManager,
    /// Bumped by every op that touches secrets; the idle-lock clock reads it.
    last_activity: Instant,
    dir: PathBuf,
    /// Set by the `stop` op; the connection handler exits once it has replied.
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// The bound listener's fd, so `handover` can pass the LIVE socket to its
    /// successor instead of unbinding and rebinding it. `None` in unit tests,
    /// which dispatch ops without ever serving a socket — and a handover with no
    /// socket to pass is refused rather than silently degraded.
    listener_fd: Option<RawFd>,
    /// Set by the `handover` op; the connection handler execs once it has
    /// replied, exactly as `stop` exits once it has replied.
    handover: Option<ExecPlan>,
}

/// What `handover` leaves behind for the connection handler to perform after the
/// reply is flushed. Both fds are already CLOEXEC-cleared; dropping this instead
/// of exec'ing closes the payload, which destroys the key material.
struct ExecPlan {
    exe: PathBuf,
    dir: PathBuf,
    listener_fd: RawFd,
    payload: OwnedFd,
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
    std::env::current_exe()
        .map(|path| ychrome_vault_proto::exe_stamp_of(&path))
        .unwrap_or_default()
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
    serve_on(dir, listener, None)
}

/// Serve on a listener and a session INHERITED from the agent this process just
/// replaced — the far side of the `handover` op's `execve`.
///
/// Same pid, same bound socket, new code. Because the listener fd survived the
/// exec, there is no unbind/rebind window at all: a client that connects during
/// the swap is queued in the socket's backlog and answered by the successor,
/// rather than told "no vault agent".
pub fn serve_adopted(dir: &Path, listener_fd: RawFd, payload_fd: RawFd) -> Result<()> {
    // SAFETY: both fds were opened by this process (before the exec that
    // replaced its image) and named on its own argv. Taking ownership here is
    // what closes them: the payload is consumed and dropped immediately, and
    // the listener lives as long as the agent.
    let listener = unsafe { UnixListener::from_raw_fd(listener_fd) };
    let payload = read_payload(payload_fd);
    serve_on(dir, listener, Some(payload))
}

/// The serve loop both entry points share, so a handed-over agent and a freshly
/// spawned one differ in exactly one thing: whether a session was adopted.
fn serve_on(
    dir: &Path,
    listener: UnixListener,
    adopted: Option<Result<HandoverPayload>>,
) -> Result<()> {
    std::fs::write(pid_path(dir), std::process::id().to_string())
        .with_context(|| format!("writing {}", pid_path(dir).display()))?;
    std::fs::set_permissions(pid_path(dir), std::fs::Permissions::from_mode(0o600))?;

    let mut manager = VaultManager::load(dir);
    let mut last_activity = Instant::now();
    if let Some(payload) = adopted {
        // Every failure here is loud and NONE of them is fatal. A successor that
        // exits would leave the user with no agent at all; one that serves
        // locked costs them a master password; one that serves unlocked with an
        // empty vault costs them a `sync`. Cheapest recoverable outcome wins,
        // and it is printed rather than inferred from an item count.
        match payload {
            Ok(payload) => {
                // Restoring the idle clock is not cosmetic: dropping it would
                // silently extend an unlock past the timeout the user set, and
                // a handover must not change when the vault locks.
                last_activity = Instant::now()
                    .checked_sub(Duration::from_secs(payload.idle_secs))
                    .unwrap_or_else(Instant::now);
                match manager.adopt_session(payload.material) {
                    Ok(count) => eprintln!(
                        "ychrome-vault: adopted an unlocked session ({count} items, idle {}s)",
                        payload.idle_secs
                    ),
                    Err(error) => eprintln!(
                        "ychrome-vault: adopted the keys but could not re-pull the ciphers \
                         ({error}) — the vault is unlocked and EMPTY; run `ychrome-vault sync`"
                    ),
                }
            }
            Err(error) => eprintln!(
                "ychrome-vault: the handover payload was unreadable ({error}) — \
                 the vault is LOCKED and must be unlocked again"
            ),
        }
    }

    let state = Arc::new(Mutex::new(AgentState {
        manager,
        last_activity,
        dir: dir.to_path_buf(),
        stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        listener_fd: Some(listener.as_raw_fd()),
        handover: None,
    }));

    spawn_idle_lock_thread(state.clone());

    eprintln!(
        "ychrome-vault: agent listening on {}",
        socket_path(dir).display()
    );
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
        // `handover` replies first for the same reason, then REPLACES this
        // process image with the newly installed binary. Same pid, same bound
        // socket, new code, unlock intact.
        let plan = state
            .lock()
            .ok()
            .and_then(|mut state| state.handover.take());
        if let Some(plan) = plan {
            let error = exec_successor(&plan);
            // `exec` only returns on failure, and the failure is survivable: the
            // keys, the socket and this loop are all still here, so keep
            // serving the old code rather than dying with the vault. Dropping
            // the plan closes the payload pipe, destroying the copy of the key.
            eprintln!(
                "ychrome-vault: handover to {} failed ({error}) — still serving the old binary",
                plan.exe.display()
            );
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
        // Change the idle-lock timeout on the LIVE agent. Without this the only
        // way to change it was to re-run `configure`, which locks the vault —
        // so the setting nobody could change quietly stayed wrong on two hosts.
        "set-lock-timeout" => {
            let seconds = request
                .get("seconds")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("set-lock-timeout needs `seconds`"))?;
            state
                .manager
                .set_lock_timeout(seconds)
                .map_err(|error| anyhow!(error.to_string()))?;
            // Do not lock: this is a policy change, not a security event.
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
        // Hand the unlocked session to the freshly installed binary WITHOUT
        // re-locking: `execve` the successor in place, keeping this pid and the
        // bound listener fd. Everything else about the process is replaced.
        //
        // The honest framing, because the obvious one is wrong: `execve` does
        // NOT keep memory. It keeps the pid and the open file descriptors and
        // throws the whole address space away, so the keys have to cross the
        // boundary one way or another. argv is refused (world-readable in
        // /proc), an environment variable is refused (same), a file is refused
        // (it would outlive the writer), and a socket is refused (another
        // process could connect). What is left is an anonymous pipe, whose
        // buffer lives in the kernel and dies with the process if the exec never
        // happens. See `payload_pipe`.
        //
        // The exec is a ONE-WAY DOOR — if the successor cannot come up, the
        // unlock is gone and the user retypes their master password — so every
        // guard below runs before the reply, and the successor is proved to run
        // at all before we commit.
        "handover" => {
            let Some(listener_fd) = state.listener_fd else {
                bail!("this agent is not serving a socket, so it has nothing to hand over");
            };
            if !state.manager.is_unlocked() {
                bail!(
                    "the vault is locked, so there is nothing to hand over — \
                     `ychrome-vault stop-agent` retires this agent at no cost"
                );
            }
            let successor = ychrome_vault_proto::installed_vault_exe();
            // The successor is resolved by the AGENT, never named by the client:
            // an exec target taken from a request would be privilege escalation
            // by asking. This is the single most important guard here.
            if let Some(refusal) = handover_refusal(&exe_stamp(), successor.as_deref()) {
                bail!(refusal);
            }
            let successor = successor.expect("the refusal above covers the None case");
            probe_successor(&successor)?;
            // Prove the SESSION is still usable before crossing, and pick up a
            // renewed bearer if this one had expired. The successor re-pulls the
            // ciphers with that token and nothing else, so handing over a dead
            // one would strand it holding keys it cannot use. This must come
            // BEFORE the export, or the export carries the stale token.
            state.manager.resync().map_err(|error| {
                anyhow!("the session cannot be refreshed, so a handover would strand it: {error}")
            })?;
            let material = state
                .manager
                .export_session()
                .ok_or_else(|| anyhow!("the session was dropped mid-handover"))?;
            let payload = HandoverPayload {
                material,
                idle_secs: state.last_activity.elapsed().as_secs(),
            };
            let pipe = payload_pipe(&payload.encode())?;
            clear_cloexec(listener_fd)?;
            let stamp = ychrome_vault_proto::exe_stamp_of(&successor);
            state.handover = Some(ExecPlan {
                exe: successor.clone(),
                dir: state.dir.clone(),
                listener_fd,
                payload: pipe,
            });
            // "accepted", not "done": the exec happens after this reply is
            // flushed, so only a SECOND round trip can prove it worked. The CLI
            // verb does exactly that.
            Ok(json!({
                "handover": "accepted",
                "successor": successor.display().to_string(),
                "successor_stamp": stamp,
                "pid": std::process::id(),
            }))
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
        // The RAW TOTP secret, decrypted but not parsed — recovers a value a
        // user mis-stored in the authenticator slot (which `totp` rejects).
        "totp-secret" => {
            let name = string("name").ok_or_else(|| anyhow!("totp-secret needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let secret = vault
                .totp_secret(&item.id)
                .ok_or_else(|| anyhow!("{} has no authenticator secret", item.name))?;
            let name = item.name.clone();
            state.touch();
            Ok(json!({ "totp_secret": secret, "name": name }))
        }
        // Custom fields also live only in the raw record (like notes), so this
        // is the sole read that surfaces a hidden/text field's value.
        "fields" => {
            let name = string("name").ok_or_else(|| anyhow!("fields needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let fields: Vec<Value> = vault
                .fields(&item.id)
                .into_iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect();
            let raw_field_count = vault.raw_field_count(&item.id);
            let name = item.name.clone();
            state.touch();
            Ok(json!({ "fields": fields, "name": name, "raw_field_count": raw_field_count }))
        }
        // A card's metadata: brand, cardholder, expiry, last four. The 130 items
        // in this vault with no password are mostly these, and before this op
        // they were reachable only through `notes`. No PAN and no CVV cross this
        // socket here — `card-secret` below is the only path to those.
        "card" => {
            let name = string("name").ok_or_else(|| anyhow!("card needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let card = vault
                .card(&item.id)
                .ok_or_else(|| anyhow!("{} is not a card", item.name))?;
            let name = item.name.clone();
            state.touch();
            Ok(json!({ "card": card, "name": name }))
        }
        // The card's FULL number and CVV, plus the rest of what a payment form
        // asks for. This exists for ONE caller: the sidebar's fill injector,
        // which puts the value into a form field and drops it. There is
        // deliberately no `ychrome-vault` CLI verb, the same rule `fido2-assert`
        // lives under.
        //
        // The boundary being defended is the TRANSCRIPT, not this socket: any
        // same-uid process can already pull every password one `get` at a time,
        // but a PAN printed to a terminal is durable — scrollback, shell
        // history, an agent CLI's JSONL — and unlike a password it cannot be
        // rotated on demand.
        "card-secret" => {
            let name = string("name").ok_or_else(|| anyhow!("card-secret needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let card = vault
                .card(&item.id)
                .ok_or_else(|| anyhow!("{} is not a card", item.name))?;
            let secret = vault
                .card_secret(&item.id)
                .ok_or_else(|| anyhow!("{} is not a card", item.name))?;
            state.touch();
            Ok(json!({
                "name": item.name,
                "number": secret.number.as_deref(),
                "code": secret.code.as_deref(),
                // From the same reader as the metadata op, so a form fill and a
                // listing can never disagree about the expiry.
                "cardholder": card.cardholder,
                "exp_month": card.exp_month,
                "exp_year": card.exp_year,
            }))
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
                clear_totp: request
                    .get("clear_totp")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
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

// ---------------------------------------------------------------------------
// The handover: carrying one unlocked session across an execve.
// ---------------------------------------------------------------------------

/// Framing marker for the handover payload. Versioned, because a successor that
/// does not recognise it must refuse loudly rather than misread key material as
/// a token — the payload is raw bytes on a pipe, with no self-description.
const HANDOVER_MAGIC: &[u8; 8] = b"YCHVHO01";

/// Refuse a payload larger than this. A pipe with no reader blocks once its
/// buffer (64 KiB by default) fills, and the reader here does not exist until
/// after the exec — so a payload that could fill it would deadlock the agent
/// while holding the state lock. The real one is a few hundred bytes; anything
/// near this ceiling means something is wrong.
const MAX_HANDOVER_PAYLOAD: usize = 32 * 1024;

/// How long the successor gets to prove it runs before we commit to exec'ing it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// One unlocked session, reduced to what has to cross the exec boundary.
struct HandoverPayload {
    material: SessionMaterial,
    /// Seconds since the outgoing agent's last activity, so the successor
    /// restores the idle-lock clock instead of silently extending the unlock.
    idle_secs: u64,
}

impl HandoverPayload {
    fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut out = Zeroizing::new(Vec::with_capacity(256));
        out.extend_from_slice(HANDOVER_MAGIC);
        out.extend_from_slice(&self.material.user_key[..]);
        push_bytes(&mut out, self.material.access_token.as_bytes());
        match &self.material.refresh_token {
            Some(token) => {
                out.push(1);
                push_bytes(&mut out, token.as_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(&self.idle_secs.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor { bytes, at: 0 };
        if cursor.take(HANDOVER_MAGIC.len())? != HANDOVER_MAGIC {
            bail!("the handover payload is not one this binary understands");
        }
        let mut user_key = Zeroizing::new([0u8; 64]);
        user_key.copy_from_slice(cursor.take(64)?);
        let access_token = Zeroizing::new(cursor.take_string()?);
        let refresh_token = match cursor.take(1)?[0] {
            0 => None,
            1 => Some(Zeroizing::new(cursor.take_string()?)),
            other => bail!("the handover payload has a bad refresh-token flag {other}"),
        };
        let idle_secs = u64::from_le_bytes(
            cursor
                .take(8)?
                .try_into()
                .expect("take(8) yields eight bytes"),
        );
        Ok(HandoverPayload {
            material: SessionMaterial {
                user_key,
                access_token,
                refresh_token,
            },
            idle_secs,
        })
    }
}

fn push_bytes(out: &mut Zeroizing<Vec<u8>>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// A bounds-checked reader over the payload. Every read is fallible: a truncated
/// payload must be an error, never a silently short key.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| anyhow!("the handover payload is truncated"))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn take_string(&mut self) -> Result<String> {
        let len = u32::from_le_bytes(self.take(4)?.try_into().expect("take(4) yields four bytes"))
            as usize;
        String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| anyhow!("the handover payload holds a non-UTF-8 token"))
    }
}

/// Put the payload on an anonymous pipe and hand back the READ end, ready to
/// survive an exec.
///
/// Not argv and not an environment variable: both are world-readable through
/// `/proc`. Not a file: it would outlive the process that wrote it. Not a
/// socket: another process could connect to it. A pipe's buffer lives in the
/// kernel, is reachable only through its two fds, and is destroyed with the
/// process if the exec never happens. The write end closes here, so the
/// successor reads to a clean EOF.
fn payload_pipe(bytes: &[u8]) -> Result<OwnedFd> {
    if bytes.len() > MAX_HANDOVER_PAYLOAD {
        bail!(
            "the handover payload is {} bytes, over the {MAX_HANDOVER_PAYLOAD}-byte ceiling \
             a reader-less pipe can hold",
            bytes.len()
        );
    }
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `pipe` either fills both slots or returns non-zero; nothing is
    // read out of the array unless it succeeded.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("creating the handover pipe");
    }
    // SAFETY: both fds come from the successful `pipe` above and are owned here.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let mut writer = std::fs::File::from(write);
    writer
        .write_all(bytes)
        .context("writing the handover payload")?;
    drop(writer);
    clear_cloexec(read.as_raw_fd())?;
    Ok(read)
}

/// Read a payload written by [`payload_pipe`] and close the fd.
fn read_payload(fd: RawFd) -> Result<HandoverPayload> {
    // SAFETY: the fd was created by this process before the exec that replaced
    // its image, and named on its own argv. Owning it here is what closes it.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut bytes = Zeroizing::new(Vec::new());
    file.read_to_end(&mut bytes)
        .context("reading the handover payload")?;
    HandoverPayload::decode(&bytes)
}

/// Let an fd survive an `execve`. Rust sets `FD_CLOEXEC` on everything it opens,
/// which is the right default and exactly what has to be opted out of here — for
/// the payload pipe AND for the listener, so the socket is never unbound.
fn clear_cloexec(fd: RawFd) -> Result<()> {
    // SAFETY: plain fcntl on an fd this process owns.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("reading fd flags");
    }
    // SAFETY: as above; the value written is the flags we just read, minus one bit.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error()).context("clearing FD_CLOEXEC");
    }
    Ok(())
}

/// Why a handover must not happen, or `None` when it may.
///
/// Pure, because the live inputs are not testable: `exe_stamp()` describes
/// wherever this binary was built and `installed_vault_exe()` whatever the box
/// has installed, and a test that depended on either would pass or fail by
/// accident of the machine.
fn handover_refusal(running_stamp: &str, successor: Option<&Path>) -> Option<String> {
    let Some(successor) = successor else {
        return Some(
            "no installed `ychrome-vault` on PATH to hand over to — install the new binary first"
                .to_string(),
        );
    };
    if ychrome_vault_proto::exe_stamp_of(successor) == running_stamp {
        return Some(format!(
            "{} is the binary this agent is ALREADY running — nothing to hand over",
            successor.display()
        ));
    }
    None
}

/// Require the successor to run and exit cleanly before we commit to it.
///
/// A truncated `scp`, a binary for the wrong architecture or one whose libc is
/// too new all fail here, cheaply, while the unlock is still recoverable. After
/// the exec there is no going back.
fn probe_successor(exe: &Path) -> Result<()> {
    let mut child = std::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("the successor {} will not start", exe.display()))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait()? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => bail!("the successor {} exited {status}", exe.display()),
            None if Instant::now() >= deadline => {
                // A hung probe would otherwise hang the agent, which is holding
                // the state lock for the whole dispatch.
                let _ = child.kill();
                bail!(
                    "the successor {} did not answer --version within {}s",
                    exe.display(),
                    PROBE_TIMEOUT.as_secs()
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Replace this process with the successor. Returns only on failure.
///
/// The fd NUMBERS ride on argv, and only the numbers: they are not secret (a
/// same-uid process could already read `/proc/<pid>/fd`), and the alternative —
/// a fixed fd number — would collide with whatever else the process has open.
fn exec_successor(plan: &ExecPlan) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;
    std::process::Command::new(&plan.exe)
        .arg("agent")
        .arg("--dir")
        .arg(&plan.dir)
        .arg("--adopt-listener")
        .arg(plan.listener_fd.to_string())
        .arg("--adopt-payload")
        .arg(plan.payload.as_raw_fd().to_string())
        .exec()
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
        }),
    };
    // Report the idle-lock policy in EVERY state, not just when unlocked. It is
    // not a secret, and it was invisible precisely when someone would look —
    // after an unexplained re-lock. 0 = never.
    let lock_timeout_secs = manager.lock_timeout_secs();
    status["lock_timeout_secs"] = json!(lock_timeout_secs);
    status["auto_lock"] = json!(lock_timeout_secs != 0);
    status["version"] = json!(env!("CARGO_PKG_VERSION"));
    status["exe_stamp"] = json!(exe_stamp());
    status
}

// The client transport — `is_running`, `request`, `request_autostart`, `stop`,
// `socket_path` — is re-exported at the top of this module from
// `ychrome-vault-proto`. The agent (server) below and those clients share one
// wire, owned there.

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
            "card",
            "card-secret",
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
            // A CARD: type 3, no login block, so it has no password at all —
            // the shape of the 130 items in the real vault that `get` refuses.
            // Its fields live only in the raw record, like notes.
            RawCipher {
                id: "cc".to_string(),
                item_type: 3,
                name: enc("HDFC Regalia"),
                raw: json!({
                    "id": "cc",
                    "type": 3,
                    "card": {
                        "brand": "Visa",
                        "cardholderName": seal(&key_bytes, b"A KUNDU").to_string(),
                        "number": seal(&key_bytes, b"4111111111114242").to_string(),
                        "expMonth": seal(&key_bytes, b"11").to_string(),
                        "expYear": seal(&key_bytes, b"2029").to_string(),
                        "code": seal(&key_bytes, b"737").to_string(),
                    },
                }),
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
            // No socket: a dispatch test serves no listener, and a handover
            // with no socket to pass must refuse rather than half-work.
            listener_fd: None,
            handover: None,
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
        assert_eq!(items.len(), 3, "two logins and a card");
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
        let suggested = dispatch(
            &json!({"op": "suggest", "host": "chat.ygg.example"}),
            &state,
        )
        .unwrap();
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

    // A card is metadata over the socket and a secret only through the op the
    // injector uses. The 130 items in the real vault with no password are mostly
    // cards, and `get` refuses every one of them before it looks at `--field`.
    #[test]
    fn agent_serves_card_metadata_without_the_number() {
        let state = synthetic_state();

        // `get` still refuses a card, and now the LIST says why: not a login.
        let error = dispatch(&json!({"op": "get", "name": "HDFC"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("has no password"), "{error}");
        let list = dispatch(&json!({"op": "list"}), &state).unwrap();
        let card_row = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == "HDFC Regalia")
            .unwrap()
            .clone();
        assert_eq!(card_row["item_type"], 3);
        assert_eq!(card_row["has_password"], false);

        let response = dispatch(&json!({"op": "card", "name": "HDFC"}), &state).unwrap();
        assert_eq!(response["card"]["brand"], "Visa");
        assert_eq!(response["card"]["cardholder"], "A KUNDU");
        assert_eq!(response["card"]["exp_month"], "11");
        assert_eq!(response["card"]["last4"], "4242");
        // THE property: the whole serialized reply carries neither the PAN nor
        // the CVV, the same assertion the passkey private key lives under.
        let wire = response.to_string();
        assert!(!wire.contains("4111111111114242"), "PAN leaked: {wire}");
        assert!(!wire.contains("737"), "CVV leaked: {wire}");

        // The injector's op is the only path to those, and it carries the whole
        // form so a fill needs one round trip.
        let secret = dispatch(&json!({"op": "card-secret", "name": "HDFC"}), &state).unwrap();
        assert_eq!(secret["number"], "4111111111114242");
        assert_eq!(secret["code"], "737");
        assert_eq!(secret["exp_year"], "2029");

        // A login is not a card, and says so rather than answering emptily.
        let error = dispatch(&json!({"op": "card", "name": "github"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not a card"), "{error}");
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
        assert_eq!(live.len(), 3, "two logins and a card, none of them trashed");
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

    // The handover is a ONE-WAY DOOR: after the exec there is no going back to
    // the unlocked session, so every refusal has to happen before the reply.
    // The two that can be tested without a real exec are tested here; the
    // stamp-equality guard is exercised through `handover_refusal` below,
    // because the live inputs (`exe_stamp`, the installed binary) differ on
    // every machine and a test may not depend on either.
    #[test]
    fn handover_refuses_a_locked_vault_and_an_agent_with_no_socket() {
        let dir = temp_dir("handover-locked");
        let locked = test_state(VaultManager::load(&dir), dir.clone());
        let error = dispatch(&json!({"op": "handover"}), &locked)
            .unwrap_err()
            .to_string();
        // A dispatch test holds no listener, so this is the refusal that fires
        // first — and it must fire, because a handover that cannot pass the
        // bound socket would leave the successor with nothing to serve on.
        assert!(error.contains("nothing to hand over"), "{error}");
        std::fs::remove_dir_all(&dir).ok();

        // With a socket but a LOCKED vault, the remedy named must be the free
        // one: stop-agent costs nothing when there are no keys to lose.
        let dir = temp_dir("handover-locked-socket");
        let state = test_state(VaultManager::load(&dir), dir.clone());
        state.lock().unwrap().listener_fd = Some(0);
        let error = dispatch(&json!({"op": "handover"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("locked"), "{error}");
        assert!(error.contains("stop-agent"), "{error}");
        // Nothing was armed: a refused handover must leave no exec behind.
        assert!(state.lock().unwrap().handover.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    // A successor that is the binary already running would exec into itself
    // forever, and a missing one has nothing to exec at all.
    #[test]
    fn handover_refuses_a_missing_or_identical_successor() {
        let dir = temp_dir("handover-stamp");
        let exe = dir.join("ychrome-vault");
        std::fs::write(&exe, b"#!/bin/true\n").unwrap();
        let stamp = ychrome_vault_proto::exe_stamp_of(&exe);

        assert!(
            handover_refusal(&stamp, None)
                .unwrap()
                .contains("no installed")
        );
        let same = handover_refusal(&stamp, Some(&exe)).expect("an identical stamp refuses");
        assert!(same.contains("ALREADY running"), "{same}");
        // A different binary is what a handover is FOR.
        assert!(handover_refusal("/somewhere/else@1", Some(&exe)).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    // The payload is the only thing that survives the exec, so its framing is
    // load-bearing: this drives it through a REAL pipe, fd handling and all —
    // everything the live handover does except the `execve` itself.
    #[test]
    fn the_handover_payload_survives_a_real_pipe() {
        let payload = HandoverPayload {
            material: SessionMaterial {
                user_key: Zeroizing::new([0x5au8; 64]),
                access_token: Zeroizing::new("bearer-abc".to_string()),
                refresh_token: Some(Zeroizing::new("refresh-xyz".to_string())),
            },
            idle_secs: 1234,
        };
        let bytes = payload.encode();
        let pipe = payload_pipe(&bytes).unwrap();
        // The read end must survive an exec, or the successor inherits nothing.
        // SAFETY: reading flags off an fd this test owns.
        let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFD) };
        assert_eq!(
            flags & libc::FD_CLOEXEC,
            0,
            "the payload fd would not survive exec"
        );

        let back = read_payload(std::os::fd::IntoRawFd::into_raw_fd(pipe)).unwrap();
        assert_eq!(&back.material.user_key[..], &[0x5au8; 64]);
        assert_eq!(back.material.access_token.as_str(), "bearer-abc");
        assert_eq!(
            back.material.refresh_token.as_deref().map(String::as_str),
            Some("refresh-xyz")
        );
        // Dropping the idle clock would silently extend the unlock past the
        // timeout the user set.
        assert_eq!(back.idle_secs, 1234);

        // An account with no refresh token round-trips too (the flag byte).
        let payload = HandoverPayload {
            material: SessionMaterial {
                user_key: Zeroizing::new([0u8; 64]),
                access_token: Zeroizing::new(String::new()),
                refresh_token: None,
            },
            idle_secs: 0,
        };
        let back = HandoverPayload::decode(&payload.encode()).unwrap();
        assert!(back.material.refresh_token.is_none());
    }

    // A truncated or foreign payload must be an error, never a silently short
    // key: the successor would come up "unlocked" with garbage and every
    // decrypt would fail its MAC check for no visible reason.
    #[test]
    fn a_malformed_handover_payload_is_refused() {
        let good = HandoverPayload {
            material: SessionMaterial {
                user_key: Zeroizing::new([7u8; 64]),
                access_token: Zeroizing::new("t".to_string()),
                refresh_token: None,
            },
            idle_secs: 5,
        }
        .encode();

        assert!(HandoverPayload::decode(b"").is_err());
        assert!(HandoverPayload::decode(b"not-a-payload-at-all").is_err());
        // Right length, wrong magic — a payload from a future framing.
        let mut wrong_magic = good.to_vec();
        wrong_magic[..8].copy_from_slice(b"YCHVHO99");
        assert!(HandoverPayload::decode(&wrong_magic).is_err());
        // Every truncation is caught, not just the obvious one.
        for cut in 1..good.len() {
            assert!(
                HandoverPayload::decode(&good[..cut]).is_err(),
                "a payload cut to {cut} bytes decoded"
            );
        }
        assert!(HandoverPayload::decode(&good).is_ok());
    }

    // The pipe's buffer is finite and the reader does not exist until after the
    // exec, so an oversized payload would deadlock the agent while it holds the
    // state lock. Refuse it instead.
    #[test]
    fn an_oversized_payload_is_refused_rather_than_deadlocking() {
        let huge = vec![0u8; MAX_HANDOVER_PAYLOAD + 1];
        let error = payload_pipe(&huge).unwrap_err().to_string();
        assert!(error.contains("ceiling"), "{error}");
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
