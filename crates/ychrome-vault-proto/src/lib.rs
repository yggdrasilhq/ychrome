//! The vault agent's client transport — the wire, with no crypto.
//!
//! `ychrome-vault`'s unlock-caching agent speaks newline-delimited JSON over a
//! unix socket at `<dir>/agent.sock`:
//!
//! ```text
//! {"op":"get","name":"github.com","user":null}
//! {"ok":true,"entry":{"name":"github.com","username":"octocat","password":"…"}}
//! ```
//!
//! This crate is JUST the client half of that conversation: connect, send one
//! `{"op":…}`, read one `{"ok":…}`. It links no crypto and no http, so the
//! ychrome browser/daemon can talk to the agent **directly** rather than
//! spawning the `ychrome-vault` CLI per operation (the vault sidebar's old
//! `Command::new("ychrome-vault")` path). The agent (server) and the crypto
//! stay in `ychrome-vault`; the two ends share this wire so it has ONE owner.
//!
//! Host-resident, like every libyggterm app's state: the agent runs on the
//! machine ychrome runs on, which over ssh is NOT the machine the GUI is on.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// Default read budget for a request. Matches the agent's own client: a `sync`
/// or `unlock` re-pulls the whole vault from the server, which is slow.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a client waits for a freshly spawned agent to bind its socket.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

/// The vault directory holding the config, socket and pid file. Host-resident
/// at `~/.yggterm/vault`, the `ychrome-vault` CLI's default `--dir`.
pub fn default_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home directory")?
        .join(".yggterm")
        .join("vault"))
}

/// `<dir>/agent.sock` — the agent's unix socket.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("agent.sock")
}

/// The agent's pid, written beside the socket. The escape hatch for retiring an
/// agent too old to know the `stop` op. The agent (server) writes it; `stop`
/// (client) reads it — one owner of the path.
pub fn pid_path(dir: &Path) -> PathBuf {
    dir.join("agent.pid")
}

fn read_pid(dir: &Path) -> Option<i32> {
    std::fs::read_to_string(pid_path(dir))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Is an agent answering on this vault dir's socket?
pub fn is_running(dir: &Path) -> bool {
    UnixStream::connect(socket_path(dir)).is_ok()
}

/// Send one request to a running agent and return its reply. Does not start one.
pub fn request(dir: &Path, request: &Value) -> Result<Value> {
    request_with_timeout(dir, request, DEFAULT_TIMEOUT)
}

/// [`request`] with an explicit read budget. A WebAuthn ceremony wants a shorter
/// one than a full `sync`, so the passkey signer passes its own.
pub fn request_with_timeout(dir: &Path, request: &Value, timeout: Duration) -> Result<Value> {
    let socket = socket_path(dir);
    let stream = UnixStream::connect(&socket).with_context(|| {
        format!(
            "no vault agent on {} — unlock with `ychrome-vault unlock` on this host",
            socket.display()
        )
    })?;
    stream.set_read_timeout(Some(timeout))?;
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{request}")?;
    writer.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: Value = serde_json::from_str(line.trim())
        .with_context(|| format!("vault agent sent a malformed response: {line:?}"))?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(response);
    }
    let error = response
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("vault agent refused the request");
    // The agent outlives the binary that spawned it. An op this build knows but
    // the agent does not means the running agent predates the last rebuild — say
    // so, instead of leaving the caller staring at "unknown op". `stop` is
    // exempt: it is the remedy, and `stop` has its own fallback for an agent too
    // old to perform it.
    let stopping = request.get("op").and_then(Value::as_str) == Some("stop");
    if error.starts_with("unknown op") && !stopping {
        bail!("{error} — the running agent predates this binary; run `ychrome-vault stop-agent`");
    }
    Err(anyhow!(error.to_string()))
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

/// The `ychrome-vault` binary to spawn an agent from. When THIS process already
/// is `ychrome-vault` (the CLI autostarting its own agent) that exact binary is
/// used; otherwise — the browser/daemon, whose `current_exe` is `ychrome` — the
/// name is resolved on `PATH`, so `Command::new` finds the installed agent.
fn resolve_vault_exe() -> PathBuf {
    which_vault_exe().unwrap_or_else(|| PathBuf::from("ychrome-vault"))
}

/// The installed `ychrome-vault` binary's path (canonicalised, so it matches the
/// agent's own `/proc/self/exe` stamp). `current_exe` wins when this process IS
/// `ychrome-vault`; otherwise the first `ychrome-vault` on `PATH`.
fn which_vault_exe() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && exe.file_name().and_then(|name| name.to_str()) == Some("ychrome-vault")
    {
        return exe.canonicalize().ok().or(Some(exe));
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("ychrome-vault");
        if candidate.is_file() {
            return candidate.canonicalize().ok().or(Some(candidate));
        }
    }
    None
}

/// Spawn `ychrome-vault agent` detached from this process group, then wait for
/// it to bind. `process_group(0)` keeps a terminal's Ctrl+C / SIGHUP from
/// reaching the agent when the shell that first needed it goes away.
fn spawn_agent(dir: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt as _;

    let exe = resolve_vault_exe();
    let mut command = std::process::Command::new(&exe);
    command
        .arg("agent")
        .arg("--dir")
        .arg(dir)
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.process_group(0);
    command
        .spawn()
        .with_context(|| format!("spawning the vault agent ({})", exe.display()))?;

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

/// `path@mtime` — identifies the exact binary a stamp came from. The agent
/// stamps its own `current_exe` with this; a client compares that against the
/// installed binary's.
pub fn exe_stamp_of(path: &Path) -> String {
    let mtime = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or(0);
    format!("{}@{mtime}", path.display())
}

/// The stamp of the INSTALLED `ychrome-vault` binary, for comparing against a
/// running agent's reported `exe_stamp` from a process that is not itself
/// `ychrome-vault`. Empty when the binary is not found — an empty stamp never
/// equals the agent's, so a missing binary reads as "stale", which is the safe
/// side (it offers the restart-agent remedy rather than hiding it).
pub fn installed_vault_exe_stamp() -> String {
    which_vault_exe()
        .map(|path| exe_stamp_of(&path))
        .unwrap_or_default()
}

/// Secret-free lock status read from `<dir>/config.json`, for when NO agent is
/// running (the agent is the source of truth only while it lives). The config
/// holds no secrets — server url, email, KDF params — so this needs no crypto.
/// Shapes match the agent's `status` reply's locked/not-configured arms.
pub fn config_status(dir: &Path) -> Value {
    let Ok(bytes) = std::fs::read(dir.join("config.json")) else {
        return json!({ "state": "not_configured" });
    };
    let Ok(config) = serde_json::from_slice::<Value>(&bytes) else {
        return json!({ "state": "not_configured" });
    };
    json!({
        "state": "locked",
        "email": config.get("email").and_then(Value::as_str).unwrap_or_default(),
        "server_url": config.get("server_url").and_then(Value::as_str).unwrap_or_default(),
    })
}

/// The lock/staleness status the vault sidebar renders from — the SSOT the CLI's
/// `status` verb also builds. A running agent is authoritative (only it knows
/// whether the vault is unlocked, and whether it predates the installed binary);
/// otherwise the secret-free config answers.
pub fn status(dir: &Path) -> Result<Value> {
    if is_running(dir) {
        let mut response = request(dir, &json!({"op": "status"}))?;
        response["agent"] = json!(true);
        let stale = response.get("exe_stamp").and_then(Value::as_str)
            != Some(&installed_vault_exe_stamp());
        response["agent_stale"] = json!(stale);
        Ok(response)
    } else {
        let mut status = config_status(dir);
        status["agent"] = json!(false);
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ychrome-vault-proto-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake agent: reply with `reply` to the first real request, and return
    /// what was asked. It TOLERATES the bare liveness-probe connections that
    /// `is_running`/`status` open first (connect, send nothing, close) — those
    /// consume an `accept` but no request line, so skip them and keep listening.
    fn fake_agent(dir: &Path, reply: Value) -> std::thread::JoinHandle<Value> {
        let listener = UnixListener::bind(socket_path(dir)).unwrap();
        std::thread::spawn(move || {
            loop {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let read = reader.read_line(&mut line).unwrap();
                if read == 0 || line.trim().is_empty() {
                    // An `is_running` probe: connected, sent nothing, closed.
                    continue;
                }
                let mut writer = stream;
                writeln!(writer, "{reply}").unwrap();
                writer.flush().unwrap();
                return serde_json::from_str::<Value>(line.trim()).unwrap();
            }
        })
    }

    #[test]
    fn socket_path_is_agent_sock() {
        assert_eq!(
            socket_path(Path::new("/x/vault")),
            PathBuf::from("/x/vault/agent.sock")
        );
    }

    #[test]
    fn is_running_false_without_socket() {
        let dir = temp_dir("norun");
        std::fs::remove_file(socket_path(&dir)).ok();
        assert!(!is_running(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn request_round_trips_ok_reply() {
        let dir = temp_dir("ok");
        std::fs::remove_file(socket_path(&dir)).ok();
        let served = fake_agent(&dir, json!({"ok": true, "items": [1, 2, 3]}));
        assert!(is_running(&dir));
        let reply = request(&dir, &json!({"op": "list", "query": null})).unwrap();
        assert_eq!(reply["items"], json!([1, 2, 3]));
        // The client sent exactly what we asked it to.
        let seen = served.join().unwrap();
        assert_eq!(seen["op"], "list");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn request_surfaces_agent_error() {
        let dir = temp_dir("err");
        std::fs::remove_file(socket_path(&dir)).ok();
        let served = fake_agent(&dir, json!({"ok": false, "error": "the vault is locked"}));
        let err = request(&dir, &json!({"op": "get", "name": "x"})).unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        served.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_op_names_the_stale_agent() {
        let dir = temp_dir("stale");
        std::fs::remove_file(socket_path(&dir)).ok();
        let served = fake_agent(&dir, json!({"ok": false, "error": "unknown op \"route\""}));
        let err = request(&dir, &json!({"op": "route"})).unwrap_err();
        assert!(err.to_string().contains("predates this binary"), "{err}");
        served.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_status_reads_locked_and_not_configured() {
        let dir = temp_dir("cfg");
        std::fs::write(
            dir.join("config.json"),
            br#"{"server_url":"https://vw.example.com","email":"a@b.c","kdf_type":0,"kdf_iterations":600000,"device_id":"x"}"#,
        )
        .unwrap();
        let status = config_status(&dir);
        assert_eq!(status["state"], "locked");
        assert_eq!(status["email"], "a@b.c");
        assert_eq!(status["server_url"], "https://vw.example.com");

        std::fs::remove_file(dir.join("config.json")).unwrap();
        assert_eq!(config_status(&dir)["state"], "not_configured");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn status_marks_agent_and_staleness() {
        let dir = temp_dir("status");
        std::fs::remove_file(socket_path(&dir)).ok();
        // A stamp that cannot match the installed binary => agent_stale true.
        let served = fake_agent(
            &dir,
            json!({"ok": true, "state": "unlocked", "item_count": 5, "exe_stamp": "/old@1"}),
        );
        let status = status(&dir).unwrap();
        assert_eq!(status["agent"], true);
        assert_eq!(status["agent_stale"], true);
        assert_eq!(status["item_count"], 5);
        served.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
