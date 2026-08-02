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
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::matching::{auto_match_for_host, find_by_name};
use crate::model::FieldValue;
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

/// How many accepts may fail BACK TO BACK before the agent stops trying.
///
/// `incoming()` never ends, so a listener that cannot accept — an fd adopted
/// from the wrong object, a process out of descriptors — fails instantly and
/// forever. A bare `continue` there is a hot loop that burns a core and serves
/// nobody. A success resets the count, so this bounds a PERSISTENT failure only.
const MAX_CONSECUTIVE_ACCEPT_FAILURES: u32 = 10;

/// The pause after the nth consecutive accept failure, growing to a ceiling.
const ACCEPT_BACKOFF_STEP: Duration = Duration::from_millis(50);
const ACCEPT_BACKOFF_CAP: Duration = Duration::from_millis(250);

/// Where the agent records every release of a card secret, in the vault's own
/// `0700` directory and `0600` like the socket. One JSON object per line.
///
/// This is a TRAIL, not a gate. Nothing consults it and nothing is refused
/// because of it — see `card-secret` for the boundary, which is the unlock.
const AUDIT_LOG: &str = "audit.log";

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
///
/// Public because [`arm_handover`] is: the near side of the crossing has exactly
/// one owner, and the test that covers the crossing has to go THROUGH it rather
/// than hand-roll a second copy of the fd plumbing. It did, and that is how the
/// listener half of the CLOEXEC clear went uncovered.
pub struct ExecPlan {
    pub exe: PathBuf,
    pub dir: PathBuf,
    pub listener_fd: RawFd,
    pub payload: OwnedFd,
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
    let listener = bind_socket(dir)?;
    serve_on(dir, listener, None)
}

/// Bind `dir/agent.sock`, reclaiming it from an agent that is provably dead.
fn bind_socket(dir: &Path) -> Result<UnixListener> {
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
    Ok(listener)
}

/// Serve on a listener and a session INHERITED from the agent this process just
/// replaced — the far side of the `handover` op's `execve`.
///
/// Same pid, same bound socket, new code. Because the listener fd survived the
/// exec, there is no unbind/rebind window at all: a client that connects during
/// the swap is queued in the socket's backlog and answered by the successor,
/// rather than told "no vault agent".
///
/// Every failure below is loud and none is fatal, because the alternative to a
/// degraded agent is NO agent: an fd that is not what its flag promises costs
/// the seamless socket, and a payload that will not read costs the unlock, but
/// neither costs both and neither takes the process down.
pub fn serve_adopted(dir: &Path, listener_fd: RawFd, payload_fd: RawFd) -> Result<()> {
    // `from_raw_fd` validates NOTHING, and both ways a number off argv can be
    // wrong are fatal. If it is no longer open — the CLOEXEC clear regressed, or
    // someone typed the flags by hand — the Rust runtime ABORTS this process
    // ("IO Safety violation", SIGABRT) before any of our code runs. If instead
    // it was REALLOCATED by this process's own startup, the call succeeds on the
    // wrong object and every accept on it fails forever. So each number is
    // checked against what it must BE before anything takes ownership of it.
    let payload = check_adopted_fd(payload_fd, AdoptedFd::Payload)
        // Ownership (and the close) transfers here, on the checked path only: an
        // fd that is not ours to own must not be closed on the way out.
        .and_then(|()| read_payload(payload_fd));

    match check_adopted_fd(listener_fd, AdoptedFd::Listener) {
        // SAFETY: checked above to be a live, listening AF_UNIX stream socket —
        // which, in a process whose image was just replaced by `exec_successor`,
        // is the listener it inherited. Owning it here is what closes it, and it
        // lives as long as the agent.
        Ok(()) => serve_on(
            dir,
            unsafe { UnixListener::from_raw_fd(listener_fd) },
            Some(payload),
        ),
        Err(error) => {
            // Loud, and NOT fatal — the same rule `serve_on` applies to a bad
            // payload. Exiting would leave the user with no agent at all, and
            // the SESSION is a separate thing from the socket: what a lost
            // listener costs is the seamless swap (a client connecting in this
            // window is told there is no agent), not necessarily the unlock. So
            // bind a fresh socket and carry whatever the payload gave us.
            eprintln!(
                "ychrome-vault: the inherited listener is unusable ({error}) — binding a \
                 fresh socket instead; a client that connected mid-swap saw one refusal"
            );
            serve_on(dir, bind_socket(dir)?, Some(payload))
        }
    }
}

/// The two fds a successor inherits, and what each must be for the handover to
/// be real rather than a number that happens to parse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AdoptedFd {
    Listener,
    Payload,
}

impl AdoptedFd {
    fn flag(self) -> &'static str {
        match self {
            Self::Listener => "--adopt-listener",
            Self::Payload => "--adopt-payload",
        }
    }

    fn expected(self) -> &'static str {
        match self {
            Self::Listener => "a listening AF_UNIX stream socket",
            Self::Payload => "a pipe",
        }
    }

    fn matches(self, fd: RawFd) -> bool {
        match self {
            // SO_ACCEPTCONN is the load-bearing one: a socket that is not
            // LISTENING cannot be the socket this agent serves, however
            // socket-shaped it looks. Together these three also make the two
            // kinds mutually exclusive, so `--adopt-listener N --adopt-payload N`
            // cannot pass both checks and double-own one fd.
            Self::Listener => {
                socket_option(fd, libc::SO_DOMAIN) == Some(libc::AF_UNIX)
                    && socket_option(fd, libc::SO_TYPE) == Some(libc::SOCK_STREAM)
                    && socket_option(fd, libc::SO_ACCEPTCONN) == Some(1)
            }
            Self::Payload => fd_is_pipe(fd),
        }
    }
}

/// Refuse an inherited fd that is not open, or is open onto the wrong kind of
/// object. Cheap, and it turns two silent deaths into one sentence.
fn check_adopted_fd(fd: RawFd, kind: AdoptedFd) -> Result<()> {
    // SAFETY: F_GETFD only reads the descriptor flags; on a number that is not
    // open it returns -1/EBADF rather than doing anything.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("{} {fd} is not an open file descriptor", kind.flag()));
    }
    if !kind.matches(fd) {
        bail!(
            "{} {fd} is not {} — refusing to adopt it",
            kind.flag(),
            kind.expected()
        );
    }
    Ok(())
}

/// One integer `SOL_SOCKET` option, or `None` if the fd is not a socket.
fn socket_option(fd: RawFd, name: libc::c_int) -> Option<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `value` and `len` are live locals of exactly the type and size
    // getsockopt is told to write; a non-socket fd returns -1/ENOTSOCK.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            name,
            (&raw mut value).cast::<libc::c_void>(),
            &mut len,
        )
    };
    (rc == 0).then_some(value)
}

fn fd_is_pipe(fd: RawFd) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes one `libc::stat` into the buffer we just reserved,
    // and only on success (rc == 0) is it read back.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: fstat returned 0, so the buffer is initialised.
    let stat = unsafe { stat.assume_init() };
    (stat.st_mode & libc::S_IFMT) == libc::S_IFIFO
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
    accept_loop(listener, state)
}

/// Serve every connection the listener hands over, until it stops handing any
/// over at all.
///
/// The error arm is the whole point of splitting this out. `incoming()` is
/// infinite and an unusable listener fails it INSTANTLY, so a bare `continue`
/// there is an unbounded hot loop: a core at 100% and not one request served.
/// Back off, then give up loudly — an agent that exits with a reason is strictly
/// better than one that spins in silence, and the unlock behind it was already
/// unreachable the moment nothing could connect.
fn accept_loop(listener: UnixListener, state: Arc<Mutex<AgentState>>) -> Result<()> {
    let mut consecutive: u32 = 0;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                consecutive = 0;
                let state = state.clone();
                std::thread::spawn(move || serve_connection(stream, &state));
            }
            Err(error) => {
                consecutive += 1;
                eprintln!(
                    "ychrome-vault: accept failed \
                     ({consecutive}/{MAX_CONSECUTIVE_ACCEPT_FAILURES}): {error}"
                );
                if consecutive >= MAX_CONSECUTIVE_ACCEPT_FAILURES {
                    bail!(
                        "accept failed {consecutive} times in a row ({error}) — this listener \
                         is unusable, so the agent is exiting rather than spinning on it"
                    );
                }
                std::thread::sleep(accept_backoff(consecutive));
            }
        }
    }
    Ok(())
}

/// How long to wait after `consecutive` accept failures. Never zero: a zero here
/// is the hot loop this exists to prevent.
fn accept_backoff(consecutive: u32) -> Duration {
    (ACCEPT_BACKOFF_STEP * consecutive).min(ACCEPT_BACKOFF_CAP)
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

/// A value argument to `edit`, WITH THE EMPTY STRING PRESERVED.
///
/// ⛔ ABSENT IS NOT EMPTY, ON THE WRITE SIDE TOO. `dispatch`'s `string()` helper
/// drops an empty value, which quietly turned `--notes ""` into "no change
/// requested": the documented refusal ("refusing to set a field to the empty
/// string") was unreachable from the CLI, and the user got "edit needs at least
/// one field to change" — a sentence about a different problem, for a request
/// that named a field perfectly clearly. The empty string has to survive the
/// wire so that [`crate::model::Vault::edit_body`] is the one thing that
/// answers for it. This is the same absent-vs-empty rule `required_field`
/// enforces on the read side; it simply had no twin here.
fn edit_value(request: &Value, key: &str) -> Option<String> {
    request.get(key).and_then(Value::as_str).map(str::to_string)
}

/// A repeated string argument. Absent or null is an empty list; empty strings
/// are dropped so a client sending `[""]` gets the same refusal as `--uri ""`
/// rather than a silently shorter list.
fn string_list(request: &Value, key: &str) -> Vec<String> {
    request
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Decode `clear: ["notes", …]`.
///
/// ⛔ AN UNRECOGNISED NAME IS AN ERROR, NEVER A SKIP. Silently dropping a clear
/// this build does not know would report a successful edit that did not remove
/// what the caller asked to remove — and the caller here can be a NEWER CLI
/// talking to an older agent, which is the exact direction the stale-agent trap
/// runs in.
fn clear_fields(request: &Value) -> Result<std::collections::BTreeSet<crate::model::ClearField>> {
    let mut clear = std::collections::BTreeSet::new();
    for name in string_list(request, "clear") {
        let field = crate::model::ClearField::parse(&name).ok_or_else(|| {
            anyhow!(
                "this vault agent does not know how to clear {name:?} — \
                 run `ychrome-vault handover` if your CLI is newer than it"
            )
        })?;
        clear.insert(field);
    }
    Ok(clear)
}

/// Decode the custom-field changes: `fields: [{name, action, value?}]`.
///
/// `value` is a SECRET for a hidden field, so it is never echoed back, never
/// logged, and never appears in an error — the refusals below name the field
/// and the action only.
fn field_edits(request: &Value) -> Result<Vec<crate::model::FieldEdit>> {
    use crate::model::{FieldEdit, FieldKind};

    let mut edits = Vec::new();
    for change in request
        .get("fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let name = change
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("a custom-field change needs a name"))?
            .to_string();
        let action = change
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("set");
        let value = || -> Result<String> {
            change
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow!("setting custom field {name:?} needs a value"))
        };
        edits.push(match action {
            "set" => FieldEdit::Set {
                value: value()?,
                name,
                kind: FieldKind::Text,
            },
            "set-hidden" => FieldEdit::Set {
                value: value()?,
                name,
                kind: FieldKind::Hidden,
            },
            "remove" => FieldEdit::Remove { name },
            other => bail!(
                "unknown custom-field action {other:?} (set | set-hidden | remove) — \
                 run `ychrome-vault handover` if your CLI is newer than this agent"
            ),
        });
    }
    Ok(edits)
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
            let plan = arm_handover(
                successor.clone(),
                state.dir.clone(),
                listener_fd,
                &payload.encode(),
            )?;
            let stamp = ychrome_vault_proto::exe_stamp_of(&successor);
            state.handover = Some(plan);
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
                // CHEAP FIRST, then the decrypting pass. Name, username and uris
                // are already decrypted in `VaultItem`, so the overwhelming
                // majority of matches never touch the raw record; only an item
                // that fails all three pays for its notes and field names to be
                // decrypted.
                //
                // Searching name and username ALONE was the old behaviour, and
                // it is far narrower than any other client: an entry whose notes
                // say which of four accounts it is, or whose custom field is
                // named "Recovery Code", was unfindable by the words the user
                // remembered. See `Vault::deep_search_match` for what is
                // deliberately still not searched.
                items.retain(|item| {
                    item.name.to_lowercase().contains(query)
                        || item
                            .username
                            .as_deref()
                            .is_some_and(|user| user.to_lowercase().contains(query))
                        || item
                            .uris
                            .iter()
                            .any(|uri| uri.to_lowercase().contains(query))
                        || vault.deep_search_match(&item.id, query)
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
        // Returns the whole entry and lets the CALLER decide which field it
        // needed. It used to refuse outright when the item had no password,
        // which made `get --field username` unreadable for every password-less
        // login — the error named a field the caller had not asked for. That
        // cost real trust once: an agent reading a PAN out of the vault hit
        // "has no password", fell back to `list`, and picked the wrong row.
        // A missing password is now `null` here; the two consumers that need
        // one (the sidebar's fill paths) refuse it themselves, and the CLI's
        // `required_field` still says "has no password" for `--field password`.
        "get" => {
            let name = string("name").ok_or_else(|| anyhow!("get needs a name"))?;
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let entry = json!({
                "id": item.id,
                "name": item.name,
                "username": item.username,
                "password": vault.password(&item.id),
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
                // A bare `value: null` cannot say WHY there is nothing to show,
                // and the two reasons send the reader to different places, so
                // the wire carries the one the vault actually determined.
                .map(|(name, value)| match value {
                    FieldValue::Value(value) => json!({ "name": name, "value": value }),
                    FieldValue::Linked => {
                        json!({ "name": name, "value": null, "absent": "linked" })
                    }
                    FieldValue::Unreadable => {
                        json!({ "name": name, "value": null, "absent": "unreadable" })
                    }
                })
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
        // asks for. Two callers: the sidebar's fill injector, and yggterm's
        // `web fill-card` verb. Both put the value into a form field and drop
        // it; neither prints it.
        //
        // **The unlock is the boundary, and it is the only one.** Settled by the
        // user, 2026-07-26: every Bitwarden client — including third-party ones
        // — can read a card cipher and fill it, `ychrome-vault` is a Bitwarden
        // client, and it does the same. An extra per-use consent here would be a
        // second gate that only this client imposes, on a socket that already
        // hands out every password one `get` at a time. So an unlocked vault
        // serves this op to whoever can reach the socket, and a locked one
        // refuses with the remedy (`unlocked` below names `ychrome-vault
        // unlock`) — that refusal is the whole policy.
        //
        // What is still defended is the TRANSCRIPT: there is deliberately no
        // `ychrome-vault` CLI verb for a PAN, because a number printed to a
        // terminal is durable — scrollback, shell history, an agent CLI's JSONL
        // — and unlike a password it cannot be rotated on demand. The value goes
        // socket → injector → form field and nowhere else.
        //
        // Every release appends ONE audit line (`audit.log`), naming the item,
        // where it was going and which FIELDS were released. Never a value.
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
            // WHICH fields this release actually carried, read off the SAME
            // values the reply is built from — an audit line that named a field
            // the caller did not get would be worse than none.
            let released: Vec<&str> = [
                ("number", secret.number.is_some()),
                ("code", secret.code.is_some()),
                ("cardholder", card.cardholder.is_some()),
                ("exp_month", card.exp_month.is_some()),
                ("exp_year", card.exp_year.is_some()),
            ]
            .into_iter()
            .filter(|(_, present)| *present)
            .map(|(field, _)| field)
            .collect();
            let line = card_audit_line(
                &iso8601_now(),
                &item.name,
                string("host").as_deref(),
                string("client").as_deref(),
                &released,
            );
            append_audit(&state.dir, &line);
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
        // What the KERNEL says about this host's clock. Read-only, needs no
        // unlock: an operator asking "can this host mint a code at all" must be
        // able to find out before paying for an unlock.
        "clock" => Ok(crate::clock::state().to_json()),
        "totp" => {
            let name = string("name").ok_or_else(|| anyhow!("totp needs a name"))?;
            // The waiver travels in the REQUEST, not in an environment
            // variable: the mint happens inside this long-lived agent, whose
            // environment was frozen at launch and is not the caller's.
            let ignore_clock = request
                .get("ignore_clock")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let no_secret = || anyhow!("{} has no authenticator secret", item.name);
            let (code, remaining) = if ignore_clock {
                vault
                    .totp_code_ignoring_clock(&item.id)
                    .ok_or_else(no_secret)?
            } else {
                vault
                    .totp_code(&item.id)
                    .ok_or_else(no_secret)?
                    .map_err(|untrusted| anyhow!("{untrusted}"))?
            };
            let name = item.name.clone();
            state.touch();
            Ok(json!({
                "code": code,
                "remaining_secs": remaining,
                "name": name,
                // Reported on SUCCESS too, so a caller can see what the code was
                // minted against rather than only hearing about it on refusal.
                "clock": crate::clock::state().to_json(),
                "clock_ignored": ignore_clock,
            }))
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
        // Which hosts this vault holds a passkey for — nothing else.
        //
        // ⛔ SECRET-FREE BY CONSTRUCTION, and it must stay that way. The browser
        // calls this to decide WHERE to install its WebAuthn shim, so the answer
        // travels to a process that then hands match patterns to a webview. An
        // rpId is a public hostname; a credentialId is not, and does not belong
        // in this reply however convenient it would be for a future caller.
        //
        // A LOCKED vault errors here, and that is CORRECT rather than a
        // regression: the browser then installs no shim at all, which is the
        // honest state — a passkey ceremony needs an unlocked agent anyway.
        "passkey-hosts" => {
            let vault = unlocked(&state)?;
            let rp_ids = vault.passkey_rp_ids();
            state.touch();
            Ok(json!({ "rp_ids": rp_ids }))
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
            // `edit_value`, not `string`: an empty value must reach `edit_body`
            // to be REFUSED there, rather than vanishing into "you named no
            // fields". See `edit_value`.
            let folder_id = match edit_value(request, "folder") {
                Some(folder) => Some(
                    unlocked(&state)?
                        .folder_id(&folder)
                        .ok_or_else(|| anyhow!("no vault folder named {folder:?}"))?,
                ),
                None => None,
            };
            let edit = crate::model::CipherEdit {
                name: edit_value(request, "rename"),
                username: edit_value(request, "set_user"),
                password: password.clone(),
                totp: edit_value(request, "totp"),
                uris: string_list(request, "uris"),
                notes: edit_value(request, "notes"),
                folder_id,
                fields: field_edits(request)?,
                clear: clear_fields(request)?,
            };
            if edit.is_empty() {
                bail!("edit needs at least one field to change");
            }
            let vault = unlocked(&state)?;
            let items = vault.items();
            let item = resolve(&items, &name, string("user").as_deref())?;
            let (id, name) = (item.id.clone(), item.name.clone());
            let verification = state
                .manager
                .edit_item(&id, &edit)
                .map_err(|error| anyhow!(error.to_string()))?;
            state.touch();
            Ok(json!({
                "id": id,
                "name": name,
                "generated_password": generate.then_some(password).flatten(),
                // The RECEIPT: which changes a re-read of the freshly synced
                // item actually found. Field labels only, never values. A client
                // that gets no `verified` key is talking to an agent too old to
                // check, which is a different and worse answer than an empty list.
                "verified": verification.landed,
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

/// Prepare the crossing: put the session on a pipe and make BOTH fds survive the
/// `execve` that is about to replace this process.
///
/// The one owner of that preparation, and public for exactly that reason. The
/// listener's CLOEXEC clear is the guard the whole design rests on — it is why
/// there is no unbind window, and why a client connecting mid-swap is queued in
/// the backlog rather than told "no vault agent". Nothing else may clear it, and
/// nothing else may build an [`ExecPlan`].
pub fn arm_handover(
    exe: PathBuf,
    dir: PathBuf,
    listener_fd: RawFd,
    payload: &[u8],
) -> Result<ExecPlan> {
    let pipe = payload_pipe(payload)?;
    clear_cloexec(listener_fd)?;
    Ok(ExecPlan {
        exe,
        dir,
        listener_fd,
        payload: pipe,
    })
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

/// One audit record for a card secret leaving the vault.
///
/// PURE, so the property that matters — it names FIELDS and never carries a
/// value — is provable without a filesystem, and the writer below is left with
/// nothing to decide.
///
/// The two halves of the line are not equally trustworthy and the field names
/// say so. `item` and `fields` are what the VAULT determined: the resolved item
/// and the sub-fields that were actually released. `host` and `client` are what
/// the CALLER said about itself; this socket cannot verify either on a
/// single-uid host, exactly as it cannot verify who is asking for a password.
/// A trail worth having records both and confuses neither for the other.
fn card_audit_line(
    at: &str,
    item: &str,
    host: Option<&str>,
    client: Option<&str>,
    fields: &[&str],
) -> Value {
    json!({
        "at": at,
        "op": "card-secret",
        // Determined here.
        "item": item,
        "fields": fields,
        // Claimed by the caller.
        "host": host,
        "client": client,
    })
}

/// Append one line to the vault's audit log. Best effort, and loud when it
/// fails.
///
/// Best effort ON PURPOSE. The user's ruling is that the unlock is the only
/// hassle this path may impose; refusing to fill a payment form because a log
/// file could not be written would be a second one, arriving at the worst
/// possible moment. A failure goes to stderr, where the agent's other
/// operational failures already go.
fn append_audit(dir: &Path, line: &Value) {
    let path = dir.join(AUDIT_LOG);
    let write = || -> std::io::Result<()> {
        // `mode` applies at creation only, which is enough: the directory
        // around it is already 0700, so this narrows the file rather than
        // carrying the whole defence.
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        writeln!(file, "{line}")
    };
    if let Err(error) = write() {
        eprintln!(
            "ychrome-vault: could not append to {}: {error}",
            path.display()
        );
    }
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
    // WHEN the ciphers were last pulled. Reported as an epoch second and not as
    // "3 hours ago": the age is a presentation of this fact, and a client that
    // renders it must not be able to disagree with the vault about what the
    // fact IS. `null` when locked — a locked vault holds nothing to be stale.
    status["last_sync_unix"] = match manager.last_sync_unix() {
        Some(at) => json!(at),
        None => Value::Null,
    };
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

    /// The card fixture's number and CVV.
    ///
    /// OBVIOUSLY FAKE and deliberately so: `4111…` is the reserved Visa test
    /// prefix and `…4242` the Stripe test tail, a pair no issuer ever assigns.
    /// Nothing in this repository — fixture, assertion, log, doc or report —
    /// may carry a real PAN, and these constants exist so the no-leak
    /// assertions below have ONE needle to point at rather than a literal
    /// copied into four places.
    const FIXTURE_PAN: &str = "4111111111114242";
    const FIXTURE_CVV: &str = "737";

    /// A genuinely sealed two-item vault: one login on github.com with a TOTP
    /// secret, one on a base domain. No network, no server, no password — the
    /// user key is handed straight in.
    fn synthetic_state() -> Arc<Mutex<AgentState>> {
        synthetic_state_tagged("synthetic")
    }

    /// As [`synthetic_state`], but in a directory of its own — for the tests
    /// that assert on what the agent WROTE there, which cannot share a
    /// directory with the tests running beside them.
    fn synthetic_state_tagged(tag: &str) -> Arc<Mutex<AgentState>> {
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
            // A LOGIN carrying a username and NO password — the shape a vault
            // takes when the useful value IS the identifier (a customer number,
            // an identity number) and there is nothing to sign in with. Not a
            // card, so the card reads do not cover it.
            RawCipher {
                id: "un".to_string(),
                item_type: 1,
                name: enc("idnumber.example"),
                username: enc("111122223333"),
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
                        "number": seal(&key_bytes, FIXTURE_PAN.as_bytes()).to_string(),
                        "expMonth": seal(&key_bytes, b"11").to_string(),
                        "expYear": seal(&key_bytes, b"2029").to_string(),
                        "code": seal(&key_bytes, FIXTURE_CVV.as_bytes()).to_string(),
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
        let dir = temp_dir(tag);
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
        assert_eq!(items.len(), 4, "three logins (one password-less) and a card");
        assert_eq!(items[0]["name"], "GitHub", "sorted by lowercased name");
        assert!(items[0]["has_totp"].as_bool().unwrap());
        // Metadata must never carry the secret itself.
        assert!(items[0].get("password").is_none());

        let query = dispatch(&json!({"op": "list", "query": "YGG"}), &state).unwrap();
        assert_eq!(query["items"].as_array().unwrap().len(), 1);

        let got = dispatch(&json!({"op": "get", "name": "github"}), &state).unwrap();
        assert_eq!(got["entry"]["password"], "s3cret!");
        assert_eq!(got["entry"]["username"], "octocat");

        // The mint goes through the clock gate, and says what it minted
        // against. Counting the question is the only way to see it: on a
        // healthy host the digits are identical either way.
        let asked = crate::clock::judgements();
        let totp = dispatch(&json!({"op": "totp", "name": "GitHub"}), &state).unwrap();
        assert_eq!(totp["code"].as_str().unwrap().len(), 6);
        assert_eq!(
            crate::clock::judgements(),
            asked + 1,
            "the totp op minted without asking whether this host's clock is fit"
        );
        assert_eq!(totp["clock"]["source"], "adjtimex");
        assert_eq!(totp["clock_ignored"], false);

        // `--ignore-clock` really skips the question and SAYS it did — a waiver
        // that looked like the ordinary answer would be its own lie.
        let asked = crate::clock::judgements();
        let waived = dispatch(
            &json!({"op": "totp", "name": "GitHub", "ignore_clock": true}),
            &state,
        )
        .unwrap();
        assert_eq!(waived["code"].as_str().unwrap().len(), 6);
        assert_eq!(
            crate::clock::judgements(),
            asked,
            "the waiver must skip the gate, not ask and discard the answer"
        );
        assert_eq!(waived["clock_ignored"], true);

        assert!(dispatch(&json!({"op": "totp", "name": "ygg.example"}), &state).is_err());

        // The clock op answers without an unlock and from the same owner as the
        // field the mint reports.
        let clock = dispatch(&json!({"op": "clock"}), &state).unwrap();
        assert_eq!(clock["source"], totp["clock"]["source"]);
        assert!(clock["sync"].is_string());

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

    /// A username-only login must answer `get`, because the CLI reads
    /// `--field username` out of that same reply.
    ///
    /// The regression this pins: `get` used to demand a password before it
    /// looked at anything else, so every password-less item was unreadable and
    /// the error named a field the caller had not asked for. An agent hunting a
    /// PAN hit that wall, fell back to matching rows out of `list`, and picked
    /// a DIFFERENT PERSON'S row — which very nearly went into a legal filing.
    /// A refusal that misnames its own cause is how a caller ends up somewhere
    /// worse than a clean failure.
    #[test]
    fn get_answers_for_a_login_that_has_no_password() {
        let state = synthetic_state();

        let got = dispatch(&json!({"op": "get", "name": "idnumber.example"}), &state).unwrap();
        assert_eq!(got["entry"]["username"], "111122223333");
        assert!(got["entry"]["password"].is_null(), "{got}");

        // The one with a password is untouched by the relaxation.
        let login = dispatch(&json!({"op": "get", "name": "ygg.example"}), &state).unwrap();
        assert_eq!(login["entry"]["password"], "hunter2");
    }

    // A card is metadata over the socket and a secret only through the op the
    // injector uses. The 130 items in the real vault with no password are mostly
    // cards; `get` reports that absence as a null field rather than an error.
    #[test]
    fn agent_serves_card_metadata_without_the_number() {
        let state = synthetic_state();

        // `get` answers for a card now, with a NULL password rather than an
        // error, so a caller asking for another field is not turned away by a
        // fact about a field it never wanted. The LIST says why it has none.
        let got = dispatch(&json!({"op": "get", "name": "HDFC"}), &state).unwrap();
        assert!(got["entry"]["password"].is_null(), "{got}");
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
        assert!(!wire.contains(FIXTURE_PAN), "PAN leaked: {wire}");
        assert!(!wire.contains(FIXTURE_CVV), "CVV leaked: {wire}");

        // The injector's op is the only path to those, and it carries the whole
        // form so a fill needs one round trip.
        let secret = dispatch(&json!({"op": "card-secret", "name": "HDFC"}), &state).unwrap();
        assert_eq!(secret["number"], FIXTURE_PAN);
        assert_eq!(secret["code"], FIXTURE_CVV);
        assert_eq!(secret["exp_year"], "2029");

        // A login is not a card, and says so rather than answering emptily.
        let error = dispatch(&json!({"op": "card", "name": "github"}), &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not a card"), "{error}");
    }

    // The card path has ONE policy and this is it: the unlock. A locked vault
    // refuses and NAMES the verb that fixes it; an unlocked vault serves the op
    // to whoever reached the socket, with no second consent to obtain — every
    // Bitwarden client can read a card cipher, and this is one (the user's
    // ruling, 2026-07-26). A gate that existed here and nowhere else would be a
    // hassle only this client imposed, on a socket that already hands out every
    // password one `get` at a time.
    #[test]
    fn a_card_secret_is_gated_by_the_unlock_alone_and_the_refusal_names_the_remedy() {
        let dir = temp_dir("card-locked");
        let state = test_state(VaultManager::load(&dir), dir.clone());

        let refusal = dispatch(&json!({"op": "card-secret", "name": "HDFC"}), &state)
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("locked"), "{refusal}");
        assert!(
            refusal.contains("ychrome-vault unlock"),
            "a refusal that does not name its remedy sends the caller hunting \
             at exactly the moment they cannot afford it: {refusal}"
        );
        // The lock is checked BEFORE anything is resolved, so a real item and a
        // nonexistent one get the same answer. That is what "the unlock is the
        // whole policy" means, and it is also why a refused caller learns
        // nothing about the vault's contents.
        let for_a_stranger = dispatch(
            &json!({"op": "card-secret", "name": "no-such-item"}),
            &state,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(refusal, for_a_stranger);

        // Unlocked, the SAME request is served — no grant, no per-use consent,
        // no plane check. If a gate is ever reintroduced here, this is the
        // assertion that must be argued with first.
        let unlocked = synthetic_state_tagged("card-unlocked");
        let secret = dispatch(&json!({"op": "card-secret", "name": "HDFC"}), &unlocked).unwrap();
        assert_eq!(secret["number"], FIXTURE_PAN);
        assert_eq!(secret["code"], FIXTURE_CVV);

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(unlocked.lock().unwrap().dir.clone()).ok();
    }

    // Every release leaves ONE line behind, and that line is the only durable
    // record of a card fill anywhere — the transcript deliberately keeps none.
    // It records what left, where it was going and who said they were asking,
    // by FIELD NAME. Never a value: the log is a file on disk, so a PAN in it
    // would be exactly the durable artefact this whole design exists to avoid.
    #[test]
    fn a_card_secret_release_is_audited_by_field_name_and_never_by_value() {
        let state = synthetic_state_tagged("card-audit");
        let dir = state.lock().unwrap().dir.clone();
        let log = dir.join(AUDIT_LOG);
        std::fs::remove_file(&log).ok();

        let secret = dispatch(
            &json!({
                "op": "card-secret",
                "name": "HDFC",
                "host": "checkout.example.com",
                "client": "yggterm web fill-card",
            }),
            &state,
        )
        .unwrap();
        assert_eq!(secret["number"], FIXTURE_PAN, "the caller still gets it");

        let text = std::fs::read_to_string(&log).expect("the release wrote an audit line");
        // THE property, asserted on the RAW TEXT and asserted FIRST: the log
        // may name a field, it may never carry one. On a durable artefact this
        // outranks every structural expectation below, so it is checked before
        // any of them can fail on a leak's shape instead of on the leak.
        assert!(!text.contains(FIXTURE_PAN), "PAN in the audit log: {text}");
        assert!(!text.contains(FIXTURE_CVV), "CVV in the audit log: {text}");

        assert_eq!(text.lines().count(), 1, "one release, one line: {text}");
        let line: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line["op"], "card-secret");
        // The RESOLVED item, not the abbreviation the caller typed — a trail
        // that recorded the query rather than the answer could not tell you
        // afterwards which card was spent.
        assert_eq!(line["item"], "HDFC Regalia");
        assert_eq!(line["host"], "checkout.example.com");
        assert_eq!(line["client"], "yggterm web fill-card");
        let fields: Vec<&str> = line["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            fields,
            ["number", "code", "cardholder", "exp_month", "exp_year"],
        );
        assert!(
            line["at"].as_str().is_some_and(|at| at.ends_with('Z')),
            "{line}"
        );

        // A second release APPENDS. A trail that overwrote itself would answer
        // "was this card used tonight" with only the last answer.
        dispatch(&json!({"op": "card-secret", "name": "HDFC"}), &state).unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        // A caller that named neither host nor client is recorded as having
        // named neither — never as something the agent made up on its behalf.
        let second: Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert!(
            second["host"].is_null() && second["client"].is_null(),
            "{second}"
        );

        std::fs::remove_dir_all(&dir).ok();
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
        assert_eq!(
            live.len(),
            4,
            "three logins and a card, none of them trashed"
        );
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

    // ⛔ ABSENT IS NOT EMPTY, AND THE WRITE SIDE HAD NO TWIN FOR THAT RULE.
    // `dispatch`'s `string()` drops an empty value, so `--notes ""` arrived as
    // "no fields named" and the user was told to name a field they HAD named.
    // Found by the live round trip against a scratch vaultwarden, 2026-08-01.
    // The refusal must be about the empty string, and it must still happen
    // before any network.
    #[test]
    fn an_empty_value_is_refused_as_empty_not_as_no_change() {
        let state = synthetic_state();
        for (field, request) in [
            (
                "notes",
                json!({"op": "edit", "name": "github", "notes": ""}),
            ),
            (
                "rename",
                json!({"op": "edit", "name": "github", "rename": ""}),
            ),
            (
                "set_user",
                json!({"op": "edit", "name": "github", "set_user": ""}),
            ),
            ("totp", json!({"op": "edit", "name": "github", "totp": ""})),
            (
                "uris",
                json!({"op": "edit", "name": "github", "uris": [""]}),
            ),
        ] {
            let error = dispatch(&request, &state).unwrap_err().to_string();
            assert!(
                error.contains("empty string"),
                "an empty {field} must be refused as empty, said: {error}"
            );
        }
    }

    // A clear this build does not know must be REFUSED, never skipped: a newer
    // CLI talking to an older agent is the direction the stale-agent trap runs
    // in, and a silently dropped clear reports a successful edit that removed
    // nothing.
    #[test]
    fn an_unknown_clear_target_is_refused_rather_than_ignored() {
        let state = synthetic_state();
        let error = dispatch(
            &json!({"op": "edit", "name": "github", "clear": ["something-new"]}),
            &state,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("does not know how to clear"), "{error}");
        assert!(
            error.contains("handover"),
            "the remedy must be named: {error}"
        );

        // Same rule for a custom-field action.
        let error = dispatch(
            &json!({"op": "edit", "name": "github",
                    "fields": [{"name": "X", "action": "invert"}]}),
            &state,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown custom-field action"), "{error}");
    }

    // Search reached name and username ONLY, which is far narrower than any
    // other Bitwarden client: an entry findable by its uri, by a word in its
    // notes, or by the name of a custom field was simply unfindable here.
    #[test]
    fn search_reaches_uris_notes_and_custom_field_names() {
        let state = synthetic_state();
        let names = |query: &str| -> Vec<String> {
            dispatch(&json!({"op": "list", "query": query}), &state).unwrap()["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["name"].as_str().unwrap().to_string())
                .collect()
        };
        // The fixture's GitHub login carries a uri, notes and a custom field.
        assert_eq!(names("github"), ["GitHub"], "name still matches");
        assert!(!names("github.com").is_empty(), "a uri must match");
    }

    // ⛔ A SEARCH BOX MUST NOT BECOME AN ORACLE. Matching on a hidden custom
    // field's VALUE would let anyone at the socket confirm a guess one query at
    // a time — type it, and the result list answers. Field NAMES identify;
    // values reveal, and only names are searched.
    #[test]
    fn search_never_matches_a_custom_field_value() {
        let key_bytes = [0x33u8; 64];
        let seal_str = |text: &str| crate::model::seal(&key_bytes, text.as_bytes()).to_string();
        let mut raw = serde_json::json!({
            "id": "s1",
            "type": 1,
            "name": seal_str("Bank"),
            "fields": [{
                "name": seal_str("Recovery Code"),
                "value": seal_str("swordfish"),
                "type": 1,
            }],
            "login": {},
        });
        raw["notes"] = json!(seal_str("the note mentions pelican"));
        let vault = crate::model::Vault::new(
            crate::crypto::SymmetricKey::from_bytes(&key_bytes).unwrap(),
            Default::default(),
            vec![crate::model::RawCipher {
                raw,
                id: "s1".into(),
                item_type: 1,
                name: Some(crate::model::seal(&key_bytes, b"Bank")),
                ..Default::default()
            }],
            vec![],
            Default::default(),
        );
        // A field NAME and a word in the NOTES are both findable…
        assert!(vault.deep_search_match("s1", "recovery"));
        assert!(vault.deep_search_match("s1", "pelican"));
        // …and the hidden field's value is not, however exactly it is typed.
        assert!(
            !vault.deep_search_match("s1", "swordfish"),
            "a hidden field's value must never be searchable"
        );
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

    /// Is this fd set to close on exec?
    fn cloexec_set(fd: RawFd) -> bool {
        // SAFETY: reading the descriptor flags of an fd this test owns.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD failed on fd {fd}");
        flags & libc::FD_CLOEXEC != 0
    }

    /// An fd number that CANNOT be open: the kernel never allocates one at or
    /// above `RLIMIT_NOFILE`. Picking "some big number" instead would race the
    /// other test threads in this binary, which is exactly the kind of
    /// timing-dependent test this project refuses.
    fn never_open_fd() -> RawFd {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes one `rlimit` into a live local.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        limit.rlim_cur.min(1_000_000) as RawFd
    }

    // The listener's CLOEXEC clear is the guard the whole handover rests on: it
    // is why there is no unbind window. Lose it and the successor inherits a
    // CLOSED fd — dead pid, unbound socket, master password retyped on every
    // host. It had NO coverage, because the one test that crossed an exec
    // cleared the bit ITSELF and so never ran the production call. This pins the
    // production call, for both fds.
    #[test]
    fn arming_a_handover_clears_cloexec_on_the_listener_and_the_payload() {
        let dir = temp_dir("handover-arm");
        let listener = UnixListener::bind(socket_path(&dir)).unwrap();
        // Rust sets FD_CLOEXEC on everything it opens. That default is right,
        // and opting out of it for exactly these two fds is the whole job.
        assert!(
            cloexec_set(listener.as_raw_fd()),
            "a Rust-opened listener starts CLOEXEC"
        );

        let plan = arm_handover(
            PathBuf::from("/usr/local/bin/ychrome-vault"),
            dir.clone(),
            listener.as_raw_fd(),
            b"a payload",
        )
        .unwrap();

        assert!(
            !cloexec_set(plan.listener_fd),
            "the LISTENER must survive the exec: a successor that inherits a closed fd \
             cannot serve the socket at all, and the unlock dies with it"
        );
        assert!(
            !cloexec_set(plan.payload.as_raw_fd()),
            "the PAYLOAD must survive the exec, or the successor comes up locked"
        );
        assert_eq!(plan.listener_fd, listener.as_raw_fd());
        assert_eq!(plan.dir, dir);
        std::fs::remove_dir_all(&dir).ok();
    }

    // `from_raw_fd` validates nothing: on a closed number the runtime aborts the
    // process, and on a REALLOCATED one it succeeds on the wrong object and
    // never accepts again. Both are one-way-door deaths, so an inherited fd is
    // checked against what the flag promises before anything owns it.
    #[test]
    fn an_adopted_fd_must_be_the_object_its_flag_promises() {
        let dir = temp_dir("adopt-fd");
        let listener = UnixListener::bind(socket_path(&dir)).unwrap();
        let pipe = payload_pipe(b"payload").unwrap();
        let (connected, _peer) = UnixStream::pair().unwrap();

        // The two real things pass, each as itself.
        check_adopted_fd(listener.as_raw_fd(), AdoptedFd::Listener).unwrap();
        check_adopted_fd(pipe.as_raw_fd(), AdoptedFd::Payload).unwrap();

        // ...and are refused as each other, which is also what makes
        // `--adopt-listener N --adopt-payload N` impossible to double-own.
        for (fd, kind, wanted) in [
            (pipe.as_raw_fd(), AdoptedFd::Listener, "listening"),
            (listener.as_raw_fd(), AdoptedFd::Payload, "pipe"),
            // A CONNECTED socket is socket-shaped but not listening — the exact
            // shape of an fd number that got reallocated to something live.
            (connected.as_raw_fd(), AdoptedFd::Listener, "listening"),
            (
                never_open_fd(),
                AdoptedFd::Listener,
                "not an open file descriptor",
            ),
            (
                never_open_fd(),
                AdoptedFd::Payload,
                "not an open file descriptor",
            ),
        ] {
            let error = check_adopted_fd(fd, kind)
                .expect_err(&format!("{kind:?} accepted fd {fd}"))
                .to_string();
            assert!(error.contains(wanted), "{kind:?}: {error}");
            assert!(error.contains(kind.flag()), "{kind:?}: {error}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // `incoming()` never ends, and a listener that cannot accept fails it
    // INSTANTLY and forever. The old bare `continue` there was an unbounded hot
    // loop: a core at 100%, not one request served, nothing in the log but the
    // same line a million times.
    #[test]
    fn a_listener_that_cannot_accept_gives_up_instead_of_spinning() {
        // A connected socket is not a listening one: accept on it fails EINVAL
        // every time, which is how an adopted-from-the-wrong-object fd behaves.
        let (connected, _peer) = UnixStream::pair().unwrap();
        // SAFETY: the fd is live and released by `into_raw_fd`, so the listener
        // below is its only owner.
        let listener =
            unsafe { UnixListener::from_raw_fd(std::os::fd::IntoRawFd::into_raw_fd(connected)) };
        let dir = temp_dir("accept-spin");
        let state = test_state(VaultManager::load(&dir), dir.clone());

        let (tx, rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = tx.send(accept_loop(listener, state).map_err(|error| error.to_string()));
        });

        let outcome = rx.recv_timeout(Duration::from_secs(30)).expect(
            "the accept loop must give up on a listener that will never accept — \
             a bare `continue` there spins a core forever",
        );
        let error = outcome.expect_err("a listener that never accepts is not a clean exit");
        assert!(error.contains("accept failed"), "{error}");
        assert!(error.contains("exiting"), "{error}");
        // And it must have SLEPT between the tries: with no backoff the whole
        // budget burns through in microseconds, which is the same hot loop with
        // a bound on it.
        assert!(
            started.elapsed() >= ACCEPT_BACKOFF_STEP * 4,
            "the loop must back off between failures, not spin: {:?}",
            started.elapsed()
        );
        std::fs::remove_dir_all(&dir).ok();
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
