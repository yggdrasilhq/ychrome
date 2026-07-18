# The YChrome Agent Engine — agent-first headless browsing at fleet scale

Status: **SPEC — approved direction 2026-07-13** (discussion: yggterm session
13b4cdb5). Nothing below is built yet except where marked "exists today".

## 1. Vision

Today an agent drives the web through ychrome's *surfaces*: native child
webviews composited over the yggterm viewport, reachable only when their
session is foreground, screenshot-able only by stealing window focus, and
clickable only through untrusted synthetic DOM events that WebKit half-honors.
Those constraints exist because the surface substrate was built human-first and
agents were bolted on.

The Agent Engine inverts that. It is a host-resident browser engine host with
**no window at all**: pages render offscreen, pixels come back as GPU/PNG
readbacks, input goes in as engine-level trusted events, and hundreds of
logical pages can be open at once under an explicit RAM/CPU budget. Agents are
the first-class user; the human viewport is a later consumer of the same
substrate (Phase F).

What makes this different from Playwright/Puppeteer (and why it is worth
building at all): the engine runs **inside the user's real browsing identity**.
Profiles, cookie jars, SOCKS egress, content blockers, userscripts, UA,
vault autofill and passkey ceremony are the SAME machinery ychrome's visible
surfaces use — one owner per concept, per AGENTS.md. An agent researching,
filling, buying, posting, or triaging does it as the user, on the user's
hosts, with the user's network egress. A lab browser cannot do that.

## 2. Non-goals (v1)

- **No compositing into the yggterm viewport.** Promote-to-visible (render the
  engine's texture inside a session viewport, unify the input seat) is Phase F,
  a separate campaign. The agent slice must not wait for it.
- **No CDP/WebDriver compatibility layer.** The control plane is ychrome's own
  JSON API. (A BiDi shim can be a later adapter if an external tool needs it.)
- **No accessibility-tree extraction.** DOM snapshots via injected JS are the
  v1 "structured read"; ATK plumbing is future work.
- **No new identity machinery.** Profiles/jars/egress/vault are reused, never
  reimplemented. If the engine needs something the surface path has, extract
  it into a shared module; do not fork it.

## 3. Architecture

> **AMENDMENT 2026-07-18 (settled with the owner, see `docs/host-daemon.md`):
> the engine has no socket, token, or lifecycle of its own.** It mounts as a
> subsystem of the per-host **ychrome daemon** under `/engine/*` on
> `~/.yggterm/ychrome/daemon.sock`, shares that daemon's journal (governor
> actions and routing verbs interleave in reading order), and the daemon's
> session registry doubles as Phase F's promote-to-visible target list.
> Every `engine.sock` / `web-engine/` path below reads as the daemon socket
> and `~/.yggterm/ychrome/`; `ychrome engine serve` reads as the engine
> subsystem starting inside `ychrome daemon` on first `/engine/*` call.

```
yggui script / agent
     │  ychrome ctl <verb> …          (CLI = thin client)
     ▼
~/.yggterm/ychrome/daemon.sock        (unix socket, 0600 — the HOST DAEMON's
     │                                 socket; engine API mounts at /engine/*)
ychrome daemon ─ engine subsystem     (one daemon per host per user)
     │  WPEPlatform headless display  (libwpewebkit-2.0, WPEDisplayHeadless)
     ├── page pool ────────────────── logical pages (100s)
     │     ├── live views (N≈12) ──── WPEWebView → WebKitWebProcess each
     │     └── parked pages ───────── serialized state, no engine resources
     ├── governor ─────────────────── PSS/CPU probes, budgets, LRU park/kill
     └── journal ──────────────────── ~/.yggterm/ychrome/journal.jsonl (shared)
```

- **Engine host process**: the engine subsystem of `ychrome daemon` —
  long-running, one per host per user, started lazily on the first
  `/engine/*` call. It owns a WPEPlatform **headless
  display** (`WPEDisplayHeadless`, available in wpewebkit-2.0 ≥ 2.44; Debian
  ships 2.52.x) and any number of `WPEWebView`s on it. No GTK, no Wayland, no
  X11, no window manager — runs identically over ssh, in cron, on oc.
- **Engine ≠ surface**: the engine never emits OSC 7717 and is not tied to a
  terminal session. It is a peer of the surface path, sharing the identity
  modules underneath.
- **CLI**: `ychrome ctl <verb> [args] [--json]` — a thin HTTP client over the
  socket. Every verb is also directly curl-able (agents may skip the CLI).
- **Rust bindings**: the gir-generated `wpe-webkit`/`wpe-platform` crates if
  usable at our WebKit version; otherwise a small `engine-sys` bindgen shim
  over `libwpewebkit-2.0` + `libwpe-1.0`. **Phase A settles this — see the
  risk register.** The vendored-wry adblock FFI precedent (webkit2gtk::ffi in
  `web_surface.rs`) shows the shim route is workable.

### 3.1 What exists today (reuse, do not rebuild)

| Concept | Owner today | Engine reuse |
|---|---|---|
| Profiles + cookie jars | `~/.yggterm/web-profiles/<p>/` | same dirs, same jar files |
| Egress (SOCKS per profile) | surface/webcontext plumbing | same tunnel reuse rule (never churn a login loop) |
| Adblock (WebKit content filters) | `webpolicy.rs` + `~/.yggterm/web-adblock/` | same compiled-filter cache |
| Userscripts | `webpolicy.rs`, document-start | same, per profile |
| UA / browser identity | `useragent.rs` | same |
| Zoom / appearance | `webzoom.rs` / `webappearance.rs` | zoom applies; appearance is chrome-paint, N/A headless |
| Vault autofill / TOTP / passkeys | `vault.rs`, `passkey.rs` | same bridge; passkey user-presence invariant UNCHANGED |
| Control-endpoint pattern | `sidebar.rs` loopback server | same shape for engine.sock |

## 4. Control API

Transport: HTTP/1.1 over the unix socket. Every request carries
`Authorization: Bearer <token>` (token minted at engine start into
`~/.yggterm/web-engine/token`, 0600 — same trust shape as the vault bridge).
All responses JSON. All verbs idempotent where meaningful.

### Page lifecycle

```
POST /open      {url?, profile, tags?, viewport?: {w,h,scale}}      → {page}
POST /close     {page_id}                                           → {closed}
GET  /pages     [?tag=…&profile=…&state=live|parked]                → {pages:[…]}
POST /goto      {page_id, url}                                      → {page}
POST /nav       {page_id, action: back|forward|reload|stop}         → {page}
```

`page` (the one status shape, everywhere):

```json
{
  "page_id": "pg_01hxyz…",
  "profile": "research",
  "url": "https://…", "title": "…",
  "state": "live" | "parked" | "crashed",
  "loading": false,
  "viewport": {"w": 1280, "h": 900, "scale": 1.0},
  "rss_mb": 187.4, "cpu_pct_1m": 2.1,
  "opened_at_ms": 0, "last_used_ms": 0,
  "tags": ["crawl-batch-3"]
}
```

### Waiting (the primitive that makes scripts honest)

```
POST /wait {page_id, until, timeout_ms=15000}
  until: {"load": "committed"|"finished"}
       | {"idle_ms": 500}                       # network+layout quiet
       | {"selector": "css", "state": "attached"|"visible"}
       | {"js": "expr"}                          # truthy poll, 100ms cadence
→ {met: true, elapsed_ms} | {met: false, reason}
```

### Reading

```
POST /shot {page_id, mode: "viewport"|"full", format: "png", scale?}
  → PNG bytes (Content-Type: image/png)         # engine snapshot, ALWAYS faithful
POST /dom  {page_id, mode: "html"|"text"|"snapshot"}
  → snapshot = the structured interactable tree: [{role,text,selector,rect,value?}…]
    built by an injected extractor script (v1: buttons, links, inputs, selects,
    textareas, [role], [contenteditable]) — the agent's "what can I act on"
POST /eval {page_id, js, await_promise: bool, timeout_ms}
  → {value} | {error}
    await_promise=true wraps in the callback shim (store to a token global,
    poll) — the engine does the polling so scripts never hand-roll it again
```

### Acting (trusted input — the whole point)

```
POST /input {page_id, events: [
  {"type":"click",  "selector":"css"}            # engine resolves center, scrolls into view
| {"type":"click",  "x":…, "y":…, "button":"left"|"right"|"middle", "count":1|2}
| {"type":"move",   "x":…, "y":…}                # real hover — menus, tooltips work
| {"type":"type",   "text":"…"}                  # keyevents to the focused element
| {"type":"key",    "key":"Enter", "mods":["ctrl"]}
| {"type":"scroll", "dx":0, "dy":…, "x"?, "y"?}
]} → {dispatched: n}
POST /fill  {page_id, selector?, entry?}          # vault autofill, reuses /fill machinery
```

Input dispatch goes through the WPE view backend's event API
(`wpe_view_…_dispatch_…` pointer/keyboard/axis events), so WebKit treats it
exactly like seat input: focus moves, `:hover` applies, default actions fire,
`isTrusted` is true. This retires the entire "synthetic clicks over-report,
Enter under-delivers" instrument-lying class documented in the picker
investigation.

Selector-addressed clicks are sugar: the engine evals
`getBoundingClientRect` on the selector, scrolls it into view, then
dispatches real coordinates. One resolver, shared by `/input` and `/dom`.

### Fleet + governance

```
GET  /pool                     → {live, parked, budgets, pressure}
POST /park   {page_id}         → {page}        # capture state, drop the view
POST /resume {page_id}         → {page}        # recreate view, restore place
POST /budget {max_live?, max_rss_mb?, per_page_rss_mb?}
GET  /metrics                  → per-page + aggregate probe dump (JSON)
```

### Batch (the 100s-of-pages verb)

```
POST /batch {open: [{url, profile, tags}…], concurrency?: 8}
  → streams NDJSON page results as each reaches load-finished
```

Batch is a convenience loop over /open + /wait with the governor in charge;
it must not bypass budgets.

## 5. Resource governance — how "hundreds of pages" actually works

The RAM truth: a real page's WebKitWebProcess costs **80–300 MB PSS**. A
hundred *live* engine views would be 10–30 GB. Nobody gets that. So:

- **Logical page ≠ live view.** A logical page is an entry in the pool with
  identity (profile, url, tags, scroll, history index, form-state snapshot).
  A live view is engine resources. The pool holds hundreds of logical pages;
  only the working set is live.
- **Working set**: `max_live` (default 12) views. LRU beyond that is
  **parked**: the engine extracts `{url, scroll, history_index, form_state}`
  (form state via the injected extractor, best-effort), destroys the view, and
  keeps the logical page. Cookies/localStorage already live in the profile's
  jar on disk — parking loses nothing durable. `resume` recreates the view and
  restores the place (same restore-is-a-PLACE rule the tab store learned).
- **Budgets, enforced not advisory**: a governor tick (2s cadence) reads
  `/proc/<webproc>/smaps_rollup` PSS per live view and process CPU deltas.
  Over `max_rss_mb` (default 4096) → park LRU until under. A single page over
  `per_page_rss_mb` (default 1500) → `webkit_web_view_terminate_web_process`,
  state `crashed`, journaled — a leaky page may not sink the fleet.
- **WebKit's own knobs**: memory-pressure settings tuned conservative;
  process-per-view is the default (isolation), with a documented
  `views_per_process` dial if measurement ever justifies sharing.
- **Backpressure**: `/open` and `/batch` return `429 pool_saturated` with the
  current pressure numbers rather than silently queueing forever. Scripts see
  the constraint; the governor never lies.

## 6. Probes and profiling — designed in, not bolted on

- **Journal**: `~/.yggterm/web-engine/journal.jsonl` — every verb with
  latency, every governor action (park/resume/kill with the numbers that
  triggered it), every page state transition, every input batch. Same
  event-trace discipline as yggterm; the telemetry campaign can mine it.
- **Per-page probes** (in `page` and `/metrics`): PSS, CPU%, nav timing
  (injected `performance` read at load-finished: ttfb, dcl, load), shot
  latency, eval latency p50/p95.
- **Aggregate**: pool occupancy, park rate, kill count, budget headroom,
  engine-host RSS itself.
- **`ychrome ctl bench`**: standardized run (open N reference pages, wait
  idle, shot, close; report p50/p95 latency + peak PSS) — the regression
  gate. Run it in CI-ish fashion after every engine change; numbers go in the
  journal so drift is visible across versions.

## 7. Security

- Socket 0600 + bearer token: same-user-only, no network exposure ever.
- **Audit is the journal**: every action an agent takes through the engine is
  attributable and replayable in reading order. No silent driving.
- **Per-profile agent policy**: `web-profiles/<p>/profile.json` gains
  `"agent_drive": "allow" | "deny"` (default **allow** — agents are
  first-class here by the owner's explicit decision; deny exists so a future
  sensitive profile can opt out). Enforced at /open.
- **Passkey ceremonies keep the user-presence invariant unchanged**: the
  engine routes `navigator.credentials` through the same shim + presence
  dialog; an agent can never self-approve a passkey. Password/TOTP autofill
  follows existing vault rules (origin-exact, per-fill journal line).
- Engine pages are real authenticated browsing. The mitigations are identity
  (unix perms), attribution (journal), and revocability (`ychrome ctl engine
  stop` kills everything) — not capability crippling.

## 8. Phases with acceptance criteria

**Phase A — the spike (GATE, do first, throw away nothing else if it fails).**
Prove on jojo: WPEDisplayHeadless + one WPEWebView; load example.com; PNG
readback matches (pixel-check the "Example Domain" text); `/eval` returns
`document.title`; a dispatched trusted click on a test page mutates DOM state
that an untrusted synthetic click provably does not (isTrusted differential).
Settles the bindings question (gir crates vs bindgen shim).
*AC: committed spike binary + journal lines proving all four, plus a written
bindings decision in this doc's §9.*

**Phase B — engine daemon + core API.** `ychrome engine serve` + socket +
token; verbs: open/close/pages/goto/nav/wait/shot/eval/input; 10 concurrent
live pages; journal.
*AC: a yggui-style script opens 10 pages, waits, screenshots all 10, clicks
through one flow (e.g. DuckDuckGo search → result), all headless over ssh.*

**Phase C — identity parity.** Profiles/jars, SOCKS egress, adblock filters,
userscripts, UA, zoom; vault /fill. The engine and the visible surface must be
the SAME browser to a website.
*AC: a page logged-in under profile X in the visible surface is logged-in in
the engine with zero re-auth; the cdn.taboola.com adblock differential passes
headless; SponsorBlock userscript state visible via /eval on a YouTube page.*

**Phase D — fleet governance.** Pool, park/resume, budgets, governor,
/metrics, /batch, bench.
*AC: 300 logical pages opened via /batch on jojo with max_live=12 and
max_rss_mb=4096; run completes; journal shows parking honoring LRU and budget;
peak engine PSS within budget +10%; bench numbers recorded.*

**Phase E — agent ergonomics.** `ychrome ctl` polish, SKILL.md section with
recipes (crawl-and-extract, form-fill, watch-page-until), /dom snapshot
extractor hardening, NDJSON streaming.
*AC: the three recipe scripts run green on jojo/dev/oc; skill documented.*

**Phase F — promote-to-visible (SEPARATE CAMPAIGN, not overnight work).**
Composite an engine view's texture into a yggterm session viewport; unify the
input seat; retire the native-child overlay for surfaces. Requires DMABUF
export + GL compositing in the shell and full input forwarding (IME, cursor,
momentum). Deliberately out of scope here; the engine's existence de-risks it.

## 9. Risk register

| Risk | Signal | Mitigation / fallback |
|---|---|---|
| Rust bindings for wpe-webkit-2.0 missing/stale | Phase A | bindgen shim over the C API (precedent: adblock FFI); worst case a tiny C helper lib |
| WPEPlatform headless API gaps at 2.52 (input dispatch, snapshot) | Phase A | fallback substrate: webkit2gtk views inside a headless wayland compositor (`weston --backend=headless` or `cage`) — same control API, uglier host; keep API identical so the substrate can swap |
| Debian packages absent on a fleet host | `apt list` on dev/oc before Phase B | `sudo apt install libwpewebkit-2.0-1 libwpe-1.0-1` is a documented one-time prereq (oc precedent: libwebkit2gtk) |
| GPU-less hosts (oc) render slowly | bench in Phase D | swrast is fine for agent work; record numbers, don't guess |
| Form-state park/restore lossy | Phase D | documented best-effort; tags let scripts re-derive; never claim more than captured |
| Shared jar: engine + visible surface open same profile concurrently | Phase C | WebKit handles multi-process jar access via the network process per session; verify with a live differential, journal a warning if two writers detected |
| Anti-bot flags headless views | Phase C | we present the SAME UA/identity as the visible browser and real input events; do not add evasion beyond that — honesty rule |

## 10. Estimate

Assuming one strong agent per overnight run, live verification between runs:

- Phase A: **1 night** (this is the gate; if the fallback substrate is needed,
  +1 night).
- Phase B: **1 night** (mechanical once A settles bindings; the control-server
  pattern already exists in sidebar.rs).
- Phase C: **1 night** (mostly extraction/reuse; the WPE settings/content
  filter API differs enough from webkit2gtk to cost real time).
- Phase D: **1 night** (governor + batch + bench).
- Phase E: **0.5 night** + docs.

**Total: 4–5 overnight runs to a fleet-scale, identity-true agent browser.**
Phase F is a separate multi-week campaign and must not be started as a side
effect of this one.
