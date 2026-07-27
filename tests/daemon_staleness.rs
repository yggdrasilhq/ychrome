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

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
