# The YChrome Agent Engine — agent-first headless browsing at fleet scale

Status: **SPEC — approved direction 2026-07-13** (discussion: yggterm session
13b4cdb5). **Phase A is BUILT and PASSING** (`ychrome engine gate`, proven on
dev 2026-07-31 — see §9.1); Phases B-E are not. Nothing else below is built
except where marked "exists today".

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
     │  headless display             (substrate seam — §9.1. INTENDED:
     │                                 WPEDisplayHeadless. ACTUAL today:
     │                                 WebKitGTK on an engine-owned Xvfb,
     │                                 because Debian's WPE has no WPEPlatform)
     ├── page pool ────────────────── logical pages (100s)
     │     ├── live views (N≈12) ──── one web view → WebKitWebProcess each
     │     └── parked pages ───────── serialized state, no engine resources
     ├── governor ─────────────────── PSS/CPU probes, budgets, LRU park/kill
     └── journal ──────────────────── ~/.yggterm/ychrome/journal.jsonl (shared)
```

- **Engine host process**: the engine subsystem of `ychrome daemon` —
  long-running, one per host per user, started lazily on the first
  `/engine/*` call. It owns a headless **display** and any number of web views
  on it, through the substrate seam in `src/engine/substrate.rs`.

  ⚠ **The intended substrate does not exist on our hosts.** This section used
  to assert "WPEDisplayHeadless, available in wpewebkit-2.0 ≥ 2.44; Debian
  ships 2.52.x". Debian's wpe-webkit-2.0 2.52.5 is built WITHOUT WPEPlatform:
  no `wpe-platform-*.pc`, no header declaring `wpe_display_headless_new`, and
  zero `wpe_display_*` symbols in `libWPEWebKit-2.0.so.1`. The version number
  was never the deciding fact. §9's sanctioned fallback is what runs today —
  WebKitGTK views on an engine-owned headless X display — behind the same
  verbs. `ychrome engine probe` reports both substrates live, so the day a
  WPEPlatform build lands, the swap is a change to `Engine::start` and nothing
  above it. Full evidence and the reasoning: §9.1.
- **Engine ≠ surface**: the engine never emits OSC 7717 and is not tied to a
  terminal session. It is a peer of the surface path, sharing the identity
  modules underneath.
- **CLI**: `ychrome ctl <verb> [args] [--json]` — a thin HTTP client over the
  socket. Every verb is also directly curl-able (agents may skip the CLI).
- **Rust bindings**: **SETTLED in Phase A — the gir-generated crates, and no
  shim.** Not the `wpe-webkit`/`wpe-platform` crates this line originally
  guessed at (they bind an API this host does not have), but the
  `webkit2gtk` / `gtk` / `gdk` / `glib` / `cairo-rs` / `javascriptcore-rs`
  crates that `wry` already links, at the versions already in `Cargo.lock`.
  Reasoning and the falsified alternatives: §9.1.

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

Transport: HTTP/1.1 over the ychrome daemon's unix socket
`~/.yggterm/ychrome/daemon.sock`, with every route below mounted under
`/engine/*` (so `/open` reads as `POST /engine/open`). All responses JSON. All
verbs idempotent where meaningful.

**There is no engine token, and there must not be one.** This section used to
require `Authorization: Bearer <token>` minted into
`~/.yggterm/web-engine/token`; the §3 AMENDMENT of 2026-07-18 settled with the
owner that the engine has no socket, token or lifecycle of its own, and the
amendment wins. It also agrees with AGENTS.md, which is explicit: "The agent's
authority is the unix socket (dir `0700`, socket `0600`). Adding a token buys
nothing against a same-uid attacker; do not add one." A token here would have
been a second encoding of an authority the socket permissions already carry —
exactly the divergence the single-source-of-truth rule forbids.

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

- **Journal**: `~/.yggterm/ychrome/journal.jsonl` — the DAEMON's journal, shared
  per the §3 amendment, not a second file. Every verb with
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

- Socket 0600, and nothing else: same-user-only, no network exposure ever. No
  bearer token (see §4).
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
Prove on the GUI host: WPEDisplayHeadless + one WPEWebView; load example.com; PNG
readback matches (pixel-check the "Example Domain" text); `/eval` returns
`document.title`; a dispatched trusted click on a test page mutates DOM state
that an untrusted synthetic click provably does not (isTrusted differential).
Settles the bindings question (gir crates vs bindgen shim).
*AC: committed spike binary + journal lines proving all four, plus a written
bindings decision in this doc's §9.*

✅ **DONE 2026-07-31, all five proofs PASS** — `ychrome engine gate`
(`src/engine/gate.rs`), re-runnable, journaling `engine.gate.proof` lines to
the daemon journal and PNGs to `~/.yggterm/ychrome/engine-gate/`. Proven on
**dev**, a headless LXC with no display server and no GPU, NOT on the GUI host as this
line asks: headless is headless, and dev is the harsher host. **Re-run it on
the GUI host to close the letter of the AC.** The gate ran on the FALLBACK substrate
because the intended one is absent (§9.1); the numbers, the two bugs it
surfaced, and the bindings decision are all in §9.1.

**Phase B — engine daemon + core API.** `ychrome engine serve` + socket +
token; verbs: open/close/pages/goto/nav/wait/shot/eval/input; 10 concurrent
live pages; journal.
*AC: a yggui-style script opens 10 pages, waits, screenshots all 10, clicks
through one flow (e.g. DuckDuckGo search → result), all headless over ssh.*

🟡 **STARTED 2026-07-31 — core API built, AC partly met.** `src/engine/api.rs`
is the one router; `ychrome engine bench` drives it on real `/engine/*`
requests from N threads, which is exactly what the socket handler does with one
thread per connection, so what is proven live IS the shipping router and not a
test-only path.

Measured on dev: **10 pages opened concurrently in 1.3-2.2 s wall** (p50 within
1 ms of the wall time, i.e. genuinely overlapped, not serialised), **10 listed
live** via `/engine/pages`, **10 PNG readbacks** verified by magic bytes at
~47 ms each. The click half of the AC is covered by Phase A's proof 5 rather
than by a DuckDuckGo flow.

What is **built but NOT live-proven**: the socket plumbing itself. The routes
are wired into `handle_unix_conn` and unit-tested over a `UnixStream` pair, but
they have not run against a deployed daemon — this branch does not deploy, and
dev has live ychrome daemons serving real browsing.

✅ **COMPLETE 2026-07-31.** `/nav`, `/wait` (all four `until` forms), `/dom`
(html/text/snapshot) and the full `/input` set landed, proven by
`ychrome engine flow` — 11 steps, all through `dispatch`, all PASS. Every input
event is a real `GdkEvent`; not one is a `dispatchEvent`, so the Phase A
property holds across the whole verb surface (the fixture logs `isTrusted` for
every event kind and the flow asserts none is `false`).

`/dom snapshot` carries a contract rather than a hope: **every selector it
emits is verified in the page to resolve to exactly the element it describes**,
and one that does not comes back `null` and counted. Mutation-tested — an
ambiguous selector turns the step red. Password inputs are redacted
(`value: null, redacted: true`); mutation-tested too, the un-redacted build
leaks the literal value and the step goes red.

**Phase C — identity parity.** Profiles/jars, SOCKS egress, adblock filters,
userscripts, UA, zoom; vault /fill. The engine and the visible surface must be
the SAME browser to a website.
*AC: a page logged-in under profile X in the visible surface is logged-in in
the engine with zero re-auth; the cdn.taboola.com adblock differential passes
headless; SponsorBlock userscript state visible via /eval on a YouTube page.*

**Phase D — fleet governance.** Pool, park/resume, budgets, governor,
/metrics, /batch, bench.
*AC: 300 logical pages opened via /batch on the GUI host with max_live=12 and
max_rss_mb=4096; run completes; journal shows parking honoring LRU and budget;
peak engine PSS within budget +10%; bench numbers recorded.*

✅ **DONE 2026-07-31 (on dev, not the GUI host — same caveat as Phase A).**
`ychrome engine govern 300`, six checks, all PASS:

| Check | Measured |
|---|---|
| run completed | 300/300 opened, 133 s |
| live never exceeded `max_live` | **12** seen, budget 12 |
| LRU parking happened | **289** parks, 288 finally parked |
| peak PSS within budget +10% | **438 MB** of 4505 MB allowed |
| every page still accounted for | 300 logical pages |
| resume restores the PLACE, not the URL | read back `["RESTORE-MARKER", 1234]` |

The last row is the one worth reading twice. The fixture's own markup says
`state-N` and its inline script scrolls somewhere else on load, so a `resume`
that merely re-fetched the URL would come back with `state-N` — and does,
exactly, when the restore path is mutated. Only a real place-restore returns
the marker and the scroll offset.

**`per_page_rss_mb` is NOT implemented**, and the reason is a measurement gap
rather than a schedule one: webkit2gtk 2.0.2 exposes no web-process identifier,
so there is no honest way to attribute a WebKitWebProcess to a view. The
aggregate budget (`max_rss_mb`) is what the governor enforces, which is what
this AC measures. `page.rss_mb` is `null` rather than `0` — a zero would read
as a measurement.

**`/batch` returns a JSON array, not the NDJSON stream §4 describes.** Chunked
responses need a streaming responder the control endpoint does not have; Phase
E owns it. A JSON array is honest, a half-stream would not be.

**Phase E — agent ergonomics.** `ychrome ctl` polish, SKILL.md section with
recipes (crawl-and-extract, form-fill, watch-page-until), /dom snapshot
extractor hardening, NDJSON streaming.
*AC: the three recipe scripts run green on the GUI host/dev/oc; skill documented.*

**Phase F — promote-to-visible (SEPARATE CAMPAIGN, not overnight work).**
Composite an engine view's texture into a yggterm session viewport; unify the
input seat; retire the native-child overlay for surfaces. Requires DMABUF
export + GL compositing in the shell and full input forwarding (IME, cursor,
momentum). Deliberately out of scope here; the engine's existence de-risks it.

## 9. Risk register

| Risk | Signal | Mitigation / fallback |
|---|---|---|
| ~~Rust bindings for wpe-webkit-2.0 missing/stale~~ **RESOLVED, differently than expected** | Phase A | Moot: there is no WPEPlatform to bind. We use the gir crates wry already links. No shim, no C helper, no build.rs — §9.1 |
| ~~WPEPlatform headless API gaps at 2.52~~ **FIRED — the whole API is absent, not just gaps** | Phase A | Fallback substrate taken. Xvfb, not `cage`/`weston`: same class of thing, already installed, and GTK3 is X11-native so it needs no new package — §9.1 |
| Debian packages absent on a fleet host | `apt list` on dev/oc before Phase B | `sudo apt install libwpewebkit-2.0-1 libwpe-1.0-1` is a documented one-time prereq (oc precedent: libwebkit2gtk) |
| GPU-less hosts (oc) render slowly | bench in Phase D | swrast is fine for agent work; record numbers, don't guess |
| Form-state park/restore lossy | Phase D | documented best-effort; tags let scripts re-derive; never claim more than captured |
| Shared jar: engine + visible surface open same profile concurrently | Phase C | WebKit handles multi-process jar access via the network process per session; verify with a live differential, journal a warning if two writers detected |
| Anti-bot flags headless views | Phase C | we present the SAME UA/identity as the visible browser and real input events; do not add evasion beyond that — honesty rule |

## 9.1 Decision: substrate and bindings (Phase A, settled 2026-07-31)

Phase A's acceptance criterion asked for "a written bindings decision in this
doc's §9". §9 is a risk-register table with no room for one, and its first row
recorded only a fallback, so this section exists to hold the decision itself.
Two questions were genuinely open. Both are now closed by measurement on dev.

### The finding that reframed both questions

**Debian's WPE WebKit carries no WPEPlatform.** `libwpewebkit-2.0-dev`
2.52.5-1 and `libwpe-1.0-dev` 1.16.3-1+b1 were installed fresh for this work,
and:

| Check | Result |
|---|---|
| `pkg-config --modversion wpe-webkit-2.0` | `2.52.5` |
| `pkg-config --modversion wpe-platform-1.0` / `-2.0` | **not found** (only `wpe-webkit-2.0`, `wpe-1.0`, `wpe-web-process-extension-2.0` exist) |
| `grep -rl 'WPEDisplayHeadless\|wpe_display_headless' /usr/include/` | **no hits** |
| `nm -D libWPEWebKit-2.0.so.1 \| grep -c wpe_display` | **0** |
| any `libWPEPlatform*.so` | **absent** |
| compile `wpe_display_headless_new()` against `wpe-webkit-2.0` | `error: unknown type name 'WPEDisplay'` |
| the view constructor actually shipped | `webkit_web_view_new(WebKitWebViewBackend*)` — the pre-WPEPlatform libwpe API |
| GIR/typelib for WPE (`/usr/share/gir-1.0`, `girepository-1.0`) | **none** |

The version number in §3 was doing all the persuading and none of the
deciding. WPEPlatform is an upstream build option; Debian's 2.52.5 is built
without it. `ychrome engine probe` performs the first four of these checks at
runtime so this stays a live fact rather than a dated paragraph.

**Consequence for the substrate.** The shipped API needs a `libwpe` *backend
implementation* to produce a `wpe_view_backend`, and Debian packages exactly
one (`libwpebackend-fdo`, not installed here) which wants a Wayland display and
EGL. There is no displayless path through the WPE packages on this host at all.
So §9's fallback row fires: **WebKitGTK behind the same verbs.**

**Chosen headless host: Xvfb, not `cage`/`weston --backend=headless`.** The
risk row named the Wayland compositors. Xvfb is the same class of thing (a
display server nobody can see), it was already installed, `webkit2gtk-4.1` is
GTK3 and therefore X11-native so it needs no extra package, and the engine
starts and kills its own instance so nothing leaks into the user's session.
`cage`/`weston` remain available if a Wayland-only behaviour ever turns out to
matter; the seam is what makes that a local change.

### Decision 1 — substrate: WebKitGTK on an engine-owned headless display

Selected by live probe, not configuration. `engine::substrate` is the single
owner; `engine::host` speaks page verbs and never names a substrate. When a
WPEPlatform build appears, `probe_wpe` starts returning available and the work
is a new arm in `Engine::start`.

**What this costs us, stated honestly**, because the fallback is not free:

- a display-server process per engine host (~4 MB, one `Xvfb`), where
  WPEPlatform would have needed none;
- software rasterisation (`libEGL warning: DRI3 error` on every run) — fine
  for agent work, and the numbers say so: full gate in **1.8-2.2 s**, snapshot
  in **118 ms** at 1024x768;
- a window that must be *mapped* for WebKit to paint. "Headless" here means the
  display has no viewer, not that the view is unmapped. A snapshot of an
  unmapped view is blank, which is precisely the instrument-lie this engine
  exists to end.

What it does **not** cost: trusted input works (proof 5), snapshots are
faithful (proof 3), and both were the specific things §9 worried the fallback
might not deliver.

### Decision 2 — bindings: the gir-generated crates, no shim, no build.rs

The open question was "gir crates vs a `bindgen` shim over the C API". The
WPEPlatform finding collapses one side of it and the answer is neither of the
two originally imagined:

- **gir crates for WPE** — impossible AND pointless. There is no `.gir` file
  installed to generate from, and the API they would bind does not exist here.
- **a `bindgen` shim over `libwpewebkit-2.0`** — buildable, useless. It would
  bind `webkit_web_view_new(WebKitWebViewBackend*)`, which cannot produce a
  headless view without a libwpe backend nobody packages. The shim was the
  fallback for *stale* bindings; the problem is a missing *engine feature*, and
  no amount of binding work reaches it.
- **the gir crates for the substrate we actually run** — `webkit2gtk 2.0.2`,
  `gtk 0.18`, `gdk 0.18`, `glib 0.18`, `cairo-rs 0.18`, `javascriptcore-rs
  1.1`. **Chosen.** They were already in `Cargo.lock` as `wry 0.55`'s
  transitive dependencies, so the engine promoted them to direct dependencies
  at the versions already resolved. Zero new crates were downloaded, zero
  version churn, no `build.rs`, no `-sys` crate of our own, no bindgen.

Feature deltas, the whole of them: `webkit2gtk/v2_40` (for
`webkit_web_view_evaluate_javascript`; wry already enables `v2_38`, so this is
one additive step on a shared dependency) and `cairo-rs/png`. Plus `libc`, for
`_exit` alone — see below.

**Raw FFI is still needed in exactly one place**, and it is worth naming so
nobody re-litigates it: `gdk-rs` 0.18 exposes *getters* for `GdkEventButton`
but no setters, so synthesising a pointer event fills
`gdk::ffi::GdkEventButton` through a raw pointer (`engine::host::dispatch_click`).
That is the same shape as the vendored-wry adblock FFI the original §3 cited as
precedent — about fifteen lines, not a binding layer.

### Decision 3 — the event loop

The brief asked for this explicitly: WPE (and WebKitGTK) need a running
`GMainContext` on their own thread, and the only loop in this repo today is
`tao`'s, which is GTK-bound and owns the browser process's main thread.

**They are not shared, and they never meet.** The engine lives in the
**daemon** process, which has no `tao` loop and no windows. There it takes one
dedicated thread that calls `gtk::init()` then `gtk::main()`, acquiring the
global default `GMainContext`. Page objects live in a `thread_local` on that
thread and are never sent anywhere. Callers on any other thread post a closure
with `glib::idle_add_once` (which is thread-safe and targets exactly that
context) and block on an `mpsc` reply.

The closure receives a `Responder` it may fire immediately *or* move into
WebKit's own callback and fire later. That is what turns a callback API into
verbs a control-plane handler can call synchronously: `goto` returning means
the load **finished**, not that a request was posted.

One consequence to carry into Phase B: the browser process must never host the
engine, because `gtk::init` and `tao` would fight over the same process-global
context — and `DISPLAY` is process-global too, which the engine sets for its
private display.

### Two bugs Phase A surfaced, both found by measuring rather than reading

1. **Exit 134 with every proof passing.** The gate printed five PASSes and
   then `SIGABRT`. gdb put it at `exit -> __run_exit_handlers ->
   g_object_unref -> WebKit -> abort` on the main thread: WebKit registers an
   atexit handler that unrefs its process-global context, and that unref needs
   a run loop which is gone once `main` has returned. The engine now ends its
   own process (`engine::exit_now`, `_exit`) after flushing; every durable
   artifact is a closed file well before that point. Left alone, a passing gate
   and a crashing one would both have exited 134 — the exit code would have
   been worthless as a signal.
2. **A leaked display number per run.** `Child::kill` is `SIGKILL`, so `Xvfb`
   never removed `/tmp/.X<n>-lock`, and `start` treats a surviving lock as
   "taken". Seven runs, seven orphaned locks, the display climbing `:90` ->
   `:96`. Now `SIGTERM` first with a 2 s grace, then unlink. Three consecutive
   runs reuse `:91`.

Neither was visible in code review. Both were visible the moment the thing ran.

### The gate's own numbers (dev, 2026-07-31)

| Proof | Evidence |
|---|---|
| 1. display + view | substrate `webkitgtk-headless`, display `:90`, 227 ms |
| 2. example.com | committed `https://example.com/`, `readyState: complete`, 856 ms |
| 3. PNG pixels | `<h1>` rect `(205,115) 614x33` read from the DOM; **1338** dark pixels inside it, **0** in a blank control region; 1024x768, 16968-byte PNG, 118 ms |
| 4. eval | `document.title` -> `"Example Domain"` |
| 5. isTrusted | synthetic `dispatchEvent`: guard `untouched`, seen `[false]`. Trusted `GdkEvent`: guard `mutated-by-trusted-input`, seen `[false, true]`. 2 events dispatched, 2 concurrent live pages |

Proof 3 is a differential, not a file-size check: a blank canvas scores 0 ink
in the heading rect, and a uniformly dark one scores ink in the control region.
Proof 5 is a differential too — the fixture mutates its guarded state **only**
when `event.isTrusted`, so a substrate with no trusted input fails it instead
of quietly recording `false` twice. A unit test locks that property of the
fixture.

**Not yet proven on the GUI host.** The AC names the GUI host as the gate host. Headless is
headless and dev is the harsher machine (no display server at all), but the
letter of the AC wants `ychrome engine gate` re-run there.


## 10. Estimate

Assuming one strong agent per overnight run, live verification between runs:

- Phase A: ✅ **done** (the fallback substrate WAS needed and cost far less
  than the +1 night budgeted, because the bindings came for free).
- Phase B: **1 night** (mechanical once A settles bindings; the control-server
  pattern already exists in sidebar.rs).
- Phase C: **1 night** (mostly extraction/reuse; the WPE settings/content
  filter API differs enough from webkit2gtk to cost real time).
- Phase D: **1 night** (governor + batch + bench).
- Phase E: **0.5 night** + docs.

**Total: 4–5 overnight runs to a fleet-scale, identity-true agent browser.**
Phase F is a separate multi-week campaign and must not be started as a side
effect of this one.
