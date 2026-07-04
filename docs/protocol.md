# Surface Protocol (v0 — as shipped in yggterm 2.9.53)

This is the contract ychrome pilots for libyggterm, as actually implemented.
The authoritative copy lives in the yggterm repo at `docs/web-surfaces.md`;
this file summarizes the app-side view. An earlier draft proposed a daemon
socket RPC lane — that was replaced: the terminal byte stream itself is the
transport.

## Session identity handshake

The yggterm host daemon exports into every PTY it owns:

```
YGGTERM_SESSION_ID=<the daemon's session key>
YGGTERM_BIN=<path to the yggterm binary that owns the PTY>
```

Presence of `YGGTERM_SESSION_ID` ⇒ thin-client mode. Absence ⇒ standalone
window. (The `$TMUX` detection pattern; survives ssh because the *remote*
daemon owns the PTY.)

## Control channel: OSC 7717 on stdout

```
ESC ] 7717 ; web-surface ; <action> ; <base64 json> BEL
```

- actions: `open`, `heartbeat` (~4s cadence, full payload), `close`
- payload: `{"session": $YGGTERM_SESSION_ID, "url": "...", "title": "..."}`

Why OSC instead of a socket RPC: the PTY relay already reaches the GUI from
every machine (remote daemon → ssh bridge → local daemon → xterm.js), so the
control channel needs no discovery, no version negotiation, and no new
transport — and unknown OSCs are invisible in plain terminals, which is the
degradation story. The GUI keys surface state by the *stream* the OSC arrived
on; the payload `session` field is diagnostic.

## Lifecycle rules the app must follow

- Emit `open` once, then `heartbeat` every ~4s while alive. The GUI expires
  surfaces after 15s without a heartbeat — a SIGKILLed app never leaks an
  overlay, and heartbeats re-heal the surface across GUI-side terminal
  remounts.
- Emit `close` on exit (Ctrl+C). The GUI's overlay ✕ sends a real Ctrl+C to
  the PTY, so handling SIGINT is the whole close protocol.
- Block in the foreground while the surface is open — a web surface is a
  foreground program, not a session.

## Egress (yggterm-side)

Remote session + loopback URL ⇒ the GUI opens `ssh -N -L` to the session's
machine; the remote sshd originates the target connection there (the egress
rule). Non-loopback URLs currently load directly from the GUI host — v0 gap;
the general fix is a per-surface SOCKS egress carried over the substrate,
still open.

## Open questions (for the next libyggterm apps)

1. Sidebar panel contributions (ytop-class apps): schema + live updates —
   probably additional OSC 7717 verbs.
2. Per-surface SOCKS egress (full network-identity borrowing) and verifying
   WebKit sends hostnames to the proxy (socks5h) so remote DNS resolves
   session-side.
3. Where the ALT+/KeyTips command registry hooks in, so app surfaces are
   keyboard- and agent-drivable like native ones.
