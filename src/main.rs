//! ychrome — a minimal profile-aware web viewport.
//!
//! v0 scope (standalone):
//!   - open a URL in a real WebKit webview window
//!   - `--profile <name>` gives each profile its own persistent storage
//!     (cookies, localStorage), so two accounts on the same site coexist
//!   - `--via <ssh-host>` opens a localhost URL that lives on a remote
//!     machine by spawning an ssh -L tunnel for the window's lifetime
//!
//! The libyggterm integration (viewport takeover inside yggterm) comes
//! later; see docs/architecture.md.

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

fn main() -> Result<()> {
    let args = Args::parse();

    let raw_url = args.url.clone().unwrap_or_else(|| "about:blank".into());
    let raw_url = if raw_url.contains("://") || raw_url == "about:blank" {
        raw_url
    } else {
        format!("http://{raw_url}")
    };

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
