//! The engine's rendering SUBSTRATE — the one owner of "what actually draws a
//! headless page".
//!
//! `docs/agent-engine.md` §3 names WPEPlatform's `WPEDisplayHeadless` as the
//! substrate. On Debian's build of WPE WebKit 2.52.5 that API **does not
//! exist**, so §9's sanctioned fallback runs instead: WebKitGTK views on a
//! headless display server, behind this same seam. `probe_wpe` is a live probe
//! rather than a claim, so the day the packages carry WPEPlatform the engine
//! reports it and the swap is a code change here and nowhere else.
//!
//! Everything above this module speaks page verbs (`open`/`goto`/`shot`/
//! `eval`/`input`) and never names a substrate. That is the whole point of the
//! seam — see the risk register row "WPEPlatform headless API gaps at 2.52".

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// The substrates the engine knows how to drive. Exactly one is selected per
/// engine process; the choice is probed, never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Substrate {
    /// The spec's intended substrate: `WPEDisplayHeadless` + `WPEWebView`, no
    /// display server at all. Requires a WPE WebKit built with
    /// `ENABLE_WPE_PLATFORM`.
    WpePlatformHeadless,
    /// The sanctioned fallback: WebKitGTK (`webkit2gtk-4.1`) views on a
    /// headless X display the engine owns and tears down. Same control verbs,
    /// uglier host.
    WebKitGtkHeadless,
}

impl Substrate {
    pub fn id(self) -> &'static str {
        match self {
            Substrate::WpePlatformHeadless => "wpe-platform-headless",
            Substrate::WebKitGtkHeadless => "webkitgtk-headless",
        }
    }
}

/// One substrate's availability, with the evidence that decided it. The
/// evidence rides into the journal so a later reader can tell "we checked and
/// it was absent" from "we never looked".
#[derive(Debug, Clone)]
pub struct Probe {
    pub substrate: Substrate,
    pub available: bool,
    pub reason: String,
    pub evidence: Value,
}

impl Probe {
    pub fn to_json(&self) -> Value {
        json!({
            "substrate": self.substrate.id(),
            "available": self.available,
            "reason": self.reason,
            "evidence": self.evidence,
        })
    }
}

/// Run `pkg-config` and return trimmed stdout on success.
fn pkg_config(args: &[&str]) -> Option<String> {
    let out = Command::new("pkg-config").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Does any header under `dir` DECLARE `needle`? Filename presence alone is a
/// weaker claim than the declaration itself, so we read the text.
fn header_declares(dir: &Path, needle: &str, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if header_declares(&path, needle, depth - 1) {
                return true;
            }
        } else if path.extension().is_some_and(|ext| ext == "h")
            && std::fs::read_to_string(&path).is_ok_and(|text| text.contains(needle))
        {
            return true;
        }
    }
    false
}

/// Live probe for the spec's intended substrate.
///
/// Two independent signals, because either alone can mislead: a
/// `wpe-platform-*.pc` file (how a WPEPlatform-enabled build advertises
/// itself), and a header that actually declares `wpe_display_headless_new`.
/// WPE WebKit can be installed and still ship no WPEPlatform — that is exactly
/// the Debian 2.52.5 case, and the reason this probe exists.
pub fn probe_wpe() -> Probe {
    let wpe_webkit = pkg_config(&["--modversion", "wpe-webkit-2.0"]);
    let platform_pc = ["wpe-platform-2.0", "wpe-platform-1.0"]
        .into_iter()
        .find(|name| pkg_config(&["--exists", name]).is_some());

    let include_dirs: Vec<PathBuf> = pkg_config(&["--cflags-only-I", "wpe-webkit-2.0"])
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|flag| flag.strip_prefix("-I"))
        .map(PathBuf::from)
        .collect();
    let headless_declared = include_dirs
        .iter()
        .any(|dir| header_declares(dir, "wpe_display_headless_new", 4));

    let available = platform_pc.is_some() && headless_declared;
    let reason = if available {
        "WPEPlatform present".to_string()
    } else if wpe_webkit.is_none() {
        "wpe-webkit-2.0 is not installed".to_string()
    } else {
        format!(
            "wpe-webkit-2.0 {} is installed but carries no WPEPlatform: \
             no wpe-platform-*.pc and no header declaring wpe_display_headless_new",
            wpe_webkit.clone().unwrap_or_default()
        )
    };

    Probe {
        substrate: Substrate::WpePlatformHeadless,
        available,
        reason,
        evidence: json!({
            "wpe_webkit_2_0_version": wpe_webkit,
            "wpe_platform_pkgconfig": platform_pc,
            "wpe_display_headless_new_declared": headless_declared,
            "include_dirs": include_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        }),
    }
}

/// Live probe for the fallback substrate: WebKitGTK plus a headless display
/// server the engine can own.
pub fn probe_webkitgtk() -> Probe {
    let webkit = pkg_config(&["--modversion", "webkit2gtk-4.1"]);
    let xvfb = ["/usr/bin/Xvfb", "/usr/local/bin/Xvfb"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.exists());

    let available = webkit.is_some() && xvfb.is_some();
    let reason = match (&webkit, &xvfb) {
        (Some(version), Some(_)) => format!("webkit2gtk-4.1 {version} + Xvfb"),
        (None, _) => "webkit2gtk-4.1 is not installed".to_string(),
        (_, None) => "no Xvfb binary for the headless display".to_string(),
    };

    Probe {
        substrate: Substrate::WebKitGtkHeadless,
        available,
        reason,
        evidence: json!({
            "webkit2gtk_4_1_version": webkit,
            "xvfb": xvfb.map(|p| p.display().to_string()),
        }),
    }
}

/// Every substrate probe, in preference order. The intended substrate is first
/// so a reader of the journal sees WHY the fallback was taken.
pub fn probe_all() -> Vec<Probe> {
    vec![probe_wpe(), probe_webkitgtk()]
}

/// Pick the best available substrate, or explain why none is.
pub fn select() -> Result<(Substrate, Vec<Probe>)> {
    let probes = probe_all();
    match probes.iter().find(|probe| probe.available) {
        Some(probe) => Ok((probe.substrate, probes)),
        None => {
            let why = probes
                .iter()
                .map(|probe| format!("{}: {}", probe.substrate.id(), probe.reason))
                .collect::<Vec<_>>()
                .join("; ");
            bail!("no usable engine substrate on this host — {why}")
        }
    }
}

// ---------------------------------------------------------------------------
// The headless display — an implementation detail the substrate OWNS
// ---------------------------------------------------------------------------

/// A private X display the engine starts and kills. Nothing above the
/// substrate knows this exists: to the control API the engine is "headless",
/// and on the WPEPlatform substrate there is genuinely no display server at
/// all.
pub struct HeadlessDisplay {
    child: Child,
    pub name: String,
    pub width: i32,
    pub height: i32,
}

/// Display numbers below this are left alone: `:0`-`:20` belong to real
/// sessions (xrdp allocates in that range on the fleet hosts) and another
/// agent's Xvfb may already hold a number just above it.
const FIRST_DISPLAY: u32 = 90;
const LAST_DISPLAY: u32 = 160;

impl HeadlessDisplay {
    /// Start Xvfb on the first display number nothing else has claimed.
    ///
    /// Claiming is checked BEFORE spawning (lock file and socket) and again
    /// after (Xvfb exits immediately when the number is taken), because the
    /// check-then-spawn window is a real race on a host where several agents
    /// run at once.
    pub fn start(width: i32, height: i32) -> Result<HeadlessDisplay> {
        let mut last_error = String::new();
        for number in FIRST_DISPLAY..=LAST_DISPLAY {
            if Path::new(&format!("/tmp/.X{number}-lock")).exists()
                || Path::new(&format!("/tmp/.X11-unix/X{number}")).exists()
            {
                continue;
            }
            let name = format!(":{number}");
            let child = Command::new("Xvfb")
                .args([
                    &name,
                    "-screen",
                    "0",
                    &format!("{width}x{height}x24"),
                    "-nolisten",
                    "tcp",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("spawning Xvfb for the engine's headless display")?;

            let mut display = HeadlessDisplay {
                child,
                name: name.clone(),
                width,
                height,
            };
            match display.wait_ready(Duration::from_secs(10)) {
                Ok(()) => return Ok(display),
                Err(error) => {
                    last_error = error.to_string();
                    continue;
                }
            }
        }
        bail!(
            "no free X display in {FIRST_DISPLAY}..={LAST_DISPLAY} for the engine \
             (last attempt: {last_error})"
        )
    }

    fn wait_ready(&mut self, timeout: Duration) -> Result<()> {
        let socket = PathBuf::from(format!("/tmp/.X11-unix/X{}", &self.name[1..]));
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                bail!("Xvfb on {} exited early ({status})", self.name);
            }
            if socket.exists() {
                return Ok(());
            }
            if Instant::now() > deadline {
                bail!("Xvfb on {} did not come up within {timeout:?}", self.name);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Point this PROCESS at the display.
    ///
    /// `DISPLAY` is process-global, not per-thread, so the engine claims it for
    /// the whole process. That is sound where the engine runs — the ychrome
    /// daemon, which owns no windows of its own — and it is why the engine is a
    /// daemon subsystem rather than something the browser process hosts
    /// alongside tao's GTK loop.
    ///
    /// `WEBKIT_DISABLE_COMPOSITING_MODE` is set because Xvfb offers no DRI3:
    /// the accelerated-compositing path has no GPU to land on here, and the
    /// non-accelerated path is what actually paints. Recorded rather than
    /// assumed — it shows up in the gate's journal line.
    pub fn install_env(&self) {
        // SAFETY: called once, from `Engine::start`, before the engine thread
        // exists and therefore before any other thread reads the environment.
        unsafe {
            std::env::set_var("DISPLAY", &self.name);
            std::env::set_var("GDK_BACKEND", "x11");
            std::env::remove_var("WAYLAND_DISPLAY");
            // ⛔⛔ SETTING `DISPLAY` IS NOT ENOUGH, AND THE FAILURE IS ON THE
            // OPERATOR'S SCREEN. GTK prefers the Wayland backend, and **GDK's
            // Wayland backend defaults to `wayland-0` when `WAYLAND_DISPLAY` is
            // UNSET** — while `XDG_RUNTIME_DIR` still points at the operator's
            // runtime dir. So an absent variable does not mean "no display": it
            // means *their compositor*. GTK connected to wayland-0, ignored the
            // Xvfb we had just started for it, and put real `ychrome` toplevels
            // on the human's desktop — including, live on 2026-07-31, a
            // filled-in brokerage login over the video he was watching.
            //
            // Unsetting a variable is never isolation. The engine must name its
            // world POSITIVELY: force the X11 backend so `DISPLAY` is the only
            // thing that can be honoured, and remove the Wayland fallback so
            // nothing can find its way back to wayland-0.
            //
            // Same family as the x11vnc refusal and the frozen-daemon-env
            // poisoning already recorded in this fleet's memory: an inherited
            // — or absent — variable that describes a different world.
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }
}

impl Drop for HeadlessDisplay {
    fn drop(&mut self) {
        // SIGTERM, not `Child::kill`'s SIGKILL. Xvfb removes its
        // `/tmp/.X<n>-lock` on a graceful stop and leaves it behind on a hard
        // kill, and `start` treats a surviving lock as "taken" — so a killed
        // display burns its number for good. Measured: seven gate runs, seven
        // orphaned locks, the display number climbing :90 -> :96.
        let pid = self.child.id() as i32;
        // SAFETY: the pid is our own direct child, which we have not reaped.
        unsafe { libc::kill(pid, libc::SIGTERM) };

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        // Belt and braces for the hard-kill path. Removing this is safe
        // precisely because `start` only chose this number when nothing else
        // held it: the lock is ours to clear.
        let _ = std::fs::remove_file(format!("/tmp/.X{}-lock", &self.name[1..]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The seam's contract: a probe REPORTS, it never panics and never guesses.
    // Both probes must answer on any host, including one with neither
    // substrate installed, because `select`'s error message is built from them.

    /// ⛔ THE ENGINE MUST NAME ITS DISPLAY POSITIVELY.
    ///
    /// Setting DISPLAY is not enough: GTK prefers Wayland, and GDK's Wayland
    /// backend falls back to `wayland-0` when WAYLAND_DISPLAY is UNSET while
    /// XDG_RUNTIME_DIR still points at the operator's runtime dir. That put
    /// real ychrome windows — one carrying a filled-in brokerage login — on the
    /// operator's desktop while the engine believed it was headless.
    ///
    /// An absent variable is not isolation. If this test is failing, someone
    /// removed the only two lines that stop the engine finding its way back to
    /// a human's compositor.
    #[test]
    fn install_env_pins_the_engine_to_its_own_x_display() {
        let source = include_str!("substrate.rs");
        let body = source
            .split("pub fn install_env(&self)")
            .nth(1)
            .expect("install_env is present")
            .split("\n    }")
            .next()
            .expect("its body closes");
        assert!(
            body.contains(r#"set_var("DISPLAY""#),
            "install_env must set DISPLAY to the engine's own Xvfb"
        );
        assert!(
            body.contains(r#"set_var("GDK_BACKEND", "x11")"#),
            "install_env MUST force the X11 backend — otherwise GTK picks Wayland \
             and DISPLAY is ignored entirely"
        );
        assert!(
            body.contains(r#"remove_var("WAYLAND_DISPLAY")"#),
            "install_env MUST remove WAYLAND_DISPLAY — an UNSET one resolves to \
             wayland-0, i.e. the operator's own compositor"
        );
    }

    #[test]
    fn probes_answer_without_panicking_and_carry_their_evidence() {
        for probe in probe_all() {
            assert!(
                !probe.reason.is_empty(),
                "{} gave no reason",
                probe.substrate.id()
            );
            assert!(
                probe.evidence.is_object(),
                "{} gave no evidence object",
                probe.substrate.id()
            );
            let rendered = probe.to_json();
            assert_eq!(rendered["substrate"], probe.substrate.id());
            assert_eq!(rendered["available"], probe.available);
        }
    }

    // The WPE probe must not report "available" off the mere presence of WPE
    // WebKit. Debian ships wpe-webkit-2.0 2.52.5 with no WPEPlatform, and an
    // earlier reading of the spec assumed the version number alone settled it.
    // Availability requires BOTH signals.
    #[test]
    fn wpe_availability_needs_the_platform_pc_and_the_headless_declaration() {
        let probe = probe_wpe();
        let platform = probe.evidence["wpe_platform_pkgconfig"].is_string();
        let declared = probe.evidence["wpe_display_headless_new_declared"]
            .as_bool()
            .unwrap_or(false);
        assert_eq!(
            probe.available,
            platform && declared,
            "WPE availability must be the AND of both signals, not the version string"
        );
    }

    // The intended substrate is probed first so the journal shows why a
    // fallback was taken rather than merely that one was.
    #[test]
    fn wpe_is_probed_before_the_fallback() {
        let probes = probe_all();
        assert_eq!(probes[0].substrate, Substrate::WpePlatformHeadless);
        assert_eq!(probes[1].substrate, Substrate::WebKitGtkHeadless);
    }
}
