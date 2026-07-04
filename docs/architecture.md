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
  webview surface. For loopback URLs (`localhost`, `127.0.0.1`, `::1`) on a
  remote session, the GUI establishes a port forward to the session's machine
  over the existing ssh substrate and rewrites the URL to the local end.
  Non-loopback URLs load directly. The right sidebar shows ychrome's panels
  while the surface is foreground.

Key property: **the tunnel is yggterm's job, not the user's.** The user never
names a machine; the session already knows it.

## Standalone mode

`src/main.rs` today: tao event loop + wry WebViewBuilder on a WebContext whose
data directory is `~/.local/share/ychrome/profiles/<profile>`. `--via` spawns
`ssh -N -L` with a free local port and rewrites the URL — the manual escape
hatch for when no yggterm daemon exists on either end. The tunnel dies with
the window.

Standalone mode is also the degradation story required of every libyggterm
app: in a bare xterm the command still does something sensible.

## What lands where (extraction discipline)

The daemon RPC, the GUI surface swap, and the automatic forward live in
yggterm first. Only once a second app (Paper or Cellulose embedding) needs the
same machinery does the shared part get extracted into libyggterm. ychrome
must not grow a private protocol that the extraction then has to undo — the
protocol doc is written against yggterm's daemon from day one.
