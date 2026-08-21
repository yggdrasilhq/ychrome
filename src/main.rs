//! ychrome — a web viewport for the Yggdrasil ecosystem.
//!
//! Two modes (docs/architecture.md):
//!   - **thin-client** (inside yggterm, detected via YGGTERM_SESSION_ID):
//!     emit the libyggterm web-surface OSC (7717) on stdout so the yggterm
//!     GUI swaps this session's viewport to a web view, heartbeat every few
//!     seconds, block until Ctrl+C, then emit the close OSC. The PTY byte
//!     relay is the transport, so this works identically over ssh.
//!   - **standalone** (no yggterm): open an own WebKit window.
//!     `--profile <name>` gives each profile its own persistent storage;
//!     `--via <ssh-host>` reaches that machine's network through an ssh
//!     forward.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

mod abp;
mod adblock;
mod build;
mod daemon;
mod engine;
mod extensions;
mod manifest;
mod passkey;
mod provision;
mod sidebar;
mod sitehost;
mod sponsorblock;
mod tlspin;
mod useragent;
mod userscript;
mod webmedia;
mod webpolicy;
mod webzoom;
use clap::Parser;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use url::Url;
use wry::{ProxyConfig, ProxyEndpoint, WebContext, WebViewBuilder};

#[derive(Parser, Debug)]
#[command(name = "ychrome", version, about)]
struct Args {
    /// URL to open (default: about:blank)
    url: Option<String>,

    /// Named profile: separate persistent cookies/storage per profile
    #[arg(long, default_value = "default")]
    profile: String,

    /// Reach the URL through an ssh tunnel to this host (uses your ssh
    /// config). Meant for http://localhost:PORT servers on that machine.
    #[arg(long)]
    via: Option<String>,

    /// Window title (default: derived from the URL)
    #[arg(long)]
    title: Option<String>,

    /// Anchor a NEW surface in this terminal even when a matching surface is
    /// already open elsewhere (the default is to route the url into that one).
    #[arg(long)]
    here: bool,
}

struct Tunnel {
    child: Child,
    local_port: u16,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_local_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Spawn `ssh -N -D <local> <via>` (a dynamic SOCKS proxy) and wait until the
/// local side accepts connections. The webview points at the SOCKS proxy, so
/// the *remote* sshd resolves DNS and originates every connection on the
/// session's machine — the egress rule, for ALL URLs (not just one loopback
/// port). `-L` was the old carrier and only forwarded a single host:port; it
/// broke internal DNS, docker networks, and cross-origin navigation.
fn open_tunnel(via: &str) -> Result<Tunnel> {
    let local_port = free_local_port()?;
    let child = Command::new("ssh")
        .args([
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ConnectTimeout=10",
            "-D",
            &format!("127.0.0.1:{local_port}"),
            via,
        ])
        .stdin(Stdio::null())
        .spawn()
        .context("spawning ssh for the SOCKS tunnel")?;
    let mut tunnel = Tunnel { child, local_port };

    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{local_port}").parse().unwrap(),
            Duration::from_millis(300),
        )
        .is_ok()
        {
            return Ok(tunnel);
        }
        if let Some(status) = tunnel.child.try_wait()? {
            bail!("ssh SOCKS tunnel to {via} exited early ({status}) — check `ssh {via}` works");
        }
        if Instant::now() > deadline {
            bail!("ssh SOCKS tunnel to {via} did not come up within 12s");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Host-owned profile jars live under `~/.yggterm/web-profiles/<name>/` on the
/// INVOKING host — the same location the yggterm GUI uses for a session's
/// surface, so a profile means the same identity whether ychrome renders it
/// itself (standalone) or hands it to the yggterm viewport (thin-client).
/// Let a page load when its certificate is one this host has explicitly pinned.
///
/// WebKitGTK's `load-failed-with-tls-errors` is the ONE place a refusal can be
/// reconsidered, and it hands us the certificate that failed. We answer only for
/// a (host, certificate) pair already recorded in `tls-pins.json` — and a pin is
/// only ever written for a chain OpenSSL verified against the system store (see
/// [`tlspin`]). Anything else returns `false` and the refusal stands, which is
/// the behaviour every other site keeps.
///
/// The refusal it exists for is not a bad certificate: it is a server sending a
/// jumbled chain whose good path GnuTLS will not search for. Without this, the
/// only thing that satisfies GnuTLS is trusting a retired root machine-wide.
#[cfg(target_os = "linux")]
fn install_tls_pin_handler(webview: &wry::WebView) {
    use webkit2gtk::gio::prelude::TlsCertificateExt;
    use webkit2gtk::{WebContextExt, WebViewExt};
    use wry::WebViewExtUnix;

    let pins = tlspin::load();
    if pins.is_empty() {
        return;
    }
    webview
        .webview()
        .connect_load_failed_with_tls_errors(move |view, failing_uri, certificate, _errors| {
            let Some(host) = url::Url::parse(failing_uri)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_string))
            else {
                return false;
            };
            let Some(der) = certificate.certificate() else {
                return false;
            };
            let Some(pin) = tlspin::matching(&pins, &host, &der) else {
                return false;
            };
            let Some(context) = view.web_context() else {
                return false;
            };
            // Scoped to this host by the API itself, so the exception cannot
            // leak to another site even if the same certificate turns up there.
            context.allow_tls_certificate_for_host(certificate, &host);
            eprintln!(
                "ychrome: allowed the pinned certificate for {host} ({}) — {}",
                &pin.sha256[..pin.sha256.len().min(16)],
                pin.reason
            );
            view.load_uri(failing_uri);
            true
        });
}

pub(crate) fn profile_dir(profile: &str) -> Result<PathBuf> {
    if profile.contains('/') || profile.contains("..") || profile.is_empty() {
        bail!("profile name must be a plain name, not a path: {profile:?}");
    }
    if profile == TEMP_PROFILE {
        // Reserved ephemeral profile: a throwaway jar under the OS temp dir,
        // unique per process, best-effort deleted on exit (see main). Never
        // touches ~/.yggterm/web-profiles/. Thin-client mode doesn't come
        // here at all — the yggterm GUI maps "temp" to a true in-memory
        // ephemeral WebContext.
        let dir = std::env::temp_dir().join(format!("ychrome-temp-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        return Ok(dir);
    }
    let base = dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("web-profiles")
        .join(profile);
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

/// Reserved profile name for an ephemeral session: no persistent jar, nothing
/// kept after close. Mirrored by yggterm's `web_surface_profile_dir` (which
/// maps it to an in-memory ephemeral WebContext on the GUI side).
const TEMP_PROFILE: &str = "temp";

/// The libyggterm web-surface control sequence (OSC 7717). Consumed by the
/// yggterm GUI's terminal parser; invisible junk-free in plain terminals
/// (unknown OSCs are ignored) — the degradation story is the channel itself.
fn emit_web_surface_osc(
    action: &str,
    session: &str,
    url: &str,
    title: &str,
    profile: &str,
    start_page: bool,
) {
    use base64::Engine as _;
    // `start_page` tells the GUI this surface opened on the app's OWN start
    // page rather than a user-chosen URL — which is what lets "continue tabs
    // from last time" ADOPT the saved active page into this tab instead of
    // parking it behind a fresh start page. Old GUIs ignore unknown keys.
    let payload = format!(
        "{{\"session\":{},\"url\":{},\"title\":{},\"profile\":{},\"start_page\":{}}}",
        serde_json_string(session),
        serde_json_string(url),
        serde_json_string(title),
        serde_json_string(profile),
        start_page,
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\u{1b}]7717;web-surface;{action};{encoded}\u{7}");
    let _ = stdout.flush();
}

/// Publish this session's queued passkey presence requests on OUR stdout.
///
/// ⛔ **The whole point is the fd.** The signer that raises a ceremony lives in
/// the host daemon, whose stdout is `/dev/null`, and the GUI routes a
/// `fido2 ; request` by the stream it arrives on — so a request written there
/// reached nobody, no approval dialog was ever raised, and every passkey
/// sign-in failed after a silent two-minute park. This process is the one that
/// holds the session's PTY, so this process is the one that writes.
///
/// Called on the surface tick rather than the ~4s heartbeat: a human is waiting
/// on a dialog, and four seconds of nothing is what a broken button looks like.
fn publish_presence_requests(session: &str) {
    let requests = daemon::drain_presence(session);
    if requests.is_empty() {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    for osc in requests {
        let _ = write!(stdout, "{osc}");
    }
    let _ = stdout.flush();
}

/// Minimal JSON string escaping (avoid a serde dependency for one payload).
fn serde_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Thin-client mode: drive the yggterm viewport via OSC and block in the
/// foreground like a proper CLI program. The heartbeat keeps the surface
/// alive (the GUI expires surfaces after ~15s without one, so a SIGKILLed
/// ychrome never leaks a full-screen overlay) and re-heals the surface
/// after a GUI-side terminal remount.
fn run_thin_client(session: &str, url: &str, title: &str, profile: &str) -> Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || {
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .context("installing Ctrl+C handler")?;
    }
    drive_surface(session, url, title, profile, false, stop)
}

/// The single owner of the web-surface loop: contribute the sidebar, open the
/// surface, and heartbeat BOTH until `stop`. The `--url` fast path and the
/// no-arg picker path (once the user has chosen a profile) both drive it, so the
/// liveness + sidebar `DECLARE BEFORE OPEN` contract has ONE implementation. The
/// picker path used to reimplement the heartbeat loop WITHOUT the sidebar, so a
/// browser opened from the `+` menu had no vault/settings rail (and its surface
/// was created with no adblock/userscript policy).
///
/// `stop` is owned by the caller because `ctrlc::set_handler` may be installed
/// only once per process; the picker path installs it before it knows the URL.
/// Print any known site-lore for this URL's host to stderr at launch, so an agent
/// co-browsing the page CANNOT miss the access methods a prior agent already
/// proved. The lore is the git-tracked markdown under the ychrome-site-lore skill
/// (one file per domain); this reads it directly — no python at launch, no runtime
/// dependency. If no lore exists yet, it prints the one-line command to record one.
/// The recall lives in the TOOL's own output on purpose: a skill an agent must
/// remember to load is a skill an agent forgets; the launch banner is not.
fn print_site_lore(url: &str) {
    let host = match Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    {
        Some(h) => h,
        None => return,
    };
    // Skill dir: env override (matches lore.py's own SKILL_DIR), else the
    // fleet-standard repo path that git keeps in sync on every host.
    let skill_dir = std::env::var("YCHROME_SITE_LORE_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("gh/ychrome/.claude/skills/ychrome-site-lore")
        });
    let lore_dir = skill_dir.join("lore");
    // Try the host as-is, then with a leading "www." stripped, so a page browsed
    // as www.davaindia.com still matches lore filed under davaindia.com.
    let mut candidates = vec![host.clone()];
    if let Some(stripped) = host.strip_prefix("www.") {
        candidates.push(stripped.to_string());
    }
    for dom in &candidates {
        if let Ok(body) = std::fs::read_to_string(lore_dir.join(format!("{dom}.md"))) {
            eprintln!("ychrome: ── site-lore for {dom} (proven methods from prior runs) ──");
            for line in body.lines() {
                eprintln!("ychrome:   {line}");
            }
            eprintln!("ychrome: ── end site-lore · add findings with lore.py log {dom} ──");
            return;
        }
    }
    eprintln!(
        "ychrome: no site-lore yet for {host} — once you learn the access method, record it so \
         the next agent inherits it: python3 {}/lore.py log {host} --slug <method> --status WORKS --body-file <f>",
        skill_dir.display()
    );
}

/// Register, then declare what THAT registration returned. The ONE way this
/// program emits a `sidebar ; declare`, and the reason it takes no endpoint
/// argument: there is no variable a caller could hand it, so it cannot publish
/// an endpoint that is not the daemon's current answer. Returns whether it
/// declared.
///
/// **Why by construction and not by a refresh someone remembers to call.** The
/// declare carries the control url AND the control token, and both move together
/// when a daemon is handed over (a fresh listener, a freshly minted token). The
/// previous shape kept the last-known endpoint in a `control` local and
/// re-declared it whenever the re-register failed — which is exactly what
/// happens DURING a handover, so the client published a url+token pair belonging
/// to a daemon that had already exited. If the successor happened to re-bind the
/// same port, the GUI then held a live url with a dead token and every pane and
/// action 403'd until the next declare corrected it.
///
/// A missed declare is cheap: it is the contribution's ~4s liveness signal
/// against the GUI's 15s expiry, so one skipped tick costs nothing, and the
/// honest silence lets a contribution expire rather than pinning the rail to an
/// endpoint nobody serves.
fn declare_current(session: &str, profile: &str) -> bool {
    let Some(endpoint) = daemon::register_supervised(session, profile) else {
        return false;
    };
    sidebar::emit_declare(
        session,
        &endpoint.url,
        &endpoint.token,
        &webpolicy::policy_version(profile),
        &webzoom::zoom_version(),
    );
    true
}

fn drive_surface(
    session: &str,
    url: &str,
    title: &str,
    profile: &str,
    start_page: bool,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    // ychrome CONTRIBUTES its vault and settings panes rather than yggterm
    // hardcoding them, and the HOST DAEMON now serves that contribution's
    // control endpoint (one process per host, one listener per session) — so
    // the pane draft survives a client exit and a routed open can be delivered.
    // A daemon failure must never take the browser down: the surface is the
    // product, the sidebar is an extra.
    //
    // DECLARE BEFORE OPEN. The GUI holds a surface's creation until it has
    // fetched the app's policy, because a userscript only injects at
    // document-start. Open first and the GUI's first apply pass sees a surface
    // with no contribution and builds it unblocked — no userscripts, no adblock,
    // silently, for the life of that webview.
    if !declare_current(session, profile) {
        eprintln!("ychrome: sidebar unavailable (daemon did not come up)");
    }
    // Drain once BEFORE the surface exists. The drain is what marks the session
    // presence-reachable, and the signer refuses a ceremony it has not seen
    // drained — so without this the first page could load into a window where a
    // real passkey request would be refused as unreachable.
    publish_presence_requests(session);
    emit_web_surface_osc("open", session, url, title, profile, start_page);
    eprintln!(
        "ychrome: web surface open — {url} [{profile}]  (Ctrl+C to close, Ctrl+Z / yggterm Zzz to suspend)"
    );
    print_site_lore(url);
    let mut ticks: u32 = 0;
    let mut last_tick = std::time::Instant::now();
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        // A large gap between ticks means we were suspended (Ctrl+Z /
        // SIGSTOP — yggterm's Zzz button) or the machine slept, and the GUI
        // may have closed or swept the surface meanwhile. Re-register (the
        // daemon may have reaped us) and re-emit "open" on resume: heartbeats
        // deliberately cannot re-CREATE a surface, and an "open" with an
        // unchanged URL is liveness-idempotent GUI-side.
        if last_tick.elapsed() > Duration::from_secs(3) {
            declare_current(session, profile);
            emit_web_surface_osc("open", session, url, title, profile, start_page);
        }
        last_tick = std::time::Instant::now();
        // Every tick, not every heartbeat: this is a human waiting on a dialog.
        publish_presence_requests(session);
        ticks += 1;
        // Heartbeat every ~4s (20 × 200ms) — the GUI's liveness truth, and the
        // daemon re-register that keeps this session in the registry.
        if ticks.is_multiple_of(20) {
            emit_web_surface_osc("heartbeat", session, url, title, profile, start_page);
            // The daemon heartbeat: re-registering keeps the entry alive (the
            // reaper drops a session whose client goes quiet) and re-earns it
            // after a daemon respawn. A moved control url means a new listener —
            // re-declare so the GUI follows it.
            declare_current(session, profile);
        }
    }
    sidebar::emit_close(session);
    daemon::deregister(session);
    emit_web_surface_osc("close", session, url, title, profile, start_page);
    eprintln!("ychrome: web surface closed");
    Ok(())
}

/// The surface the picker's heartbeat currently points at. Starts as the
/// loopback control endpoint (action "pick" — the yggterm GUI renders a
/// NATIVE profile picker and GETs /open on this server); the /open handler
/// retargets it (url+profile, action "open") and the heartbeat carries the
/// new value from then on.
struct SurfaceTarget {
    url: String,
    title: String,
    profile: String,
    /// OSC action for the current target: "pick" until the user chooses,
    /// "open" after.
    action: &'static str,
    /// The chosen URL is the app's own start page (picker URL field left
    /// empty), not a user-chosen destination.
    start_page: bool,
}

/// Existing host-owned profiles, for the picker to list. Reads directory names
/// under `~/.yggterm/web-profiles/` (the same jars `--profile` creates). Always
/// includes "default" even before it exists on disk.
fn enumerate_profiles() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(base) = dirs::home_dir().map(|h| h.join(".yggterm").join("web-profiles"))
        && let Ok(entries) = std::fs::read_dir(&base)
    {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && let Some(name) = entry.file_name().to_str()
                && !name.is_empty()
                && !name.starts_with('.')
                // "temp" is reserved for the ephemeral profile; a stray dir
                // with that name is never a real jar (both sides ignore it).
                && name != TEMP_PROFILE
            {
                names.push(name.to_string());
            }
        }
    }
    if !names.iter().any(|n| n == "default") {
        names.push("default".to_string());
    }
    names.sort();
    names.dedup();
    names
}

/// Sanitize a picker-chosen profile to one path-safe component (mirrors the
/// yggterm side's `normalize_web_surface_profile`): a hostile value can never
/// escape `~/.yggterm/web-profiles/`. Falls back to "default".
fn sanitize_profile(name: &str) -> String {
    let name = name.trim();
    let safe = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(std::path::is_separator);
    if safe {
        name.to_string()
    } else {
        "default".to_string()
    }
}

/// Read a string key from `~/.yggterm/web-surface.json` — the ONE config file
/// the yggterm GUI also reads (`web_surface_config_string` there), so ychrome's
/// omnibox and the GUI address bar share a single source of truth for the search
/// engine and start page.
fn web_surface_config_string(key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(dirs::home_dir()?.join(".yggterm").join("web-surface.json"))
        .ok()?;
    let config: serde_json::Value = serde_json::from_str(&raw).ok()?;
    config
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Default start page when the picker's URL field is left empty — the configured
/// engine's home (default Brave). Native child webviews aren't iframes, so
/// X-Frame-Options no longer constrains the choice (the historical reason
/// DuckDuckGo's html/ endpoint was hard-coded).
fn default_start_url() -> String {
    web_surface_config_string("default_start_url")
        .unwrap_or_else(|| "https://search.brave.com/".to_string())
}

/// Search-engine URL template with a `{q}` placeholder for the URL-encoded
/// query (default Brave). Same key/default the yggterm GUI uses.
fn search_url_template() -> String {
    web_surface_config_string("search_url_template")
        .filter(|template| template.contains("{q}"))
        .unwrap_or_else(|| "https://search.brave.com/search?q={q}".to_string())
}

/// Turn a picker URL field into an http(s) URL the yggterm surface will accept
/// (`web_surface_url_scheme_allowed` only permits http/https). Mirrors the
/// documented omnibox rule: scheme kept as-is; a bare host gets http for
/// loopback / https otherwise; anything word-like becomes a search.
fn normalize_target_url(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return default_start_url();
    }
    if raw.contains("://") {
        return raw.to_string();
    }
    let authority = raw.split(['/', '?', '#']).next().unwrap_or(raw);
    let host = authority.split(':').next().unwrap_or(authority);
    let is_hostish = !raw.contains(char::is_whitespace)
        && (host == "localhost" || authority.contains('.') || authority.contains(':'));
    if is_hostish {
        let loopback = matches!(
            host,
            "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]"
        );
        let scheme = if loopback { "http" } else { "https" };
        format!("{scheme}://{raw}")
    } else {
        let q: String = url::form_urlencoded::byte_serialize(raw.as_bytes()).collect();
        search_url_template().replace("{q}", &q)
    }
}

/// Title for a picked (url, profile) pair — same shape as the standalone titles.
fn surface_title(url: &str, profile: &str) -> String {
    let host = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    match (host, profile) {
        (Some(h), "default") => format!("ychrome — {h}"),
        (Some(h), p) => format!("ychrome — {h} [{p}]"),
        (None, _) => "ychrome".to_string(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The picker page: a URL field plus one card per existing profile (Chrome's
/// profile picker, condensed). Submitting GETs `/open` on this same loopback
/// server, which re-emits the OSC pointing at the chosen url+profile.
fn picker_html(profiles: &[String]) -> String {
    let mut cards = String::new();
    for p in profiles {
        let checked = if p == "default" { " checked" } else { "" };
        let initial = p
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        let pe = html_escape(p);
        let lower = p.to_lowercase();
        cards.push_str(&format!(
            "<label class=\"card\" data-profile=\"{le}\"><input type=\"radio\" name=\"profile\" value=\"{pe}\"{checked}>\
             <span class=\"avatar\">{ie}</span><span class=\"pname\">{pe}</span></label>",
            le = html_escape(&lower),
            ie = html_escape(&initial),
        ));
    }
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ychrome — choose a profile</title>
<style>
:root {{ color-scheme: light dark; }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; min-height: 100vh; display: grid; place-items: center;
  font: 15px/1.4 system-ui, -apple-system, sans-serif;
  background: #f4f4f6; color: #1b1b1f; }}
@media (prefers-color-scheme: dark) {{ body {{ background: #161619; color: #e8e8ea; }} }}
.panel {{ width: min(560px, 92vw); padding: 40px 36px 32px; text-align: center; }}
h1 {{ font-size: 22px; font-weight: 600; margin: 0 0 4px; }}
.sub {{ opacity: .62; margin: 0 0 28px; font-size: 14px; }}
.urlrow {{ display: flex; gap: 10px; margin: 0 auto 30px; max-width: 460px; }}
.urlrow input[type=text] {{ flex: 1; padding: 12px 15px; font-size: 15px;
  border: 1px solid #cfcfd6; border-radius: 11px; background: #fff; color: inherit; }}
@media (prefers-color-scheme: dark) {{ .urlrow input[type=text] {{
  background: #202024; border-color: #38383f; }} }}
.urlrow input[type=text]:focus {{ outline: 2px solid #6c8cff; outline-offset: 0; border-color: transparent; }}
button {{ padding: 12px 22px; font-size: 15px; font-weight: 600; cursor: pointer;
  border: 0; border-radius: 11px; background: #4f6bff; color: #fff; }}
button:hover {{ background: #3d59f0; }}
.grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(112px, 1fr));
  gap: 14px; }}
.card {{ position: relative; display: flex; flex-direction: column; align-items: center;
  gap: 9px; padding: 18px 8px 14px; border: 1px solid #dcdce3; border-radius: 14px;
  cursor: pointer; background: #fff; transition: border-color .12s, background .12s; }}
@media (prefers-color-scheme: dark) {{ .card {{ background: #202024; border-color: #33333a; }} }}
.card:hover {{ border-color: #9db0ff; }}
.card input {{ position: absolute; opacity: 0; pointer-events: none; }}
.card:has(input:checked) {{ border-color: #4f6bff; box-shadow: 0 0 0 1px #4f6bff inset; }}
.avatar {{ width: 46px; height: 46px; border-radius: 50%; display: grid; place-items: center;
  font-size: 20px; font-weight: 600; color: #fff;
  background: linear-gradient(135deg, #6c8cff, #9a6bff); }}
.card.newcard .avatar {{ background: none; color: #7a7a86; border: 2px dashed #b6b6c0; }}
.card.tempcard .avatar {{ background: linear-gradient(135deg, #5f6672, #3a3f4a); }}
.pname {{ font-size: 13px; max-width: 100%; overflow: hidden; text-overflow: ellipsis;
  white-space: nowrap; }}
#newprofile {{ margin-top: 12px; width: 100%; max-width: 240px; padding: 9px 12px;
  font-size: 14px; border: 1px solid #cfcfd6; border-radius: 9px; background: #fff; color: inherit; }}
@media (prefers-color-scheme: dark) {{ #newprofile {{ background: #202024; border-color: #38383f; }} }}
.profilesearch {{ display:flex; gap:8px; margin:0 auto 14px; max-width:460px; }}
.profilesearch input {{ flex:1; padding:10px 13px; font-size:14px; border:1px solid #cfcfd6; border-radius:10px; background:#fff; color:inherit; }}
@media (prefers-color-scheme: dark) {{ .profilesearch input {{ background:#202024; border-color:#38383f; }} }}
.profilesearch input:focus {{ outline:2px solid #6c8cff; outline-offset:0; border-color:transparent; }}
#profilecount {{ font-size:13px; opacity:.6; margin:8px 0 0; }}
.card[hidden] {{ display:none !important; }}
</style></head><body>
<form class="panel" action="/open" method="get">
  <h1>Choose a profile</h1>
  <p class="sub">Each profile keeps its own cookies and logins. Type a URL, or leave it blank to start on search.</p>
  <div class="urlrow">
    <input type="text" name="url" placeholder="URL or search — e.g. localhost:8000" autocomplete="off" spellcheck="false">
    <button type="submit">Open</button>
  </div>
  <div class="profilesearch">
    <input type="text" id="profilesearch" placeholder="Search profiles — filters as you type" autocomplete="off" spellcheck="false" aria-label="Search profiles">
  </div>
  <p id="profilecount" aria-live="polite"></p>
  <div class="grid" id="profilegrid">
    {cards}
    <label class="card tempcard" data-profile="__temp__" title="No history, cookies or storage kept — everything vanishes on close">
      <input type="radio" name="profile" value="temp">
      <span class="avatar">&#9202;</span><span class="pname">Temporary</span></label>
    <label class="card newcard" data-profile="__new__"><input type="radio" name="profile" value="" id="newradio">
      <span class="avatar">+</span><span class="pname">New profile</span></label>
  </div>
  <input type="text" name="newprofile" id="newprofile" placeholder="new profile name" autocomplete="off" spellcheck="false" hidden>
</form>
<script>
  var nr = document.getElementById('newradio'), ni = document.getElementById('newprofile');
  if (nr && ni) {{
    nr.addEventListener('change', function () {{ ni.hidden = false; ni.focus(); }});
    ni.addEventListener('input', function () {{ if (ni.value) nr.checked = true; }});
  }}
  (function() {{
    var q = document.getElementById('profilesearch');
    var grid = document.getElementById('profilegrid');
    var count = document.getElementById('profilecount');
    if (!q || !grid) return;
    var cards = Array.prototype.slice.call(grid.querySelectorAll('.card'));
    function norm(s) {{ return s.toLowerCase(); }}
    function apply() {{
      var needle = norm(q.value.trim());
      var visible = 0;
      var total = 0;
      for (var i=0;i<cards.length;i++) {{
        var c = cards[i];
        var key = c.getAttribute('data-profile') || '';
        var isSentinel = key === '__temp__' || key === '__new__';
        if (isSentinel) {{ c.hidden = false; continue; }}
        total++;
        var hay = key + ' ' + norm(c.textContent || '');
        var show = !needle || hay.indexOf(needle) !== -1;
        c.hidden = !show;
        if (show) visible++;
      }}
      if (count) {{
        if (!needle) count.textContent = total + ' profile' + (total===1?'':'s');
        else count.textContent = visible + ' of ' + total + ' match' + (visible===1?'':'es');
      }}
    }}
    q.addEventListener('input', apply);
    apply();
    var urlInput = document.querySelector('input[name=url]');
    if (urlInput && !urlInput.value) q.focus();
  }})();
</body></html>"#,
        cards = cards,
    )
}

/// Interstitial shown for the instant between the form submit and the yggterm
/// surface retargeting to the real destination (the OSC-driven load supersedes
/// this page, so it is rarely seen).
fn opening_html(url: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><body style=\"margin:0;height:100vh;\
         display:grid;place-items:center;font:16px system-ui;background:#161619;color:#e8e8ea\">\
         Opening {}…</body>",
        html_escape(url)
    )
}

fn parse_open_query(query: &str) -> (String, String) {
    let mut url = String::new();
    let mut profile = String::new();
    let mut newprofile = String::new();
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "url" => url = v.into_owned(),
            "profile" => profile = v.into_owned(),
            "newprofile" => newprofile = v.into_owned(),
            _ => {}
        }
    }
    let chosen = if !newprofile.trim().is_empty() {
        newprofile
    } else {
        profile
    };
    (url, chosen)
}

fn respond_html(mut stream: TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "OK",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn respond_empty(mut stream: TcpStream, status: u16) {
    let reason = if status == 204 { "No Content" } else { "OK" };
    let resp =
        format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// Handle one loopback request. `/` serves the picker; `/open?url=&profile=`
/// records the chosen (url, profile) as the new target with action "open".
///
/// It does NOT emit the OSC itself: the picker main loop notices the flip and
/// hands off to `drive_surface`, which DECLARES the sidebar before it opens the
/// page. Emitting "open" here would create the surface before the declare lands
/// — the GUI builds a surface with no contribution unblocked, and that webview
/// runs its whole life with no vault rail, no adblock, no userscripts.
fn handle_picker_conn(stream: TcpStream, session: &str, target: &Arc<Mutex<SurfaceTarget>>) {
    let _ = session;
    let peek = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(peek);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    // Request line: "GET /path?query HTTP/1.1"
    let request_target = line.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = request_target
        .split_once('?')
        .unwrap_or((request_target, ""));
    match path {
        "/" => respond_html(stream, 200, &picker_html(&enumerate_profiles())),
        "/open" => {
            let (raw_url, raw_profile) = parse_open_query(query);
            let start_page = raw_url.trim().is_empty();
            let url = normalize_target_url(&raw_url);
            let profile = sanitize_profile(&raw_profile);
            let title = surface_title(&url, &profile);
            {
                let mut t = target.lock().unwrap();
                *t = SurfaceTarget {
                    url: url.clone(),
                    title: title.clone(),
                    profile: profile.clone(),
                    action: "open",
                    start_page,
                };
            }
            eprintln!("ychrome: picker → {url} [{profile}]");
            respond_html(stream, 200, &opening_html(&url));
        }
        "/favicon.ico" => respond_empty(stream, 204),
        _ => respond_html(stream, 404, "<!doctype html><title>404</title>not found"),
    }
}

/// No-arg thin-client: serve a profile picker on a loopback HTTP server and
/// point the yggterm surface at it. Replaces the old `about:blank` open (which
/// the GUI rejected via `web_surface_url_scheme_allowed`). The user's choice
/// re-emits the OSC with a real url+profile; heartbeats then carry that target.
fn run_thin_client_picker(session: &str) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding picker loopback server")?;
    let port = listener.local_addr()?.port();
    let picker_url = format!("http://127.0.0.1:{port}/");

    let target = Arc::new(Mutex::new(SurfaceTarget {
        url: picker_url.clone(),
        title: "ychrome — choose a profile".to_string(),
        profile: "default".to_string(),
        action: "pick",
        start_page: false,
    }));

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .context("installing Ctrl+C handler")?;
    }

    // Loopback picker server. The accept loop thread is detached; a blocked
    // accept is torn down when the process exits on Ctrl+C.
    {
        let session = session.to_string();
        let target = target.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle_picker_conn(stream, &session, &target);
            }
        });
    }

    // Announce the picker (action "pick": the GUI renders a NATIVE profile
    // picker; the OSC url is this loopback CONTROL endpoint the GUI GETs /open
    // on), then heartbeat "pick" to keep it alive until the user chooses. The
    // server thread swaps `target` to the chosen url+profile (action "open") on
    // submit; when we see that flip we STOP the picker phase and hand off to
    // `drive_surface`, the ONE loop that declares the sidebar before it opens.
    {
        let t = target.lock().unwrap();
        emit_web_surface_osc(
            t.action,
            session,
            &t.url,
            &t.title,
            &t.profile,
            t.start_page,
        );
        eprintln!("ychrome: profile picker open — {picker_url}  (Ctrl+C to close)");
    }
    let mut ticks: u32 = 0;
    let mut last_tick = std::time::Instant::now();
    let chosen = loop {
        if stop.load(Ordering::SeqCst) {
            break None;
        }
        // The user submitted the picker: hand the chosen target to the shared
        // surface loop (which contributes the sidebar). Kept behind the lock's
        // scope so drive_surface runs without holding it.
        {
            let t = target.lock().unwrap();
            if t.action == "open" {
                break Some((
                    t.url.clone(),
                    t.title.clone(),
                    t.profile.clone(),
                    t.start_page,
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
        // Suspend/resume gap (Ctrl+Z / yggterm Zzz / machine sleep): the GUI
        // may have closed the surface, and heartbeats can't re-create one —
        // re-announce the picker.
        if last_tick.elapsed() > Duration::from_secs(3) {
            let t = target.lock().unwrap();
            emit_web_surface_osc(
                t.action,
                session,
                &t.url,
                &t.title,
                &t.profile,
                t.start_page,
            );
        }
        last_tick = std::time::Instant::now();
        ticks += 1;
        if ticks.is_multiple_of(20) {
            let t = target.lock().unwrap();
            emit_web_surface_osc("pick", session, &t.url, &t.title, &t.profile, t.start_page);
        }
    };
    match chosen {
        // Hand off to the shared loop: it declares the sidebar, emits the first
        // "open" (so DECLARE precedes OPEN), and heartbeats both. The detached
        // picker HTTP thread is orphaned but harmless — it dies with the process.
        Some((url, title, profile, start_page)) => {
            drive_surface(session, &url, &title, &profile, start_page, stop)
        }
        // Ctrl+C during the picker phase: never opened a page, so just close.
        None => {
            let t = target.lock().unwrap();
            emit_web_surface_osc("close", session, &t.url, &t.title, &t.profile, t.start_page);
            eprintln!("ychrome: web surface closed");
            Ok(())
        }
    }
}

/// Detect a yggterm-owned PTY. Primary signal is YGGTERM_SESSION_ID (the
/// daemon exports it into every PTY it owns). Fallback: the ssh bridge also
/// exports YGGTERM_TERM_PROGRAM=yggterm, and older remote daemons predate the
/// session-id handshake — the GUI keys surfaces by the STREAM the OSC arrives
/// on (the payload session field is diagnostic only), so a placeholder id
/// still yields a working surface.
fn yggterm_thin_client_session() -> Option<String> {
    if let Ok(session) = std::env::var("YGGTERM_SESSION_ID")
        && !session.is_empty()
    {
        return Some(session);
    }
    if std::env::var("YGGTERM_TERM_PROGRAM").is_ok_and(|value| value == "yggterm") {
        return Some("env-unknown".to_string());
    }
    None
}

/// Standalone mode opens a GTK window; without a display GTK aborts the
/// process with CRITICAL assertions instead of failing politely — check
/// first and produce a real error.
#[cfg(target_os = "linux")]
fn display_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
        || std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty())
}
#[cfg(not(target_os = "linux"))]
fn display_available() -> bool {
    true
}

/// `ychrome status [--json]` — host-side truth for agents (docs/host-daemon.md
/// §6): the registry, queue depths, vault-agent reachability, config stamps, the
/// daemon version, and a self-staleness stamp so the stale-daemon class ("an old
/// daemon running for hours while the fix sits on disk") cannot silently recur.
/// Spawns a daemon if none is running — a status query should not need a browser
/// already open.
fn run_status(as_json: bool) -> Result<()> {
    let status = daemon::status()?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    let version = status["version"].as_str().unwrap_or("?");
    let pid = status["pid"].as_u64().unwrap_or(0);
    let uptime = status["uptime_secs"].as_u64().unwrap_or(0);
    let stale = status["stale"].as_bool().unwrap_or(false);
    let held = status["live_sessions"].as_u64().unwrap_or(0);
    let vault = status["vault_agent_reachable"].as_bool().unwrap_or(false);
    println!("ychrome daemon {version}  pid {pid}  up {uptime}s");
    // Reaching this line stale means `ensure()` already declined to retire it,
    // which it only does when surfaces are attached. Say which of the two facts
    // is keeping the old code alive, and name the verb that ends it.
    if stale {
        println!(
            "  [STALE] this daemon is serving old code: the binary on disk changed after it started."
        );
        if held > 0 {
            println!(
                "  [STALE] {held} live surface(s) are attached, so nothing retired it for you."
            );
        }
        println!("  [STALE] hand it over with:  ychrome daemon restart");
    }
    println!(
        "vault agent: {}",
        if vault { "reachable" } else { "not reachable" }
    );
    let sessions = status["sessions"].as_array().cloned().unwrap_or_default();
    if sessions.is_empty() {
        println!("no anchored sessions");
    } else {
        println!("{} anchored session(s):", sessions.len());
        for session in &sessions {
            let env = session["env_id"].as_str().unwrap_or("?");
            let profile = session["profile"].as_str().unwrap_or("?");
            let depth = session["queue_depth"].as_u64().unwrap_or(0);
            let routable = session["routing_capable"].as_bool().unwrap_or(false);
            println!(
                "  {env}  profile={profile}  queue={depth}  routable={}  passkeys={}",
                if routable { "yes" } else { "no" },
                // Same standard as `routable`: EXPLICITLY false, never merely
                // absent. A daemon older than the presence channel omits the
                // field, and "?" says "this daemon cannot answer" rather than
                // inventing a "no" that would read as a fault in the surface.
                match session["presence_reachable"].as_bool() {
                    Some(true) => "yes",
                    Some(false) => "NO",
                    None => "?",
                }
            );
        }
        // ⛔ A SESSION THAT CANNOT ASK FOR APPROVAL CANNOT USE A PASSKEY, and
        // nothing else on this host says so. The failure is silent by nature:
        // the surface works, the vault is unlocked, the shim is installed, and
        // every ceremony simply refuses. Named here with the fix, exactly as
        // [NO PANES] is.
        let no_presence: Vec<&str> = sessions
            .iter()
            .filter(|session| session["presence_reachable"].as_bool() == Some(false))
            .filter_map(|session| session["env_id"].as_str())
            .collect();
        if !no_presence.is_empty() {
            println!(
                "  [NO PASSKEYS] {} session(s) have no view client publishing presence \
                 requests, so a site asking for a passkey is refused rather than \
                 approved:",
                no_presence.len()
            );
            for env in &no_presence {
                println!("  [NO PASSKEYS]   {env}");
            }
            println!(
                "  [NO PASSKEYS] the client publishes on its surface tick; a session whose \
                 ychrome predates that channel must be restarted."
            );
        }
        // A session whose CLI predates the control-token gate keeps its ad
        // blocking and its userscripts (those routes are open) and silently
        // cannot open its vault or settings pane, forever. It is invisible
        // everywhere else, so it is named here with the only thing that fixes
        // it — the same standard as the [STALE] daemon lines above.
        //
        // EXPLICITLY false, never merely absent. A daemon older than this field
        // omits it — and that daemon has no gate at all, so its panes work and a
        // warning would be a false alarm on the one deploy where everything is
        // fine. Silence means "not asked", not "no".
        let no_courier: Vec<&str> = sessions
            .iter()
            .filter(|session| session["control_token_declared"].as_bool() == Some(false))
            .filter_map(|session| session["env_id"].as_str())
            .collect();
        if !no_courier.is_empty() {
            println!(
                "  [NO PANES] {} session(s) run a ychrome CLI that predates the control-token \
                 gate, so their vault and settings panes answer 403 and cannot open:",
                no_courier.len()
            );
            for env in &no_courier {
                println!("  [NO PANES]   {env}");
            }
            println!(
                "  [NO PANES] ad blocking and userscripts are unaffected. A daemon restart does \
                 NOT fix this. Press Ctrl+C in each session's terminal and run ychrome again."
            );
        }
    }
    Ok(())
}

/// `ychrome daemon restart` — the deliberate handover.
///
/// `ensure()` replaces an outdated daemon by itself only when nothing is
/// attached to it. With live surfaces attached it refuses, because retiring it
/// drops their sidebar rail, their pane drafts and any passkey signature in
/// flight. This verb is the user saying "do it anyway, I am ready", and it
/// reports what it cost rather than pretending it cost nothing.
/// One census row as a person reads it: what it is, what it runs, and the
/// verdict that decides whether anything may be done to it.
fn print_census_row(row: &serde_json::Value, verdict_key: &str) {
    let deleted = if row["exe_deleted"].as_bool() == Some(true) {
        "  ⛔ its binary is DELETED from disk"
    } else {
        ""
    };
    println!(
        "  pid {:<8} up {:>7}s  {} child(ren)  [{}]{}",
        row["pid"].as_u64().unwrap_or(0),
        row["up_secs"].as_u64().unwrap_or(0),
        row["children"].as_u64().unwrap_or(0),
        row[verdict_key].as_str().unwrap_or("?"),
        deleted,
    );
    println!("    root: {}", row["serving_root"].as_str().unwrap_or("(holds no daemon.lock)"));
    println!("    exe:  {}", row["exe"].as_str().unwrap_or("?"));
    println!("    {}", row["note"].as_str().or_else(|| row["outcome"].as_str()).unwrap_or(""));
}

fn run_daemon_verb(sub: Option<&str>, as_json: bool) -> Result<()> {
    match sub {
        // The read half. Run it before `reap` — it kills nothing and it names
        // the root of every daemon on the host, which is the one fact
        // `daemon restart` cannot see past.
        Some("list") => {
            let census = daemon::list()?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&census)?);
                return Ok(());
            }
            let rows = census["daemons"].as_array().cloned().unwrap_or_default();
            println!(
                "{} ychrome daemon(s) on this host; this shell resolves {}",
                rows.len(),
                census["root"].as_str().unwrap_or("?"),
            );
            for row in &rows {
                print_census_row(row, "verdict");
            }
            Ok(())
        }
        Some("reap") => {
            let dry_run = std::env::args().any(|arg| arg == "--dry-run");
            let done = daemon::reap(daemon::ReapScope::AlsoForeignRoots, dry_run, None)?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&done)?);
                return Ok(());
            }
            print_reap(&done);
            Ok(())
        }
        Some("restart") => {
            let done = daemon::restart()?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&done)?);
                return Ok(());
            }
            let new_pid = done["pid"].as_u64().unwrap_or(0);
            match done["old_pid"].as_u64() {
                Some(old) => println!(
                    "ychrome daemon restarted: pid {old} retired, pid {new_pid} now serving"
                ),
                None => {
                    println!("ychrome daemon started: pid {new_pid} now serving (none was running)")
                }
            }
            let rows = done["sessions_reattaching"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() {
                println!("no surfaces were attached");
            } else {
                println!(
                    "{} surface(s) re-register on their next heartbeat (~4s); pane drafts and \
                     queued opens did not survive:",
                    rows.len()
                );
                for row in &rows {
                    println!("  {}", row.as_str().unwrap_or("?"));
                }
            }
            // ⛔ THE HANDOVER IS NOT THE WHOLE HOST. `restart` retires the daemon
            // on the socket it resolves and is blind to every daemon bound
            // elsewhere, which is why three of them once accumulated on dev
            // behind a restart that reported success.
            //
            // `Abandoned` only, and the narrowness is load-bearing: a daemon on
            // another root may be a fixture BETWEEN CALLS, which is idle by every
            // signal and still in use. A restart therefore collects what provably
            // cannot be reached and NAMES the rest; ending them is `daemon reap`,
            // which a person types on purpose.
            //
            // `old_pid` is excluded: it released its lock before answering and
            // may still be taking its engine down, by design.
            let swept = daemon::reap(
                daemon::ReapScope::Abandoned,
                false,
                done["old_pid"].as_u64(),
            )?;
            print_reap(&swept);
            Ok(())
        }
        Some(other) => bail!("unknown daemon verb {other:?} (known: list, reap, restart)"),
        None => bail!("usage: ychrome daemon <list|reap|restart>"),
    }
}

/// What the sweep did, and — the half that matters — what it deliberately left
/// alone and why. A reaper that only prints its kills reads as complete when it
/// is not.
fn print_reap(done: &serde_json::Value) {
    let rows = done["daemons"].as_array().cloned().unwrap_or_default();
    let others: Vec<&serde_json::Value> = rows.iter().collect();
    if others.is_empty() {
        println!("no other ychrome daemon on this host");
        return;
    }
    let retired = done["retired"].as_array().map(Vec::len).unwrap_or(0);
    if done["dry_run"].as_bool() == Some(true) {
        println!("dry run — {} other daemon(s), nothing signalled:", others.len());
    } else {
        println!("{} other daemon(s), {retired} retired:", others.len());
    }
    for row in others {
        print_census_row(row, "action");
    }
}

/// What a SECOND invocation (`ychrome <url>` typed while a surface may already
/// be running) does, decided from the daemon's routing reply plus the
/// registry's answer for THIS terminal's stream. Pure, so the hijack lock can
/// hold it down without a daemon.
///
/// The contract (yggterm pending-bugs "AGENT CO-BROWSE" A4): a second
/// invocation NEVER replaces a running page.
///   - Routed ⇒ the url became a NEW TAB in the running session (the daemon
///     enqueued `open_tab`, the GUI verb that MINTS a tab); this process only
///     reports that and exits 0 — the running CLI stays the session owner.
///   - Not routed ⇒ anchor a NEW surface in THIS terminal, saying so out loud.
///   - Not routed AND this terminal's stream already carries another client's
///     live surface ⇒ REFUSE by name. The GUI keys web surfaces by PTY stream,
///     so an anchor here would not open beside that surface's page — it would
///     retarget it. A refusal the user can read beats a page they silently
///     lose.
#[derive(Debug, PartialEq, Eq)]
enum SecondInvocation {
    /// Say what happened, exit 0. Never anchors.
    OpenedAsTab { session: String },
    /// Anchor here, with the act and its reason named (never silent).
    AnchorHere { notice: String },
    /// Anchoring would replace a live surface's page on this same stream:
    /// refuse loudly, exit nonzero.
    Refuse { error: String },
}

/// Why routing did not deliver, phrased for the refusal line.
fn route_refusal_clause(route_reply: Option<&serde_json::Value>) -> &'static str {
    match route_reply.and_then(|reply| reply["reason"].as_str()) {
        Some("gui_not_routing_capable") => " (its GUI predates tab routing)",
        Some(_) => " (the daemon found no routable match)",
        None => " (the daemon did not answer)",
    }
}

fn plan_second_invocation(
    profile: &str,
    route_reply: Option<&serde_json::Value>,
    live_anchor: Option<&daemon::LiveAnchor>,
) -> SecondInvocation {
    if let Some(reply) = route_reply
        && reply["routed"].as_bool() == Some(true)
    {
        return SecondInvocation::OpenedAsTab {
            session: reply["session"]
                .as_str()
                .unwrap_or("a running surface")
                .to_string(),
        };
    }
    // Not routed. This arm is the one that kills the hijack: an anchor lands
    // on THIS terminal's PTY stream, and if another live client already owns a
    // surface on it, the GUI's upsert would retarget that surface's page — the
    // exact silent kill A4 reported. Refuse, whatever profile was asked for
    // (the stream conflict is per-stream, not per-profile).
    if let Some(anchor) = live_anchor {
        return SecondInvocation::Refuse {
            error: format!(
                "this terminal's stream already carries a live [{}] surface (pid {}), and the \
                 url could not be routed into it as a tab{} — anchoring here would REPLACE that \
                 surface's page, so ychrome refuses. Open from another terminal, or close the \
                 running surface first (`ychrome status` lists it).",
                anchor.profile,
                anchor.pid,
                route_refusal_clause(route_reply),
            ),
        };
    }
    let notice = match route_reply {
        Some(reply) if reply["reason"].as_str() == Some("gui_not_routing_capable") => format!(
            "a running [{profile}] surface exists but its GUI predates tab routing — anchoring \
             a new surface in this terminal instead (the running page is untouched)"
        ),
        Some(_) => format!(
            "no running [{profile}] session to open a tab in — anchoring a new surface in this \
             terminal"
        ),
        None => format!(
            "ychrome daemon unreachable, cannot see running [{profile}] sessions — anchoring a \
             new surface in this terminal"
        ),
    };
    SecondInvocation::AnchorHere { notice }
}

/// The honest one-liner for a routed open. It says WHAT HAPPENED — a new tab
/// in the running session, not a navigation — because the old line ("opened
/// <url> in session <id>") read exactly like the page replacement A4's
/// reporter watched destroy a live logged-in page.
fn opened_as_tab_line(url: &str, profile: &str, session: &str) -> String {
    format!("ychrome: opened {url} as a new tab in the running [{profile}] session ({session})")
}

/// Every verb `main` dispatches off argv before clap. **This is the one list**
/// — the dispatch arms below read as string literals for clarity, and
/// `every_dispatched_subcommand_is_reserved` locks them to this array, so a new
/// verb cannot be added without appearing here.
///
/// Reserving a name is what lets an unknown-but-subcommand-shaped token fail
/// loudly instead of being browsed to. See the guard at the end of `main`.
const RESERVED_SUBCOMMANDS: &[&str] = &[
    "status",
    "daemon",
    "provision",
    "adblock",
    "ctl",
    "engine",
    "identity",
    "tls",
];

/// `ychrome identity [<host>] [--set <preset>|--reset] [--json]`.
///
/// With no host it reports the browser-wide decision and every per-site
/// override. With a host it reports what that ONE site gets, which is the
/// question a user actually has ("why does this login keep looping?").
///
/// The warning is printed on every `--set`, not tucked into `--help`: a spoofed
/// identity that breaks a fingerprint-gated login fails as a challenge that
/// never clears, and nothing on screen would otherwise connect the two.
fn run_identity_verb(args: &[String]) -> Result<()> {
    let as_json = args.iter().any(|arg| arg == "--json");
    let set = args
        .iter()
        .position(|arg| arg == "--set")
        .and_then(|index| args.get(index + 1))
        .map(String::as_str);
    // ⛔ A FLAG'S VALUE IS NOT A HOSTNAME. This used to take the first argument
    // that did not start with `--`, so `identity --set chrome` read "chrome" as
    // the SITE and wrote a per-site rule for a host that does not exist, while
    // reporting success — the browser-wide preset it was asked for never moved.
    // Every value-taking flag must be skipped along with its value, and a new
    // one added below without touching this list would put the bug straight
    // back, so the list lives in ONE place.
    const VALUE_FLAGS: [&str; 2] = ["--set", "--profile"];
    let mut positional: Vec<&str> = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if VALUE_FLAGS.contains(&arg.as_str()) {
            skip = true;
            continue;
        }
        if !arg.starts_with("--") {
            positional.push(arg);
        }
    }
    let host = positional.first().copied();
    let reset = args.iter().any(|arg| arg == "--reset");
    // ⭐ `--profile P` scopes the read AND the write to one profile. Without it
    // this verb speaks for the browser, which is the layer every profile
    // inherits — the same division the settings pane draws, where the pane is
    // always inside a profile and therefore always writes one.
    let profile = args
        .iter()
        .position(|arg| arg == "--profile")
        .and_then(|index| args.get(index + 1))
        .map(String::as_str);

    if set.is_some() || reset {
        match host {
            Some(host) => {
                useragent::set_site_scoped(profile, host, set)?;
                if set.is_some() {
                    eprintln!("ychrome: {}", useragent::OVERRIDE_WARNING);
                }
            }
            // No host means the whole-browser decision for whatever scope was
            // named. `--reset` there is the engine preset, which IS the default
            // — spelled out rather than silently doing nothing.
            None => useragent::set_preset_scoped(profile, set.unwrap_or("engine"))?,
        }
    }

    let (global, sites) = match profile {
        Some(profile) => (
            useragent::preset_for_profile(profile),
            useragent::sites_for_profile(profile),
        ),
        None => (useragent::preset(), useragent::sites()),
    };
    if as_json {
        let effective = host.map(|host| {
            serde_json::json!({
                "host": host,
                "preset": useragent::preset_for_host(&sites, global, host).id(),
                "user_agent": match profile {
                    Some(profile) => useragent::effective_for_profile_host(profile, host),
                    None => useragent::effective_for_host(host),
                },
            })
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "profile": profile,
                "preset": global.id(),
                "user_agent": match profile {
                    Some(profile) => useragent::effective_for_profile(profile),
                    None => useragent::effective(),
                },
                "sites": match profile {
                    Some(profile) => useragent::sites_json_for_profile(profile),
                    None => useragent::sites_json(),
                },
                "site": effective,
            }))?
        );
        return Ok(());
    }
    // `(engine default)` rather than an invented string: WebKitGTK owns its own
    // UA, and printing a copy here is how a stale constant gets born.
    let show = |ua: Option<String>| ua.unwrap_or_else(|| "(engine default)".to_string());
    println!(
        "{}: {} — {}",
        match profile {
            Some(profile) => format!("profile “{profile}”"),
            None => "browser-wide".to_string(),
        },
        global.label(),
        show(match profile {
            Some(profile) => useragent::effective_for_profile(profile),
            None => useragent::effective(),
        })
    );
    if sites.is_empty() {
        println!("per-site overrides: none");
    } else {
        println!("per-site overrides:");
        for (site, preset) in &sites {
            println!("  {site}: {}", preset.label());
        }
    }
    if let Some(host) = host {
        println!(
            "\n{host}: {} — {}",
            useragent::preset_for_host(&sites, global, host).label(),
            show(match profile {
                Some(profile) => useragent::effective_for_profile_host(profile, host),
                None => useragent::effective_for_host(host),
            })
        );
    }
    Ok(())
}

/// Whether `token` is subcommand-shaped rather than URL-shaped: a single bare
/// label with no scheme, no dot, no port and no path.
///
/// `localhost` is the one bare label that IS reachable, so it is allowed
/// through; every other bare word would become `https://<word>`, which cannot
/// resolve. Deterministic by construction — no guessing, no distance metric,
/// no network lookup.
fn looks_like_a_bare_subcommand(token: &str) -> bool {
    if token.is_empty() || token == "localhost" || token == "about:blank" {
        return false;
    }
    !token.contains("://")
        && !token.contains('.')
        && !token.contains(':')
        && !token.contains('/')
        && !token.contains('?')
}

fn main() -> Result<()> {
    // Internal/agent entry points, dispatched off argv before clap so the
    // open-a-url arg shape stays exactly as it was. `--daemon` is the host
    // daemon itself (spawned detached by the view client); `status` is the
    // host-side truth for agents; `daemon <verb>` supervises the running one.
    // Every name here must also appear in RESERVED_SUBCOMMANDS — locked.
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).map(String::as_str) == Some("--daemon") {
        // ⛔ THE DAEMON MUST RECONCILE TOO, and this call is why. The reconcile
        // below (before `policy()`) only runs on the open-a-url path, so a host
        // whose ychrome is only ever *started as a daemon* — which is the normal
        // case, since `ensure()` spawns it detached — would never notice that
        // the binary on disk now carries newer bundled assets than the copies in
        // ~/.yggterm. Caught live on 2026-07-31: after a fleet deploy and a
        // `daemon restart`, the host still held the old 60-rule ruleset and no
        // cosmetic script, and only a hand-run `ychrome provision` fixed it.
        // A bundled asset that can sit dead on a host is the exact failure this
        // module exists to end, so the daemon takes the same reconcile at the
        // earliest moment after a binary swap that anything runs at all.
        provision::reconcile_and_report();
        return daemon::run();
    }
    if raw.get(1).map(String::as_str) == Some("status") {
        let as_json = raw.iter().any(|arg| arg == "--json");
        return run_status(as_json);
    }
    if raw.get(1).map(String::as_str) == Some("daemon") {
        let as_json = raw.iter().any(|arg| arg == "--json");
        return run_daemon_verb(raw.get(2).map(String::as_str), as_json);
    }
    // `adblock <verb>` is maintenance on THIS host's ruleset: refresh it from
    // the upstream lists, or say what is installed. Same pre-clap dispatch as
    // the verbs above, so the open-a-url arg shape is untouched.
    // `provision` runs the bundled-asset reconcile on demand and reports it.
    // The same call `main` makes at launch, so an agent can see exactly what a
    // launch would do without opening a surface.
    if raw.get(1).map(String::as_str) == Some("provision") {
        return provision::run(raw.iter().any(|arg| arg == "--json"));
    }
    if raw.get(1).map(String::as_str) == Some("adblock") {
        return adblock::run(raw.get(2).map(String::as_str), &raw[2.min(raw.len())..]);
    }
    // The agent engine (docs/agent-engine.md). Headless by construction: it
    // never opens a window on the invoking terminal's display and never emits
    // OSC 7717, so it is a peer of the surface path rather than a mode of it.
    // `ychrome ctl <verb>` — the engine's thin client (docs/agent-engine.md §3).
    // Dispatched here for the same reason as `engine`: argv-prefix, before
    // clap, so the browser's url arg shape is untouched.
    if raw.get(1).map(String::as_str) == Some("ctl") {
        let outcome = engine::ctl::run(&raw[2..]);
        engine::exit_with(outcome);
    }
    if raw.get(1).map(String::as_str) == Some("engine") {
        let as_json = raw.iter().any(|arg| arg == "--json");
        engine::run_verb_and_exit(raw.get(2).map(String::as_str), as_json);
    }
    // `identity` — read or set what this browser says it is. The settings pane
    // is the same decision through a different door; this one exists because a
    // host with no GUI still has to be able to answer "what am I sending, and to
    // whom", and because an override is a thing an agent should be able to
    // PROVE it set rather than click and hope.
    if raw.get(1).map(String::as_str) == Some("identity") {
        return run_identity_verb(&raw[2..]);
    }
    // `tls` — the per-host exception list for servers whose chain is valid but
    // presented in an order GnuTLS will not search (see `tlspin`). Dispatched
    // here with the others so it is auditable from a host with no GUI: the
    // question "what has this browser been told to excuse, and why" must be
    // answerable without opening a window.
    if raw.get(1).map(String::as_str) == Some("tls") {
        return tlspin::run(&raw[2..]);
    }

    // ⛔ A SUBCOMMAND-SHAPED TOKEN MUST NEVER FALL THROUGH INTO THE URL.
    //
    // `Args` takes a positional `[URL]`, so before this guard ANY bare word in
    // argv[1] was accepted as a URL — including one that is obviously a verb.
    // `ychrome ctl --help` printed the plain usage and **exited 0** on a binary
    // that had never heard of `ctl`, and bare `ychrome ctl` did not fail at all:
    // it tried to browse to `https://ctl` and HUNG.
    //
    // That is not cosmetic. `cmd sub --help; echo $?` is how everyone probes
    // whether a build has a feature, and it answered YES on a build without it
    // — which produced a false fleet-wide deploy report on 2026-07-31.
    //
    // Reserving the known verbs is necessary but NOT sufficient, and it is
    // worth being clear why: an old binary cannot know the name of a verb added
    // after it was built, so a reserved list can never fix the cross-version
    // case that caused the bad report. What fixes it permanently is refusing a
    // token that CANNOT be a URL. A single bare label has no scheme, no dot, no
    // port and no path; `https://ctl` can never resolve, while `localhost` can,
    // so that one name is allowed through. Anything else bare is ambiguous, and
    // the honest answer to an ambiguous argument is a question, not a hang.
    if let Some(first) = raw.get(1).map(String::as_str)
        && !first.starts_with('-')
        && looks_like_a_bare_subcommand(first)
    {
        bail!(
            "ychrome: {first:?} is not a known subcommand of this build (ychrome {}), \
             and it cannot be a URL — a bare word has no scheme, dot or port.\n\
             \x20 known subcommands: {}\n\
             \x20 if you meant a host, give it a scheme: ychrome http://{first}",
            env!("CARGO_PKG_VERSION"),
            RESERVED_SUBCOMMANDS.join(", "),
        );
    }

    let args = Args::parse();

    // Declare ourselves to this host's yggterm launcher registry, on EVERY run:
    // that is what repairs the recorded binary path after an upgrade moves it.
    // Never fatal — a browser must not refuse to start over a menu entry.
    if let Err(error) = manifest::write() {
        eprintln!("ychrome: could not register launcher manifest ({error})");
    }

    // Bring this host's copies of the bundled assets up to date BEFORE the
    // policy is built, because `policy()` is a read of the disk and a GET must
    // not mutate it. This is where a userscript that predates its metadata
    // block gets replaced, and where a host that never had an adblock ruleset
    // gets one — both silently degraded states until now, and both loud from
    // here on (crate::provision).
    provision::reconcile_and_report();

    let raw_url = args.url.clone().unwrap_or_else(|| "about:blank".into());
    let raw_url = if raw_url.contains("://") || raw_url == "about:blank" {
        raw_url
    } else {
        format!("http://{raw_url}")
    };

    // Inside yggterm: thin-client mode — the yggterm GUI renders; locality
    // comes from where this command runs. `--via` is standalone-only by
    // design.
    if args.via.is_none()
        && let Some(session) = yggterm_thin_client_session()
    {
        // No URL → profile picker on a loopback http page. This also replaces
        // the old about:blank open, which the GUI's scheme gate rejects.
        if args.url.is_none() {
            return run_thin_client_picker(&session);
        }
        // A url typed in a terminal ROUTES into a matching running surface as
        // a NEW TAB (Chrome-like: raise the session, add the tab; the running
        // CLI stays the anchor and the session owner), unless --here forces a
        // new anchor in THIS terminal. Anything not routed anchors here —
        // LOUDLY, never silently, and NEVER onto a stream that already carries
        // another client's live surface: the GUI keys web surfaces by PTY
        // stream, so a second anchor on one stream does not open beside the
        // running page, it RETARGETS it (the A4 hijack — a live logged-in page
        // was destroyed mid-job this way). The queue-and-ping transport is the
        // only fleet-correct one (docs/host-daemon.md §4).
        if !args.here {
            let route_reply = daemon::route(&args.profile, &raw_url, None, false).ok();
            let routed = route_reply
                .as_ref()
                .is_some_and(|reply| reply["routed"].as_bool() == Some(true));
            // The registry probe matters only when we are about to anchor; a
            // routed url never anchors.
            let anchor = if routed {
                None
            } else {
                daemon::live_anchor(&session, std::process::id())
            };
            match plan_second_invocation(&args.profile, route_reply.as_ref(), anchor.as_ref()) {
                SecondInvocation::OpenedAsTab { session: target } => {
                    println!("{}", opened_as_tab_line(&raw_url, &args.profile, &target));
                    return Ok(());
                }
                SecondInvocation::Refuse { error } => bail!("{error}"),
                SecondInvocation::AnchorHere { notice } => eprintln!("ychrome: {notice}"),
            }
        } else if let Some(anchor) = daemon::live_anchor(&session, std::process::id()) {
            // --here means "a second surface in THIS terminal", but one stream
            // holds one surface: forcing it would retarget the live surface's
            // page, not open beside it. Same hijack, same refusal.
            bail!(
                "--here cannot anchor: this terminal's stream already carries a live [{}] \
                 surface (pid {}), and a second anchor on one stream replaces its page instead \
                 of opening beside it. Run from another terminal, or close that surface first \
                 (`ychrome status` lists it).",
                anchor.profile,
                anchor.pid
            );
        }
        let title = args.title.clone().unwrap_or_else(|| {
            Url::parse(&raw_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .map(|h| format!("ychrome — {h}"))
                .unwrap_or_else(|| "ychrome".to_string())
        });
        return run_thin_client(&session, &raw_url, &title, &args.profile);
    }

    if !display_available() {
        bail!(
            "no display (DISPLAY/WAYLAND_DISPLAY unset) — standalone mode needs a desktop.\n\
             Inside a yggterm terminal ychrome drives the session viewport instead; that mode\n\
             activates automatically via YGGTERM_SESSION_ID / YGGTERM_TERM_PROGRAM. If this IS\n\
             a yggterm session, the host daemon predates the env handshake — update yggterm on\n\
             this machine or run: export YGGTERM_TERM_PROGRAM=yggterm"
        );
    }

    // Resolve --via: open a SOCKS tunnel and route the webview through it.
    // The URL is UNCHANGED (the remote sshd resolves the host); only the
    // network path is rewritten, so https certs match and cross-origin
    // navigation stays on the session's network. The tunnel handle must
    // outlive the event loop, so it is held below.
    let mut tunnel: Option<Tunnel> = None;
    let proxy_config = if let Some(via) = &args.via {
        // Parse only to fail early on a nonsense URL; the value is untouched.
        Url::parse(&raw_url).context("parsing URL for --via")?;
        eprintln!("ychrome: opening ssh SOCKS tunnel via {via} …");
        let t = open_tunnel(via)?;
        let local_port = t.local_port;
        eprintln!(
            "ychrome: tunnel up — egress on {via}'s network (socks5://127.0.0.1:{local_port})"
        );
        tunnel = Some(t);
        Some(ProxyConfig::Socks5(ProxyEndpoint {
            host: "127.0.0.1".to_string(),
            port: local_port.to_string(),
        }))
    } else {
        None
    };
    let final_url = raw_url;

    let title = args.title.clone().unwrap_or_else(|| {
        let host = Url::parse(&final_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        match (host, args.via.as_deref(), args.profile.as_str()) {
            (Some(h), Some(v), _) => format!("ychrome — {h} via {v}"),
            (Some(h), None, "default") => format!("ychrome — {h}"),
            (Some(h), None, p) => format!("ychrome — {h} [{p}]"),
            _ => "ychrome".to_string(),
        }
    });

    let data_dir = profile_dir(&args.profile)?;
    // The temp profile's throwaway jar is deleted on window close (below);
    // remember where it is.
    let temp_jar = (args.profile == TEMP_PROFILE).then(|| data_dir.clone());
    let mut web_context = WebContext::new(Some(data_dir));

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(&title)
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 840.0))
        .build(&event_loop)
        .context("creating window")?;

    let mut builder = WebViewBuilder::new_with_web_context(&mut web_context).with_url(&final_url);
    if let Some(proxy_config) = proxy_config {
        builder = builder.with_proxy_config(proxy_config);
    }
    // The same identity the thin-client surfaces get (yggterm applies it there,
    // from `/policy`). A standalone window is the same browser.
    //
    // Resolved against THIS window's host, so a per-site override applies here
    // too: wry fixes the UA at webview creation and a standalone window opens on
    // exactly one URL, so the host is known at the only moment that matters.
    // With no override and no global preset this is `None` — WebKitGTK's own
    // identity, which is the coherent one (see `useragent`).
    let identity_host = Url::parse(&final_url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string));
    let user_agent = match identity_host.as_deref() {
        Some(host) => crate::useragent::effective_for_host(host),
        None => crate::useragent::effective(),
    };
    if let Some(user_agent) = user_agent {
        builder = builder.with_user_agent(&user_agent);
    }

    #[cfg(not(target_os = "linux"))]
    let _webview = builder.build(&window).context("creating webview")?;
    #[cfg(target_os = "linux")]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().context("no gtk vbox")?;
        let webview = builder.build_gtk(vbox).context("creating webview")?;
        install_tls_pin_handler(&webview);
        webview
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            // Dropping the tunnel kills the ssh child.
            tunnel.take();
            // Best-effort: a temp-profile jar leaves nothing behind.
            if let Some(jar) = &temp_jar {
                let _ = std::fs::remove_dir_all(jar);
            }
            *control_flow = ControlFlow::Exit;
        }
    });
}

#[cfg(test)]
mod second_invocation_tests {
    use super::*;
    use serde_json::json;

    fn live(profile: &str, pid: i64) -> daemon::LiveAnchor {
        daemon::LiveAnchor {
            pid,
            profile: profile.to_string(),
        }
    }

    // The browser convention, locked: a routed url became a NEW TAB in the
    // running session; this process reports exactly that and exits — it never
    // reaches an anchor. The message must NAME the act, because the old
    // phrasing ("opened <url> in session <id>") was indistinguishable from
    // the navigation that destroyed A4's live page.
    #[test]
    fn a_routed_url_reports_a_new_tab_in_the_running_session_and_never_anchors() {
        let reply = json!({ "ok": true, "routed": true, "session": "env-1" });
        assert_eq!(
            plan_second_invocation("health", Some(&reply), None),
            SecondInvocation::OpenedAsTab {
                session: "env-1".to_string()
            }
        );
        let line = opened_as_tab_line("https://example.com", "health", "env-1");
        assert!(
            line.contains("as a new tab in the running"),
            "the routed line must say a NEW TAB was opened, not read like a navigation: {line}"
        );
        assert!(line.contains("[health]") && line.contains("env-1"));
    }

    // THE HIJACK LOCK (yggterm pending-bugs "AGENT CO-BROWSE" A4). When the
    // url could not be routed and THIS terminal's stream already carries
    // another client's live surface, the second invocation must REFUSE — every
    // unrouted shape (no match, a pre-routing GUI, no daemon answer) included.
    // The GUI keys surfaces by stream: an anchor here would retarget the live
    // surface's page, which is precisely the silent page replacement this
    // change exists to kill. If this test turns red, the hijack is back.
    #[test]
    fn an_unrouted_url_on_a_stream_with_a_live_anchor_is_refused_never_anchored() {
        let anchor = live("health", 4242);
        let unrouted_shapes: Vec<Option<serde_json::Value>> = vec![
            Some(json!({ "ok": true, "routed": false, "reason": "no_match" })),
            Some(json!({ "ok": true, "routed": false, "reason": "gui_not_routing_capable" })),
            Some(json!({ "ok": false, "error": "malformed" })),
            None, // daemon unreachable (defense in depth: the probe shares its socket)
        ];
        for reply in &unrouted_shapes {
            match plan_second_invocation("work", reply.as_ref(), Some(&anchor)) {
                SecondInvocation::Refuse { error } => {
                    assert!(
                        error.contains("REPLACE") && error.contains("refuses"),
                        "the refusal must name the hijack it prevents: {error}"
                    );
                    assert!(
                        error.contains("[health]") && error.contains("4242"),
                        "the refusal must name the surface it protects: {error}"
                    );
                }
                other => panic!(
                    "an unrouted url over a live anchor must refuse, got {other:?} for {reply:?}"
                ),
            }
        }
    }

    // The fallback is kept (anchor a new surface HERE when nothing routable
    // exists and this stream is free) but it may never be silent: each shape
    // names what it is about to do and why.
    #[test]
    fn every_anchor_fallback_names_the_act_out_loud() {
        let cases: Vec<(Option<serde_json::Value>, &str)> = vec![
            (
                Some(json!({ "ok": true, "routed": false, "reason": "no_match" })),
                "no running [work] session",
            ),
            (
                Some(json!({ "ok": true, "routed": false, "reason": "gui_not_routing_capable" })),
                "predates tab routing",
            ),
            (None, "daemon unreachable"),
        ];
        for (reply, expected) in &cases {
            match plan_second_invocation("work", reply.as_ref(), None) {
                SecondInvocation::AnchorHere { notice } => {
                    assert!(
                        notice.contains(expected),
                        "the notice must name its reason ({expected}): {notice}"
                    );
                    assert!(
                        notice.contains("anchoring a new surface in this terminal"),
                        "the notice must name the act: {notice}"
                    );
                }
                other => panic!("a free stream anchors here, got {other:?} for {reply:?}"),
            }
        }
    }

    /// THE STALE DECLARE, made impossible rather than merely avoided.
    ///
    /// The declare carries the control url AND the control token, and a daemon
    /// handover moves both at once. The loop used to keep the last-known
    /// endpoint in a local and re-declare it whenever the re-register failed —
    /// which is exactly what happens DURING a handover — so it published a pair
    /// belonging to a daemon that had already exited. This locks the shape that
    /// removes the possibility: exactly one `emit_declare` in the whole program,
    /// inside a function that registers first and takes no endpoint to be handed
    /// a stale one.
    #[test]
    fn a_declare_can_only_carry_a_registration_it_just_made() {
        let source = include_str!("main.rs");
        let product = source
            .split("\n#[cfg(test)]")
            .next()
            .expect("the product half of main.rs");
        assert_eq!(
            product.matches("sidebar::emit_declare(").count(),
            1,
            "a second declare call site is a second place a cached endpoint can \
             be published; there is one, and it is `declare_current`"
        );
        let body = product
            .split("fn declare_current(session: &str, profile: &str) -> bool {")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("declare_current body present");
        assert!(
            body.contains("daemon::register_supervised(session, profile)")
                && body.contains("sidebar::emit_declare("),
            "the declare must be fed by a registration made in the same call"
        );
        let drive = product
            .split("fn drive_surface(")
            .nth(1)
            .and_then(|rest| rest.split("\n/// The surface the picker").next())
            .expect("drive_surface body present");
        assert!(
            !drive.contains("ControlEndpoint"),
            "the surface loop is holding an endpoint across ticks again — that \
             variable is the only way a dead url+token pair reaches the GUI"
        );
        assert!(
            !drive.contains("emit_declare"),
            "the loop declares directly again, bypassing the register-then-declare \
             pairing that makes staleness impossible"
        );
    }

    /// ⛔ THE SURFACE LOOP IS THE ONLY WAY A PASSKEY REQUEST REACHES A HUMAN.
    ///
    /// The signer runs inside the host daemon, whose stdout is `/dev/null`, and
    /// the GUI routes a presence request by the STREAM it arrives on. This
    /// process holds that stream, so a ceremony that this loop does not publish
    /// is a ceremony nobody can approve — the page waits out the full two-minute
    /// timeout and then reports a generic failure, which is exactly what the
    /// button being broken would look like.
    ///
    /// Asserted on the loop with comments stripped, because the paragraph above
    /// this one would otherwise satisfy the check by itself.
    #[test]
    fn the_surface_loop_publishes_passkey_requests_on_every_tick() {
        let product = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("source before the test module");
        let drive = product
            .split("fn drive_surface(")
            .nth(1)
            .and_then(|rest| rest.split("\n/// The surface the picker").next())
            .expect("drive_surface body present");
        let code: String = drive
            .lines()
            .map(|line| match line.find("//") {
                Some(at) => &line[..at],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let loop_body = code
            .split("while !stop.load(")
            .nth(1)
            .expect("the surface loop body");
        assert!(
            loop_body.contains("publish_presence_requests(session)"),
            "the surface loop stopped publishing presence requests: every passkey \
             ceremony now parks for its full timeout with no dialog"
        );
        // On the TICK, not gated behind the ~4s heartbeat: a human is waiting on
        // a dialog, and four seconds of nothing is a broken button.
        let before_heartbeat = loop_body
            .split("if ticks.is_multiple_of(20)")
            .next()
            .expect("the pre-heartbeat part of the tick");
        assert!(
            before_heartbeat.contains("publish_presence_requests(session)"),
            "presence publishing moved behind the heartbeat gate, so a passkey \
             dialog can now be up to a heartbeat late"
        );
    }

    /// ⛔ `daemon restart` sweeps ONLY what no client can reach.
    ///
    /// The wide scope belongs to `daemon reap`, which a person types on purpose.
    /// Widening it here would make every restart end idle daemons on roots it
    /// knows nothing about — including a test fixture's, between two of its own
    /// calls, where "idle" is true and "unused" is not. The scope rule itself is
    /// proven in `daemon::tests::a_restart_scope_collects_the_unreachable_and_asks_nothing`;
    /// this locks the caller that chooses it.
    #[test]
    fn the_restart_sweep_never_reaches_another_root() {
        let source = include_str!("main.rs");
        let arm = source
            .split("Some(\"restart\") => {")
            .nth(1)
            .and_then(|suffix| suffix.split("\n        Some(other)").next())
            .expect("the restart arm");
        assert!(
            arm.contains("daemon::ReapScope::Abandoned"),
            "the restart sweep must be the narrow one — a foreign root's daemon \
             is not a restart's to end"
        );
        assert!(
            !arm.contains("AlsoForeignRoots"),
            "the restart arm reaches other roots again"
        );
    }

    /// Every verb `main` actually dispatches must be in `RESERVED_SUBCOMMANDS`.
    /// Without this, adding a verb and forgetting to reserve it silently
    /// restores the swallow-as-URL bug for that one name.
    #[test]
    fn every_dispatched_subcommand_is_reserved() {
        let source = include_str!("main.rs");
        let body = source
            .split("fn main() -> Result<()> {")
            .nth(1)
            .expect("main body present")
            .split("let args = Args::parse();")
            .next()
            .expect("the pre-clap dispatch region");
        let mut dispatched: Vec<String> = Vec::new();
        for piece in body
            .split("raw.get(1).map(String::as_str) == Some(\"")
            .skip(1)
        {
            let name = piece.split('"').next().expect("verb literal closes");
            dispatched.push(name.to_string());
        }
        assert!(
            !dispatched.is_empty(),
            "the dispatch region must contain verbs — did the shape change?"
        );
        for verb in &dispatched {
            if verb.starts_with("--") {
                continue; // `--daemon` is a flag, not a browsable token
            }
            assert!(
                RESERVED_SUBCOMMANDS.contains(&verb.as_str()),
                "{verb:?} is dispatched but NOT reserved — a token that means a \
                 verb here would be browsed to as https://{verb} on any build \
                 that does not dispatch it"
            );
        }
    }

    /// The guard's shape rule, stated as cases rather than prose.
    #[test]
    fn a_bare_label_is_a_subcommand_and_anything_url_shaped_is_not() {
        for verb in ["ctl", "engine", "clt", "stauts", "someverb"] {
            assert!(
                looks_like_a_bare_subcommand(verb),
                "{verb:?} is a bare label and can never resolve as https://{verb}"
            );
        }
        for url in [
            "example.com",
            "http://example.com",
            "https://example.com/x?y",
            "localhost",
            "localhost:3000",
            "about:blank",
            "198.51.100.1",
            "oi.example.com/c/abc",
        ] {
            assert!(
                !looks_like_a_bare_subcommand(url),
                "{url:?} is URL-shaped and must reach the browser untouched"
            );
        }
    }

    /// ⛔ THE DAEMON RECONCILES ITS BUNDLED ASSETS BEFORE IT SERVES.
    ///
    /// This was live-broken on 2026-07-31 and no test noticed. `--daemon`
    /// returned from `main()` long before the reconcile on the open-a-url path,
    /// so a host whose ychrome is only ever *started as a daemon* — the normal
    /// case, because `ensure()` spawns it detached — kept its old assets
    /// forever. After a fleet deploy plus a `daemon restart`, the GUI host still held
    /// the 60-rule ruleset and had no cosmetic script at all; only a hand-run
    /// `ychrome provision` repaired it.
    ///
    /// The provision module exists precisely so a bundled asset cannot sit dead
    /// on a host, and the daemon door was a hole straight through it. Asserting
    /// the ORDER matters as much as the presence: reconciling after
    /// `daemon::run()` would never run, since that call does not return.
    #[test]
    fn the_daemon_arm_reconciles_bundled_assets_before_it_starts_serving() {
        let source = include_str!("main.rs");
        let body = source
            .split("fn main() -> Result<()> {")
            .nth(1)
            .expect("main body present");
        let arm = body
            .split(r#"== Some("--daemon")"#)
            .nth(1)
            .expect("the --daemon arm is present");
        let reconcile_at = arm.find("provision::reconcile_and_report();").expect(
            "the --daemon arm MUST reconcile bundled assets — a daemon-only host \
             never reaches the open-a-url reconcile, so without this a deployed \
             ruleset sits dead on disk (live-caught 2026-07-31)",
        );
        let serve_at = arm
            .find("return daemon::run();")
            .expect("the --daemon arm serves via daemon::run()");
        assert!(
            reconcile_at < serve_at,
            "reconcile must come BEFORE daemon::run(), which never returns"
        );
    }

    // ANCHOR: the wiring in main(). The plan is only a lock if the
    // second-invocation block actually consults it, the routed arm returns
    // BEFORE the anchor call, and the refusal arm bails. A refactor that
    // reverted to calling `daemon::route` and anchoring inline would leave
    // every test above green while re-opening the hijack.
    #[test]
    fn main_routes_the_second_invocation_through_the_plan_before_any_anchor() {
        let source = include_str!("main.rs");
        let body = source
            .split("fn main() -> Result<()> {")
            .nth(1)
            .expect("main body present");
        let plan_at = body
            .find("plan_second_invocation(&args.profile")
            .expect("main's url path must consult plan_second_invocation");
        let anchor_at = body
            .find("run_thin_client(&session, &raw_url")
            .expect("the thin-client anchor call is present");
        assert!(
            plan_at < anchor_at,
            "the plan must be consulted before the thin-client anchor"
        );
        let routed_arm = body
            .split("SecondInvocation::OpenedAsTab { session: target } => {")
            .nth(1)
            .and_then(|rest| rest.split("SecondInvocation::Refuse").next())
            .expect("the routed arm exists in main");
        assert!(
            routed_arm.contains("return Ok(())"),
            "the routed arm must exit without anchoring"
        );
        assert!(
            body.contains("SecondInvocation::Refuse { error } => bail!"),
            "the refusal arm must exit nonzero, not fall through to an anchor"
        );
        // --here has the same stream-conflict guard: one stream, one surface.
        assert!(
            body.contains("else if let Some(anchor) = daemon::live_anchor(&session"),
            "--here must consult the live-anchor probe before anchoring"
        );
    }
}
