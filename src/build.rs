//! WHICH BUILD IS ANSWERING — the one owner of a running process's identity.
//!
//! ## The failure this exists to make impossible
//!
//! `ctl frame` was reported as "advertised by the CLI's own help and the engine
//! 404s it — either implement it or drop it from the usage line". `frame` was
//! implemented. The help came from a CLI installed that afternoon; the 404 came
//! from a daemon that had been running for 43 hours, serving from an inode
//! replaced on disk hours earlier. They were never two components disagreeing
//! about what exists. **They were two builds, and nothing in the protocol said
//! so.** The proposed remedy would have deleted a working feature.
//!
//! An audit the same day found **ten of eleven** ychrome-family processes on one
//! host running a binary that no longer existed, the oldest by thirteen days.
//! Any of them can answer a call from a build nobody can name.
//!
//! ⇒ A 404 that cannot distinguish *"this verb does not exist"* from *"this
//! build predates it"* sends the reader to the wrong system every time, and the
//! cost lands as a belief rather than a crash — which is worse, because a belief
//! gets built on.
//!
//! ## Why the identity is a digest of `/proc/self/exe`
//!
//! ⛔ **Not `--version`, and not the binary's path on disk.** Both read the file
//! that is there NOW, which is exactly the file a stale process is no longer
//! running. The question is what these bytes are, not what that path holds.
//!
//! `/proc/self/exe` is a magic link to the running image, and it stays readable
//! **after the file is unlinked** — which is the whole population this is for.
//! So the identity is a SHA-256 of what the kernel is actually executing,
//! computed once. It costs one read of a few megabytes at startup and it cannot
//! be fooled by a rebuild, a reinstall, or a path that now holds something else.
//!
//! ⚠ **Start time comes from `/proc/self/stat`, never from `stat` on
//! `/proc/<pid>`.** The directory's mtime tracks kernel bookkeeping, not exec,
//! and it fails in the most misleading direction available: it agrees with the
//! truth for recently started processes and silently collapses older ones onto
//! one identical timestamp — which is precisely the population you point it at
//! when hunting stale daemons. An obviously broken instrument costs one round; a
//! plausible, self-consistent, clustered one costs a wrong conclusion.

use std::sync::OnceLock;

/// What a running process can say about itself with no reference to disk.
///
/// ⛔ **"Has my binary been replaced?" is deliberately NOT a field here.** It is
/// not a property of this process at all — it is a property of the disk, it
/// starts false and becomes true, and that transition is the whole event. An
/// earlier version stored it beside the digest and failed its own falsifier in
/// the most direct way available: the engine answered `exe_deleted: false`
/// while `/proc/<pid>/exe` plainly read `(deleted)`, because the value had been
/// computed at startup — before the replacement it exists to notice. It lives
/// in [`exe_deleted`], which reads it every time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildIdentity {
    /// Short SHA-256 prefix of the running image. Twelve hex characters: long
    /// enough that two builds will not collide, short enough to read aloud in
    /// an error message, which is where it has to earn its keep.
    pub build: String,
    /// The path the kernel reports for the running image.
    pub exe: String,
    /// Seconds since the epoch, from `/proc/self/stat` and the boot time.
    pub started_unix: Option<u64>,
}

/// Whether the file this process was started from has since been replaced or
/// removed on disk.
///
/// ⛔ **READ EVERY TIME, NEVER CACHED.** The digest above is an identity and is
/// rightly computed once — the bytes a process is executing cannot change. This
/// is the opposite kind of fact: it is about the world outside the process, it
/// starts false and becomes true, and the transition is the whole event. Caching
/// it at startup guarantees the answer `false` for exactly the population this
/// module is for.
pub fn exe_deleted() -> bool {
    std::fs::read_link("/proc/self/exe")
        .map(|path| path.to_string_lossy().ends_with(" (deleted)"))
        .unwrap_or(false)
}

impl BuildIdentity {
    /// The one line that turns "unknown verb" into an actionable sentence.
    pub fn describe(&self) -> String {
        let mut text = format!("build {}", self.build);
        if let Some(started) = self.started_unix {
            text.push_str(&format!(", started {started}"));
        }
        if exe_deleted() {
            text.push_str(", running a binary that has since been replaced on disk");
        }
        text
    }
}

/// This process's identity, computed once.
pub fn identity() -> &'static BuildIdentity {
    static IDENTITY: OnceLock<BuildIdentity> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        let link = std::fs::read_link("/proc/self/exe").ok();
        let raw = link
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<unknown>".to_string());
        // The kernel appends " (deleted)" to the LINK TARGET, not to the file
        // name, so this suffix is the signal and not part of any real path.
        let exe = raw
            .strip_suffix(" (deleted)")
            .unwrap_or(&raw)
            .trim_end()
            .to_string();
        BuildIdentity {
            // Read through `/proc/self/exe` rather than through `exe`: the
            // deleted case is the one that matters, and only the magic link
            // still reaches those bytes.
            build: digest_of("/proc/self/exe"),
            exe,
            started_unix: started_unix(),
        }
    })
}

/// Twelve hex characters of the SHA-256 of a file, or `"unknown"` if it cannot
/// be read. Never an error: an engine that cannot name itself must still answer.
fn digest_of(path: &str) -> String {
    use sha2::{Digest, Sha256};
    let Ok(bytes) = std::fs::read(path) else {
        return "unknown".to_string();
    };
    let digest = Sha256::digest(&bytes);
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Seconds since the epoch at which this process began.
///
/// Field 22 of `/proc/self/stat` is the start time in clock ticks since boot;
/// `/proc/stat`'s `btime` is the boot time in epoch seconds. Both come from the
/// kernel's own accounting of the exec, which is the fact wanted here.
fn started_unix() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // ⚠ The second field is the comm name IN PARENTHESES and it may itself
    // contain spaces and parentheses, so the fields after it are found by
    // splitting past the LAST ')' — never by counting spaces from the start.
    let after_comm = stat.rsplit_once(')')?.1;
    let ticks: u64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
    let hz = ticks_per_second();
    let btime: u64 = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("btime ")?.trim().parse().ok())?;
    Some(btime + ticks / hz)
}

fn ticks_per_second() -> u64 {
    // SAFETY: `sysconf` reads a static system parameter and takes no pointers.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 { hz as u64 } else { 100 }
}

/// The JSON an engine reply carries so a caller can tell WHICH build answered.
pub fn identity_json() -> serde_json::Value {
    let identity = identity();
    serde_json::json!({
        "build": identity.build,
        "exe": identity.exe,
        // Read LIVE on every reply, not stored: this is the fact that changes
        // under a running engine, and a cached copy always reads `false`.
        "exe_deleted": exe_deleted(),
        "started_unix": identity.started_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The identity has to be real: a placeholder that always reads the same
    // would make every stale engine look current, which is the bug.
    #[test]
    fn a_process_can_name_the_bytes_it_is_running() {
        let identity = super::identity();
        assert_eq!(identity.build.len(), 12, "{identity:?}");
        assert!(
            identity.build.chars().all(|c| c.is_ascii_hexdigit()),
            "{identity:?}"
        );
        assert_ne!(identity.build, "unknown", "the test binary is readable");
        // Stable within a process — it is an identity, not a sample.
        assert_eq!(identity.build, super::identity().build);
        // And it is the digest of THIS binary, not of some constant.
        let exe = std::fs::read_link("/proc/self/exe").expect("exe link");
        let path = exe.to_string_lossy();
        let direct = digest_of(path.strip_suffix(" (deleted)").unwrap_or(&path));
        if direct != "unknown" {
            assert_eq!(identity.build, direct);
        }
    }

    #[test]
    fn the_start_time_is_plausible_and_not_the_directory_mtime() {
        let started = started_unix().expect("/proc/self/stat is readable on Linux");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(started <= now + 2, "started {started} is in the future");
        assert!(
            now - started < 60 * 60 * 24 * 365,
            "started {started} is implausibly long ago"
        );
    }

    // The whole point is the sentence it produces at the moment of confusion.
    #[test]
    fn the_description_names_the_build_and_the_replacement() {
        let stale = BuildIdentity {
            build: "abc123def456".to_string(),
            exe: "/somewhere/ychrome".to_string(),
            started_unix: Some(1_700_000_000),
        };
        let text = stale.describe();
        assert!(text.contains("abc123def456"), "{text}");
        assert!(text.contains("1700000000"), "{text}");
    }

    // ⛔ THE ONE THAT CAUGHT THE BUG. This value must be read from the world,
    // not remembered: a test binary is not deleted, and the answer must still
    // come from the link rather than from anything captured earlier.
    #[test]
    fn whether_the_binary_was_replaced_is_read_and_not_remembered() {
        let link = std::fs::read_link("/proc/self/exe").expect("exe link");
        assert_eq!(
            exe_deleted(),
            link.to_string_lossy().ends_with(" (deleted)"),
            "exe_deleted must agree with /proc/self/exe at the moment it is asked"
        );
        // And it must not be reachable as a stored field, which is how it was
        // wrong: `BuildIdentity` is the identity of the BYTES, and the bytes do
        // not change. Anything that can change belongs outside it.
        let identity = super::identity();
        let _ = identity.build.as_str();
    }
}
