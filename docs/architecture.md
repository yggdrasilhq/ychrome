# Architecture

## Two modes, one binary

```
ychrome <url>
   │
   ├── yggterm session detected (YGGTERM_SESSION_ID + daemon socket env)
   │      → thin-client mode: ask the host daemon for a web surface,
   │        block until it closes. Rendering happens in the yggterm GUI.
   │
   └── no yggterm
          → standalone mode: open an own tao/wry (WebKitGTK) window.
```

## Thin-client mode (the yggterm path)

Participants:

- **ychrome (CLI)** — runs on whatever machine the user's terminal session is
  on. Resolves the session identity from environment variables exported by the
  yggterm daemon into every PTY it owns (the `TMUX`-variable pattern). Sends
  `web-surface open` over the daemon's local socket; stays in the foreground;
  Ctrl+C or daemon notification of surface close terminates it.
- **yggterm host daemon** — relays the request to attached GUI clients,
  tagged with the session id. Owns lifecycle truth: if the CLI dies, the
  surface closes; if the surface is closed in the GUI, the CLI is told.
- **yggterm GUI** — swaps that session's viewport from the terminal to a
  webview surface. The right sidebar shows ychrome's panels while the surface
  is foreground.

## The egress rule

**A surface's network egress is the invoking host's network — for all URLs,
always.** The connection to the target service is made *on the session's
machine, by the session's side* (CLI or daemon); the GUI only renders.

Mechanism: a per-surface SOCKS proxy relayed over the existing substrate. The
session-side component is the proxy egress; the GUI configures the surface's
web context (each surface has its own context already, for profile isolation)
to route all requests — including DNS resolution — through it. Loopback
services, internal DNS names, docker networks, VPN-only routes, and
source-IP-checked services all behave exactly as they would in a browser
running on that machine.

A GUI-side `ssh -L` port forward was considered and rejected: it originates
the connection on the GUI host and only special-cases loopback, silently
breaking every other only-reachable-from-there case.

Key property: **the network plumbing is yggterm's job, not the user's.** The
user never names a machine; where they typed the command is where the URL
resolves.

## Standalone mode

`src/main.rs` today: tao event loop + wry WebViewBuilder on a WebContext whose
data directory is `~/.local/share/ychrome/profiles/<profile>`. `--via <host>`
is the same egress rule with plain ssh as the carrier: borrow that host's
entire network identity for the window via a dynamic SOCKS forward
(`ssh -N -D`), with the web context's proxy pointed at the local end. (The
current implementation still uses a single-port `-L` rewrite; migrating it to
`-D` + per-context proxy is the open v0 task, tracked below.) The forward dies
with the window.

Standalone mode is also the degradation story required of every libyggterm
app: in a bare xterm the command still does something sensible.

## What lands where (extraction discipline)

The daemon RPC, the GUI surface swap, and the automatic forward live in
yggterm first. Only once a second app (Paper or Cellulose embedding) needs the
same machinery does the shared part get extracted into libyggterm. ychrome
must not grow a private protocol that the extraction then has to undo — the
protocol doc is written against yggterm's daemon from day one.
