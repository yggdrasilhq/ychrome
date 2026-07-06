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

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
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
    let base = dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("web-profiles")
        .join(profile);
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

/// The libyggterm web-surface control sequence (OSC 7717). Consumed by the
/// yggterm GUI's terminal parser; invisible junk-free in plain terminals
/// (unknown OSCs are ignored) — the degradation story is the channel itself.
fn emit_web_surface_osc(action: &str, session: &str, url: &str, title: &str, profile: &str) {
    use base64::Engine as _;
    use std::io::Write as _;
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
    emit_web_surface_osc("open", session, url, title, profile);
    eprintln!("ychrome: web surface open — {url} [{profile}]  (Ctrl+C to close)");
    let mut ticks: u32 = 0;
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        ticks += 1;
        // Heartbeat every ~4s (20 × 200ms) — the GUI's liveness truth.
        if ticks.is_multiple_of(20) {
            emit_web_surface_osc("heartbeat", session, url, title, profile);
        }
    }
    emit_web_surface_osc("close", session, url, title, profile);
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
            *control_flow = ControlFlow::Exit;
        }
    });
}
