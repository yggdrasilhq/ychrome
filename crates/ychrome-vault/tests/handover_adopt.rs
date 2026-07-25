//! The far side of a handover: a real `ychrome-vault agent` process picking up
//! an inherited listener and an inherited session.
//!
//! The `execve` itself cannot be unit-tested — its whole signature is "same pid,
//! new image", which only a live agent can show. Everything else about the
//! crossing CAN be, and is here: a listener fd and a payload pipe prepared by
//! the PRODUCTION arming code, inherited by a genuinely separate `ychrome-vault`
//! process, which must adopt the socket and come up UNLOCKED without a master
//! password. `fork`+`exec` inherits fds by exactly the rules `execve` does, so
//! this covers the plumbing; what it does not cover is that the pid is preserved.
//!
//! The fd plumbing goes through `agent::arm_handover` deliberately. This file
//! used to clear CLOEXEC itself before spawning, which meant the production
//! clear was never on the path under test: the listener half could be deleted
//! outright and every test here still passed, while the live agent it describes
//! would have died on its next handover.
//!
//! The payload is still spelled out BY HAND below rather than by calling the
//! encoder. A round trip against ourselves would pass even if both halves
//! drifted together; an independently written writer checking the reader is the
//! rule this crate already applies to its crypto. Only the plumbing is shared.
//!
//! Nothing here touches the real vault: an explicit `--dir`, a synthetic key,
//! and a server URL that refuses instantly so no test ever reaches the network.

use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use ychrome_vault::agent::ExecPlan;

fn temp_dir(tag: &str, lock_timeout_secs: u64) -> PathBuf {
    // Short, because a unix socket path must fit in SUN_LEN (~108 bytes) and
    // the usual scratch paths do not.
    let dir = PathBuf::from(format!("/tmp/yv-adopt-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    // A configured but unreachable server: `resync` fails on connect rather
    // than resolving anything, so the test is offline and instant.
    std::fs::write(
        dir.join("config.json"),
        format!(
            r#"{{"server_url":"http://127.0.0.1:1","email":"a@b.c","kdf_type":0,
                "kdf_iterations":600000,"device_id":"test",
                "lock_timeout_secs":{lock_timeout_secs}}}"#
        ),
    )
    .unwrap();
    dir
}

/// An fd number that CANNOT be open: the kernel never allocates one at or above
/// `RLIMIT_NOFILE`, and the limit is inherited across the spawn, so it is closed
/// in the child too. Picking "some big number" would be a guess about another
/// process's fd table.
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

/// The handover framing, written independently of `agent.rs`'s encoder:
/// magic ‖ 64-byte user key ‖ u32-prefixed access token ‖ refresh flag
/// (‖ u32-prefixed refresh token) ‖ u64 idle seconds, little-endian throughout.
fn payload_bytes(user_key: &[u8; 64], access: &str, refresh: Option<&str>, idle: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"YCHVHO01");
    out.extend_from_slice(user_key);
    out.extend_from_slice(&(access.len() as u32).to_le_bytes());
    out.extend_from_slice(access.as_bytes());
    match refresh {
        Some(token) => {
            out.push(1);
            out.extend_from_slice(&(token.len() as u32).to_le_bytes());
            out.extend_from_slice(token.as_bytes());
        }
        None => out.push(0),
    }
    out.extend_from_slice(&idle.to_le_bytes());
    out
}

/// Prepare the crossing with the SAME code the live `handover` op runs: the
/// payload onto a pipe, CLOEXEC cleared on both fds. Not a copy of it.
fn arm(dir: &Path, listener: &UnixListener, payload: &[u8]) -> ExecPlan {
    ychrome_vault::agent::arm_handover(
        PathBuf::from(env!("CARGO_BIN_EXE_ychrome-vault")),
        dir.to_path_buf(),
        listener.as_raw_fd(),
        payload,
    )
    .expect("arming the handover")
}

struct Adopted {
    child: std::process::Child,
    dir: PathBuf,
}

/// The ONLY kill path, because `std::process::Child` has no killing `Drop` and a
/// panic between `start()` and `finish()` would otherwise leave a real agent
/// running on a real socket. That is not hypothetical: an assertion failure in
/// this file orphaned an agent for 1.8 hours on the dev box, holding its
/// listener and an inherited payload pipe.
impl Drop for Adopted {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

impl Adopted {
    /// Start a real agent on an inherited listener and an inherited payload.
    fn start(dir: &Path, payload: &[u8]) -> Adopted {
        let listener = UnixListener::bind(dir.join("agent.sock")).unwrap();
        let plan = arm(dir, &listener, payload);
        let agent = Adopted::spawn(dir, plan.listener_fd, plan.payload.as_raw_fd());
        // Both fds are the child's now. Dropping OUR copies matters: while this
        // process still holds the listener, a connect would succeed even if the
        // child were dead, and the test would hang instead of failing.
        drop(listener);
        drop(plan);
        agent
    }

    /// Start a real agent on the fd NUMBERS given, whatever they point at.
    fn spawn(dir: &Path, listener_fd: RawFd, payload_fd: RawFd) -> Adopted {
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_ychrome-vault"))
            .arg("agent")
            .arg("--dir")
            .arg(dir)
            .arg("--adopt-listener")
            .arg(listener_fd.to_string())
            .arg("--adopt-payload")
            .arg(payload_fd.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the agent binary runs");
        Adopted {
            child,
            dir: dir.to_path_buf(),
        }
    }

    /// The adopted agent's `status`, waited for — it re-pulls the ciphers before
    /// it accepts, so the first connection can land while it is still starting.
    fn status(&self) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match ychrome_vault_proto::request_with_timeout(
                &self.dir,
                &json!({"op": "status"}),
                Duration::from_secs(20),
            ) {
                Ok(status) => return status,
                Err(error) if Instant::now() >= deadline => panic!("no answer: {error}"),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// Stop the agent and return what it said on stderr. The kill is what closes
    /// the pipe, so the read below terminates; the dir and the reap are `Drop`'s.
    fn finish(&mut self) -> String {
        self.child.kill().ok();
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            pipe.read_to_string(&mut stderr).ok();
        }
        self.child.wait().ok();
        stderr
    }
}

/// Is a pid still a live `ychrome-vault agent`? Compared against the cmdline,
/// not just `/proc/<pid>`, so a recycled pid cannot read as a leak.
fn still_an_agent(pid: u32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|raw| String::from_utf8_lossy(&raw).contains("--adopt-listener"))
        .unwrap_or(false)
}

// THE point of the whole handover: a new process, given nothing but two
// inherited fds, serves the same socket with the vault still UNLOCKED. No
// master password was typed, and none could have been — stdin is /dev/null.
#[test]
fn an_adopted_agent_serves_the_inherited_socket_still_unlocked() {
    let dir = temp_dir("ok", 0);
    let mut agent = Adopted::start(
        &dir,
        &payload_bytes(&[0x5au8; 64], "bearer-abc", Some("refresh-xyz"), 42),
    );

    let status = agent.status();
    assert_eq!(
        status["state"], "unlocked",
        "the successor must inherit the unlock, not ask for it: {status}"
    );
    // The ciphers are NOT carried across; the successor re-pulls them, and this
    // server refuses instantly, so the count is 0 and the agent says so rather
    // than pretending the vault is there.
    assert_eq!(status["item_count"], 0);
    assert_eq!(status["email"], "a@b.c", "the config was read from --dir");

    let stderr = agent.finish();
    assert!(
        stderr.contains("adopted the keys but could not re-pull"),
        "the successor must say the vault is empty, not hide it: {stderr}"
    );
    assert!(stderr.contains("ychrome-vault sync"), "{stderr}");
    // THE no-unbind-window property, and the only thing that separates a real
    // handover from a restart: it served the socket it INHERITED. A successor
    // that fell back to binding its own would still answer "unlocked", so
    // without this the CLOEXEC clear could go missing and this test would not
    // notice.
    assert!(
        !stderr.contains("binding a fresh socket"),
        "the inherited listener was not used, so the socket WAS unbound: {stderr}"
    );
}

// A payload that will not decode must leave the vault LOCKED. Half-adopting it
// would produce an agent that reports "unlocked" and fails every MAC check, and
// the user would have no idea why — the loud, cheap failure is the right one.
#[test]
fn a_corrupt_payload_leaves_the_successor_locked_rather_than_wrong() {
    let dir = temp_dir("corrupt", 0);
    let mut bytes = payload_bytes(&[0x11u8; 64], "bearer", None, 0);
    bytes.truncate(40); // a torn write: magic intact, key half there
    let mut agent = Adopted::start(&dir, &bytes);

    let status = agent.status();
    assert_eq!(
        status["state"], "locked",
        "a truncated key must never read as an unlock: {status}"
    );

    let stderr = agent.finish();
    assert!(stderr.contains("unreadable"), "{stderr}");
    assert!(stderr.contains("LOCKED"), "{stderr}");
}

// The successor inherits the outgoing agent's IDLE CLOCK, not a fresh one.
// Restarting it silently extends the unlock past the timeout the user set: hand
// a session that has already been idle 60s to an agent whose policy is 30s and
// it must lock on its FIRST tick, not 30 seconds after coming up.
//
// (This reads the monotonic clock 60s into the past, which needs 60s of uptime.
// A machine cannot build this crate in less.)
#[test]
fn an_adopted_agent_inherits_the_idle_clock_instead_of_restarting_it() {
    let dir = temp_dir("idle", 30);
    let mut agent = Adopted::start(&dir, &payload_bytes(&[0x77u8; 64], "bearer", None, 60));

    // The idle thread ticks every 5s, so an inherited 60s of idle is already
    // past the 30s policy on the first look; a clock that restarted here would
    // hold the unlock until 30s from now, well past this deadline.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = agent.status();
        if status["state"] == "locked" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "60s of inherited idle against a 30s timeout must lock on the first tick, \
             but the vault is still {}",
            status["state"]
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    let stderr = agent.finish();
    // Locking at start-up (never having adopted) would satisfy the poll above
    // for the wrong reason. This message is printed ONLY by the idle thread,
    // and only about a vault that WAS unlocked.
    assert!(
        stderr.contains("adopted the keys"),
        "it must have adopted the session first: {stderr}"
    );
    assert!(stderr.contains("idle 30s"), "{stderr}");
    assert!(stderr.contains("vault locked"), "{stderr}");
}

// An inherited fd number that is not what the flag promises used to be fatal:
// `UnixListener::from_raw_fd` validates nothing, so a closed number ABORTED the
// process ("IO Safety violation", SIGABRT) before a line of our code ran. On a
// one-way door that is a dead pid, an unbound socket and a master password
// retyped on every host. It must refuse the fd and recover instead.
#[test]
fn an_inherited_fd_that_is_not_what_it_claims_is_refused_not_aborted_on() {
    // Nothing bound, and two numbers that cannot be open in the child.
    let dir = temp_dir("badfd", 0);
    let mut agent = Adopted::spawn(&dir, never_open_fd(), never_open_fd());
    let status = agent.status();
    assert_eq!(
        status["state"], "locked",
        "no listener means no adopted session, but it must still SERVE: {status}"
    );
    let stderr = agent.finish();
    assert!(stderr.contains("not an open file descriptor"), "{stderr}");
    assert!(
        stderr.contains("binding a fresh socket"),
        "an agent the user can unlock beats no agent at all: {stderr}"
    );

    // A GOOD payload with a bad listener loses the seamless socket and nothing
    // else. The session and the socket are separate things, and taking the
    // unlock away too would charge the user a master password for a failure
    // that did not touch the keys. (The scratch listener is armed on its own
    // path so the agent's own socket is free for the fresh bind — no race with
    // this process still holding it.)
    let dir = temp_dir("badlistener", 0);
    let scratch = UnixListener::bind(dir.join("arm.sock")).unwrap();
    let plan = arm(
        &dir,
        &scratch,
        &payload_bytes(&[0x55u8; 64], "bearer", None, 0),
    );
    let mut agent = Adopted::spawn(&dir, never_open_fd(), plan.payload.as_raw_fd());
    drop(scratch);
    drop(plan);

    let status = agent.status();
    assert_eq!(
        status["state"], "unlocked",
        "a lost listener must not cost the unlock as well: {status}"
    );
    let stderr = agent.finish();
    assert!(stderr.contains("binding a fresh socket"), "{stderr}");
    assert!(stderr.contains("adopted the keys"), "{stderr}");

    // A real listener with a bad payload keeps the socket and serves LOCKED —
    // the cheaper half of the same rule.
    let dir = temp_dir("badpayload", 0);
    let listener = UnixListener::bind(dir.join("agent.sock")).unwrap();
    let plan = arm(
        &dir,
        &listener,
        &payload_bytes(&[0x33u8; 64], "bearer", None, 0),
    );
    let mut agent = Adopted::spawn(&dir, plan.listener_fd, never_open_fd());
    drop(listener);
    drop(plan);

    let status = agent.status();
    assert_eq!(status["state"], "locked", "{status}");
    let stderr = agent.finish();
    assert!(stderr.contains("unreadable"), "{stderr}");
    assert!(
        !stderr.contains("binding a fresh socket"),
        "the inherited listener was fine and must have been kept: {stderr}"
    );
}

// A failing assertion must not leave a live agent behind. `Child` has no killing
// `Drop`, so before this harness had one, any panic between `start()` and
// `finish()` orphaned a real `ychrome-vault agent` onto init — still holding its
// bound listener and, from an iteration that had not yet read it, the payload
// pipe with the session key in it.
#[test]
fn a_failing_assertion_kills_the_agent_instead_of_orphaning_it() {
    let dir = temp_dir("orphan", 0);
    let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let recorded = pid.clone();
    let started = dir.clone();

    // The panic below is EXPECTED and is printed by the default hook; it stands
    // in for the assertion failure that leaked an agent for 1.8 hours.
    let outcome = std::panic::catch_unwind(move || {
        let agent = Adopted::start(&started, &payload_bytes(&[0x44u8; 64], "bearer", None, 0));
        recorded.store(agent.child.id(), std::sync::atomic::Ordering::SeqCst);
        assert_eq!(agent.status()["state"], "unlocked", "it is genuinely up");
        panic!("the assertion a real test would have failed on");
    });
    assert!(outcome.is_err(), "the panic must have happened");

    let pid = pid.load(std::sync::atomic::Ordering::SeqCst);
    assert_ne!(pid, 0, "the agent never started, so nothing was proven");
    assert!(
        !still_an_agent(pid),
        "pid {pid} outlived the panic: a failing assertion must not leak an agent \
         holding a live socket and a session key"
    );
    assert!(
        !dir.exists(),
        "the socket dir must go with it: {}",
        dir.display()
    );
}
