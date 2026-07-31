//! The staleness handover, driven through the REAL `ychrome` binary.
//!
//! The bug these lock down is one nothing in-process can show: `ensure()` only
//! ever compared the daemon's REPORTED VERSION against its own, and ychrome's
//! version is the constant `0.1.0`, so after a rebuild the daemon already
//! running kept serving the old code indefinitely. The daemon on the machine
//! this was written on had been doing exactly that for 6.7 days. Proving the fix
//! needs two processes and a binary whose mtime moves underneath one of them,
//! which is what a spawned daemon plus a copied executable buy.
//!
//! Every test runs in a throwaway `HOME`, so the daemon it drives is its own and
//! never the user's: `daemon_dir()` is `$HOME/.yggterm/ychrome`, and these tests
//! send `stop`. `assert_isolated` checks that before anything destructive
//! happens, because getting this wrong would retire a daemon holding somebody's
//! live browser surfaces.
//!
//! The wire is spoken by hand rather than through the crate. A round trip
//! against our own client code would pass even if both halves drifted together,
//! and the daemon's socket API is a contract other programs (the yggterm GUI's
//! host side, agents) read too.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// A throwaway HOME with its own copy of the binary.
///
/// The path is short on purpose: a unix socket path must fit `SUN_LEN` (~108
/// bytes) and the daemon builds `$HOME/.yggterm/ychrome/daemon.sock` out of
/// this one. A scratch dir under the usual agent temp root is already too long,
/// and the daemon fails to bind with nothing but "did not come up" to show for
/// it.
struct Host {
    home: PathBuf,
    exe: PathBuf,
}

impl Host {
    fn new(tag: &str) -> Host {
        let home = PathBuf::from(format!("/tmp/ych-stale-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        std::fs::create_dir_all(home.join("bin")).unwrap();
        let exe = home.join("bin").join("ychrome");
        // A COPY, never the cargo artifact: the staleness stamp is `path@mtime`
        // of the running binary and these tests move that mtime. Touching the
        // artifact would move it for every other test in the run, and for cargo.
        std::fs::copy(env!("CARGO_BIN_EXE_ychrome"), &exe).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        Host { home, exe }
    }

    /// Run our copy of the binary against our own HOME.
    ///
    /// ETXTBSY is retried, and it is NOT a product failure: while one test
    /// thread is still copying its binary, another thread's `fork` duplicates
    /// that open write fd into a child, and any `exec` of that inode fails with
    /// "Text file busy" until the child reaches `exec` and CLOEXEC closes it.
    /// The window is microseconds wide and it made this file fail about one run
    /// in three at `--test-threads=8`. A flaky lock teaches people to ignore
    /// locks, so it is retried here rather than left to chance.
    fn run(&self, args: &[&str]) -> Output {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match Command::new(&self.exe)
                .args(args)
                .env("HOME", &self.home)
                .stdin(Stdio::null())
                .output()
            {
                Ok(output) => return output,
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("the ychrome binary runs: {error}"),
            }
        }
    }

    fn status(&self) -> Value {
        let out = self.run(&["status", "--json"]);
        let text = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!(
                "`ychrome status --json` did not answer JSON ({error}): {text}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    /// The guard that has to hold before any test sends a `stop`: the daemon we
    /// are talking to must be OURS. If `HOME` ever stopped reaching
    /// `dirs::home_dir()`, every test in this file would be aimed at the user's
    /// own daemon and its live surfaces.
    fn assert_isolated(&self, status: &Value) {
        let stamp = status["exe_stamp"].as_str().unwrap_or("");
        assert!(
            stamp.starts_with(&self.exe.to_string_lossy().to_string()),
            "this is not our daemon, so nothing here may run: exe_stamp {stamp:?} is not {}",
            self.exe.display()
        );
    }

    fn socket(&self) -> PathBuf {
        self.home
            .join(".yggterm")
            .join("ychrome")
            .join("daemon.sock")
    }

    /// One request on the daemon's unix socket, written by hand: newline JSON in,
    /// one line of JSON out.
    fn ask(&self, request: Value) -> Value {
        let mut stream = UnixStream::connect(self.socket()).expect("the daemon socket is there");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(stream, "{request}").unwrap();
        stream.flush().unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).expect("the daemon answers JSON")
    }

    /// Attach a surface the way a view client does. The daemon reaps a session
    /// ~14s after its last heartbeat, so a test that needs one attached does its
    /// work straight after this.
    fn attach_surface(&self, env_id: &str) {
        let reply = self.ask(json!({
            "op": "register",
            "env_id": env_id,
            "profile": "default",
            "pid": std::process::id(),
        }));
        assert_eq!(reply["ok"], json!(true), "register refused: {reply}");
    }

    /// Replace the binary as far as the staleness stamp is concerned. `touch`,
    /// not `File::set_modified`: the binary is RUNNING, and opening a running
    /// executable for write is ETXTBSY. `utimensat` on the path is not.
    fn age_the_binary(&self) {
        let future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 120;
        let done = Command::new("touch")
            .arg("-m")
            .arg("-d")
            .arg(format!("@{future}"))
            .arg(&self.exe)
            .status()
            .expect("touch runs");
        assert!(done.success(), "could not move the binary's mtime");
    }

    /// Attach a surface the way a **PRE-GATE** view client does: the same
    /// register, without the `declares_control_token` claim, because a binary
    /// built before 2026-07-28 had never heard of it. This is the shape that
    /// produced the live 403s, so it is worth being able to write down.
    fn attach_pre_gate_surface(&self, env_id: &str, profile: &str) -> Value {
        let reply = self.ask(json!({
            "op": "register",
            "env_id": env_id,
            "profile": profile,
            "pid": std::process::id(),
        }));
        assert_eq!(reply["ok"], json!(true), "register refused: {reply}");
        reply
    }

    /// Run the REAL surface client, in thin-client mode, with its OSC stream
    /// captured. This is the only way to see what a live browser actually
    /// declares — and the declare is the token's one and only courier, so
    /// nothing short of reading it off the client's own stdout proves anything
    /// about the token the GUI ends up holding.
    fn spawn_client(&self, env_id: &str, profile: &str) -> ClientProcess {
        let mut child = Command::new(&self.exe)
            .args(["--profile", profile, "https://example.com/"])
            .env("HOME", &self.home)
            .env("YGGTERM_SESSION_ID", env_id)
            // A display would send it down the standalone GTK path instead.
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the ychrome client runs");
        let declares = Arc::new(Mutex::new(Vec::<Value>::new()));
        let stdout = child.stdout.take().expect("stdout is piped");
        {
            let declares = Arc::clone(&declares);
            std::thread::spawn(move || {
                let mut reader = stdout;
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(read) = reader.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    while let Some(declare) = take_declare(&mut buffer) {
                        declares.lock().unwrap().push(declare);
                    }
                }
            });
        }
        ClientProcess { child, declares }
    }

    /// Daemons spawned from OUR copy, found by cmdline. Matching on the copy's
    /// path is what makes the cleanup incapable of killing the user's daemon.
    fn daemon_pids(&self) -> Vec<u32> {
        let needle = self.exe.to_string_lossy().to_string();
        let mut pids = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return pids;
        };
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Ok(raw) = std::fs::read(entry.path().join("cmdline")) {
                let cmdline = String::from_utf8_lossy(&raw).replace('\0', " ");
                if cmdline.contains(&needle) && cmdline.contains("--daemon") {
                    pids.push(pid);
                }
            }
        }
        pids
    }
}

/// A live surface client, and the declares it has emitted so far.
struct ClientProcess {
    child: Child,
    declares: Arc<Mutex<Vec<Value>>>,
}

impl ClientProcess {
    /// The most recent declare, or `None` if it has not spoken yet.
    fn latest(&self) -> Option<Value> {
        self.declares.lock().unwrap().last().cloned()
    }

    /// Wait for a declare whose control endpoint differs from `previous`. That
    /// is the client following a daemon handover: a new listener, and a freshly
    /// minted token that moves with it.
    fn wait_for_moved_endpoint(&self, previous: &Value, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            if let Some(latest) = self.latest()
                && latest["control"] != previous["control"]
            {
                return latest;
            }
            assert!(
                Instant::now() < deadline,
                "the client never re-declared a moved endpoint after the handover; \
                 last declare was {:?}",
                self.latest()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for ClientProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Pull one complete `OSC 7717 ; sidebar ; declare ; <base64>` BEL out of the
/// buffer, leaving anything after it. `None` when no complete one is there yet.
///
/// The client's stream also carries `web-surface` OSCs on the same 7717
/// dispatcher, so the verb is matched, not the prefix.
fn take_declare(buffer: &mut Vec<u8>) -> Option<Value> {
    use base64::Engine as _;
    const HEAD: &[u8] = b"]7717;sidebar;declare;";
    let start = buffer
        .windows(HEAD.len())
        .position(|window| window == HEAD)?;
    let payload_at = start + HEAD.len();
    let bel = buffer[payload_at..].iter().position(|byte| *byte == 0x07)?;
    let encoded = buffer[payload_at..payload_at + bel].to_vec();
    buffer.drain(..payload_at + bel + 1);
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .expect("a declare's payload is base64");
    Some(serde_json::from_slice(&raw).expect("a declare's payload is json"))
}

/// One request against a session's control endpoint, spoken by hand for the
/// same reason the unix ops are: this is the wire the yggterm GUI speaks, and a
/// round trip through our own client code would pass even if both halves
/// drifted. Returns `(status, body)`.
fn control_get(control_url: &str, path: &str, token: Option<&str>) -> (u16, Value) {
    let addr = control_url
        .strip_prefix("http://")
        .expect("a loopback control url");
    let mut stream = std::net::TcpStream::connect(addr)
        .unwrap_or_else(|error| panic!("connecting to {addr}: {error}"));
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let auth = match token {
        Some(token) => format!("X-Ychrome-Control: {token}\r\n"),
        None => String::new(),
    };
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n"
    )
    .expect("the control endpoint takes a request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("a control response");
    let (head, body) = raw.split_once("\r\n\r\n").expect("a response has a body");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// Is this control endpoint still answering at all? A handed-over daemon takes
/// its listeners with it, so the old port stops connecting.
fn control_is_dead(control_url: &str) -> bool {
    let addr = control_url
        .strip_prefix("http://")
        .expect("a loopback control url");
    std::net::TcpStream::connect(addr).is_err()
}

/// A failing assertion must not leave a real daemon running on a real socket:
/// `Child` has no killing `Drop`, and these daemons are detached from us anyway
/// (setsid, own process group), so they are found by cmdline and killed by pid.
impl Drop for Host {
    fn drop(&mut self) {
        for pid in self.daemon_pids() {
            Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status()
                .ok();
        }
        std::fs::remove_dir_all(&self.home).ok();
    }
}

// THE bug. A rebuild replaces the binary; the daemon already running keeps
// serving the old code because its VERSION still matches. With nothing attached
// to it there is nothing to lose by replacing it, so the next invocation must
// simply do so, with no command from anyone.
#[test]
fn a_stale_daemon_with_nothing_attached_is_replaced_by_the_next_invocation() {
    let host = Host::new("idle");
    let first = host.status();
    host.assert_isolated(&first);
    let old_pid = first["pid"].as_u64().expect("a daemon pid");
    assert_eq!(first["stale"], json!(false), "it starts current: {first}");
    assert_eq!(first["live_sessions"], json!(0));

    host.age_the_binary();

    let after = host.status();
    assert_ne!(
        after["pid"].as_u64().expect("a daemon pid"),
        old_pid,
        "the stale daemon was left serving old code, which is the whole bug: {after}"
    );
    assert_eq!(
        after["stale"],
        json!(false),
        "the replacement must be running the binary that is on disk now: {after}"
    );
    assert_ne!(
        after["exe_stamp"], first["exe_stamp"],
        "the same stamp is the same code: {after}"
    );
    assert!(
        !host.daemon_pids().contains(&(old_pid as u32)),
        "pid {old_pid} outlived its replacement: a handover that leaves the old process \
         running is two daemons, not one"
    );
}

// The other failure, and the one that costs the user something real. The daemon
// holds every attached surface's control endpoint, its sidebar pane draft and
// its passkey signer. None of that survives its exit, so an automatic restart
// spends the user's work to save them a command.
#[test]
fn a_stale_daemon_a_surface_is_attached_to_is_left_running() {
    let host = Host::new("busy");
    let first = host.status();
    host.assert_isolated(&first);
    let old_pid = first["pid"].as_u64().expect("a daemon pid");

    host.attach_surface("env-busy");
    host.age_the_binary();

    let after = host.status();
    assert_eq!(
        after["pid"].as_u64().expect("a daemon pid"),
        old_pid,
        "the daemon was retired underneath a live surface: {after}"
    );
    assert_eq!(
        after["stale"],
        json!(true),
        "and it must still admit what it is: {after}"
    );
    assert_eq!(after["live_sessions"], json!(1));
    let ids: Vec<&str> = after["sessions"]
        .as_array()
        .expect("a sessions array")
        .iter()
        .filter_map(|row| row["env_id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["env-busy"],
        "the registry must have survived intact: {after}"
    );
}

// Refusing to restart it is only half the contract. An outdated daemon nobody
// is told about is the 6.7-day daemon again, so every invocation that meets one
// says so, names it, and names the one command that ends it.
#[test]
fn an_outdated_daemon_in_use_names_itself_and_the_remedy_on_stderr() {
    let host = Host::new("loud");
    let first = host.status();
    host.assert_isolated(&first);
    let pid = first["pid"].as_u64().expect("a daemon pid");

    host.attach_surface("env-loud");
    host.age_the_binary();

    let out = host.run(&["status", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("OLD CODE"),
        "the staleness must reach the user, not just the JSON: {stderr:?}"
    );
    assert!(
        stderr.contains(&format!("pid {pid}")),
        "it must name WHICH daemon: {stderr:?}"
    );
    assert!(
        stderr.contains("ychrome daemon restart"),
        "a warning with no remedy is a warning nobody acts on: {stderr:?}"
    );
    // And the same fact on the human-readable status, which is where an agent
    // or a user looks when something feels wrong.
    let printed = String::from_utf8_lossy(&host.run(&["status"]).stdout).to_string();
    assert!(printed.contains("[STALE]"), "{printed}");
    assert!(printed.contains("ychrome daemon restart"), "{printed}");
}

// A healthy daemon must be boring. Churning one that is already running the
// current binary would cost every attached surface its control url on every
// invocation, which is the same damage as the silent kill, just spread out.
#[test]
fn a_fresh_daemon_is_left_alone_by_every_invocation() {
    let host = Host::new("fresh");
    let first = host.status();
    host.assert_isolated(&first);
    let pid = first["pid"].as_u64().expect("a daemon pid");

    for round in 0..3 {
        let out = host.run(&["status", "--json"]);
        let after: Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("status JSON");
        assert_eq!(
            after["pid"].as_u64().expect("a daemon pid"),
            pid,
            "round {round} replaced a daemon that was already current: {after}"
        );
        assert_eq!(after["stale"], json!(false), "{after}");
        assert!(
            String::from_utf8_lossy(&out.stderr).trim().is_empty(),
            "a current daemon must say nothing at all: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// The deliberate half: the user, having been told, ends it in one command. This
// is the ONLY path that retires a daemon with surfaces attached, and it says
// which ones paid for it rather than reporting a clean success.
#[test]
fn daemon_restart_hands_over_a_busy_daemon_and_says_what_reattaches() {
    let host = Host::new("restart");
    let first = host.status();
    host.assert_isolated(&first);
    let old_pid = first["pid"].as_u64().expect("a daemon pid");

    host.attach_surface("env-restart");
    host.age_the_binary();

    let out = host.run(&["daemon", "restart"]);
    let printed = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "restart failed: {printed} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        printed.contains(&format!("pid {old_pid} retired")),
        "it must name the daemon it retired: {printed}"
    );
    assert!(
        printed.contains("env-restart"),
        "the user must be told which surface paid for it: {printed}"
    );
    // The verb is already acting on the staleness, so it must not also announce
    // that the daemon "was left running" while it is retiring it.
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !stderr.contains("left running"),
        "the deliberate handover contradicted itself on stderr: {stderr:?}"
    );

    let after = host.status();
    assert_ne!(
        after["pid"].as_u64().expect("a daemon pid"),
        old_pid,
        "the busy daemon was NOT handed over even though it was asked: {after}"
    );
    assert_eq!(
        after["stale"],
        json!(false),
        "the successor must run the binary on disk: {after}"
    );
    assert!(
        !host.daemon_pids().contains(&(old_pid as u32)),
        "pid {old_pid} is still running after its own handover"
    );
}

// The daemon owns the idle-or-attached decision, in one round trip, under its
// own lock. A client that asked "are you idle?" and then said "stop" would be
// acting on a fact that could change in between; this cannot.
#[test]
fn retire_if_idle_refuses_while_a_surface_is_attached_and_names_it() {
    let host = Host::new("verb");
    let first = host.status();
    host.assert_isolated(&first);
    let pid = first["pid"].as_u64().expect("a daemon pid");

    host.attach_surface("env-verb");
    let refused = host.ask(json!({ "op": "retire_if_idle" }));
    assert_eq!(refused["retiring"], json!(false), "{refused}");
    assert_eq!(refused["held_by"], json!(["env-verb"]), "{refused}");
    assert_eq!(
        host.status()["pid"].as_u64().expect("a daemon pid"),
        pid,
        "it answered `no` and left anyway"
    );

    // With the client gone it retires on the same verb, and `retiring: true`
    // has to mean it: a daemon that answers "leaving" and stays is two daemons
    // as soon as a successor comes up, with the socket belonging to whichever
    // bound it last.
    //
    // (The socket is unlinked BEFORE the reply is written so a successor's bind
    // cannot race this process's cleanup. That ordering is deliberate and
    // commented at the call site, but its failure window is microseconds wide
    // and no black-box assertion here can tell the two orderings apart without
    // being a coin flip, so it is NOT claimed as locked.)
    host.ask(json!({ "op": "deregister", "env_id": "env-verb" }));
    let retiring = host.ask(json!({ "op": "retire_if_idle" }));
    assert_eq!(retiring["retiring"], json!(true), "{retiring}");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while host.daemon_pids().contains(&(pid as u32)) {
        assert!(
            std::time::Instant::now() < deadline,
            "pid {pid} answered `retiring: true` and is still running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // And the successor is a working daemon, not a corpse holding a path.
    let after = host.status();
    assert_ne!(after["pid"].as_u64().expect("a daemon pid"), pid, "{after}");
    assert_eq!(after["ok"], json!(true), "{after}");
}

// ---------------------------------------------------------------------------
// THE CONTROL TOKEN ACROSS A HANDOVER
//
// Reported live 2026-07-31: the vault and settings panes rendered one line,
// `control endpoint returned 403`, while ad blocking and SponsorBlock kept
// working — after the ychrome daemon had been handed over six times without the
// per-session CLIs being cycled.
//
// The token's only courier is the client's `sidebar ; declare` OSC. A daemon
// handover mints a NEW token, so a client that publishes anything other than its
// current registration hands the GUI a credential the endpoint will refuse.
// Nothing in-process can show that: it needs a real client process, a real
// handover, and a real gated request over the real wire.
// ---------------------------------------------------------------------------

// THE ACCEPTANCE. A gated route answers the GUI, the daemon is handed over
// underneath it, and the same gated route answers again — using only what the
// client re-declared, with no restart of the client and nobody remembering to
// refresh anything.
#[test]
fn a_gated_route_survives_a_daemon_handover_on_what_the_client_re_declares() {
    let host = Host::new("token");
    let first = host.status();
    host.assert_isolated(&first);

    let client = host.spawn_client("env-token", "tokprobe");
    let before = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(declare) = client.latest() {
                break declare;
            }
            assert!(
                Instant::now() < deadline,
                "the client never declared its sidebar contribution"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let old_url = before["control"]
        .as_str()
        .expect("a control url")
        .to_string();
    let old_token = before["control_token"]
        .as_str()
        .expect("a declared control token")
        .to_string();
    assert!(!old_token.is_empty(), "the declare must carry a token");

    // The GUI's call, before anything moves.
    let (status, _) = control_get(&old_url, "/pane/settings", Some(&old_token));
    assert_eq!(status, 200, "the declared token must drive the pane");
    // And the same call without it is the 403 the user saw, so the 200 above is
    // the token's doing and not an absent gate.
    let (refused, body) = control_get(&old_url, "/pane/settings", None);
    assert_eq!(refused, 403, "the gate must be live: {body}");

    // Hand the daemon over, exactly as the deploy step does.
    let out = host.run(&["daemon", "restart"]);
    assert!(
        out.status.success(),
        "restart failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The client follows on its own heartbeat. Both halves move together: a new
    // listener AND a new token.
    let after = client.wait_for_moved_endpoint(&before, Duration::from_secs(20));
    let new_url = after["control"]
        .as_str()
        .expect("a control url")
        .to_string();
    let new_token = after["control_token"]
        .as_str()
        .expect("a declared control token")
        .to_string();
    assert_ne!(new_token, old_token, "a new daemon mints a new token");

    // ⭐ THE PROOF: the gated route answers again, on nothing but what the client
    // re-declared. This is what 403'd for the life of the session before.
    let (status, schema) = control_get(&new_url, "/pane/settings", Some(&new_token));
    assert_eq!(
        status, 200,
        "the pane must answer across a handover: {schema}"
    );

    // And it is genuinely a new generation, not the same endpoint under a new
    // name: the old token is refused, and the old port is gone with its daemon.
    let (stale, body) = control_get(&new_url, "/pane/settings", Some(&old_token));
    assert_eq!(stale, 403, "the retired daemon's token must not still work");
    assert_eq!(body["cause"], "token_mismatch", "{body}");
    assert!(
        control_is_dead(&old_url) || old_url == new_url,
        "the retired daemon's listener outlived it: {old_url}"
    );
}

// THE BUG AS THE USER MET IT, and the thing that was missing when they did: a
// client that cannot carry the token is a session whose panes can never open,
// and until now nothing said so. The 403 named neither the cause nor the cure,
// `ychrome status` showed a perfectly healthy row, and `/policy` kept answering
// — so ad blocking worked and the panes did not, with no way to connect the two.
#[test]
fn a_pre_gate_client_is_named_as_such_by_the_refusal_and_by_status() {
    let host = Host::new("pregate");
    let first = host.status();
    host.assert_isolated(&first);

    let registered = host.attach_pre_gate_surface("env-pregate", "oldcli");
    let control_url = registered["control_url"].as_str().expect("a control url");

    // What still works, and is exactly why this hid: the open routes.
    let (status, _) = control_get(control_url, "/policy", None);
    assert_eq!(status, 200, "ad blocking and userscripts keep working");

    // What does not, and what it now says about it.
    let (status, body) = control_get(control_url, "/pane/vault", None);
    assert_eq!(status, 403);
    assert_eq!(body["cause"], "client_predates_control_token", "{body}");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("predates the control-token gate")
            && error.contains("Ctrl+C")
            && error.contains("restarting the daemon does not fix it"),
        "the refusal must carry the cause AND the remedy: {error}"
    );

    // The registry says it too, so this is findable without provoking a 403.
    let status_json = host.status();
    let row = status_json["sessions"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["env_id"] == "env-pregate"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        row["control_token_declared"],
        json!(false),
        "the registry must mark a session whose panes cannot open: {row}"
    );

    // And a human reading `ychrome status` is told, with the one fix that works.
    let printed = String::from_utf8_lossy(&host.run(&["status"]).stdout).to_string();
    assert!(
        printed.contains("[NO PANES]") && printed.contains("env-pregate"),
        "status must name the session: {printed}"
    );
    assert!(
        printed.contains("A daemon restart does NOT fix this"),
        "status must rule out the remedy people reach for first: {printed}"
    );
}
