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
use wry::{WebContext, WebViewBuilder};

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

/// Spawn `ssh -N -L <local>:<host>:<port> <via>` and wait until the local
/// side accepts connections.
fn open_tunnel(via: &str, remote_host: &str, remote_port: u16) -> Result<Tunnel> {
    let local_port = free_local_port()?;
    let forward = format!("{local_port}:{remote_host}:{remote_port}");
    let child = Command::new("ssh")
        .args([
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ConnectTimeout=10",
            "-L",
            &forward,
            via,
        ])
        .stdin(Stdio::null())
        .spawn()
        .context("spawning ssh for the tunnel")?;
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
            bail!("ssh tunnel to {via} exited early ({status}) — check `ssh {via}` works and the remote port {remote_port} is listening");
        }
        if Instant::now() > deadline {
            bail!("ssh tunnel to {via} did not come up within 12s");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn profile_dir(profile: &str) -> Result<PathBuf> {
    if profile.contains('/') || profile.contains("..") {
        bail!("profile name must be a plain name, not a path: {profile}");
    }
    let base = dirs::data_dir()
        .context("no XDG data dir")?
        .join("ychrome")
        .join("profiles")
        .join(profile);
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

/// The libyggterm web-surface control sequence (OSC 7717). Consumed by the
/// yggterm GUI's terminal parser; invisible junk-free in plain terminals
/// (unknown OSCs are ignored) — the degradation story is the channel itself.
fn emit_web_surface_osc(action: &str, session: &str, url: &str, title: &str) {
    use base64::Engine as _;
    use std::io::Write as _;
    let payload = format!(
        "{{\"session\":{},\"url\":{},\"title\":{}}}",
        serde_json_string(session),
        serde_json_string(url),
        serde_json_string(title),
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
fn run_thin_client(session: &str, url: &str, title: &str) -> Result<()> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || {
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .context("installing Ctrl+C handler")?;
    }
    emit_web_surface_osc("open", session, url, title);
    eprintln!("ychrome: web surface open — {url}  (Ctrl+C to close)");
    let mut ticks: u32 = 0;
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        ticks += 1;
        // Heartbeat every ~4s (20 × 200ms) — the GUI's liveness truth.
        if ticks.is_multiple_of(20) {
            emit_web_surface_osc("heartbeat", session, url, title);
        }
    }
    emit_web_surface_osc("close", session, url, title);
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
        return run_thin_client(&session, &raw_url, &title);
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

    // Resolve --via: rewrite the URL to a local tunnel endpoint. The
    // tunnel handle must outlive the event loop, so it is held below.
    let mut tunnel: Option<Tunnel> = None;
    let final_url = if let Some(via) = &args.via {
        let parsed = Url::parse(&raw_url).context("parsing URL for --via")?;
        if parsed.scheme() == "https" {
            eprintln!(
                "ychrome: warning: --via rewrites the host to 127.0.0.1; \
                 https certificates will not match. Use plain http dev servers."
            );
        }
        let remote_host = parsed.host_str().unwrap_or("127.0.0.1").to_string();
        let remote_port = parsed
            .port_or_known_default()
            .context("URL has no port and no default")?;
        eprintln!("ychrome: opening ssh tunnel via {via} → {remote_host}:{remote_port} …");
        let t = open_tunnel(via, &remote_host, remote_port)?;
        let mut rewritten = parsed.clone();
        rewritten.set_host(Some("127.0.0.1"))?;
        rewritten.set_port(Some(t.local_port)).ok();
        let s = rewritten.to_string();
        eprintln!("ychrome: tunnel up on {s}");
        tunnel = Some(t);
        s
    } else {
        raw_url
    };

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

    let builder = WebViewBuilder::new_with_web_context(&mut web_context).with_url(&final_url);

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
