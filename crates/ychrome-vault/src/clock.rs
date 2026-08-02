//! Is this host's clock fit to mint a one-time code?
//!
//! ⛔ **The failure this module exists to end.** On 2026-07-31 manin's clock was
//! **72 seconds slow**, and dev and a headless host are LXCs that share its `CLOCK_REALTIME`.
//! A TOTP window is 30 s and servers accept at most ±1 window, so
//! `ychrome-vault totp` on those hosts emitted a code that was **always wrong**
//! while looking perfectly well formed: six digits, stable within its window,
//! no error. It cost two live-brokerage 2FA attempts before anyone suspected the
//! clock. Waiting never helps — a constant offset means the host can never drift
//! into the right window.
//!
//! That is a lie-of-success, the same family as a click that reports a dispatch
//! and hits nothing. The rule this module enforces is the operator's: **refuse,
//! do not guess.**
//!
//! ⚠ **THE INSTRUMENT THAT LIED, so it is not consulted here.** chrony reported
//! `Last offset : -0.000112349 seconds` and `RMS offset : 0.0003 s` — it
//! believed it was tracking *perfectly* while system time was 72 s out. Reading
//! a time daemon's opinion of itself is how the skew went unnoticed for a day.
//! What DID read the truth was `timedatectl`'s `System clock synchronized: no`,
//! and that field is nothing but the kernel's own NTP state: `adjtimex(2)`
//! answering `TIME_ERROR` with `STA_UNSYNC` set. So this module reads the
//! kernel, not a daemon, and never a wall-clock comparison against itself.
//!
//! **Why not ask a server for the time.** A code is minted from a cached vault
//! with no network in the path, and putting an HTTP round trip inside the mint
//! would buy a fresh reference at the cost of latency, a new failure mode, and a
//! request that says "somebody is authenticating right now" to whoever is
//! watching. The kernel already knows; asking it costs a syscall.
//!
//! **Where this is deliberately silent.** On a platform whose NTP state we
//! cannot read, [`state`] answers [`Sync::Unknown`] and [`check`] ALLOWS. A
//! refusal has to be evidence-backed: refusing everywhere we cannot measure
//! would be its own lie, in the opposite direction.

use std::fmt;

/// What the kernel says about whether its clock is disciplined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    /// The kernel holds a disciplined clock: `STA_UNSYNC` clear and the clock
    /// state is not `TIME_ERROR`.
    Synchronized,
    /// The kernel says its clock is NOT disciplined. This is the state the
    /// poisoned host was in while chrony called itself healthy.
    Unsynchronized,
    /// We could not ask. Not a verdict, and never a refusal.
    Unknown,
}

impl Sync {
    fn as_str(self) -> &'static str {
        match self {
            Sync::Synchronized => "synchronized",
            Sync::Unsynchronized => "unsynchronized",
            Sync::Unknown => "unknown",
        }
    }
}

/// The kernel's account of its own clock, as read once.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockState {
    pub sync: Sync,
    /// The kernel's own upper bound on its error, in seconds, when it has one.
    /// Reported for the operator; it is NOT what the verdict turns on — the
    /// kernel caps this value, so a host 72 s out reports far less than 72.
    pub max_error_secs: Option<f64>,
    /// Which mechanism answered, so a reader can tell "measured" from "assumed".
    pub source: &'static str,
}

impl ClockState {
    /// The wire/report shape. One owner, so the CLI, the agent op and a refusal
    /// message can never describe the same clock three ways.
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "sync": self.sync.as_str(),
            "max_error_secs": self.max_error_secs,
            "source": self.source,
        })
    }
}

/// The refusal. Carries the state it refused on, so the caller can report the
/// evidence rather than just the verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct ClockUntrusted {
    pub state: ClockState,
    pub period_secs: u64,
}

impl fmt::Display for ClockUntrusted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "clock_unsynchronized (this host's kernel reports its clock is not disciplined, \
             and a {}s TOTP window tolerates no more than one window of skew — a code minted \
             here would be confidently wrong. Fix the host clock (chronyd/systemd-timesyncd; \
             on an LXC that is the HYPERVISOR's clock, not the container's), then retry. \
             ⚠ Read `timedatectl`'s `System clock synchronized:` line or `chronyc tracking`'s \
             `System time :` line — chrony's `Last offset`/`RMS offset` report perfect \
             tracking on a host that is 72s out. To override once, pass --ignore-clock.)",
            self.period_secs
        )
    }
}

impl std::error::Error for ClockUntrusted {}

/// Read the kernel's NTP state. Pure of policy: it reports, it does not judge.
pub fn state() -> ClockState {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `adjtimex` with `modes == 0` is a read-only query. The struct
        // is zeroed first so no uninitialised field is ever handed to the
        // kernel, and it is written only by the kernel afterwards.
        let mut tx: libc::timex = unsafe { std::mem::zeroed() };
        tx.modes = 0;
        let rc = unsafe { libc::adjtimex(&mut tx) };
        if rc >= 0 {
            // BOTH conditions, because they can disagree: `STA_UNSYNC` is the
            // flag a time daemon clears when it is disciplining the clock, and
            // `TIME_ERROR` is the clock state the kernel reports when it is not
            // synchronised. systemd reads the same pair for "System clock
            // synchronized".
            const TIME_ERROR: libc::c_int = 5;
            let unsynced = (tx.status & libc::STA_UNSYNC) != 0 || rc == TIME_ERROR;
            return ClockState {
                sync: if unsynced {
                    Sync::Unsynchronized
                } else {
                    Sync::Synchronized
                },
                max_error_secs: Some(tx.maxerror as f64 / 1_000_000.0),
                source: "adjtimex",
            };
        }
    }
    ClockState {
        sync: Sync::Unknown,
        max_error_secs: None,
        source: "unavailable",
    }
}

/// The ONE policy: may this host mint a code for a `period_secs` window?
///
/// `Unknown` allows. That is not laxity, it is the same rule the rest of this
/// codebase holds itself to — a refusal must name evidence, and "I could not
/// measure" is not evidence of a bad clock.
pub fn check(period_secs: u64) -> Result<ClockState, ClockUntrusted> {
    judge(state(), period_secs)
}

/// The policy, separated from the reading of it.
///
/// Split so the verdict is testable on a host whose clock is FINE — which every
/// host in this fleet now is, since manin was stepped. A rule that can only be
/// exercised by breaking the operator's clock is a rule nobody will ever
/// exercise, and this one exists precisely because it went unnoticed for a day.
pub fn judge(state: ClockState, period_secs: u64) -> Result<ClockState, ClockUntrusted> {
    #[cfg(test)]
    JUDGEMENTS.with(|count| count.set(count.get() + 1));
    match state.sync {
        Sync::Unsynchronized => Err(ClockUntrusted { state, period_secs }),
        Sync::Synchronized | Sync::Unknown => Ok(state),
    }
}

#[cfg(test)]
thread_local! {
    static JUDGEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many clock verdicts THIS THREAD has asked for.
///
/// ⛔ It exists because of a gap no output assertion can close: on a healthy
/// host a gated mint and an ungated one produce the same six digits, so a test
/// that reads the code cannot tell whether the question was ever asked. Two
/// call sites — `Totp::now` and the agent's `totp` op — survived a deliberate
/// "quietly stop asking" mutation for exactly that reason. Counting the
/// question is the only thing that goes red.
///
/// Thread-local so parallel tests cannot see each other's counts, which makes
/// the assertion exact rather than a `>=`.
#[cfg(test)]
pub(crate) fn judgements() -> usize {
    JUDGEMENTS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn state_that_is(sync: Sync) -> ClockState {
        ClockState {
            sync,
            max_error_secs: Some(0.5),
            source: "test",
        }
    }

    /// The verdict rule, exhaustively, driving the SHIPPING function: only a
    /// POSITIVE measurement of a bad clock refuses.
    #[test]
    fn only_a_measured_bad_clock_refuses() {
        assert!(judge(state_that_is(Sync::Synchronized), 30).is_ok());
        assert!(
            judge(state_that_is(Sync::Unknown), 30).is_ok(),
            "a platform we cannot measure must not be refused — that is a guess \
             in the other direction"
        );
        assert!(judge(state_that_is(Sync::Unsynchronized), 30).is_err());
    }

    /// `check` is `judge` over the LIVE reading, and nothing else. Mutating it
    /// to judge a fabricated state — the shape a "temporarily disable the gate"
    /// patch takes — makes this go red.
    #[test]
    fn check_judges_what_the_kernel_actually_said() {
        let live = state();
        let direct = judge(live, 30);
        let through_check = check(30);
        assert_eq!(direct.is_ok(), through_check.is_ok());
        assert_eq!(through_check.map(|s| s.sync).unwrap_or(live.sync), live.sync);
    }

    /// The refusal has to be actionable, and it has to carry the trap that cost
    /// the session: chrony's offset lines report health on a broken clock.
    #[test]
    fn the_refusal_names_the_remedy_and_the_lying_instrument() {
        let message = ClockUntrusted {
            state: ClockState {
                sync: Sync::Unsynchronized,
                max_error_secs: Some(16.0),
                source: "adjtimex",
            },
            period_secs: 30,
        }
        .to_string();
        assert!(message.starts_with("clock_unsynchronized"), "{message}");
        assert!(message.contains("30s"), "{message}");
        assert!(
            message.contains("timedatectl") && message.contains("System time :"),
            "the remedy must name the field that tells the truth: {message}"
        );
        assert!(
            message.contains("RMS offset"),
            "the instrument that lied must be named, or the next reader repeats \
             the mistake: {message}"
        );
        assert!(message.contains("--ignore-clock"), "{message}");
    }

    /// The report shape is one owner's, and it says which mechanism answered so
    /// "measured" is distinguishable from "assumed".
    #[test]
    fn the_report_says_which_mechanism_answered() {
        let json = state().to_json();
        assert!(json["sync"].is_string());
        assert!(json["source"].is_string());
        assert_ne!(
            json["source"], "",
            "a state with no named source cannot be audited"
        );
    }

    /// On Linux the state must come from the kernel, not from a default. This
    /// goes red if the `adjtimex` read is ever dropped or stubbed.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reads_the_kernel_rather_than_assuming() {
        let state = state();
        assert_eq!(
            state.source, "adjtimex",
            "the kernel's NTP state is the only instrument that read the truth \
             during the 72s incident"
        );
        assert!(
            state.max_error_secs.is_some(),
            "a kernel read that reports no error bound did not happen"
        );
        assert_ne!(state.sync, Sync::Unknown);
    }
}
