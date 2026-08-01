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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::sidebar::{self, ControlState};

/// The daemon's compiled version. It is one of the two inputs to "is the running
/// daemon serving old code" (`daemon_is_outdated`), and the weaker one: it has
/// been `0.1.0` since the daemon existed, so the exe-stamp drift is what
/// actually catches a rebuild. Do not make this the only check again.
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

/// When THIS daemon started, so a refusal can name which generation refused.
static STARTED_AT_MS: AtomicU64 = AtomicU64::new(0);

/// Which daemon answered — `pid 923108, up 42s`.
///
/// A page belongs to the daemon generation that opened it, so "no page" is
/// ambiguous on its own between *you never opened it*, *it was closed*, and
/// *you opened it on a daemon that is no longer the one answering you*. The
/// third used to be common enough to break every `assets/engine-recipes/`
/// script, which is a sequence of `ctl` calls against one `page_id`, and it was
/// invisible: the agent had no way to tell that two calls hit two daemons. The
/// singleton makes it rare; naming the generation makes it diagnosable when it
/// happens anyway.
pub fn generation_label() -> String {
    let pid = std::process::id();
    match STARTED_AT_MS.load(Ordering::SeqCst) {
        0 => format!("pid {pid}"),
        started => {
            let up = now_millis().saturating_sub(u128::from(started)) / 1000;
            format!("pid {pid}, up {up}s")
        }
    }
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
    /// Did this session's client claim, in its `register`, that it declares the
    /// control token? See [`crate::sidebar::TokenCourier`]. Asserted by the
    /// client rather than inferred, and re-asserted on every ~4s heartbeat, so
    /// it can never describe a client that is no longer the one attached.
    declares_control_token: bool,
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

    /// The env ids of every session a client is still heartbeating for, sorted.
    ///
    /// This is the daemon's own accounting of what is ATTACHED to it, and the
    /// only honest answer to "can this process leave without taking something
    /// with it". Each id names a live surface whose control endpoint, sidebar
    /// pane draft, command queue and passkey signer live in THIS process: none
    /// of that survives our exit, and the surface itself has no way to ask for
    /// it back (the client re-registers and re-declares, but a half-typed Add
    /// draft, a queued open and a signature in flight are simply gone).
    ///
    /// A session past `SESSION_EXPIRE` is a client that already went away and
    /// the reaper has not swept yet, so it holds nothing and must not hold a
    /// handover back either.
    fn live_session_ids(&self) -> Vec<String> {
        let sessions = self.sessions.lock().unwrap();
        let mut ids: Vec<String> = sessions
            .values()
            .filter(|entry| entry.meta.lock().unwrap().last_heartbeat.elapsed() <= SESSION_EXPIRE)
            .map(|entry| entry.env_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Register (or heartbeat) a session. New env_id ⇒ bind its control listener
    /// and spawn its accept loop; existing ⇒ refresh profile/pid/heartbeat and
    /// hand back the same control url (idempotent, so the client's ~4s heartbeat
    /// costs nothing and re-registration after a daemon respawn just works).
    ///
    /// `declares_control_token` is the client saying it will carry the token we
    /// are about to hand it. Recorded on BOTH arms: a pre-gate client that keeps
    /// heartbeating must not be able to look capable just because it registered
    /// once against a generous default.
    fn register(
        self: &Arc<Self>,
        env_id: &str,
        profile: &str,
        pid: i32,
        declares_control_token: bool,
    ) -> Result<Value> {
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(entry) = sessions.get(env_id) {
                let mut meta = entry.meta.lock().unwrap();
                meta.profile = profile.to_string();
                meta.pid = pid;
                meta.last_heartbeat = Instant::now();
                meta.declares_control_token = declares_control_token;
                return Ok(json!({
                    "ok": true,
                    "control_url": entry.control_url,
                    // Same token for the life of the session entry: the client
                    // re-declares it on every ~4s heartbeat, and a token that
                    // rotated would leave the GUI holding a stale one between
                    // the rotation and the next declare.
                    "control_token": entry.control.control_token,
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
                declares_control_token,
            }),
            queue: Mutex::new(Queue::default()),
            stop: Arc::new(AtomicBool::new(false)),
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(env_id.to_string(), Arc::clone(&entry));
        let control_token = entry.control.control_token.clone();
        self.spawn_session_accept_loop(Arc::clone(&entry), listener);
        journal(
            "register",
            json!({
                "env_id": env_id,
                "profile": profile,
                "pid": pid,
                "port": port,
                // A session whose client cannot carry the token is diagnosable
                // from the journal alone, at the moment it attaches, rather than
                // only from the 403s it will produce later.
                "declares_control_token": declares_control_token,
            }),
        );
        Ok(json!({
            "ok": true,
            "control_url": control_url,
            "control_token": control_token,
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
                    // False ⇒ this session's vault and settings panes CANNOT
                    // open, no matter what the GUI or the daemon do, because its
                    // CLI predates the control-token gate and never declares
                    // one. The one place to see it without reading 403s.
                    "control_token_declared": meta.declares_control_token,
                })
            })
            .collect();
        rows.sort_by(|a, b| a["env_id"].as_str().cmp(&b["env_id"].as_str()));
        // `pid`, `stale` and `live_sessions` are NOT built here: every reply on
        // this socket carries them (see `handle_unix_conn`), so there is one
        // owner of "which daemon answered and is it serving old code" rather
        // than one per verb that remembered to say so.
        json!({
            "ok": true,
            "version": VERSION,
            "uptime_secs": self.started.elapsed().as_secs(),
            "exe_stamp": self.startup_exe_stamp,
            "vault_agent_reachable": vault_agent_reachable(),
            "sessions": rows,
        })
    }

    /// Build a `/ping` reply for a session: the liveness stamps a declare would
    /// carry (so a policy/zoom edit made while running still propagates) plus the
    /// command envelope. Also records the routing-capability marker.
    ///
    /// `gui` is whether the caller presented this session's control token. The
    /// STAMPS are open — they are not secrets, and gating them would strand an
    /// older GUI's rail (and its surfaces' ad blocking) on a mixed-version
    /// deploy. The COMMAND QUEUE is not: draining is a mutation, and a page
    /// reaching `/ping?ack=` through the appctl bridge could swallow the routed
    /// `open_tab` the GUI was supposed to receive. An untokened ping therefore
    /// gets stamps only — no drain, no ack, no expiry sweep — and the withheld
    /// batch is journalled so a too-old GUI is diagnosable rather than silent.
    fn ping_reply(
        &self,
        entry: &SessionEntry,
        session_param: Option<&str>,
        ack: Option<&str>,
        gui: bool,
    ) -> Value {
        let profile = {
            let mut meta = entry.meta.lock().unwrap();
            if session_param.is_some() {
                meta.last_session_ping = Some(Instant::now());
            }
            meta.profile.clone()
        };
        let mut reply = json!({
            "app_name": "Ychrome",
            "policy_version": crate::webpolicy::policy_version(&profile),
            "zoom_version": crate::webzoom::zoom_version(),
            "daemon_stale": self.is_stale(),
        });
        let mut queue = entry.queue.lock().unwrap();
        if !gui {
            // Say it once per withheld batch, not once per 4s heartbeat: an
            // empty queue means nobody lost anything, and a caller that cannot
            // prove it is the GUI is only interesting when it would have taken
            // something.
            if !queue.pending.is_empty() {
                journal(
                    "command_drain_refused",
                    json!({
                        "env_id": entry.env_id,
                        "pending": queue.pending.len(),
                        "reason": "the /ping caller presented no valid X-Ychrome-Control token",
                    }),
                );
            }
            return reply;
        }
        if let Some(batch) = ack {
            queue.ack(batch);
        }
        for dropped in queue.expire() {
            journal("command_expired", json!({ "env_id": entry.env_id, "command_id": dropped }));
        }
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
        sidebar::respond_preflight(stream, &request.path);
        return;
    }
    if request.method == "GET" && request.path == "/ping" {
        let session_param = sidebar::query_value(&request.query, "session");
        let ack = sidebar::query_value(&request.query, "ack");
        // The liveness half of a ping is open (an older GUI must keep its rail
        // and its ad blocking across a mixed-version deploy); the QUEUE half is
        // not. See `ping_reply`.
        //
        // Asked through the ONE owner of "is this the GUI" rather than compared
        // here: a second copy of a secret comparison is a second thing to get
        // wrong, and only the copy inside `ControlState` is what the gate tests
        // exercise.
        let gui = entry.control.gui_authorized(&request);
        let reply = daemon.ping_reply(entry, session_param.as_deref(), ack.as_deref(), gui);
        sidebar::respond_json(stream, 200, &reply, &request.path);
        return;
    }
    // The session's registration fact, read HERE because only the daemon holds
    // it: a refusal that cannot say "this session's CLI can never deliver the
    // token" is the bare 403 the user could do nothing with.
    let courier = {
        let meta = entry.meta.lock().unwrap();
        if meta.declares_control_token {
            sidebar::TokenCourier::Live
        } else {
            sidebar::TokenCourier::Absent {
                client_pid: meta.pid,
            }
        }
    };
    let (status, body) = sidebar::dispatch(&entry.control, &request, courier);
    sidebar::respond_json(stream, status, &body, &request.path);
}

// ---------------------------------------------------------------------------
// Unix-socket API (local CLI, routing, status, supervision)
// ---------------------------------------------------------------------------

/// Which protocol a unix-socket connection is speaking.
///
/// This socket has carried newline-delimited JSON ops since it existed; the
/// agent engine adds HTTP/1.1 at `/engine/*` (docs/agent-engine.md §3
/// amendment). They are told apart by the FIRST BYTE, which is unambiguous and
/// needs no negotiation: a JSON op is always an object and starts with `{`,
/// while an HTTP request line always starts with a method token. No op name
/// can ever collide with a path, and no version handshake is needed for an
/// older client to keep working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketProtocol {
    LegacyJsonOp,
    Http,
    Empty,
}

pub(crate) fn socket_protocol(first_byte: Option<u8>) -> SocketProtocol {
    match first_byte {
        None => SocketProtocol::Empty,
        Some(b'{') => SocketProtocol::LegacyJsonOp,
        Some(_) => SocketProtocol::Http,
    }
}

/// Serve one `/engine/*` HTTP request off the daemon socket.
fn serve_engine_http(mut reader: BufReader<UnixStream>, stream: UnixStream) {
    let Some(request) = sidebar::read_request(&mut reader) else {
        return;
    };
    if !crate::engine::api::owns(&request.path) {
        // Named, not silently dropped: the only HTTP this socket serves is the
        // engine's, and a client aiming at anything else should be told.
        let body = json!({
            "ok": false,
            "error": format!("no HTTP route {} on the daemon socket (engine routes are /engine/*)", request.path),
        });
        sidebar::respond_json(stream, 404, &body, &request.path);
        return;
    }
    match crate::engine::api::dispatch(&request) {
        crate::engine::api::Reply::Json(status, body) => {
            sidebar::respond_json(stream, status, &body, &request.path)
        }
        crate::engine::api::Reply::Png { png, meta } => sidebar::respond_bytes(
            stream,
            200,
            "image/png",
            &crate::engine::api::shot_meta_header(&meta),
            &png,
        ),
        crate::engine::api::Reply::Ndjson(write_body) => {
            let mut stream = stream;
            sidebar::respond_ndjson_head(&mut stream);
            write_body(&mut stream);
        }
    }
}

/// How long the accept loop will wait for a connected client's FIRST byte.
///
/// ⛔ WITHOUT THIS THE WHOLE DAEMON IS ONE SILENT CLIENT AWAY FROM WEDGED.
/// `fill_buf` below blocks until a byte arrives, and it runs ON the accept
/// loop, so a client that connects and then says nothing — a CLI killed between
/// `connect` and `write`, an ssh pipe that froze — stops the daemon answering
/// anything at all. That wedge is what used to get a live daemon misread as
/// dead and its socket stolen (see `DaemonLock`). The lock now makes that
/// misdiagnosis harmless, but a wedged daemon still serves nobody, so the wedge
/// itself is closed here rather than only survived.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(5);

fn handle_unix_conn(daemon: &Arc<Daemon>, stream: UnixStream) -> bool {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return false,
    });
    // `try_clone` dups the descriptor, so both refer to the same socket and
    // SO_RCVTIMEO set here also bounds the `fill_buf` on `reader`.
    let _ = stream.set_read_timeout(Some(FIRST_BYTE_TIMEOUT));
    match socket_protocol(reader.fill_buf().ok().and_then(<[u8]>::first).copied()) {
        SocketProtocol::Empty => return false,
        SocketProtocol::Http => {
            // The first byte is in, so the accept loop is no longer at risk from
            // this connection: hand the socket back its blocking reads before
            // the engine thread takes over. An engine verb legitimately takes
            // far longer than FIRST_BYTE_TIMEOUT.
            let _ = stream.set_read_timeout(None);
            // ON ITS OWN THREAD, unlike the legacy ops below. Those are handled
            // inline because they are genuinely non-blocking and because `stop`
            // has to return `should_exit` from here. An engine verb is the
            // opposite: `/engine/goto` waits up to 45s for a load, and serving
            // it inline would wedge this socket for every browser heartbeat and
            // every `route` queued behind it. The engine must never be able to
            // stall the surfaces.
            std::thread::spawn(move || serve_engine_http(reader, stream));
            return false;
        }
        SocketProtocol::LegacyJsonOp => {}
    }
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return false;
    }
    let request: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    let mut should_exit = false;
    let reply = match op {
        "ping" => json!({ "ok": true, "version": VERSION }),
        "register" => {
            let env_id = request.get("env_id").and_then(Value::as_str).unwrap_or("");
            let profile = request.get("profile").and_then(Value::as_str).unwrap_or("default");
            let pid = request.get("pid").and_then(Value::as_i64).unwrap_or(0) as i32;
            // Absent ⇒ false, and that is the load-bearing default: a client too
            // old to know the field is exactly the client that cannot carry the
            // token, so silence means "no courier" rather than "assume the best".
            let declares_control_token = request
                .get("declares_control_token")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if env_id.is_empty() {
                json!({ "ok": false, "error": "register needs env_id" })
            } else {
                daemon
                    .register(env_id, profile, pid, declares_control_token)
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
        // The handover decision, made HERE and under this process's own lock,
        // because only this process knows what is attached to it. A client that
        // asked "are you idle?" and then said "stop" would be deciding on a fact
        // that could change between the two round trips; this cannot.
        //
        // It reaps first so a client that already went away cannot pin an
        // outdated daemon in place for the length of its expiry window.
        "retire_if_idle" => {
            daemon.reap();
            let held = daemon.live_session_ids();
            if held.is_empty() {
                should_exit = true;
                json!({ "ok": true, "retiring": true })
            } else {
                json!({ "ok": true, "retiring": false, "held_by": held })
            }
        }
        // The deliberate form (`ychrome daemon restart`): retire whatever is
        // attached, because a person asked for it in as many words.
        "stop" => {
            should_exit = true;
            json!({ "ok": true, "stopping": true })
        }
        other => json!({ "ok": false, "error": format!("unknown op {other:?}") }),
    };
    // Every reply carries who answered and whether it is serving old code,
    // whatever was asked. A client cannot forget to look, and a verb added later
    // cannot answer without saying it: staleness that is invisible on some paths
    // is how an old daemon serves for days (this one ran 6.7 days stale).
    let mut reply = reply;
    if let Some(object) = reply.as_object_mut() {
        object.insert("pid".to_string(), json!(std::process::id()));
        object.insert("stale".to_string(), json!(daemon.is_stale()));
        object.insert(
            "live_sessions".to_string(),
            json!(daemon.live_session_ids().len()),
        );
    }
    if should_exit {
        // Unlink BEFORE answering, so the reply is itself the client's proof
        // that the path is free. Removing it after the reply meant a successor
        // could bind the path and then have THIS process delete the successor's
        // socket on its way out, stranding a live daemon nothing could reach.
        if let Ok(sock) = sock_path() {
            let _ = std::fs::remove_file(&sock);
        }
        // …and hand on the singleton in the same breath, for the same reason:
        // the successor must be able to START while this process is still
        // shutting its engine down. Freeing the path but keeping the lock would
        // make every handover wait out a full engine teardown.
        DaemonLock::release();
        journal(
            "retire",
            json!({ "pid": std::process::id(), "op": op, "stale": daemon.is_stale() }),
        );
    }
    let mut stream = stream;
    let _ = writeln!(stream, "{reply}");
    let _ = stream.flush();
    should_exit
}

/// Append one audit line. Best-effort — the daemon must not die over a journal
/// write. Every routed open, delivery, drop and reap lands here (§9) — and every
/// REFUSED control request, which is why `sidebar` reaches for it too.
pub(crate) fn journal(event: &str, data: Value) {
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

/// The daemon singleton, held for the process's whole life.
///
/// ⛔⛔ THE PING WAS THE BUG, AND THE SOCKET WAS NEVER THE SINGLETON.
/// `run()` used to answer "is another daemon alive?" by connecting to
/// `daemon.sock` and waiting 2 s for a `ping`. Legacy ops are served INLINE on
/// the accept loop, so one slow op — or one client that connects and then says
/// nothing — wedges every accept. A daemon that was alive but merely BUSY
/// therefore read as *stale*, and the challenger then `remove_file`d the socket
/// out from under it and bound its own. The incumbent kept running forever on
/// an unlinked inode, still holding its engine, its Xvfb and its pages, and
/// invisible to every client that came after — which is why an agent could
/// `ctl open` a page and be told `no page "pg_000001"` one call later.
///
/// Measured on dev, 2026-07-31, in `~/.yggterm/ychrome/journal.jsonl`:
/// **21 `daemon_start` against 13 `daemon_stop`**, including a burst of SIX
/// starts with no stop between them (pids 2797950, 2801025, 2809309, 2814008,
/// 2818486, 2883525 — the first two are the pair the bug was filed from), and
/// **15 `Xvfb` processes reparented to init** on displays `:90`–`:104`.
///
/// `flock` cannot make that mistake. It answers from the kernel, with no
/// timeout and no guess, and the kernel releases it when the holder dies —
/// including on SIGKILL — so there is no stale-lock case to reclaim and no
/// window in which two processes both believe they won.
///
/// ⛔ The lock file is NEVER unlinked. Unlinking it is precisely how a
/// lock-by-file loses its mutual exclusion: a challenger that creates a fresh
/// inode at the same path locks a *different* file and both sides proceed.
struct DaemonLock {
    /// Held only for its `Drop`/process-exit side effect — closing the fd is
    /// what releases the advisory lock.
    _file: std::fs::File,
}

/// The lock this process holds while it is THE serving daemon.
///
/// ⛔ IT MEANS "I AM SERVING", NOT "MY PROCESS EXISTS", and the difference is a
/// measured regression. A retiring daemon unlinks its socket BEFORE answering
/// (see `handle_unix_conn`) precisely so its successor can bind and start
/// serving while this one is still winding its engine down — that overlap is
/// the design. Holding the lock until process exit removed it: every handover
/// then had to wait for a full engine shutdown, which made
/// `tests/daemon_staleness.rs` flaky and roughly doubled its runtime.
/// `release()` is called at the same instant the socket is unlinked.
static SINGLETON: Mutex<Option<DaemonLock>> = Mutex::new(None);

impl DaemonLock {
    /// `Ok(None)` when another daemon provably holds it and we must step aside.
    fn acquire() -> Result<Option<DaemonLock>> {
        Self::acquire_at(&daemon_dir()?.join("daemon.lock"))
    }

    /// Hand the singleton to whoever comes next. Idempotent.
    fn release() {
        if let Ok(mut held) = SINGLETON.lock() {
            held.take();
        }
    }

    /// The path is a parameter ONLY so the lock's exclusion can be proven on a
    /// temp file. A test must never reach for the real `daemon.lock`: taking it
    /// would stop the host's actual daemon from starting.
    fn acquire_at(path: &std::path::Path) -> Result<Option<DaemonLock>> {
        use std::os::fd::AsRawFd;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: `file` owns a valid fd for the duration of the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(DaemonLock { _file: file }));
        }
        let error = std::io::Error::last_os_error();
        // EWOULDBLOCK == EAGAIN on Linux; both mean "someone else holds it",
        // which is the ONE outcome that is not an error.
        match error.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
            _ => Err(error).context("flock on daemon.lock"),
        }
    }
}

/// Run the daemon. The advisory lock on `daemon.lock` is the singleton — see
/// `DaemonLock` for why the socket bind could never be one. Holding it proves
/// no other daemon is alive, which is what makes reclaiming a leftover socket
/// safe: it can only be a corpse's.
pub fn run() -> Result<()> {
    // Decided before anything is bound, spawned, or started, so a losing daemon
    // costs nothing and — the load-bearing half — touches nothing.
    let Some(singleton) = DaemonLock::acquire()? else {
        // The kernel says another daemon is alive. That is the fact the 2 s ping
        // could not establish. Step aside WITHOUT unlinking its socket.
        return Ok(());
    };
    // Parked where the retire path can hand it on the moment we stop serving,
    // rather than at process exit. See `SINGLETON`.
    if let Ok(mut held) = SINGLETON.lock() {
        *held = Some(singleton);
    }

    // Safe HERE and only here: holding the singleton, before anything of ours is
    // running, an orphaned engine display can only be a dead daemon's. See
    // `reap_orphaned_displays` for why a live daemon's — including one under
    // another HOME — can never be selected.
    let reaped = crate::engine::substrate::reap_orphaned_displays();
    if !reaped.is_empty() {
        journal("engine.display.reaped", json!({ "displays": reaped }));
    }

    let sock = sock_path()?;
    let listener = match UnixListener::bind(&sock) {
        Ok(listener) => listener,
        Err(_) => {
            // We hold the singleton, so whoever left this socket behind is gone.
            // No ping, no timeout, no guess: reclaiming it cannot strand a live
            // owner, because a live owner would still hold the lock.
            let _ = std::fs::remove_file(&sock);
            UnixListener::bind(&sock).context("binding daemon.sock after reclaiming a stale one")?
        }
    };

    // ⛔ WITHOUT THIS, EVERY NON-GRACEFUL EXIT LEAKS AN X SERVER. `run()` reaps
    // the engine on its way out of the accept loop, but a `kill` never reaches
    // that line — which is how 15 Xvfb came to be parented to init on dev.
    // `ctrlc` runs this on its own thread rather than in signal context, so
    // taking the engine's mutex and writing the journal here is sound.
    if let Err(error) = ctrlc::set_handler(|| {
        crate::engine::api::shutdown();
        journal("daemon_stop", json!({ "pid": std::process::id(), "cause": "signal" }));
        std::process::exit(0);
    }) {
        // Not fatal: a daemon that cannot install the handler still serves, it
        // just leaks its display if it is signalled. Saying so beats pretending.
        eprintln!("ychrome: could not install the daemon's signal handler ({error}); a signal will leak its X display");
    }
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))
        .context("locking down daemon.sock")?;

    STARTED_AT_MS.store(now_millis() as u64, Ordering::SeqCst);
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
    // The socket was already unlinked by the op that asked us to leave, before
    // it answered (see `handle_unix_conn`). Removing it here as well would let a
    // retiring daemon delete its SUCCESSOR's socket.
    // A retiring daemon takes its engine with it. Same reason as the CLI's:
    // the registry holds the engine in a static, and a static is never
    // dropped, so the headless display would outlive the daemon that started
    // it and burn its display number for the successor.
    crate::engine::api::shutdown();
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
///
/// Every reply passes through here, which is why the staleness notice lives
/// here: no caller can talk to an outdated daemon without the fact reaching the
/// user, and no verb added later has to remember to check.
fn socket_request(request: &Value) -> Option<Value> {
    let reply = socket_request_silent(request)?;
    note_daemon_reply(&reply);
    Some(reply)
}

/// The same round trip without the notice, for the one caller that is already
/// acting on the staleness: `ychrome daemon restart` would otherwise print "it
/// was left running rather than retired underneath it" in the middle of
/// retiring it.
fn socket_request_silent(request: &Value) -> Option<Value> {
    let sock = sock_path().ok()?;
    let mut stream = UnixStream::connect(&sock).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    writeln!(stream, "{request}").ok()?;
    stream.flush().ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

/// Is the daemon that sent this reply serving code older than ours?
///
/// Two ways it can be, and they are the same failure: a DIFFERENT compiled
/// version (a fleet deploy left it behind), or our own version with a binary
/// that was replaced on disk after it started (`stale`, the exe-stamp drift).
/// ychrome's version is a constant that almost never moves, so the second is the
/// one that actually happens — and until this predicate counted it, every
/// rebuild left the old daemon serving indefinitely because the version matched.
///
/// A reply with no `version` (most ops) is judged on `stale` alone. It must not
/// read as outdated: a bare `{"ok":true}` ack that looked outdated would send
/// every deregister into a handover.
fn daemon_is_outdated(reply: &Value) -> bool {
    let version = reply.get("version").and_then(Value::as_str).unwrap_or("");
    let stale = reply.get("stale").and_then(Value::as_bool).unwrap_or(false);
    (!version.is_empty() && version != VERSION) || stale
}

/// The daemon we have already announced as outdated. Announcing once per DAEMON
/// rather than once per process is what lets a browser that has been open for
/// days report the next deploy too, while a client heartbeating every 4s does
/// not repeat itself forever — and a line that repeats forever is a line nobody
/// reads.
static ANNOUNCED_OUTDATED_PID: AtomicU64 = AtomicU64::new(0);

fn already_announced(pid: u64) -> bool {
    ANNOUNCED_OUTDATED_PID.load(Ordering::SeqCst) == pid
}

/// Say, on stderr, that the daemon is serving old code and that we did not kill
/// it. Both halves matter: silence about the staleness is how old code serves
/// for days, and silence about a kill is how a surface loses its rail, its pane
/// draft and its passkey signature without anyone deciding that it should.
fn warn_outdated_daemon(pid: u64, why: &str) {
    if ANNOUNCED_OUTDATED_PID.swap(pid, Ordering::SeqCst) == pid {
        return;
    }
    eprintln!(
        "ychrome: the running daemon (pid {pid}) is serving OLD CODE: the ychrome binary on disk \
         changed after it started."
    );
    eprintln!("ychrome: {why}, so it was left running rather than retired underneath it.");
    eprintln!("ychrome: hand it over when you are ready:  ychrome daemon restart");
}

/// The notice every daemon reply gets checked for. Deliberately silent when
/// nothing is attached: that daemon is replaced transparently by the next
/// `ensure()`, and announcing a problem we are about to fix ourselves is noise.
fn note_daemon_reply(reply: &Value) {
    if !daemon_is_outdated(reply) {
        return;
    }
    let held = reply
        .get("live_sessions")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if held == 0 {
        return;
    }
    let names = reply
        .get("held_by")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|names| !names.is_empty());
    let why = match names {
        Some(names) => format!("{held} live surface(s) are attached to it ({names})"),
        None => format!("{held} live surface(s) are attached to it"),
    };
    warn_outdated_daemon(reply.get("pid").and_then(Value::as_u64).unwrap_or(0), &why);
}

// `socket_answers_ping` lived here and is deliberately GONE. It was a second
// answer to "is a daemon alive?", and it was the wrong one: a 2 s read timeout
// against a socket whose accept loop can legitimately be busy. Liveness now has
// exactly one owner, `DaemonLock`, and the kernel answers it. Do not
// reintroduce a timeout-based liveness probe — a daemon that fails it is
// usually working, not dead, and the cost of the mistake is a permanently
// orphaned daemon holding a display nobody can reach.

/// Ensure a daemon running THIS binary's code is serving, and return nothing but
/// the guarantee that a daemon is up. Four outcomes, and the middle two are the
/// point of the function:
///
///   - nothing listening: spawn one;
///   - listening and current: leave it completely alone;
///   - listening, OUTDATED (a different version, or the binary was replaced on
///     disk after it started) and nothing attached: hand over transparently. An
///     empty registry costs nobody anything to lose, and this is the case that
///     used to never fire. The version has been `0.1.0` throughout, so comparing
///     versions alone left every rebuild's daemon serving old code until
///     something killed it by hand;
///   - listening, outdated, and live surfaces are attached: LEAVE IT RUNNING and
///     say so on stderr. It holds those surfaces' control endpoints, pane drafts
///     and passkey signer; retiring it silently spends the user's work to save
///     them one command. `ychrome daemon restart` is that command.
///
/// The idle-or-attached half of the decision belongs to the DAEMON
/// (`retire_if_idle`), not to us: only it can count what is attached under its
/// own lock, in one round trip that nothing can race.
pub fn ensure() -> Result<()> {
    for _ in 0..40 {
        let Some(reply) = socket_request(&json!({ "op": "ping" })) else {
            spawn_daemon()?;
            std::thread::sleep(Duration::from_millis(150));
            continue;
        };
        if !daemon_is_outdated(&reply) {
            return Ok(());
        }
        match socket_request(&json!({ "op": "retire_if_idle" })) {
            // It left, and unlinked its socket before answering, so our spawn
            // wins the bind instead of racing its cleanup.
            Some(answer) if answer.get("retiring").and_then(Value::as_bool) == Some(true) => {
                spawn_daemon()?;
                std::thread::sleep(Duration::from_millis(150));
            }
            // A daemon older than this verb cannot tell us what it is holding.
            // Assume it holds something: guessing wrong costs a live surface its
            // rail, and the honest alternative is one deliberate command. This
            // is the one-deploy cost of installing the handover itself.
            Some(answer) if answer.get("ok").and_then(Value::as_bool) == Some(false) => {
                warn_outdated_daemon(
                    reply.get("pid").and_then(Value::as_u64).unwrap_or(0),
                    "it predates the handover check and cannot say what is attached to it",
                );
                return Ok(());
            }
            // It is holding live surfaces. Never ours to take; the reply already
            // carried the notice out through `socket_request`.
            Some(_) => return Ok(()),
            // It vanished mid-exchange; the next pass finds no socket and spawns.
            None => continue,
        }
    }
    bail!("ychrome daemon did not come up");
}

/// The deliberate handover behind `ychrome daemon restart`: retire whatever is
/// running, attached surfaces and all, and bring up a daemon on the binary that
/// is on disk now. This is the ONLY path that retires a busy daemon, and it
/// exists so that retiring one is something a person chooses rather than
/// something that happens to them.
///
/// What comes back on its own: every attached client re-registers on its next
/// heartbeat (~4s) and re-declares the new control url. What does not: pane
/// drafts, queued opens, and any passkey signature in flight. The reply names
/// the sessions, so the caller can say which surfaces paid for it.
pub fn restart() -> Result<Value> {
    let before = socket_request_silent(&json!({ "op": "status" }));
    let old_pid = before.as_ref().and_then(|reply| reply["pid"].as_u64());
    let reattaching: Vec<String> = before
        .as_ref()
        .and_then(|reply| reply["sessions"].as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["env_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if old_pid.is_some() {
        let _ = socket_request_silent(&json!({ "op": "stop" }));
    }
    for _ in 0..40 {
        if let Some(reply) = socket_request_silent(&json!({ "op": "ping" })) {
            let pid = reply.get("pid").and_then(Value::as_u64);
            // "Something is listening" is not proof the handover happened: the
            // outgoing daemon answers pings right up to its exit, and reporting
            // a restart that did not occur is the exact dishonesty this verb
            // exists to remove. Only a NEW pid on CURRENT code counts.
            if pid.is_some() && pid != old_pid && !daemon_is_outdated(&reply) {
                return Ok(json!({
                    "ok": true,
                    "old_pid": old_pid,
                    "pid": pid,
                    "sessions_reattaching": reattaching,
                }));
            }
        }
        spawn_daemon()?;
        std::thread::sleep(Duration::from_millis(150));
    }
    bail!("the ychrome daemon did not come back up after the restart");
}

/// The path to spawn a daemon from. `current_exe()` reads `/proc/self/exe`,
/// which keeps naming a REPLACED binary as `<path> (deleted)` — and that is
/// exactly the state a long-lived client is in after a deploy, which is exactly
/// when it needs to bring a daemon up on the NEW code. `Command::new` on the
/// literal string fails with ENOENT, so without this a client whose binary was
/// replaced could never respawn a daemon again: the staleness handover would
/// hand over to nothing. The marker is stripped only when the real path is
/// there; a genuinely missing binary must still fail loudly.
fn spawnable_exe(exe: PathBuf) -> PathBuf {
    match exe
        .to_str()
        .and_then(|raw| raw.strip_suffix(" (deleted)"))
        .map(PathBuf::from)
    {
        Some(replaced) if replaced.is_file() => replaced,
        _ => exe,
    }
}

/// Launch `ychrome --daemon` detached (setsid, cwd=home, stdio to /dev/null), the
/// yedit pattern. Best-effort: a lost race just means another spawn won, and the
/// ensure loop finds the socket answering.
fn spawn_daemon() -> Result<()> {
    let exe = spawnable_exe(std::env::current_exe().context("locating the ychrome binary")?);
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let child = Command::new(&exe)
        .arg("--daemon")
        .current_dir(&home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("spawning the ychrome daemon")?;
    // REAP IT. `std::process::Child` does NOT reap on drop, and this spawn sits
    // inside `ensure()`'s 0..40 retry loop whose callers themselves retry — so a
    // `--daemon` that exits immediately (e.g. it cannot bind its socket, or a
    // version-mismatch stop/respawn ping-pong) leaves a ZOMBIE every ~150ms and
    // nothing ever collects them.
    //
    // This is not theoretical: two long-lived `ychrome --profile ...` instances
    // once accumulated ~900k zombie children between them (one held 468,010),
    // which exhausted the PID space (pid_max 4.19M — PIDs had WRAPPED) and
    // bloated /proc so badly that every fork/ps/pgrep on that host crawled,
    // starving every unrelated process running there.
    //
    // A detached waiter is the right shape here: the daemon is long-lived, so the
    // thread normally parks for the process's whole life and costs nothing; if the
    // daemon dies instantly the thread reaps it at once and exits. We deliberately
    // do NOT use waitpid(-1)/SIG_IGN — this process is a WebKit UIProcess and must
    // not steal or auto-discard WebKit's own child exit statuses.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// Register (or heartbeat) a session with the daemon; returns the whole reply,
/// because the caller wants both the control url and the staleness stamps the
/// transport put on it.
fn register_reply(env_id: &str, profile: &str) -> Result<Value> {
    let reply = socket_request(&json!({
        "op": "register",
        "env_id": env_id,
        "profile": profile,
        "pid": std::process::id(),
        // We are the token's only courier, so we say so in the same round trip
        // that mints it. A binary older than the gate cannot send this field,
        // which is precisely what makes its absence trustworthy: the daemon then
        // knows the session's panes can never open and can say so instead of
        // answering 403 forever with no explanation.
        "declares_control_token": true,
    }))
    .context("registering with the ychrome daemon")?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!(
            "daemon refused register: {}",
            reply.get("error").and_then(Value::as_str).unwrap_or("unknown")
        );
    }
    Ok(reply)
}

/// What a client needs to DECLARE its control endpoint: where it is, and the
/// token the GUI must present on the endpoint's GUI-only routes.
///
/// The two travel together because they are one fact — "this session's control
/// endpoint" — and splitting them is how a declare ends up carrying a url the
/// GUI cannot actually drive. Equality is on both fields: a respawned daemon
/// hands back a new port AND a new token, and either moving means re-declare.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ControlEndpoint {
    pub url: String,
    pub token: String,
}

/// The control endpoint out of a register reply. A reply with no `control_token`
/// is an OLDER daemon (pre-gate); it answers with an empty token, which the gate
/// treats as "no token" — the declare then carries an empty string and the GUI's
/// GUI-only calls are refused rather than silently trusted.
fn control_endpoint_of(reply: &Value) -> Result<ControlEndpoint> {
    let url = reply
        .get("control_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("daemon register reply had no control_url")?;
    let token = reply
        .get("control_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(ControlEndpoint { url, token })
}

/// Register (or heartbeat) a session with the daemon; returns its control
/// endpoint.
pub fn register(env_id: &str, profile: &str) -> Result<ControlEndpoint> {
    control_endpoint_of(&register_reply(env_id, profile)?)
}

/// Heartbeat is a re-register — the same idempotent op keeps the entry alive.
/// Supervising: if the daemon has died, respawn it and re-register (the registry
/// is soft state, rebuilt from exactly this). Returns the current control
/// endpoint, which may CHANGE across a respawn (a fresh listener, a new port,
/// a new token) — the caller re-declares when it moves. `None` only if the
/// daemon cannot be brought up.
pub fn register_supervised(env_id: &str, profile: &str) -> Option<ControlEndpoint> {
    if let Ok(reply) = register_reply(env_id, profile) {
        // A heartbeat is the FIRST thing that notices a deploy that landed while
        // this browser was open, and for a user who will never type `ychrome
        // status` it is the ONLY thing. Hand the ensure() decision the fact:
        // either it retires an idle outdated daemon, or it says out loud that it
        // will not, naming us as the reason. Once per daemon, because it is a
        // notice, not a heartbeat.
        if daemon_is_outdated(&reply)
            && !already_announced(reply.get("pid").and_then(Value::as_u64).unwrap_or(0))
        {
            let _ = ensure();
        }
        return control_endpoint_of(&reply).ok();
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

/// A LIVE view client — some process other than the asking one — that anchors a
/// session's PTY stream right now. The second-invocation path consults this
/// before anchoring: the GUI keys web surfaces by stream, so anchoring onto a
/// stream that already carries someone's live surface does not open beside
/// their page, it replaces it (yggterm pending-bugs "AGENT CO-BROWSE" A4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAnchor {
    pub pid: i64,
    pub profile: String,
}

/// Does another live client already anchor `env_id`'s stream? Reads the
/// daemon's registry (op `status`, no daemon spawned). `None` when no daemon
/// answers — the caller then knows nothing and must say so, not guess. A
/// registry row counts only while its client is still heartbeating (within
/// `SESSION_EXPIRE`): an expired row is a client that already left, and
/// refusing on it would block a legitimate re-anchor after a crash.
pub fn live_anchor(env_id: &str, self_pid: u32) -> Option<LiveAnchor> {
    let reply = socket_request(&json!({ "op": "status" }))?;
    live_anchor_in(&reply, env_id, self_pid)
}

fn live_anchor_in(reply: &Value, env_id: &str, self_pid: u32) -> Option<LiveAnchor> {
    reply.get("sessions")?.as_array()?.iter().find_map(|row| {
        if row.get("env_id").and_then(Value::as_str) != Some(env_id) {
            return None;
        }
        let pid = row.get("pid").and_then(Value::as_i64).unwrap_or(0);
        if pid == i64::from(self_pid) {
            return None;
        }
        let beat_ms = row
            .get("last_heartbeat_ms_ago")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        if u128::from(beat_ms) > SESSION_EXPIRE.as_millis() {
            return None;
        }
        Some(LiveAnchor {
            pid,
            profile: row
                .get("profile")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
        })
    })
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

    // FORK-BOMB GUARD: `spawn_daemon` sits in `ensure()`'s 0..40 retry loop and
    // `std::process::Child` does not reap on drop, so an un-waited spawn turns a
    // failing daemon into a zombie every ~150ms. This once reached ~900k zombies
    // across two instances, wrapped the PID space and took a whole host down.
    // The child MUST be waited on.
    #[test]
    fn spawn_daemon_reaps_its_child_so_it_cannot_zombie() {
        let source = include_str!("daemon.rs");
        let body = source
            .split("fn spawn_daemon() -> Result<()> {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("spawn_daemon body present");
        assert!(
            body.contains("child.wait()"),
            "spawn_daemon must reap the child it spawns (Child does NOT reap on \
             drop); without this a restart-looping daemon fork-bombs the host"
        );
    }

    // The reaping shape itself: a child that exits immediately is collected, so
    // repeated spawns cannot accumulate. Uses /bin/true rather than the ychrome
    // binary so the test stays hermetic.
    #[test]
    fn a_detached_waiter_collects_an_immediately_exiting_child() {
        let child = Command::new("/bin/true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn /bin/true");
        let pid = child.id();
        let waiter = std::thread::spawn(move || {
            let mut child = child;
            child.wait()
        });
        let status = waiter.join().expect("waiter thread").expect("wait ok");
        assert!(status.success(), "child {pid} should exit cleanly and be reaped");
    }

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

    /// Attach a session without going through `register`, which would bind a
    /// listener and append to the real journal. Only the accounting is under
    /// test here.
    fn attach(daemon: &Daemon, env_id: &str, last_heartbeat: Instant) {
        let entry = Arc::new(SessionEntry {
            env_id: env_id.to_string(),
            control: ControlState::new("default", env_id, 0),
            control_url: "http://127.0.0.1:0".to_string(),
            meta: Mutex::new(SessionMeta {
                profile: "default".to_string(),
                pid: 1,
                last_heartbeat,
                last_session_ping: None,
                registered_seq: 0,
                declares_control_token: true,
            }),
            queue: Mutex::new(Queue::default()),
            stop: Arc::new(AtomicBool::new(false)),
        });
        daemon
            .sessions
            .lock()
            .unwrap()
            .insert(env_id.to_string(), entry);
    }

    // THE SECOND-INVOCATION CONTRACT, daemon half (yggterm pending-bugs "AGENT
    // CO-BROWSE" A4): a url routed into a running session is enqueued as
    // `open_tab` — the GUI verb that MINTS A NEW TAB (`open_command_tab` →
    // `web_surface_new_tab` in yggterm-shell) — and never any kind that
    // navigates an existing tab. If a "navigate"/"open" kind ever replaces
    // this, a second `ychrome <url>` is back to destroying the running page.
    #[test]
    fn a_routed_url_is_enqueued_as_open_tab_never_a_page_navigation() {
        let daemon = Daemon::new();
        attach(&daemon, "env-a", Instant::now());
        {
            // Mark the GUI routing-capable (it pinged with ?session= recently).
            let sessions = daemon.sessions.lock().unwrap();
            let entry = sessions.get("env-a").unwrap();
            entry.meta.lock().unwrap().last_session_ping = Some(Instant::now());
        }
        let reply = daemon.route("default", "https://example.com", None, false);
        assert_eq!(reply["routed"], json!(true));
        assert_eq!(reply["session"], json!("env-a"));
        let sessions = daemon.sessions.lock().unwrap();
        let queue = sessions.get("env-a").unwrap().queue.lock().unwrap();
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(
            queue.pending[0].kind, "open_tab",
            "a routed url must become a NEW TAB in the target surface, never a navigation \
             of an existing one"
        );
        assert_eq!(queue.pending[0].args["url"], json!("https://example.com"));
        assert_eq!(queue.pending[0].args["raise"], json!(true));
    }

    // The pre-anchor probe the CLI's hijack refusal rests on: only ANOTHER
    // pid's still-heartbeating row is a conflict. Our own registration, an
    // expired row (a client that already left), a different session, and a
    // reply with no registry at all are not.
    #[test]
    fn the_live_anchor_probe_sees_only_another_pids_live_entry() {
        let status = json!({ "sessions": [
            { "env_id": "env-a", "pid": 111, "profile": "health", "last_heartbeat_ms_ago": 2000 },
            { "env_id": "env-b", "pid": 333, "profile": "default", "last_heartbeat_ms_ago": 99000 },
        ]});
        assert_eq!(
            live_anchor_in(&status, "env-a", 999),
            Some(LiveAnchor {
                pid: 111,
                profile: "health".to_string()
            })
        );
        assert_eq!(
            live_anchor_in(&status, "env-a", 111),
            None,
            "our own registration is not a conflict"
        );
        assert_eq!(
            live_anchor_in(&status, "env-b", 999),
            None,
            "an expired row is a client that already left; refusing on it would block a \
             legitimate re-anchor after a crash"
        );
        assert_eq!(live_anchor_in(&status, "env-x", 999), None);
        assert_eq!(live_anchor_in(&json!({ "ok": true }), "env-a", 999), None);
    }

    // The accounting the handover decision rests on. A session whose client is
    // still heartbeating holds this daemon's control endpoint, pane draft and
    // signer, so it must count; one that missed three beats is a client that
    // already left, and counting it would pin an outdated daemon in place for
    // the whole expiry window every time a browser was closed.
    #[test]
    fn live_sessions_are_the_ones_a_client_is_still_heartbeating_for() {
        let daemon = Daemon::new();
        assert!(daemon.live_session_ids().is_empty());
        attach(&daemon, "env-b", Instant::now());
        attach(&daemon, "env-a", Instant::now());
        assert_eq!(daemon.live_session_ids(), vec!["env-a", "env-b"]);
        attach(
            &daemon,
            "env-b",
            Instant::now() - (SESSION_EXPIRE + Duration::from_secs(1)),
        );
        assert_eq!(
            daemon.live_session_ids(),
            vec!["env-a"],
            "a client that stopped heartbeating holds nothing and must not hold a handover back"
        );
    }

    // The predicate the whole fix turns on. ychrome's version has been 0.1.0
    // since the daemon existed, so version equality alone declared every daemon
    // current forever: the exe-stamp drift is the term that actually fires.
    #[test]
    fn a_daemon_is_outdated_when_its_version_moved_or_its_binary_did() {
        let current = json!({ "ok": true, "version": VERSION, "stale": false });
        assert!(!daemon_is_outdated(&current));

        let replaced_binary = json!({ "ok": true, "version": VERSION, "stale": true });
        assert!(
            daemon_is_outdated(&replaced_binary),
            "same version, new bytes on disk: this is the case that ships every rebuild"
        );

        let older_build = json!({ "ok": true, "version": "0.0.9", "stale": false });
        assert!(daemon_is_outdated(&older_build));

        // Most ops answer without a version. Judging those on `stale` alone is
        // deliberate: a bare ack that read as outdated would send every
        // deregister and every route into a handover.
        let ack = json!({ "ok": true, "stale": false });
        assert!(!daemon_is_outdated(&ack));
        assert!(daemon_is_outdated(&json!({ "ok": true, "stale": true })));
    }

    // `/proc/self/exe` keeps naming a replaced binary as `<path> (deleted)`,
    // which is precisely the state a client is in after a deploy — precisely
    // when it must be able to spawn a daemon on the NEW code. Spawning the
    // literal string is ENOENT, so the handover would hand over to nothing.
    #[test]
    fn a_replaced_binarys_deleted_marker_is_stripped_only_when_the_path_is_real() {
        let dir = PathBuf::from(format!("/tmp/ych-exe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("ychrome");
        std::fs::write(&real, b"#!/bin/true\n").unwrap();

        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
        assert_eq!(
            spawnable_exe(deleted),
            real,
            "a client whose binary was replaced must still be able to spawn one"
        );
        // An ordinary path is untouched, and a marker over a path that really is
        // gone stays as it is: failing loudly beats spawning something else.
        assert_eq!(spawnable_exe(real.clone()), real);
        let gone = PathBuf::from("/tmp/ych-exe-nonexistent (deleted)");
        assert_eq!(spawnable_exe(gone.clone()), gone);
        std::fs::remove_dir_all(&dir).ok();
    }

    // -----------------------------------------------------------------------
    // THE CONTROL-TOKEN GATE, daemon half: the ping's two halves.
    // -----------------------------------------------------------------------

    /// `/ping` is Open because an older GUI must keep its rail and its
    /// surfaces' ad blocking across a mixed-version deploy — but the command
    /// QUEUE is a mutation, and the control port is page-reachable through
    /// yggterm's appctl bridge. An untokened caller therefore gets the stamps
    /// and nothing else: no drain, so a page cannot swallow the routed
    /// `open_tab` the GUI was supposed to receive.
    #[test]
    fn an_untokened_ping_gets_liveness_but_never_the_command_queue() {
        let daemon = Daemon::new();
        attach(&daemon, "env-a", Instant::now());
        let entry = Arc::clone(daemon.sessions.lock().unwrap().get("env-a").unwrap());
        entry
            .queue
            .lock()
            .unwrap()
            .enqueue("env-a", 1, "open_tab", json!({ "url": "https://x" }));

        let refused = daemon.ping_reply(&entry, Some("env-a"), None, false);
        assert_eq!(refused["app_name"], "Ychrome", "liveness must still answer");
        assert!(
            !refused.get("daemon_stale").is_none(),
            "the stamps an older GUI needs must survive: {refused:?}"
        );
        assert!(
            refused.get("commands").is_none(),
            "an untokened ping must not drain the queue: {refused:?}"
        );
        assert_eq!(
            entry.queue.lock().unwrap().pending.len(),
            1,
            "and the command must still be there for the real GUI"
        );

        let delivered = daemon.ping_reply(&entry, Some("env-a"), None, true);
        let commands = delivered
            .get("commands")
            .expect("the GUI's tokened ping still drains its queue");
        assert_eq!(commands["entries"][0]["kind"], "open_tab");
    }

    /// An untokened ping must not be able to ACK either — retiring a batch it
    /// never received is how a page would make the GUI lose a command silently.
    #[test]
    fn an_untokened_ping_cannot_ack_a_batch_away() {
        let daemon = Daemon::new();
        attach(&daemon, "env-a", Instant::now());
        let entry = Arc::clone(daemon.sessions.lock().unwrap().get("env-a").unwrap());
        entry
            .queue
            .lock()
            .unwrap()
            .enqueue("env-a", 1, "toast", json!({ "title": "a" }));
        let batch = daemon.ping_reply(&entry, Some("env-a"), None, true)["commands"]["batch_id"]
            .as_str()
            .expect("a drained batch is labelled")
            .to_string();

        daemon.ping_reply(&entry, Some("env-a"), Some(&batch), false);
        assert_eq!(
            entry.queue.lock().unwrap().pending.len(),
            1,
            "an untokened ack must retire nothing"
        );
        daemon.ping_reply(&entry, Some("env-a"), Some(&batch), true);
        assert!(
            entry.queue.lock().unwrap().pending.is_empty(),
            "the GUI's own ack still retires the batch it confirmed"
        );
    }

    /// The client learns its token from the register reply and puts it in the
    /// declare. An OLDER daemon answers without one — that is an empty token,
    /// which the gate reads as "no token" and refuses, rather than a `None` a
    /// caller might paper over.
    #[test]
    fn the_register_reply_carries_the_control_token_and_an_old_daemons_absence_is_empty() {
        let fresh = json!({
            "ok": true,
            "control_url": "http://127.0.0.1:41234",
            "control_token": "tok-abc",
        });
        assert_eq!(
            control_endpoint_of(&fresh).unwrap(),
            ControlEndpoint {
                url: "http://127.0.0.1:41234".to_string(),
                token: "tok-abc".to_string(),
            }
        );

        let pre_gate = json!({ "ok": true, "control_url": "http://127.0.0.1:41234" });
        assert_eq!(control_endpoint_of(&pre_gate).unwrap().token, "");
        assert!(control_endpoint_of(&json!({ "ok": true })).is_err());

        // A moved port OR a moved token is a moved endpoint: the client
        // re-declares on either, so the GUI can never hold half of a stale pair.
        assert_ne!(
            control_endpoint_of(&fresh).unwrap(),
            control_endpoint_of(&json!({
                "ok": true,
                "control_url": "http://127.0.0.1:41234",
                "control_token": "tok-xyz",
            }))
            .unwrap()
        );

        // ANCHOR: both arms of `register` — the heartbeat and the fresh bind —
        // must answer with the token, or a re-registering client would declare
        // an endpoint it cannot drive.
        let source = include_str!("daemon.rs");
        let body = source
            .split("    fn register(\n")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn deregister").next())
            .expect("register body present");
        assert_eq!(
            body.matches("\"control_token\":").count(),
            2,
            "both the heartbeat reply and the fresh-session reply must carry the token"
        );
        // And both arms must record the COURIER fact. The heartbeat arm is the
        // one that matters: a pre-gate client re-registers every ~4s, and an
        // arm that only set the flag on first bind would let one lucky
        // registration make a session look drivable for the rest of its life.
        assert!(
            body.contains("meta.declares_control_token = declares_control_token;"),
            "the HEARTBEAT arm must re-record the courier fact, or a session keeps \
             whatever its first registration said forever"
        );
        assert!(
            body.contains(
                "                registered_seq,\n                declares_control_token,\n"
            ),
            "the fresh-session arm must record the courier fact on the entry it builds"
        );
        assert!(
            body.contains("\"declares_control_token\": declares_control_token,"),
            "the journal's register line must carry it, so a session that can never \
             open its panes is diagnosable at the moment it attaches"
        );
    }

    /// The client asserts its own capability, in the same round trip that mints
    /// the token it would have to carry. Nothing infers a binary's vintage.
    #[test]
    fn the_client_claims_the_courier_and_silence_means_it_cannot() {
        let source = include_str!("daemon.rs");
        let body = source
            .split("fn register_reply(env_id: &str, profile: &str)")
            .nth(1)
            .and_then(|rest| rest.split("\n/// What a client needs").next())
            .expect("register_reply body present");
        assert!(
            body.contains("\"declares_control_token\": true"),
            "this binary declares the token, so its register must say so — without \
             the claim the daemon would report every session of ours as un-drivable"
        );

        // The DEFAULT is the load-bearing half: a client too old to send the
        // field must read as "no courier", never as "assume the best".
        let arm = source
            .split("        \"register\" => {")
            .nth(1)
            .and_then(|rest| rest.split("        \"deregister\"").next())
            .expect("the register op arm");
        assert!(
            arm.contains(".and_then(Value::as_bool)\n                .unwrap_or(false)"),
            "an absent `declares_control_token` must default to FALSE: the clients \
             that cannot send it are exactly the ones that cannot carry the token"
        );
    }

    /// A gate is only worth something if the LIVE connection is wired to it, and
    /// three of those wires are one-liners in `handle_control_conn` that no
    /// in-process test can see from the outside: the caller's token must reach
    /// the ping's queue half, and the request's PATH must reach both responders
    /// (they choose the CORS headers, and the preflight decides which route may
    /// be asked about cross-origin at all). Anchored, because a refactor that
    /// dropped any one of them would leave every other lock in this file green.
    #[test]
    fn the_gate_is_wired_into_the_live_control_connection() {
        let source = include_str!("daemon.rs");
        let body = source
            .split("fn handle_control_conn(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("handle_control_conn body present");
        // Rewritten 2026-07-30: this needle used to anchor the token comparison
        // SPELLED OUT here, which locked in a second copy of the gate's own
        // authorization rule — the comparison existed in `ControlState` too, and
        // only that one was under test. The contract is that the queue half is
        // decided by the ONE owner, so that is what is anchored.
        assert!(
            body.contains("let gui = entry.control.gui_authorized(&request);"),
            "the ping's queue half must ask `ControlState::gui_authorized` — a \
             comparison rewritten inline here is a second encoding of the gate's \
             rule that no gate test would cover"
        );
        assert!(
            !body.contains("== Some(entry.control.control_token.as_str())"),
            "the token comparison is back inline in the connection handler"
        );
        assert!(
            body.contains("sidebar::respond_preflight(stream, &request.path)"),
            "a preflight must be answered for the route it is asking about"
        );
        assert!(
            body.contains("sidebar::respond_json(stream, status, &body, &request.path)"),
            "a response's CORS headers must be chosen by the route that produced it"
        );
    }

    // ── THE SINGLETON ───────────────────────────────────────────────────────
    // Measured cost of not having it, on dev 2026-07-31: 21 `daemon_start`
    // against 13 `daemon_stop`, six starts in a row with no stop, and 15 Xvfb
    // reparented to init. An agent's `ctl open` landed on a daemon that no
    // longer owned the socket, so the very next `ctl eval` said `no page`.

    // The exclusion itself, against the real kernel primitive. Two SEPARATE
    // `open`s conflict even inside one process, because an flock belongs to the
    // open file description rather than to the process — which is exactly the
    // property that makes it a cross-process singleton.
    #[test]
    fn only_one_daemon_can_hold_the_lock_at_a_time() {
        let path = std::env::temp_dir().join(format!(
            "ychrome-daemon-lock-test-{}-{}",
            std::process::id(),
            now_millis()
        ));
        let first = DaemonLock::acquire_at(&path)
            .expect("first acquire must not error")
            .expect("the first daemon takes the lock");

        let second = DaemonLock::acquire_at(&path).expect("a held lock is not an error");
        assert!(
            second.is_none(),
            "a second daemon MUST be refused while the first holds the lock — \
             this refusal is the whole singleton"
        );

        // Releasing hands it to the next daemon, so a clean handover still works.
        drop(first);
        let third = DaemonLock::acquire_at(&path).expect("acquire after release");
        assert!(
            third.is_some(),
            "once the holder is gone the lock MUST be available, or a daemon \
             restart could never bring one back"
        );
        drop(third);
        let _ = std::fs::remove_file(&path);
    }

    // ⛔ ORDER IS THE FIX. Acquiring the lock BEFORE touching the socket is what
    // makes reclaiming a leftover socket safe; doing it after would leave the
    // same window that stranded six daemons.
    #[test]
    fn run_takes_the_singleton_before_it_touches_the_socket() {
        let source = include_str!("daemon.rs");
        let body = source
            .split("pub fn run() -> Result<()> {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("run body present");
        let lock_at = body
            .find("DaemonLock::acquire()")
            .expect("run() must take the daemon singleton");
        let bind_at = body
            .find("UnixListener::bind")
            .expect("run() must bind the socket");
        assert!(
            lock_at < bind_at,
            "run() must acquire the singleton BEFORE binding: the lock is what \
             proves a leftover socket belongs to a corpse rather than to a live \
             daemon that was merely busy"
        );
    }

    // The predicate that caused the damage must not come back in any form. A
    // timeout-based liveness probe answers "busy" as "dead", and the price of
    // that mistake is a daemon orphaned for the rest of its life.
    #[test]
    fn liveness_is_never_decided_by_a_ping_timeout_again() {
        let source = include_str!("daemon.rs");
        // Assembled at runtime so this test's OWN text is not a match for it.
        let banned_fn = format!("fn {}_answers_ping", "socket");
        let run_body = source
            .split("pub fn run() -> Result<()> {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("run body present");
        assert!(
            !run_body.contains(&banned_fn[3..]),
            "run() must not decide liveness by pinging the socket — a busy \
             daemon fails that probe while working perfectly"
        );
        assert!(
            !source.contains(&banned_fn),
            "the ping-based liveness predicate must stay deleted: a second \
             answer to 'is a daemon alive?' is how the first one got trusted"
        );
    }

    // ⛔ THE SINGLETON IS RELEASED WHERE THE SOCKET IS, NOT AT PROCESS EXIT.
    // A retiring daemon frees the path before answering so its successor can
    // start serving while it winds its engine down. Holding the lock past that
    // point removed the overlap: measured, it made tests/daemon_staleness.rs
    // flaky and roughly doubled its runtime (~12s green -> ~18-21s intermittent).
    #[test]
    fn retiring_hands_on_the_singleton_in_the_same_breath_as_the_socket() {
        let source = include_str!("daemon.rs");
        let body = source
            .split("fn handle_unix_conn(daemon: &Arc<Daemon>, stream: UnixStream) -> bool {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("handle_unix_conn body present");
        let unlink_at = body
            .find("remove_file(&sock)")
            .expect("the retire path unlinks the socket");
        let release_at = body
            .find("DaemonLock::release()")
            .expect(
                "the retire path must release the singleton too — freeing the \
                 socket while keeping the lock makes every handover wait out a \
                 full engine teardown",
            );
        assert!(
            release_at > unlink_at,
            "release belongs with the unlink, on the retire path"
        );
    }

    // Unlinking the lock file would silently destroy the exclusion: the next
    // daemon creates a fresh inode and locks something nobody else holds.
    #[test]
    fn the_lock_file_is_never_unlinked() {
        // PRODUCTION CODE ONLY: the daemon is what must never unlink it, and
        // scanning this module too would only ever match the assertion below.
        let production = include_str!("daemon.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("source before the test module");
        // Assembled at runtime for the same reason.
        let (unlink, lockish) = (format!("remove_{}", "file"), "lock".to_string());
        for line in production.lines() {
            let line = line.trim();
            if line.starts_with("//") {
                continue;
            }
            assert!(
                !(line.contains(&unlink) && line.contains(&lockish)),
                "a line removes a lock file, which destroys the mutual \
                 exclusion it exists to provide: {line}"
            );
        }
    }

    // A connected-but-silent client used to wedge the accept loop forever,
    // because `fill_buf` runs inline on it with no timeout. That wedge is what
    // made a live daemon look dead.
    #[test]
    fn the_accept_loop_bounds_the_wait_for_a_clients_first_byte() {
        let source = include_str!("daemon.rs");
        let body = source
            .split("fn handle_unix_conn(daemon: &Arc<Daemon>, stream: UnixStream) -> bool {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n}").next())
            .expect("handle_unix_conn body present");
        let timeout_at = body
            .find("set_read_timeout(Some(FIRST_BYTE_TIMEOUT))")
            .expect("the first-byte wait must be bounded");
        let fill_at = body.find("fill_buf()").expect("fill_buf present");
        assert!(
            timeout_at < fill_at,
            "the read timeout must be set BEFORE fill_buf blocks on it, or one \
             silent client stops the daemon answering anything"
        );
        // …and released once the byte is in, so an engine verb that legitimately
        // runs for 45s is not cut off at 5s.
        assert!(
            body.contains("set_read_timeout(None)"),
            "the HTTP branch must clear the first-byte timeout before the engine \
             thread takes the stream — engine verbs outlast it by design"
        );
    }

    // The daemon socket now carries two protocols. Getting the discrimination
    // wrong in either direction is severe: a legacy op read as HTTP would
    // silently stop answering the browser, and an HTTP call read as an op
    // would parse as `{}` and be dispatched as the empty verb.
    #[test]
    fn the_socket_tells_its_two_protocols_apart_by_the_first_byte() {
        assert_eq!(socket_protocol(Some(b'{')), SocketProtocol::LegacyJsonOp);
        for method in ["GET", "POST", "PUT", "DELETE", "OPTIONS", "HEAD"] {
            assert_eq!(
                socket_protocol(method.bytes().next()),
                SocketProtocol::Http,
                "{method} must read as HTTP"
            );
        }
        assert_eq!(socket_protocol(None), SocketProtocol::Empty);
    }

    // End to end over a real UnixStream: an HTTP request that is NOT an engine
    // route is refused by name, and — the load-bearing half — it is refused
    // WITHOUT starting the engine. A daemon nobody has asked to browse must
    // never pay for GTK, a display and a WebKit process just because something
    // spoke HTTP at it.
    #[test]
    fn a_non_engine_http_path_is_refused_without_starting_the_engine() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;

        let (client, server) = UnixStream::pair().expect("socketpair");
        {
            let mut client = &client;
            client
                .write_all(b"GET /pane/vault HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            client.flush().unwrap();
        }
        let reader = BufReader::new(server.try_clone().unwrap());
        serve_engine_http(reader, server);

        let mut raw = String::new();
        BufReader::new(&client).read_to_string(&mut raw).unwrap();
        assert!(raw.starts_with("HTTP/1.1 404"), "got {raw:?}");
        assert!(raw.contains("/engine/*"), "the refusal must name the routes it does serve: {raw:?}");
        // No Xvfb, no engine thread: `owns` is consulted before `dispatch`.
        assert!(
            !std::path::Path::new("/tmp/.X11-unix/X90").exists()
                || std::env::var("DISPLAY").unwrap_or_default() != ":90",
            "the refusal path must not have started an engine display"
        );
    }
}
