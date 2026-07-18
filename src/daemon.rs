//! The ychrome host daemon — one per host per user.
//!
//! Two channels reach the GUI from the host an app runs on, and neither lets an
//! app *push* to the GUI: the PTY OSC stream is identity-bound to the emitting
//! session, and the control endpoint is fetched BY the GUI. So a `ychrome <url>`
//! typed in one terminal cannot open a tab in a surface anchored by another. The
//! fleet-correct transport is a host-resident QUEUE the GUI's liveness ping
//! drains on its reply — and a queue needs something durable on the app host to
//! hold it. That thing is this daemon. Consolidation is not a prerequisite of
//! routing; it is the routing mechanism (docs/host-daemon.md).
//!
//! What the daemon owns, that used to be per-invocation:
//!   - the control endpoint every anchored session serves (schemas, policy,
//!     zoom, appearance, actions, the passkey signer) — one process, one control
//!     listener PER registered session (a plain `http://127.0.0.1:<port>`, so
//!     the contribution protocol and the `yggterm-appctl://` bridge are byte-for-
//!     byte unchanged and passkeys keep working with no GUI change),
//!   - the session registry `{env_id, profile, pid}` (soft state, rebuilt from
//!     the clients' heartbeats),
//!   - the per-session command queue routing enqueues into,
//!   - a journal of every routed open, delivery, drop, and reap.
//!
//! The view client stays a blocking foreground anchor (Zzz/fg/picker/close
//! unchanged); it just registers with the daemon, declares the daemon's control
//! url, and re-registers on its heartbeat. If the daemon dies its clients respawn
//! it — daemon death is self-healing, not an incident.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::sidebar::{self, ControlState};

/// The daemon's compiled version. A client whose own version is newer stops the
/// running daemon and respawns it, so a fleet deploy self-heals on next use.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A session is reaped this long after its last client heartbeat. The client
/// re-registers every ~4s (its OSC heartbeat cadence), so three missed beats
/// retire it — closing its control listener, which fails the GUI's next ping and
/// lets the contribution expire on the GUI's zombie pipeline. Comparable to the
/// old OSC-declare-stops expiry, so a SIGKILLed client leaves no phantom rail.
const SESSION_EXPIRE: Duration = Duration::from_secs(14);

/// A queued command the GUI never drained is dropped after this, with a journal
/// line. Matches the platform contract (docs/protocol.md): the queue is
/// in-memory and at-least-once, so a lost open is a retyped command.
const COMMAND_EXPIRE: Duration = Duration::from_secs(60);

/// How recently a session must have seen a `?session=` ping to be routing-
/// capable. Its presence is the marker that the GUI understands the command
/// envelope; without it /route refuses (skew honesty) and the CLI anchors.
const ROUTING_CAPABLE_WITHIN: Duration = Duration::from_secs(30);

/// `~/.yggterm/ychrome/` — the daemon's home. `0700`, same trust shape as the
/// vault bridge: reaching the socket already requires being this uid.
fn daemon_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("ychrome");
    std::fs::create_dir_all(&dir)?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    Ok(dir)
}

fn sock_path() -> Result<PathBuf> {
    Ok(daemon_dir()?.join("daemon.sock"))
}

fn journal_path() -> Result<PathBuf> {
    Ok(daemon_dir()?.join("journal.jsonl"))
}

/// `path@mtime` of the running binary, the vault agent's staleness precedent.
/// The daemon records it at startup; when the on-disk mtime later differs, the
/// binary was replaced and the daemon is stale (docs/host-daemon.md §6).
fn exe_stamp() -> String {
    let Ok(path) = std::env::current_exe() else {
        return String::new();
    };
    let mtime = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or(0);
    format!("{}@{mtime}", path.display())
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Command queue
// ---------------------------------------------------------------------------

/// One explicit, user-initiated operation queued for a session's GUI. Enters the
/// queue ONLY from a CLI verb (routing) — never synthesized by heartbeat logic,
/// so the "a ping can only ever REFRESH" contract holds.
struct QueuedCommand {
    /// Globally unique id the GUI dedups on. Carries the daemon's start nonce so
    /// a restart cannot mint an id the GUI already drained (and would skip).
    id: String,
    seq: u64,
    enqueued: Instant,
    /// `open_tab` or `toast`.
    kind: String,
    /// The command's own fields, merged into its envelope entry (`url`+`raise`,
    /// or `title`+`body`+`tone`).
    args: Value,
}

/// A session's pending commands plus the batch bookkeeping the ack needs. Each
/// `/ping` mints one batch (all still-pending entries) with a fresh id and a
/// high-water seq; the GUI acks a batch only once it has FULLY delivered it
/// (`should_ack`), so retiring everything at or below that batch's high-water is
/// safe — anything enqueued afterwards has a higher seq and survives.
#[derive(Default)]
struct Queue {
    pending: Vec<QueuedCommand>,
    next_seq: u64,
    next_batch: u64,
    /// `(batch_id, high_water_seq)` for the few most recent batches, so a lagging
    /// ack can still be resolved. Bounded — an ack for a forgotten batch is a
    /// no-op (its members already retired or expired).
    batches: VecDeque<(String, u64)>,
}

const MAX_REMEMBERED_BATCHES: usize = 16;

impl Queue {
    fn enqueue(&mut self, env_id: &str, start_nonce: u128, kind: &str, args: Value) -> String {
        let seq = self.next_seq;
        self.next_seq += 1;
        let id = format!("{env_id}:{start_nonce}:{seq}");
        self.pending.push(QueuedCommand {
            id: id.clone(),
            seq,
            enqueued: Instant::now(),
            kind: kind.to_string(),
            args,
        });
        id
    }

    /// Retire everything an acked batch confirmed delivered.
    fn ack(&mut self, batch_id: &str) {
        if let Some(high_water) = self
            .batches
            .iter()
            .find(|(id, _)| id == batch_id)
            .map(|(_, hw)| *hw)
        {
            self.pending.retain(|command| command.seq > high_water);
            // Drop this batch and any older than it — their members are gone.
            self.batches.retain(|(_, hw)| *hw > high_water);
        }
    }

    /// Drop commands the GUI never took. Returns the ids dropped, for the journal.
    fn expire(&mut self) -> Vec<String> {
        let mut dropped = Vec::new();
        self.pending.retain(|command| {
            if command.enqueued.elapsed() > COMMAND_EXPIRE {
                dropped.push(command.id.clone());
                false
            } else {
                true
            }
        });
        dropped
    }

    /// Mint the command envelope for one `/ping` reply, or `None` when nothing is
    /// pending. `env_id` labels each entry's target session (the GUI reverses it
    /// to a session path).
    fn drain_batch(&mut self, env_id: &str) -> Option<Value> {
        if self.pending.is_empty() {
            return None;
        }
        let high_water = self.pending.iter().map(|command| command.seq).max()?;
        let batch_id = format!("{env_id}#{}", self.next_batch);
        self.next_batch += 1;
        self.batches.push_back((batch_id.clone(), high_water));
        while self.batches.len() > MAX_REMEMBERED_BATCHES {
            self.batches.pop_front();
        }
        let entries: Vec<Value> = self
            .pending
            .iter()
            .map(|command| {
                let mut entry = json!({
                    "id": command.id,
                    "kind": command.kind,
                    "session": env_id,
                });
                if let (Some(object), Some(extra)) =
                    (entry.as_object_mut(), command.args.as_object())
                {
                    for (key, value) in extra {
                        object.insert(key.clone(), value.clone());
                    }
                }
                entry
            })
            .collect();
        Some(json!({ "batch_id": batch_id, "entries": entries }))
    }
}

// ---------------------------------------------------------------------------
// Session registry
// ---------------------------------------------------------------------------

struct SessionMeta {
    profile: String,
    pid: i32,
    /// Bumped by every client re-register; the reaper reads it.
    last_heartbeat: Instant,
    /// The last time the GUI pinged this session with a `?session=` param — the
    /// routing-capability marker.
    last_session_ping: Option<Instant>,
    /// Registration order; the routing tie-break ("most recently registered
    /// wins") picks the highest.
    registered_seq: u64,
}

/// One anchored session: its control state (pane + signer), its dedicated control
/// listener, its command queue, and its liveness/registry metadata.
struct SessionEntry {
    env_id: String,
    control: ControlState,
    control_url: String,
    meta: Mutex<SessionMeta>,
    queue: Mutex<Queue>,
    /// Cleared by the reaper; the session's accept loop exits and drops the
    /// listener, closing the port.
    stop: Arc<AtomicBool>,
}

struct Daemon {
    sessions: Mutex<HashMap<String, Arc<SessionEntry>>>,
    /// Monotonic registration counter for the routing tie-break.
    next_registered_seq: Mutex<u64>,
    /// Millis-since-epoch at startup, mixed into command ids so a restart never
    /// re-mints an id the GUI already saw.
    start_nonce: u128,
    startup_exe_stamp: String,
    started: Instant,
}

impl Daemon {
    fn new() -> Self {
        Daemon {
            sessions: Mutex::new(HashMap::new()),
            next_registered_seq: Mutex::new(0),
            start_nonce: now_millis(),
            startup_exe_stamp: exe_stamp(),
            started: Instant::now(),
        }
    }

    /// True when the on-disk binary's mtime has drifted from startup — the binary
    /// was replaced, so this running daemon is stale.
    fn is_stale(&self) -> bool {
        !self.startup_exe_stamp.is_empty() && exe_stamp() != self.startup_exe_stamp
    }

    /// Register (or heartbeat) a session. New env_id ⇒ bind its control listener
    /// and spawn its accept loop; existing ⇒ refresh profile/pid/heartbeat and
    /// hand back the same control url (idempotent, so the client's ~4s heartbeat
    /// costs nothing and re-registration after a daemon respawn just works).
    fn register(self: &Arc<Self>, env_id: &str, profile: &str, pid: i32) -> Result<Value> {
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(entry) = sessions.get(env_id) {
                let mut meta = entry.meta.lock().unwrap();
                meta.profile = profile.to_string();
                meta.pid = pid;
                meta.last_heartbeat = Instant::now();
                return Ok(json!({
                    "ok": true,
                    "control_url": entry.control_url,
                    "env_id": env_id,
                    "version": VERSION,
                }));
            }
        }

        // New session: its own control listener. Ephemeral loopback port, so the
        // declared control url stays a plain `http://127.0.0.1:<port>`.
        let listener =
            TcpListener::bind("127.0.0.1:0").context("binding a session control listener")?;
        listener
            .set_nonblocking(true)
            .context("marking the control listener non-blocking")?;
        let port = listener.local_addr()?.port();
        let control_url = format!("http://127.0.0.1:{port}");
        let registered_seq = {
            let mut seq = self.next_registered_seq.lock().unwrap();
            let value = *seq;
            *seq += 1;
            value
        };
        let entry = Arc::new(SessionEntry {
            env_id: env_id.to_string(),
            control: ControlState::new(profile, env_id, port),
            control_url: control_url.clone(),
            meta: Mutex::new(SessionMeta {
                profile: profile.to_string(),
                pid,
                last_heartbeat: Instant::now(),
                last_session_ping: None,
                registered_seq,
            }),
            queue: Mutex::new(Queue::default()),
            stop: Arc::new(AtomicBool::new(false)),
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(env_id.to_string(), Arc::clone(&entry));
        self.spawn_session_accept_loop(Arc::clone(&entry), listener);
        journal("register", json!({ "env_id": env_id, "profile": profile, "pid": pid, "port": port }));
        Ok(json!({
            "ok": true,
            "control_url": control_url,
            "env_id": env_id,
            "version": VERSION,
        }))
    }

    fn deregister(&self, env_id: &str) {
        if let Some(entry) = self.sessions.lock().unwrap().remove(env_id) {
            entry.stop.store(true, Ordering::SeqCst);
            journal("deregister", json!({ "env_id": env_id }));
        }
    }

    /// The routing decision (docs/host-daemon.md §4). Returns a reply the CLI
    /// turns into "opened in <session>" or an anchor.
    fn route(&self, profile: &str, url: &str, session: Option<&str>, here: bool) -> Value {
        if here {
            return json!({ "ok": true, "routed": false, "reason": "here" });
        }
        let sessions = self.sessions.lock().unwrap();
        // Candidates: matching profile, optionally pinned to a session id.
        let mut matches: Vec<&Arc<SessionEntry>> = sessions
            .values()
            .filter(|entry| {
                let meta = entry.meta.lock().unwrap();
                meta.profile == profile
                    && session.map(|id| id == entry.env_id).unwrap_or(true)
            })
            .collect();
        if matches.is_empty() {
            let reason = if session.is_some() { "no_such_session" } else { "no_match" };
            return json!({ "ok": true, "routed": false, "reason": reason });
        }
        // Skew honesty: a match the GUI cannot drive is not a place to route.
        let routing_capable = |entry: &Arc<SessionEntry>| {
            entry
                .meta
                .lock()
                .unwrap()
                .last_session_ping
                .map(|seen| seen.elapsed() < ROUTING_CAPABLE_WITHIN)
                .unwrap_or(false)
        };
        matches.retain(|entry| routing_capable(entry));
        if matches.is_empty() {
            return json!({ "ok": true, "routed": false, "reason": "gui_not_routing_capable" });
        }
        // Most recently registered wins.
        matches.sort_by_key(|entry| entry.meta.lock().unwrap().registered_seq);
        let target = matches.last().unwrap();
        let id = target.queue.lock().unwrap().enqueue(
            &target.env_id,
            self.start_nonce,
            "open_tab",
            json!({ "url": url, "raise": true }),
        );
        journal(
            "route",
            json!({ "env_id": target.env_id, "profile": profile, "url": url, "command_id": id }),
        );
        json!({
            "ok": true,
            "routed": true,
            "session": target.env_id,
            "command_id": id,
        })
    }

    /// Host-side truth for agents (docs/host-daemon.md §6).
    fn status(&self) -> Value {
        let sessions = self.sessions.lock().unwrap();
        let mut rows: Vec<Value> = sessions
            .values()
            .map(|entry| {
                let meta = entry.meta.lock().unwrap();
                let queue = entry.queue.lock().unwrap();
                let profile = meta.profile.clone();
                json!({
                    "env_id": entry.env_id,
                    "profile": profile,
                    "pid": meta.pid,
                    "control_url": entry.control_url,
                    "queue_depth": queue.pending.len(),
                    "routing_capable": meta
                        .last_session_ping
                        .map(|seen| seen.elapsed() < ROUTING_CAPABLE_WITHIN)
                        .unwrap_or(false),
                    "last_heartbeat_ms_ago": meta.last_heartbeat.elapsed().as_millis(),
                    "policy_version": crate::webpolicy::policy_version(&profile),
                    "zoom_version": crate::webzoom::zoom_version(),
                })
            })
            .collect();
        rows.sort_by(|a, b| a["env_id"].as_str().cmp(&b["env_id"].as_str()));
        json!({
            "ok": true,
            "version": VERSION,
            "pid": std::process::id(),
            "uptime_secs": self.started.elapsed().as_secs(),
            "exe_stamp": self.startup_exe_stamp,
            "stale": self.is_stale(),
            "vault_agent_reachable": vault_agent_reachable(),
            "sessions": rows,
        })
    }

    /// Build a `/ping` reply for a session: the liveness stamps a declare would
    /// carry (so a policy/zoom edit made while running still propagates) plus the
    /// command envelope. Also records the routing-capability marker.
    fn ping_reply(&self, entry: &SessionEntry, session_param: Option<&str>, ack: Option<&str>) -> Value {
        let profile = {
            let mut meta = entry.meta.lock().unwrap();
            if session_param.is_some() {
                meta.last_session_ping = Some(Instant::now());
            }
            meta.profile.clone()
        };
        let mut queue = entry.queue.lock().unwrap();
        if let Some(batch) = ack {
            queue.ack(batch);
        }
        for dropped in queue.expire() {
            journal("command_expired", json!({ "env_id": entry.env_id, "command_id": dropped }));
        }
        let mut reply = json!({
            "app_name": "Ychrome",
            "policy_version": crate::webpolicy::policy_version(&profile),
            "zoom_version": crate::webzoom::zoom_version(),
            "daemon_stale": self.is_stale(),
        });
        if let Some(commands) = queue.drain_batch(&entry.env_id) {
            reply["commands"] = commands;
        }
        reply
    }

    fn spawn_session_accept_loop(self: &Arc<Self>, entry: Arc<SessionEntry>, listener: TcpListener) {
        let daemon = Arc::clone(self);
        std::thread::spawn(move || {
            loop {
                if entry.stop.load(Ordering::SeqCst) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        let daemon = Arc::clone(&daemon);
                        let entry = Arc::clone(&entry);
                        std::thread::spawn(move || handle_control_conn(&daemon, &entry, stream));
                    }
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => break,
                }
            }
            // Dropping `listener` here closes the port, so the GUI's next ping to
            // this reaped session fails and its contribution expires.
        });
    }

    /// Reap sessions whose client stopped heartbeating.
    fn reap(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        let dead: Vec<String> = sessions
            .iter()
            .filter(|(_, entry)| {
                entry.meta.lock().unwrap().last_heartbeat.elapsed() > SESSION_EXPIRE
            })
            .map(|(env_id, _)| env_id.clone())
            .collect();
        for env_id in dead {
            if let Some(entry) = sessions.remove(&env_id) {
                entry.stop.store(true, Ordering::SeqCst);
                journal("reap", json!({ "env_id": env_id }));
            }
        }
    }
}

/// Serve one control-endpoint connection: OPTIONS preflight, `/ping` (the daemon
/// drains the queue), or the app routes (`sidebar::dispatch`).
fn handle_control_conn(daemon: &Daemon, entry: &SessionEntry, stream: TcpStream) {
    let Some(request) = sidebar::read_request(&stream) else {
        return;
    };
    if request.method == "OPTIONS" {
        sidebar::respond_preflight(stream);
        return;
    }
    if request.method == "GET" && request.path == "/ping" {
        let session_param = sidebar::query_value(&request.query, "session");
        let ack = sidebar::query_value(&request.query, "ack");
        let reply = daemon.ping_reply(entry, session_param.as_deref(), ack.as_deref());
        sidebar::respond_json(stream, 200, &reply);
        return;
    }
    let (status, body) = sidebar::dispatch(&entry.control, &request);
    sidebar::respond_json(stream, status, &body);
}

// ---------------------------------------------------------------------------
// Unix-socket API (local CLI, routing, status, supervision)
// ---------------------------------------------------------------------------

fn handle_unix_conn(daemon: &Arc<Daemon>, stream: UnixStream) -> bool {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return false,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return false;
    }
    let request: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    let mut should_exit = false;
    let reply = match op {
        "ping" => json!({ "ok": true, "version": VERSION, "pid": std::process::id(), "stale": daemon.is_stale() }),
        "register" => {
            let env_id = request.get("env_id").and_then(Value::as_str).unwrap_or("");
            let profile = request.get("profile").and_then(Value::as_str).unwrap_or("default");
            let pid = request.get("pid").and_then(Value::as_i64).unwrap_or(0) as i32;
            if env_id.is_empty() {
                json!({ "ok": false, "error": "register needs env_id" })
            } else {
                daemon
                    .register(env_id, profile, pid)
                    .unwrap_or_else(|error| json!({ "ok": false, "error": error.to_string() }))
            }
        }
        "deregister" => {
            let env_id = request.get("env_id").and_then(Value::as_str).unwrap_or("");
            daemon.deregister(env_id);
            json!({ "ok": true })
        }
        "route" => {
            let profile = request.get("profile").and_then(Value::as_str).unwrap_or("default");
            let url = request.get("url").and_then(Value::as_str).unwrap_or("");
            let session = request.get("session").and_then(Value::as_str);
            let here = request.get("here").and_then(Value::as_bool).unwrap_or(false);
            if url.is_empty() {
                json!({ "ok": false, "error": "route needs a url" })
            } else {
                daemon.route(profile, url, session, here)
            }
        }
        "status" => daemon.status(),
        "stop" => {
            should_exit = true;
            json!({ "ok": true, "stopping": true })
        }
        other => json!({ "ok": false, "error": format!("unknown op {other:?}") }),
    };
    let mut stream = stream;
    let _ = writeln!(stream, "{reply}");
    let _ = stream.flush();
    should_exit
}

/// Append one audit line. Best-effort — the daemon must not die over a journal
/// write. Every routed open, delivery, drop and reap lands here (§9).
fn journal(event: &str, data: Value) {
    let Ok(path) = journal_path() else { return };
    let line = json!({ "ts_ms": now_millis(), "event": event, "data": data });
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Is the vault agent's socket answering? Secret-free: connect + `ping`, for
/// `ychrome status`'s reachability line. Never unlocks, never reads an item.
fn vault_agent_reachable() -> bool {
    let Some(sock) = dirs::home_dir().map(|h| h.join(".yggterm").join("vault").join("agent.sock"))
    else {
        return false;
    };
    let Ok(mut stream) = UnixStream::connect(&sock) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if writeln!(stream, "{}", json!({ "op": "ping" })).is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).is_ok() && !line.trim().is_empty()
}

// ---------------------------------------------------------------------------
// Daemon entry point (`ychrome --daemon`)
// ---------------------------------------------------------------------------

/// Run the daemon. The unix-socket BIND is the singleton: only one process holds
/// a given path. If the bind fails because another daemon owns it we exit; if it
/// fails on a stale socket (a crashed daemon), we unlink and retry once.
pub fn run() -> Result<()> {
    let sock = sock_path()?;
    let listener = match UnixListener::bind(&sock) {
        Ok(listener) => listener,
        Err(_) => {
            // Someone holds the path. Alive ⇒ step aside; stale ⇒ reclaim it.
            if socket_answers_ping(&sock) {
                return Ok(());
            }
            let _ = std::fs::remove_file(&sock);
            UnixListener::bind(&sock).context("binding daemon.sock after reclaiming a stale one")?
        }
    };
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))
        .context("locking down daemon.sock")?;

    let daemon = Arc::new(Daemon::new());
    write_daemon_json(&daemon)?;
    journal("daemon_start", json!({ "version": VERSION, "pid": std::process::id() }));

    // The reaper: retire sessions whose client stopped heartbeating.
    {
        let daemon = Arc::clone(&daemon);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(3));
                daemon.reap();
            }
        });
    }

    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let daemon = Arc::clone(&daemon);
                // Each op is a short request/response; a `stop` returns true and
                // we break to exit. Handle inline (fast) but don't let one client
                // wedge the socket — `stop` aside, ops are non-blocking.
                if handle_unix_conn(&daemon, stream) {
                    break;
                }
            }
            Err(_) => continue,
        }
    }
    let _ = std::fs::remove_file(&sock);
    journal("daemon_stop", json!({ "pid": std::process::id() }));
    Ok(())
}

/// `daemon.json`: pid/version/sock for discovery and post-hoc debugging. The
/// socket, not this file, is the singleton and the liveness witness.
fn write_daemon_json(daemon: &Daemon) -> Result<()> {
    let path = daemon_dir()?.join("daemon.json");
    let body = json!({
        "pid": std::process::id(),
        "version": VERSION,
        "sock": sock_path()?.to_string_lossy(),
        "exe_stamp": daemon.startup_exe_stamp,
        "start_ms": now_millis(),
    });
    std::fs::write(path, serde_json::to_string_pretty(&body)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Client side (the view client + the CLI talk to the daemon through here)
// ---------------------------------------------------------------------------

/// One request/response against the daemon socket. `None` if no daemon is
/// listening (or it died mid-exchange).
fn socket_request(request: &Value) -> Option<Value> {
    let sock = sock_path().ok()?;
    let mut stream = UnixStream::connect(&sock).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    writeln!(stream, "{request}").ok()?;
    stream.flush().ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

fn socket_answers_ping(sock: &std::path::Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if writeln!(stream, "{}", json!({ "op": "ping" })).is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).is_ok() && line.contains("\"ok\":true")
}

/// Ensure a daemon of THIS binary's version is running, and return nothing but
/// the guarantee. Spawns one if absent; if the running one is a different
/// version (a fleet deploy left it behind), stops it and respawns — daemon death
/// is self-healing, and the clients' re-register rebuilds the registry in one
/// heartbeat.
pub fn ensure() -> Result<()> {
    for _ in 0..40 {
        if let Some(reply) = socket_request(&json!({ "op": "ping" })) {
            let version = reply.get("version").and_then(Value::as_str).unwrap_or("");
            if version == VERSION {
                return Ok(());
            }
            // A stale-version daemon: ask it to leave, then respawn ours.
            let _ = socket_request(&json!({ "op": "stop" }));
            std::thread::sleep(Duration::from_millis(200));
        }
        spawn_daemon()?;
        std::thread::sleep(Duration::from_millis(150));
    }
    bail!("ychrome daemon did not come up");
}

/// Launch `ychrome --daemon` detached (setsid, cwd=home, stdio to /dev/null), the
/// yedit pattern. Best-effort: a lost race just means another spawn won, and the
/// ensure loop finds the socket answering.
fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe().context("locating the ychrome binary")?;
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    Command::new(&exe)
        .arg("--daemon")
        .current_dir(&home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("spawning the ychrome daemon")?;
    Ok(())
}

/// Register (or heartbeat) a session with the daemon; returns its control url.
pub fn register(env_id: &str, profile: &str) -> Result<String> {
    let reply = socket_request(&json!({
        "op": "register",
        "env_id": env_id,
        "profile": profile,
        "pid": std::process::id(),
    }))
    .context("registering with the ychrome daemon")?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!(
            "daemon refused register: {}",
            reply.get("error").and_then(Value::as_str).unwrap_or("unknown")
        );
    }
    reply
        .get("control_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("daemon register reply had no control_url")
}

/// Heartbeat is a re-register — the same idempotent op keeps the entry alive.
/// Supervising: if the daemon has died, respawn it and re-register (the registry
/// is soft state, rebuilt from exactly this). Returns the current control url,
/// which may CHANGE across a respawn (a fresh listener, a new port) — the caller
/// re-declares when it moves. `None` only if the daemon cannot be brought up.
pub fn register_supervised(env_id: &str, profile: &str) -> Option<String> {
    if let Ok(url) = register(env_id, profile) {
        return Some(url);
    }
    let _ = ensure();
    register(env_id, profile).ok()
}

pub fn deregister(env_id: &str) {
    let _ = socket_request(&json!({ "op": "deregister", "env_id": env_id }));
}

/// Ask the daemon to route a url. Returns the parsed reply (`routed`, `session`,
/// `reason`).
pub fn route(profile: &str, url: &str, session: Option<&str>, here: bool) -> Result<Value> {
    let mut request = json!({ "op": "route", "profile": profile, "url": url, "here": here });
    if let Some(session) = session {
        request["session"] = json!(session);
    }
    socket_request(&request).context("asking the ychrome daemon to route")
}

/// Fetch the daemon's status (spawns one if absent — a status query should not
/// need a browser already open).
pub fn status() -> Result<Value> {
    ensure()?;
    socket_request(&json!({ "op": "status" })).context("querying the ychrome daemon")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drained_batch_carries_the_entry_shape_the_gui_expects() {
        let mut queue = Queue::default();
        queue.enqueue("env-a", 7, "open_tab", json!({ "url": "https://x", "raise": true }));
        let batch = queue.drain_batch("env-a").expect("a batch");
        assert!(batch["batch_id"].as_str().is_some_and(|id| !id.is_empty()));
        let entries = batch["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        // id (dedup), kind, session=<env_id> (the GUI reverses it), plus the
        // command's own args merged into the envelope.
        assert_eq!(entry["id"], json!("env-a:7:0"));
        assert_eq!(entry["kind"], json!("open_tab"));
        assert_eq!(entry["session"], json!("env-a"));
        assert_eq!(entry["url"], json!("https://x"));
        assert_eq!(entry["raise"], json!(true));
    }

    #[test]
    fn an_ack_retires_only_what_the_batch_confirmed_delivered() {
        let mut queue = Queue::default();
        queue.enqueue("e", 1, "toast", json!({ "title": "a" }));
        let first = queue.drain_batch("e").unwrap();
        let first_id = first["batch_id"].as_str().unwrap().to_string();
        // A second command arrives BEFORE the first batch is acked.
        queue.enqueue("e", 1, "toast", json!({ "title": "b" }));
        let second = queue.drain_batch("e").unwrap();
        // The second batch re-sends the un-acked first entry plus the new one
        // (at-least-once; the GUI dedups by id).
        assert_eq!(second["entries"].as_array().unwrap().len(), 2);
        // Acking the FIRST batch retires only its member; the later command,
        // with a higher seq, survives.
        queue.ack(&first_id);
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(queue.pending[0].args["title"], json!("b"));
        // Acking the second batch clears the rest.
        let second_id = second["batch_id"].as_str().unwrap().to_string();
        queue.ack(&second_id);
        assert!(queue.pending.is_empty());
    }

    #[test]
    fn an_undelivered_command_expires_after_the_horizon() {
        let mut queue = Queue::default();
        queue.enqueue("e", 1, "open_tab", json!({ "url": "https://x", "raise": true }));
        // Backdate it past the 60s horizon.
        queue.pending[0].enqueued = Instant::now() - (COMMAND_EXPIRE + Duration::from_secs(1));
        let dropped = queue.expire();
        assert_eq!(dropped, vec!["e:1:0".to_string()]);
        assert!(queue.pending.is_empty());
        // Nothing pending ⇒ no envelope at all (a ping only ever refreshes).
        assert!(queue.drain_batch("e").is_none());
    }

    #[test]
    fn an_ack_for_a_forgotten_batch_is_a_no_op() {
        let mut queue = Queue::default();
        queue.enqueue("e", 1, "toast", json!({ "title": "a" }));
        queue.ack("e#999"); // never minted
        assert_eq!(queue.pending.len(), 1);
    }
}
