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

use anyhow::{bail, Context, Result};

mod manifest;
mod passkey;
mod sidebar;
mod webpolicy;
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
fn profile_dir(profile: &str) -> Result<PathBuf> {
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
fn emit_web_surface_osc(action: &str, session: &str, url: &str, title: &str, profile: &str) {
    use base64::Engine as _;
    let payload = format!(
        "{{\"session\":{},\"url\":{},\"title\":{},\"profile\":{}}}",
        serde_json_string(session),
        serde_json_string(url),
        serde_json_string(title),
        serde_json_string(profile),
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\u{1b}]7717;web-surface;{action};{encoded}\u{7}");
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
    // ychrome CONTRIBUTES its vault and settings panes rather than yggterm
    // hardcoding them. A failure here must never take the browser down: the
    // surface is the product, the sidebar is an extra.
    //
    // DECLARE BEFORE OPEN. The GUI holds a surface's creation until it has
    // fetched the app's policy, because a userscript only injects at
    // document-start. Open first and the GUI's first apply pass sees a surface
    // with no contribution and builds it unblocked — no userscripts, no adblock,
    // silently, for the life of that webview.
    let sidebar = match sidebar::spawn(profile, session) {
        Ok(sidebar) => {
            sidebar::emit_declare(session, &sidebar.control_url, &webpolicy::policy_version(profile));
            Some(sidebar)
        }
        Err(error) => {
            eprintln!("ychrome: sidebar unavailable ({error})");
            None
        }
    };
    emit_web_surface_osc("open", session, url, title, profile);
    eprintln!("ychrome: web surface open — {url} [{profile}]  (Ctrl+C to close, Ctrl+Z / yggterm Zzz to suspend)");
    let mut ticks: u32 = 0;
    let mut last_tick = std::time::Instant::now();
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        // A large gap between ticks means we were suspended (Ctrl+Z /
        // SIGSTOP — yggterm's Zzz button) or the machine slept, and the GUI
        // may have closed or swept the surface meanwhile. Re-emit "open" on
        // resume: heartbeats deliberately cannot re-CREATE a surface, and an
        // "open" with an unchanged URL is liveness-idempotent GUI-side.
        if last_tick.elapsed() > Duration::from_secs(3) {
            // Same order as the first emit, for the same reason: the GUI may have
            // swept the contribution while we were stopped, and a surface
            // recreated before the policy lands loses its userscripts.
            if let Some(sidebar) = &sidebar {
                sidebar::emit_declare(
                    session,
                    &sidebar.control_url,
                    &webpolicy::policy_version(profile),
                );
            }
            emit_web_surface_osc("open", session, url, title, profile);
        }
        last_tick = std::time::Instant::now();
        ticks += 1;
        // Heartbeat every ~4s (20 × 200ms) — the GUI's liveness truth.
        if ticks.is_multiple_of(20) {
            emit_web_surface_osc("heartbeat", session, url, title, profile);
            // Re-declaring IS the sidebar's heartbeat: it is idempotent, and
            // the GUI expires a contribution whose declares stop, so a SIGKILLed
            // ychrome never leaves phantom buttons in the rail. The stamp rides
            // along so the GUI notices a policy edit made while ychrome runs —
            // it is stat-only, so recomputing it every 4s is cheap.
            if let Some(sidebar) = &sidebar {
                sidebar::emit_declare(
                    session,
                    &sidebar.control_url,
                    &webpolicy::policy_version(profile),
                );
            }
        }
    }
    if let Some(sidebar) = &sidebar {
        sidebar::emit_close(session);
        sidebar.stop();
    }
    emit_web_surface_osc("close", session, url, title, profile);
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
    config.get(key).and_then(|value| value.as_str()).map(str::to_string)
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
        let loopback =
            matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]");
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
        cards.push_str(&format!(
            "<label class=\"card\"><input type=\"radio\" name=\"profile\" value=\"{pe}\"{checked}>\
             <span class=\"avatar\">{ie}</span><span class=\"pname\">{pe}</span></label>",
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
</style></head><body>
<form class="panel" action="/open" method="get">
  <h1>Choose a profile</h1>
  <p class="sub">Each profile keeps its own cookies and logins. Type a URL, or leave it blank to start on search.</p>
  <div class="urlrow">
    <input type="text" name="url" placeholder="URL or search — e.g. localhost:8000" autofocus autocomplete="off" spellcheck="false">
    <button type="submit">Open</button>
  </div>
  <div class="grid">
    {cards}
    <label class="card tempcard" title="No history, cookies or storage kept — everything vanishes on close">
      <input type="radio" name="profile" value="temp">
      <span class="avatar">&#9202;</span><span class="pname">Temporary</span></label>
    <label class="card newcard"><input type="radio" name="profile" value="" id="newradio">
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
</script>
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
    let resp = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// Handle one loopback request. `/` serves the picker; `/open?url=&profile=`
/// retargets the surface (updates the heartbeat target + emits a fresh OSC
/// open with the chosen url+profile).
fn handle_picker_conn(stream: TcpStream, session: &str, target: &Arc<Mutex<SurfaceTarget>>) {
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
                };
            }
            emit_web_surface_osc("open", session, &url, &title, &profile);
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
    // picker; the OSC url is this loopback CONTROL endpoint the GUI GETs
    // /open on), then heartbeat the CURRENT target — which the server thread
    // swaps to the chosen url+profile (action "open") on submit. A "pick"
    // heartbeat keeps the picker alive; an "open" one the page.
    {
        let t = target.lock().unwrap();
        emit_web_surface_osc(t.action, session, &t.url, &t.title, &t.profile);
        eprintln!("ychrome: profile picker open — {picker_url}  (Ctrl+C to close)");
    }
    let mut ticks: u32 = 0;
    let mut last_tick = std::time::Instant::now();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        // Suspend/resume gap (Ctrl+Z / yggterm Zzz / machine sleep): the GUI
        // may have closed the surface, and heartbeats can't re-create one —
        // re-announce the current target ("pick" re-announces itself; "open"
        // is liveness-idempotent when nothing changed).
        if last_tick.elapsed() > Duration::from_secs(3) {
            let t = target.lock().unwrap();
            emit_web_surface_osc(t.action, session, &t.url, &t.title, &t.profile);
        }
        last_tick = std::time::Instant::now();
        ticks += 1;
        if ticks.is_multiple_of(20) {
            let t = target.lock().unwrap();
            if t.action == "pick" {
                emit_web_surface_osc("pick", session, &t.url, &t.title, &t.profile);
            } else {
                emit_web_surface_osc("heartbeat", session, &t.url, &t.title, &t.profile);
            }
        }
    }
    let t = target.lock().unwrap();
    emit_web_surface_osc("close", session, &t.url, &t.title, &t.profile);
    eprintln!("ychrome: web surface closed");
    Ok(())
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

fn main() -> Result<()> {
    let args = Args::parse();

    // Declare ourselves to this host's yggterm launcher registry, on EVERY run:
    // that is what repairs the recorded binary path after an upgrade moves it.
    // Never fatal — a browser must not refuse to start over a menu entry.
    if let Err(error) = manifest::write() {
        eprintln!("ychrome: could not register launcher manifest ({error})");
    }

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
        eprintln!("ychrome: tunnel up — egress on {via}'s network (socks5://127.0.0.1:{local_port})");
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

    #[cfg(not(target_os = "linux"))]
    let _webview = builder.build(&window).context("creating webview")?;
    #[cfg(target_os = "linux")]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().context("no gtk vbox")?;
        builder.build_gtk(vbox).context("creating webview")?
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
