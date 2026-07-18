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

## Sidebar contribution (`sidebar` verb) — SHIPPED

ychrome contributes its vault and settings panes rather than yggterm hardcoding
them. `RightPanelMode::Vault` and `::AppSidebar` are both deleted from yggterm.

```
ESC ] 7717 ; sidebar ; declare ; <base64 {"session","control","app_name","panes":[{id,icon,title}],"policy_version","zoom_version"}> BEL
ESC ] 7717 ; sidebar ; close   ; <base64 {"session"}> BEL
```

`declare` is idempotent and re-emitted on the same ~4s heartbeat cadence as the
web surface — that IS the contribution's liveness signal, and the GUI expires a
contribution whose declares stop, so a SIGKILLed ychrome leaves no phantom
buttons. It carries **no schema and no secret**: only a loopback control
endpoint, the pane buttons, and a stamp over this host's web-content policy.

The GUI then talks to the control endpoint itself, over a plain socket (through
an `ssh -L` forward when ychrome runs remotely):

```
GET  <control>/pane/vault?host=<page host>            -> the schema
GET  <control>/pane/settings?host=<host>&zoom=<pct>   -> the schema
GET  <control>/policy                                 -> {adblock_rules, userscripts}
GET  <control>/zoom                                   -> {sites:{host:percent}}
POST <control>/action  {pane, action, values}         -> {schema?, toast?, eval?, reload_surface?, refetch_zoom?}
```

An action is routed by the `pane` it came from, not by its name: the two panes
return different schemas. The GUI injects the active surface's page host as
`values.host` and its live effective zoom as `values.zoom` on every action, so a
pane control can act relative to what is on screen.

### `app_name`

The display name yggterm shows on the main zoom control ("Ychrome Global Zoom").
The app names itself; yggterm hardcodes nothing. Absent ⇒ the generic "Web View
Zoom".

### `policy_version` and `/policy`

Ad blocking and userscripts are ychrome's config, on ychrome's host — but they
act on the GUI's webview, so the GUI must apply them. ychrome serves the
*effective* policy (every enable/disable decision already made) and yggterm
persists nothing but a compiled-filter cache WebKit demands.

`policy_version` is a **stat-only** stamp: paths, lengths, mtimes, plus the
adblock decision. The GUI refetches `/policy` only when it moves, so a 10 KB
`rules.json` never rides the ~4s heartbeat.

**`declare` is emitted BEFORE `web-surface;open`** — in `run_thin_client` and in
the post-suspend re-emit. Userscripts inject at document-start, so the GUI holds
the surface's creation until the policy lands. Open first and the surface is
created unblocked: no userscripts, no adblock, silently, for its whole life.

### `zoom_version` and `/zoom` — per-site zoom

yggterm has one global "Ychrome Global Zoom" (`web_surface_zoom_percent`). A
daily browser needs a per-site number too: some sites read better at 130%, some
at 80%, and it must persist per site. That is browsing config, so it lives on
ychrome's host in `~/.yggterm/web-zoom.json` (`{ "sites": { host: percent } }`,
host-global across profiles — zoom is readability, not identity).

`zoom_version` is a **stat-only-style** stamp over that map, the same trick as
`policy_version`: the GUI refetches `/zoom` only when it moves, so an unchanged
map never rides the ~4s heartbeat. `/zoom` returns `{ "sites": { host: percent } }`.

yggterm does the matching on the live page (longest-suffix: an entry for
`youtube.com` covers `music.youtube.com` too — the sub-domain reach Chrome lacks;
a bare TLD is never consulted) and applies the override on navigation, falling
back to the global when a site has none. Unlike `/policy`, `/zoom` is
**non-gating**: a slow fetch just leaves a surface on the global until the map
lands. The old map stays applied while a new one is in flight, so there is no
flicker to global and back.

The settings pane's "This site" row steps the current host's override with `−`/
`+` (from the live `values.zoom` the GUI injects) and clears it with `Reset`. The
action reply sets `refetch_zoom: true` so the GUI re-reads `/zoom` and applies
the change to the live page at once, without waiting for the next declare.

`eval` is a script the GUI runs in the surface — the only way a host-resident
credential reaches a client-rendered page. ychrome computes it; the GUI injects
it. The vault never crosses the OSC, and a schema never carries a secret.

**The app owns every field's value.** A schema declares what each field holds;
the GUI's copy is only the user's edits since that schema arrived, and applying a
schema replaces it. So a schema must **echo the draft back** or the fields blank
— ychrome keeps the Add-tab draft in its own `PaneState`. Conversely, a value the
schema stops declaring is dropped by the GUI, which is what stops a typed password
riding along on the next unrelated action.

**Secrets are one-way.** A `secret` text-input carries what the user typed UP to
ychrome on an action; ychrome declares it back empty. A generated password is
never echoed down: an empty password field means `ychrome-vault add --generate`,
so the password is rolled on this host and stored encrypted without ever entering
yggterm.

An agent can open a contributed pane without clicking:
`yggterm server app right-panel pane:vault`.

Implementation: `src/sidebar.rs`. Widget vocabulary and the GUI side:
`yggterm/.agents/skills/libyggterm-surfaces/SKILL.md`.

## Command envelope on ping replies — SPEC agreed 2026-07-18, NOT BUILT

The app→GUI ingress primitive (design: `docs/host-daemon.md`; normative
platform copy: yggterm's `libyggterm-surfaces` SKILL). Wire shape from the
app's side:

- The GUI pings `GET <control>/ping?session=<env_id>&ack=<batch_id>`. The
  `session` param names which session the ping is about (the env id our
  declare carried); its presence marks a routing-capable GUI. `ack` names
  the last command batch the GUI executed.
- Our /ping reply MAY carry
  `commands: {batch_id, entries:[{id, kind, session, ...args}]}` with v1
  kinds `open_tab {session, url, raise}` and `toast {title, body, tone}`.
- At-least-once delivery: we retain a batch until acked; the GUI dedups by
  entry id. We expire undeliverable entries after 60s with a journal line.
- Commands enter the queue ONLY from an explicit CLI verb (`ychrome <url>`
  routing). Heartbeat/ping logic never synthesizes one; a ping only ever
  refreshes.

## Open questions (for the next libyggterm apps)

1. Per-surface SOCKS egress (full network-identity borrowing) and verifying
   WebKit sends hostnames to the proxy (socks5h) so remote DNS resolves
   session-side.
2. Where the ALT+/KeyTips command registry hooks in, so app surfaces are
   keyboard- and agent-drivable like native ones.
3. Live pane updates (an ytop-class app pushing a new schema without the user
   acting). Today a schema changes only on open or in an action's reply.
