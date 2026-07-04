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
- Loopback URL on a remote session ⇒ establish a port forward to the
  session's host over the existing ssh machinery, rewrite the URL to the
  local end, tear the forward down with the surface.
- Expose the app's sidebar panels while the surface is foreground.
- Report close back through the daemon so the CLI unblocks.

## Open questions (to be answered by the pilot)

1. Does the surface follow the *session* (survives GUI restart, like the
   terminal does) or the *CLI process*? Leaning: the CLI process — a web
   surface is a foreground program, not a session.
2. Sidebar panel schema: static manifest at open vs live updates over the
   socket. ytop-class apps need live updates; v0 ychrome does not.
3. Where the ALT+/KeyTips command registry hooks in, so app surfaces are
   keyboard- and agent-drivable like native ones.
