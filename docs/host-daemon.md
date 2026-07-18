# The ychrome host daemon and the routing verb

Status: **BUILT 2026-07-18** (the daemon, the routing verb, the `/ping` command
envelope, and `ychrome status` ship in `src/daemon.rs`; the GUI half shipped in
yggterm a7534bca). Design agreed 2026-07-18 (four open calls settled with the
owner: route-on-profile-match, vault agent stays a peer process, the command
envelope is a generic libyggterm primitive, the agent engine mounts inside this
daemon). Two things are NOT built and stay spec-only, marked below: the vault
agent PEER hop (`ychrome-vault-proto` — the pane still shells out to the CLI,
which is already a peer, so nothing regressed) and the agent engine (§7).

**One implementation call the spec did not foresee (built as such):** the daemon
serves ONE control listener PER registered session (a plain
`http://127.0.0.1:<port>`), not a single host-wide port demuxed by session. A
single port would need the declared control url to carry a per-session
discriminator (a path prefix), and the GUI's vendored `yggterm-appctl://` proxy
(`forward_to_control`) parses a control base as `http://host:port` with no path —
a prefix breaks the passkey signer and would force a coordinated GUI change. Per-
session listeners keep the contribution protocol and the appctl bridge byte-for-
byte unchanged (zero GUI change, passkeys untouched) at the cost of the "one
ssh -L per host" reduction, which is deferred to when the appctl proxy learns to
preserve a base path. State, lifecycle, registry, queue and routing are still
fully consolidated in one daemon process — the routing mechanism the spec is
about.

## 1. Why these are one design, not two

The campaign carried two remaining items: "daemon consolidation" and "the
routing verb `ychrome [--profile P] <url>`". They are the same item, because
of a transport fact:

A remote app host has exactly two channels to the GUI. The PTY byte stream
(OSC 7717) is identity-bound to the emitting session, so an OSC from a
routing invocation would open the tab in the wrong session. The control
endpoint is fetched BY the GUI (ssh -L, GUI-initiated); the app can never
call the GUI. App-control ingress (`yggterm server app ...`) reaches the
daemon of the host it runs on, never the GUI host, so it cannot route from
an ssh session at all.

The only fleet-correct transport is therefore: the routed open sits in a
host-resident queue, and the GUI's existing endpoint ping (Phase 2 liveness)
carries it back on its reply. A queue needs something durable on the app
host to hold it. That thing is the daemon. Consolidation is not a
prerequisite of routing; it is the routing mechanism.

## 2. Architecture

```
ychrome <url> ──────────┐ (thin view client: anchors the session, blocks,
ychrome --profile P url │  emits OSC open/heartbeat/declare, registers)
ychrome status          │
     │ unix socket      ▼
~/.yggterm/ychrome/daemon.sock          (0600, host-local API)
     │
ychrome daemon        (one per host per user; auto-spawned; supervised
     │                 by its clients, yedit pattern)
     ├── control endpoint (TCP loopback) ← GUI GETs schemas/policy/zoom/
     │                                     appearance/ping over ssh -L
     ├── session registry {env_id, profile, pid}
     ├── pending command queue (open_tab, ...)
     ├── pane state (vault/settings drafts)
     ├── journal  ~/.yggterm/ychrome/journal.jsonl
     ├── (future) /engine/*  ← the agent engine mounts HERE, not on a
     │                          second socket (agent-engine.md §3 amendment)
     └── vault client (ychrome-vault-proto) ──► vault agent socket
                                                (separate process, holds keys)
```

- **One daemon per host per user.** Auto-spawned by the first ychrome
  invocation (setsid, cwd=home). `~/.yggterm/ychrome/control-url` records
  the TCP loopback endpoint and is trusted only when it answers /ping (the
  yedit lesson). Singleton via lockfile.
- **Two listeners, one server, one state**: TCP loopback for the GUI
  (unchanged contribution protocol; the declare simply carries the daemon's
  URL now), unix socket for host-local clients (the CLI, agents, later the
  engine ctl). A remote host needs ONE ssh -L forward per host instead of
  one per invocation.
- **Clients supervise the daemon.** Each view client probes the daemon on
  its heartbeat cadence and respawns it if dead; the registry is soft state
  and rebuilds from the clients' 30s re-register heartbeats. Daemon death
  is self-healing, not an incident.
- **The view client stays a blocking foreground anchor.** Zzz/Ctrl+Z/fg,
  close-sends-Ctrl+C, the picker, standalone-window mode: all unchanged.
  Surface open/close still ride the PTY OSC (session identity binding and
  the degradation story); contribution liveness rides daemon pings.

### What moves into the daemon (today it is per-invocation)

| Concern | Today | After |
|---|---|---|
| Control endpoint (schemas, actions, /policy /zoom /appearance /ping) | one server per `ychrome` process | the daemon, one per host |
| Pane drafts (Add tab, search) | die with the invocation | survive client exit, die with daemon (journaled) |
| Vault access from the pane | shell out to `ychrome-vault` CLI → agent socket | daemon speaks the agent socket directly via `ychrome-vault-proto` |
| Routing ingress | none (the campaign's open problem) | registry + queue + ping-reply envelope |

## 3. The vault agent stays a peer process (settled)

The daemon is a **client** of the vault agent's unix socket through a new
crate `ychrome-vault-proto`: op enums and the socket client only, no
crypto, no API dependencies. Consequences:

- The lean-build decision is not reopened; the browser still links no
  crypto (campaign blocker a resolved).
- Keys never live in the web-facing daemon. **Browser deploys never
  re-lock the vault.** `stop-agent` is needed only when the vault binary
  itself changes (campaign blocker b shrinks from "every consolidation
  deploy" to "rare, schedule with the user").
- The process boundary keeps bulk secret reads journal-visible at the
  agent socket, and Debian's yama ptrace default keeps the daemon from
  reading the agent's memory even same-uid.
- The CLI shell-out chain (pane → CLI → agent) collapses to one hop.

## 4. The routing verb

```
ychrome [--profile P] [--here] <url>
```

- **Route on profile match** (settled): a live registered session with the
  REQUESTED profile (the default profile when no flag) exists on this host
  → POST /route on the daemon socket → enqueue `open_tab {session, url,
  raise:true}` → print "opened in <session>" → exit 0.
- **No match → anchor here**, exactly today's behavior (thin client in a
  yggterm terminal, standalone window outside one).
- `--here` forces anchoring even when a match exists (the "I want a second
  surface in THIS terminal" case).
- **Deterministic pick** when several sessions share the profile: most
  recently registered wins; `--session <env_id>` disambiguates explicitly;
  the choice is journaled.
- **Raise semantics**: the GUI opens the tab in that session's surface AND
  activates the session, Chrome-like. Multiple queued opens apply in
  order; the last is foreground.
- **Honesty under version skew**: the daemon marks a GUI as
  routing-capable when it has seen a `?session=` ping recently. If none,
  /route refuses and the CLI warns and anchors instead of printing a
  success it cannot deliver. (One GUI exists; deploying it first makes
  this transient.)
- **The fleet router is ssh** (settled non-feature): `ssh dev ychrome
  <url>` routes on dev with dev's identity and egress. Documented recipe,
  zero code.

Latency: ≤2.5s when any ychrome session is foreground, ≤10s otherwise (see
§5). A lost queue on daemon crash is a retyped command, journaled; the
queue is in-memory by design.

## 5. The command envelope (generic libyggterm primitive, settled)

The GUI-ingress mechanism is part of the platform contract, not a ychrome
hack. Normative copy lives in yggterm's `libyggterm-surfaces` SKILL; the
wire shape:

- The GUI's contribution ping becomes
  `GET <control>/ping?session=<env_id>&ack=<batch_id>`. The `session` is
  the env id the declare carried (the GUI stores it on the contribution);
  its presence is also the routing-capability marker.
- A ping reply MAY carry
  `commands: {batch_id, entries:[{id, kind, session, args...}]}`.
  v1 kinds: `open_tab {session, url, raise}` and
  `toast {title, body, tone}`.
- **At-least-once with idempotent ids**: the daemon retains a batch until
  a ping acks it; the GUI dedups by entry id and executes commands only
  for sessions whose contribution it holds (env id → session path via the
  stored declare id). Unknown targets are dropped and journaled; the
  daemon expires undeliverable entries after 60s.
- **Commands are explicit user-initiated operations queued by a CLI verb,
  never synthesized by heartbeat logic.** The "heartbeats must not
  navigate" lesson stands: a heartbeat/ping can only ever REFRESH; a
  command enters the queue solely from an explicit act.
- yggterm-side cadence change: the active-visible contribution is pinged
  every WorkingFlags tick (2.5s, unchanged); every OTHER live contribution
  every 4th tick (~10s). A background ping refreshes liveness identically
  (bonus: background stamp propagation, which today waits for
  foregrounding).
- Second consumer, already real: `yedit file.md` routes an open to its
  daemon today but nothing can focus the GUI session it landed in;
  `open_tab`'s sibling `focus_session` is yedit's when it wants it.

## 6. `ychrome status` (host-side truth for agents)

```
ychrome status [--json]
```

Sessions + profiles + queue depth + vault-agent reachability + policy/zoom
stamp versions + daemon version + a **self-staleness stamp** (running exe
vs on-disk binary, the vault agent's `exe_stamp` precedent) so the
stale-daemon class ("2.10.3 running for 19h while the fix sat on disk")
cannot silently recur in ychrome. The same staleness rides the /ping reply
so the GUI settings pane can show "daemon outdated, restart".

## 7. The agent engine mounts here (settled)

`docs/agent-engine.md` §3 is amended: no separate `engine.sock`/token/
lifecycle. When that campaign runs (its Phase A gate is unchanged), the
engine is a subsystem of THIS daemon: `/engine/*` on the daemon socket,
one journal shared with routing and governor actions, and the session
registry doubles as Phase F's promote-to-visible target list. The engine
campaign inherits a home instead of building one.

## 8. Failure modes

- **Daemon dies**: GUI pings fail → contributions expire on the Phase 0
  zombie pipeline (rail torn down); the SURFACE lives on, carried by the
  client's PTY heartbeats. The client's supervision respawns the daemon;
  its next re-declare re-earns the contribution. Self-healing, no zombie.
- **Vault agent dies/locked**: pane renders its locked state exactly as
  today; routing and policy serving are unaffected (peer isolation).
- **Mixed fleet mid-deploy**: old per-invocation ychrome processes keep
  declaring their own URLs; the GUI follows whatever a declare carries.
  No flag day.

## 9. Security

- daemon.sock 0600, same-user trust shape as the vault bridge; the TCP
  loopback control endpoint is unchanged from today's exposure.
- Every routed open, command batch, delivery, drop, and staleness event is
  a journal line: attribution over prevention, per the engine spec's
  audit stance.
- No secret ever enters the queue, a schema, or a ping reply (standing
  contract).

## 10. Non-goals (what this spec deliberately does not cover)

- Profile-jar sync across machines (campaign 3.0.0 slice; jars stay
  GUI-host-resident for remote sessions, wrinkle unchanged).
- Any engine implementation (own campaign, own Phase-A gate).
- Phase F compositing.
- Picker changes; bare no-arg `ychrome` keeps the picker exactly as is.
- Tab ownership: tabs, history, jars stay GUI-side, always. The daemon
  holds an OPEN REQUEST queue, never tab state.
- Detached view clients (`--detach`): the anchor model stays foreground;
  revisit only if real use demands it.

## 11. Deploy plan (fleet rule honored)

1. yggterm GUI-only deploy on jojo (ping widening + `?session=`/ack +
   command drain + tests). No version bump, no daemon touch.
2. ychrome fleet deploy (dev + jojo + oc, `./scripts/deploy-fleet.sh`,
   same artifact everywhere). First invocation per host spawns the daemon.
3. Vault agent untouched: **no re-unlock anywhere**.
4. Live proofs (acceptance):
   - jojo-local: `ychrome <url>` in a plain terminal routes into a running
     default-profile surface, session raised, tab foreground, journal line
     on both ends.
   - Cross-host: `ychrome --profile work <url>` in a dev ssh session
     routes into the dev-anchored work surface rendered in jojo's GUI.
   - Skew honesty: with the old GUI (pre-deploy), /route refuses and the
     CLI anchors with a warning.
   - `ychrome status --json` from an agent shows the registry and a clean
     staleness stamp; after replacing the binary on disk it shows stale.

## 12. Later (named so they don't sneak in)

- Watchtower surfacing: weak/reused counts as a vault-pane badge via a
  daemon chore + `toast`; breach checks (HIBP k-anonymity) strictly
  opt-in and journaled. Not v1.
- `focus_session` command kind for yedit routing.
- BiDi/CDP shims, accessibility trees: engine campaign's non-goals list
  governs.
