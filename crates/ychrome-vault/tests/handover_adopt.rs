//! The far side of a handover: a real `ychrome-vault agent` process picking up
//! an inherited listener and an inherited session.
//!
//! The `execve` itself cannot be unit-tested — its whole signature is "same pid,
//! new image", which only a live agent can show. Everything else about the
//! crossing CAN be, and is here: a listener fd and a pipe fd created in this
//! process, CLOEXEC cleared on both, inherited by a genuinely separate
//! `ychrome-vault` process, which must adopt the socket and come up UNLOCKED
//! without a master password. `fork`+`exec` inherits fds by exactly the rules
//! `execve` does, so this covers the plumbing; what it does not cover is that
//! the pid is preserved.
//!
//! The payload is spelled out BY HAND below rather than by calling the encoder.
//! A round trip against ourselves would pass even if both halves drifted
//! together; an independently written writer checking the reader is the rule
//! this crate already applies to its crypto.
//!
//! Nothing here touches the real vault: an explicit `--dir`, a synthetic key,
//! and a server URL that refuses instantly so no test ever reaches the network.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

fn temp_dir(tag: &str) -> PathBuf {
    // Short, because a unix socket path must fit in SUN_LEN (~108 bytes) and
    // the usual scratch paths do not.
    let dir = PathBuf::from(format!("/tmp/yv-adopt-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    // A configured but unreachable server: `resync` fails on connect rather
    // than resolving anything, so the test is offline and instant.
    std::fs::write(
        dir.join("config.json"),
        br#"{"server_url":"http://127.0.0.1:1","email":"a@b.c","kdf_type":0,
             "kdf_iterations":600000,"device_id":"test","lock_timeout_secs":0}"#,
    )
    .unwrap();
    dir
}

fn clear_cloexec(fd: RawFd) {
    // SAFETY: fcntl on an fd this process owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        assert!(flags >= 0, "F_GETFD failed");
        assert!(libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) >= 0);
    }
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

/// An anonymous pipe holding `bytes`, with the write end closed and the read end
/// ready to survive an exec — the same shape the agent's `payload_pipe` builds.
fn pipe_holding(bytes: &[u8]) -> OwnedFd {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `pipe` fills both slots or returns non-zero.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe failed");
    // SAFETY: both fds come from the successful `pipe` above.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let mut writer = std::fs::File::from(write);
    writer.write_all(bytes).unwrap();
    drop(writer);
    clear_cloexec(read.as_raw_fd());
    read
}

struct Adopted {
    child: std::process::Child,
    dir: PathBuf,
}

impl Adopted {
    /// Start a real agent on an inherited listener and an inherited payload.
    fn start(dir: &Path, payload: &[u8]) -> Adopted {
        let listener = UnixListener::bind(dir.join("agent.sock")).unwrap();
        clear_cloexec(listener.as_raw_fd());
        let payload_fd = pipe_holding(payload);

        let child = std::process::Command::new(env!("CARGO_BIN_EXE_ychrome-vault"))
            .arg("agent")
            .arg("--dir")
            .arg(dir)
            .arg("--adopt-listener")
            .arg(listener.as_raw_fd().to_string())
            .arg("--adopt-payload")
            .arg(payload_fd.as_raw_fd().to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the agent binary runs");

        // Both fds are the child's now. Dropping OUR copies matters: while this
        // process still holds the listener, a connect would succeed even if the
        // child were dead, and the test would hang instead of failing.
        drop(listener);
        drop(unsafe { OwnedFd::from_raw_fd(payload_fd.into_raw_fd()) });
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

    /// Kill the agent and return what it said on stderr.
    fn finish(mut self) -> String {
        self.child.kill().ok();
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            pipe.read_to_string(&mut stderr).ok();
        }
        self.child.wait().ok();
        std::fs::remove_dir_all(&self.dir).ok();
        stderr
    }
}

// THE point of the whole handover: a new process, given nothing but two
// inherited fds, serves the same socket with the vault still UNLOCKED. No
// master password was typed, and none could have been — stdin is /dev/null.
#[test]
fn an_adopted_agent_serves_the_inherited_socket_still_unlocked() {
    let dir = temp_dir("ok");
    let agent = Adopted::start(
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
}

// A payload that will not decode must leave the vault LOCKED. Half-adopting it
// would produce an agent that reports "unlocked" and fails every MAC check, and
// the user would have no idea why — the loud, cheap failure is the right one.
#[test]
fn a_corrupt_payload_leaves_the_successor_locked_rather_than_wrong() {
    let dir = temp_dir("corrupt");
    let mut bytes = payload_bytes(&[0x11u8; 64], "bearer", None, 0);
    bytes.truncate(40); // a torn write: magic intact, key half there
    let agent = Adopted::start(&dir, &bytes);

    let status = agent.status();
    assert_eq!(
        status["state"], "locked",
        "a truncated key must never read as an unlock: {status}"
    );

    let stderr = agent.finish();
    assert!(stderr.contains("unreadable"), "{stderr}");
    assert!(stderr.contains("LOCKED"), "{stderr}");
}
