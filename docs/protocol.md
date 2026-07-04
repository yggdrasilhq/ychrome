# Surface Protocol (draft)

Draft of the daemon-relay contract ychrome pilots for libyggterm. Everything
here is subject to what the yggterm integration actually teaches; this file is
the current best guess, not a spec.

## Session identity handoff

The yggterm host daemon exports into every PTY it owns:

```
YGGTERM_SESSION_ID=<session uuid>
YGGTERM_DAEMON_SOCK=<path to the host daemon control socket>
```

A libyggterm app treats the presence of both as "I am inside yggterm."
Absence of either → standalone/degraded mode. (Same detection contract tmux
established with `$TMUX`; survives ssh because the *remote* daemon is the one
that owns the PTY and sets the vars.)

## Requests (CLI → daemon)

```
web-surface open   { session_id, url, title?, profile? }   → { surface_id }
web-surface close  { surface_id }
web-surface status { surface_id }                          → { state }
```

The CLI holds the socket open for the surface lifetime; socket drop = implicit
close (crash-safe: a killed CLI never leaks a surface).

## Relay (daemon → GUI)

The daemon forwards surface events to attached GUI clients tagged with the
session id, exactly like session output. The GUI that has the session focused
(or any GUI showing it) performs the viewport swap. Multiple attached GUIs see
the same surface state — consistent with the ssh-as-keycard collaboration
model.

## GUI responsibilities

- Swap the session viewport terminal → webview; restore on close.
- Configure the surface's web context to route all traffic (including DNS)
  through the surface's session-side SOCKS egress (see the egress rule in
  architecture.md) — never originate target connections from the GUI host.
- Expose the app's sidebar panels while the surface is foreground.
- Report close back through the daemon so the CLI unblocks.

## Session-side responsibilities (daemon or CLI)

- Act as the SOCKS egress for the surface: resolve names and open target
  connections on the session's machine, relaying streams over the substrate.
- Tear the egress down with the surface.

## Open questions (to be answered by the pilot)

1. Does the surface follow the *session* (survives GUI restart, like the
   terminal does) or the *CLI process*? Leaning: the CLI process — a web
   surface is a foreground program, not a session.
2. Sidebar panel schema: static manifest at open vs live updates over the
   socket. ytop-class apps need live updates; v0 ychrome does not.
3. Where the ALT+/KeyTips command registry hooks in, so app surfaces are
   keyboard- and agent-drivable like native ones.
4. SOCKS relay transport: multiplex over the daemon's existing channel vs a
   dedicated `ssh -D` alongside it. Also verify WebKitGTK/libsoup passes
   hostnames to the SOCKS proxy (socks5h behavior) so remote DNS actually
   resolves session-side.
