# The YChrome Agent Engine — agent-first headless browsing at fleet scale

Status: **BUILT — phases A through E are all implemented and proven**
(approved 2026-07-13, delivered 2026-07-31). Run the proofs yourself:
`ychrome engine gate` (A, green on dev **and** the GUI host), `engine flow` (B),
`engine hit` (B, selector-click hittability), `engine parity` (C),
`engine govern 300` (D), `assets/engine-recipes/run-all.sh`
(E, green on dev/the GUI host/a headless host). **Phase F (promote-to-visible) remains a separate
campaign and is untouched.** Read §10 HONEST GAPS before believing any claim
this document makes about what is covered.

## ⛔ 0. THE SUBSTRATE IS WEBKITGTK, AND THAT IS THE SETTLED ANSWER

**This document is named for WPE, and WPE is not what it runs on.** Read this
before §3 or you will chase an API that does not exist here.

Debian's WPE WebKit 2.52.5 ships with **WPEPlatform compiled out** — an upstream
build flag. Verified independently, twice: no `wpe-platform-1.0`/`-2.0`
pkg-config, **zero** headers declaring `wpe_display_headless_new`, and **zero**
`wpe_display_*` symbols exported by `libWPEWebKit-2.0.so.1`. The version number
in §3 was doing all the persuading and none of the deciding. This is not a
version to wait out; it is a packaging decision.

**Owner's ruling, 2026-07-31: WebKitGTK stays.** The options were put and
settled — *"as long as all agents are happy and yggterm is happy, webkitgtk
stays."* So the engine runs **WebKitGTK behind an engine-owned Xvfb, kept behind
a swappable substrate seam**, and it delivers the two properties §9's risk
register feared the fallback would not: trusted input and faithful snapshots.

**Do NOT re-litigate this by vendoring WebKit.** The blocking objection is not
build cost (real: hours, ~20 GB) — it is that vendoring a browser engine makes
us permanently responsible for shipping its CVE fixes, for a browser holding the
user's real cookie jars, passwords and vault. Distro and Flatpak runtimes carry
that maintenance for us.

⚠ And be clear about what WPE would have bought: **not speed.** The measured
~650 ms is WebKit's own WebProcess startup, identical in both ports. WPE buys
no-X-server and somewhat lower memory — a modest prize.

If dropping the Xvfb dependency ever becomes worth a spike, the cheap route is
**libwpe + `libwpebackend-fdo`** (Debian packages both, and ships `cog`, a
working WPE browser on that path); `webkit_web_view_backend_new` **is** exported
by the installed library. That is the pre-WPEPlatform backend API, and it is a
far smaller change than building WebKit. It is not scheduled.

**Believe `ychrome engine probe`, never this document's prose.**

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
                 ⚠ `url` is read from the LIVE VIEW, not from what a caller
                 last asked for — it was the latter until 2026-08-06, which
                 made the one verb an agent can poll during a navigation
                 unable to observe one.
POST /goto      {page_id, url}                                      → {page}
POST /nav       {page_id, action: back|forward|reload|stop}         → {page}
```

`page` (the one status shape, everywhere).

⚠ **`rss_mb` and `cpu_pct_1m` are always `null`, and that is permanent on this
substrate.** webkit2gtk 2.0.2 exposes no web-process identifier, so there is no
honest way to say which `WebKitWebProcess` belongs to which view. They are
`null` rather than `0` because a zero would read as a measurement. Aggregate
memory IS measured and IS enforced — see `/engine/metrics` and `max_rss_mb` in
§5.

```json
{
  "page_id": "pg_01hxyz…",
  "profile": "research",
  "url": "https://…", "title": "…",
  "state": "live" | "parked" | "crashed",
  "loading": false,
  "viewport": {"w": 1280, "h": 900, "scale": 1.0},
  "rss_mb": null, "cpu_pct_1m": null,
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
POST /shot {page_id,
            region: "viewport" | "full" | "element" | "rect",   # default viewport
            selector: "css", nth?: k, require_unique?: bool, padding?: px,  # element
            rect: {x, y, w, h},                  # rect — DOCUMENT-space CSS px
            prescroll?: bool}                    # walk the page first (lazy loads)
  → PNG bytes (Content-Type: image/png)         # engine snapshot, ALWAYS faithful
  + X-Ychrome-Shot: {region, mode, width, height, scale, document:{…},
                     crop?:{css,device}, selector?:{matches,hittable,nth,…},
                     prescroll?:{steps,capped,…}}
POST /dom  {page_id, mode: "html"|"text"|"snapshot"}
  → snapshot = the structured interactable tree: [{role,text,selector,rect,value?}…]
    built by an injected extractor script (v1: buttons, links, inputs, selects,
    textareas, [role], [contenteditable]) — the agent's "what can I act on"
POST /console {page_id, clear?: bool = true}
  → {installed, entries: [{kind, level, text, at, source?, line?, col?, stack?}…]}
    kind: "console" | "error" | "rejection" | "resource"
POST /eval {page_id, js, await_promise: bool, timeout_ms}
  → {value} | {error}
    await_promise=true wraps in the callback shim (store to a token global,
    poll) — the engine does the polling so scripts never hand-roll it again
```

#### `/shot`'s four regions, and the two traps in full-page capture

**Full-page capture is native, and that settles the design.** WebKitGTK's
`webkit_web_view_snapshot` takes `WEBKIT_SNAPSHOT_REGION_FULL_DOCUMENT`, which
renders the whole laid-out document in ONE call. The obvious fallback —
scroll, capture a viewport, scroll, capture again, stitch — is strictly worse
and is not used: it seams at every step, it repeats every `position: fixed`
header once per tile, and it leaves the page scrolled somewhere the caller did
not put it. Measured on a 1280x2910 fixture: one snapshot, one fixed header at
the top, no seam.

`element` and `rect` are that same full-document snapshot **cropped from the
pixels already in hand**, never a second snapshot. Two snapshots taken a scroll
or an animation frame apart would let "the element" and "the full page it is
part of" show different content.

⚠ **Trap 1 — the crop scale is MEASURED, never assumed.** A caller speaks CSS
pixels; the snapshot is device pixels. The engine divides the snapshot's real
width by the document width the page reported, and crops with that. Nothing
depends on `devicePixelRatio` (it is reported alongside so a disagreement is
visible). Rounding is outward, so a 1 px border is never shaved.

⚠ **Trap 2 — `full` does not load what was never near the viewport.** A
full-document snapshot renders what is LAID OUT. A lazily-loaded page has never
run its `IntersectionObserver` callbacks below the fold, so it captures as a
full-height document of empty boxes — and the snapshot is not lying, the images
really are not there. `prescroll: true` walks the document a viewport at a time
with a 120 ms settle between steps and puts the scroll back. It cannot be one
`eval`: a synchronous scroll loop never yields the event-loop turns the
observers and fetches need, so the loop lives in Rust with the settle between
steps. Proven side by side on a fixture: without it three of five bands stayed
unloaded; with it, all five.

A `rect` is in **DOCUMENT** coordinates, not viewport ones. That is the one
mistake a caller makes here, so a crop that misses the capture is refused by
name and says so rather than answering 200 with a blank PNG.

From the CLI (`--out` catches the bytes and prints the account as one JSON
object):

```sh
ychrome ctl shot page_id=pg_000001                        --out shot.png
ychrome ctl shot page_id=pg_000001 region=full            --out page.png
ychrome ctl shot page_id=pg_000001 region=full prescroll=true --out page.png
ychrome ctl shot page_id=pg_000001 region=element selector='#main' padding=8 --out el.png
ychrome ctl shot page_id=pg_000001 region=rect \
    rect='{"x":0,"y":1100,"w":700,"h":400}'               --out area.png
```

`region=element` resolves its selector through the **same hittable pool**
`/engine/input` clicks through — same filter, same `nth` default, same
`{matches, hittable, hidden, zero_size}` account — so "screenshot the button I
am about to click" cannot pick a different element than the click will.

### Acting (trusted input — the whole point)

```
POST /input {page_id, events: [
  {"type":"click",  "selector":"css", "nth"?: k, "require_unique"?: bool}
| {"type":"click",  "x":…, "y":…, "button":"left"|"right"|"middle", "count":1|2}
| {"type":"move",   "x":…, "y":…}                # real hover — menus, tooltips work
| {"type":"type",   "text":"…"}                  # keyevents to the focused element
| {"type":"key",    "key":"Enter", "mods":["ctrl"]}
| {"type":"scroll", "dx":0, "dy":…, "x"?, "y"?}
]} → 200 {ok, dispatched: n, resolved: [{selector, matches, hittable, hidden,
                                         zero_size, nth, ambiguous, x, y, tag}…]}
  | 400 {ok:false, error}                         # the BATCH is malformed
  | 409 {ok:false, error, dispatched, failed_at, resolved}   # the PAGE refused
POST /fill  {page_id, entry, user?}               # vault autofill — SHIPPED 2026-08-02
  → {ok, entry, filled: "filled"|"user-only"|"no-fields"}
  | 502 {error: "vault: …"}                       # the VAULT refused (locked, no item)
    `user` disambiguates when one item name holds several logins.
    The reply names FIELDS, never a value: the secret comes off the host's
    vault agent, goes straight into the eval script, and is dropped.
    ⚠ This line described a route that did not exist for months, and the
    gap was only found when a run needed it and had to put the payload in a
    0600 file instead. A documented verb with no implementation is worse
    than an undocumented one: it is read as a capability and planned around.
```

Input dispatch goes through the WPE view backend's event API
(`wpe_view_…_dispatch_…` pointer/keyboard/axis events), so WebKit treats it
exactly like seat input: focus moves, `:hover` applies, default actions fire,
`isTrusted` is true. This retires the entire "synthetic clicks over-report,
Enter under-delivers" instrument-lying class documented in the picker
investigation.

### Selector-addressed clicks: hittability, and the semantics decided 2026-07-31

⛔ **This section used to read "the engine evals `getBoundingClientRect` on the
selector, scrolls it into view, then dispatches real coordinates", and that
description was the bug.** `document.querySelector` returns the FIRST match and
real pages carry hidden duplicates — IBKR's login page has six-plus
`button[type=submit]`, five of them dead and the live one third in document
order. The engine resolved the first, dispatched at its centre, and answered
`{"dispatched":3,"ok":true}` for a click that reached nothing. That reported
success cost a reporting agent three wrong conclusions in one session, one of
which blamed the operator's vault for a 2FA rejection that had never been
submitted.

**The rule now: the pool is the HITTABLE matches, in document order, and the
default is the first of them.** Hidden duplicates are noise, not ambiguity — a
page with five dead submit buttons and one live one poses a question with exactly
one answer, and refusing it would be refusing to answer. This is also what the
visible surface plane's matcher already does, and two planes answering the same
question differently is the divergence AGENTS.md forbids.

Both alternatives are reachable **on the request**, before the click rather than
after it: `"nth": k` takes the k-th hittable match, and `"require_unique": true`
refuses `ambiguous_selector` when more than one match is hittable. Every
selector click echoes its `resolved` report, so a caller that took the default
still learns it was one of nine.

Resolution is **two-phase**, the same shape the surface plane arrived at:
classify every match and pin the live pool; pin one candidate and
`scrollIntoView`; **let the scroll settle (120 ms)**; then RE-measure the pinned
node. A rect read in the same tick as the scroll is the pre-scroll rect, which is
how a click lands where an element used to be. Phase B stamps `post_scroll` as a
contract token, and a payload without it is refused rather than trusted.

Hittability is the browser's own answer: `document.elementFromPoint` at the
centre must return the element **or a descendant of it**. ⛔ Not an ancestor — a
click on an ancestor reaches the ancestor, and since `<body>` contains every
element on the page, accepting an ancestor hit accepts every candidate
unconditionally. That single clause is what made the walk a no-op.

Refusals use the surface plane's vocabulary, not a second one: `no element
matches`, `no_hittable_match` (with the `zero_size_element` / `hidden` counts),
`detached_node`, `target_moved`, `handle_lost`, `rect_not_reresolved`, plus
`ambiguous_selector` for the opt-in above.

**A batch resolves each event against the page as it is when that event is
dispatched.** Resolving up front measured event 2 before event 1 had moved
anything, so `[{click "#open"},{click "#item"}]` could not work and a click that
merely MOVED its target landed where the target used to be. Shape-checking is
still done for the whole batch up front, so a malformed batch dispatches nothing;
a mid-batch refusal is a fact about the page and is answered with the count
actually dispatched and the index that stopped it.

Proven by **`ychrome engine hit`** — 15 steps against a fixture carrying a
hidden duplicate ahead of the real control, a covered button, a
`pointer-events:none` button, an unreachable one, real twins, and a node that
detaches itself mid-resolve. Mutation-proven: restoring `hit.contains(el)` turns
the ancestor-hit step red, removing the liveness filter turns two steps red, and
both together (the historical resolver) turn the reported case red with
`dispatched:3` and an unchanged `document.title`.

### `/console` — why a page that "does nothing" is doing something

⛔ **An SDK that throws looks exactly like an SDK that is inert, and until
2026-08-06 the engine could not tell you which.** A payment SDK builds its
`<iframe>` and navigates it a few statements later; if anything in between
raises, the element is in the document, visible, sized, and its `src` is empty
forever. Every DOM probe an agent can run reports "constructed, never
navigated" — a symptom, not a cause. A live investigation into a government
payment gateway stopped at exactly that wall, because the reason was in a
console message nobody could read.

The page instrument is injected at `LoadEvent::Committed` — the earliest this
engine can reach, with the document created and its own scripts not yet run — and
`/engine/console` drains what it saw. Reading CLEARS by default: a buffer nobody
drains fills with the same error a hundred times and buries the next one. Every
read is journaled as `engine.page.console`, so an agent who never asks still
leaves the page's errors in the daemon's record.

**Errors and rejections cost the page nothing.** They are captured with
`addEventListener` on `error` (capturing, so a failed subresource is caught too)
and `unhandledrejection` — additive, so no handler the page owns is replaced and
`window.onerror` is left for the page to set. A rejection reports its MESSAGE
with the stack beside it; WebKit's `Error.stack` is bare frames with no message
line, so preferring the stack reported `@sdk.js:79:32` for a rejection whose
whole value was its sentence.

⚠ **`console` is the one patched thing, and the cost is stated rather than
hidden.** There is no additive listener for it, so the five methods are wrapped;
each calls the original and reports the native source from `toString`, which
defeats the cheap sniff but not a determined fingerprint. It is not put in the
profile's content manager on purpose: that manager is the identity `/policy`
declares, and a debugging aid living there would make the engine a different
browser from the visible surface for every site, forever.

### The mechanisms an embedded SDK uses — `ychrome engine embed`

A separate proof, and it REPORTS rather than gates: which mechanisms a substrate
supports is a measurement, and a run that went red on a mechanism nobody uses
would teach an agent to ignore it.

Thirteen cases, one real ES module defining one real custom element, building a
dynamic iframe against a second origin. **All eleven population mechanisms work
on webkitgtk-headless** — `src` before and after append, `srcdoc`,
`document.write` into the initial `about:blank` child, `contentWindow.location`
assign and replace, a form whose `target` NAMES the frame, a `postMessage`
handshake, an iframe inside a shadow root, and a module that builds at top level
instead of in a ready handler. So "the SDK constructs its iframe and never
navigates it" is **not** a substrate gap, and the remaining two cases show what
it is instead: a bootstrap that builds the frame and then throws, and one whose
async bootstrap rejects. Both reproduce the live symptom exactly — frame
present, `src` empty — and both pass only when `/engine/console` names the
reason.

⚠ One case is an EXPECTED refusal and is the control that says so: a form whose
`target` names no existing frame asks for a new window, and WebKit blocks a
window nobody clicked for. **An `id` is not a `name`**, and an SDK that confuses
them gets silence.

### What the PAGE asks for: new windows and script dialogs

Two things a page does on its own account, neither of them a verb, and both of
which the engine silently dropped until 2026-08-06. Proven by
**`ychrome engine gateway`** — seven steps against two real loopback origins,
mutation-proven (removing the two handlers turns exactly four of them red).

**A new window becomes a PAGE.** `window.open`, a `target="_blank"` link, and a
form whose target is a new window all reach WebKit's `create` signal. With no
handler, `window.open` answers `null` and the navigation is discarded: measured
on a two-origin fixture, **not one byte left the host**, while `/engine/input`
answered `{"dispatched":3,"ok":true}` and the page sat where it was. That is the
shape a bank-payment gateway takes — the merchant's `frmPayment` targets a popup
— so an agent driving a real government payment saw a successful click and a
page that never moved.

The handler mints a real child view (built with `related_view`, which WebKit
requires, and with the profile's own content manager so the popup keeps the
ruleset and userscripts), registers it as a logical page tagged
`opened-by-page`, and journals `engine.window.create`. Collapsing the popup into
its opener instead would be a second lie: the page asked for two documents and
would get one. ⚠ A popup is LISTED at its provisional url the moment the
navigation commits, before the document is parsed — `/engine/wait` is what tells
you it is a document. Its load answers no responder, so
`engine.window.load` / `engine.window.load_failed` in the journal is the only
record of how it went.

⛔ **A popup must not be shown before `create` returns.** Realising the view runs
WebKit's page-proxy setup, and doing that while WebKit is still inside
`createNewPage` loses the navigation it is handing over: the view comes back
listed at the right url with `location.href === "about:blank"` and a load that
never finishes.

**A script dialog is answered, never raised.** `alert` is dismissed, `confirm`
and the beforeunload confirm are accepted, `prompt` answers with the page's own
default text, and each is journaled as `engine.script.dialog` with the answer
given. Unanswered, WebKitGTK's default puts up a modal on a display with no
viewer, the page's script stays parked inside `alert()`, the navigation it was
about to make never happens, and **every later verb on that page times out** —
measured as `engine call did not answer within 30s`, which is character for
character what a live run against a government payment page recorded before the
cause was known. The wedge is page-local; the rest of the pool keeps answering.

There is no operator on this display, so "let the human decide" is not the
alternative to answering — a page that hangs until the daemon dies is. The
journal is what makes the decision attributable.

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

⚠ **`/engine/batch` returns a JSON array today, not an NDJSON stream.** Chunked
responses need a streaming responder the control endpoint does not have; Phase E
owns it. An honest array beats a half-stream.

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
  ⚠ **NOT IMPLEMENTED, and not schedulable**: it needs per-page attribution
  this substrate cannot give (see §4). `max_rss_mb` is enforced on the measured
  aggregate and is what keeps the fleet inside its budget.
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

✅ **DONE 2026-07-31 (dev).** `ychrome engine parity`, five checks, all PASS.
`src/engine/identity.rs` is an ADAPTER that owns nothing: the jar comes from
`crate::profile_dir`, adblock + userscripts + UA from `webpolicy::policy`, zoom
from `webzoom`, the ruleset stamp from `adblock::rules_stamp`. A unit test locks
that, because a forked jar path would still WORK and would silently mean the
engine and the surface are different browsers.

| check | evidence |
|---|---|
| identity comes from its owners | jar/UA/script-count all equal what the owners report |
| **filter compiles once, loads thereafter** | cold store **17,855 ms compile**; second profile **0 ms load**; next process **1 ms load** |
| same jar, zero re-auth | cookie set on page 1 read back on page 2, in `web-profiles/default` — ⛔ **this check was passing on a jar that did not exist**, see below |
| **adblock differential, headless** | `.Adsense` → `display:none` with blocking on, `block` with it off; control class visible on both |
| userscript in the declared world | `window.__ychromeParity.world === "main"` via `/eval` |

**The rule named**: EasyList `##.Adsense`, emitted as `css-display-none` with
`"url-filter": ".*"` and no `if-domain` — one of 971 generic cosmetic rules
covering 36,028 selectors. The differential is deterministic and needs no ad
server: the same markup, the same page, two profiles.

**The filter-cost trap was real and is handled.** `..._save` recompiles
unconditionally (17.9 s measured here, 15.7 s by the adblock lane); the engine
loads first, keyed by `adblock::rules_stamp` over the ruleset's own bytes, and
saves only on a miss. A hand-edited `rules.json` therefore gets a new identifier
and a fresh compile instead of silently serving stale bytecode. **Per profile
per store, not per page and not per launch.**

**A real race, named rather than tuned away.** WebKitGTK queues key events in
the UI process and only sends the next after the previous is acknowledged,
while `evaluate_javascript` is sent at once — so a read issued straight after a
`/engine/input type` can overtake the final keystroke and see `"ada lovelac"`.
It reproduced on roughly two runs in three. `/engine/input` now drains the loop
and flushes before returning, which took it to five in six; the honest fix is
that **a caller must `/engine/wait` on the state it expects**, which is what
`wait` is for. With the wait, eight of eight. The mitigation is documented as a
heuristic in `INPUT_FLUSH` rather than described as a solution.

### ⛔ "Same jar, zero re-auth" was FALSE for three months, and its own test could not see it

Found 2026-07-31. A `WebsiteDataManager` built with base directories still gets
a **memory-only** cookie store; WebKitGTK persists only when someone calls
`set_persistent_storage`. wry does that for the visible surface and the
standalone window (`<profile>/cookies`, Netscape text); `identity.rs` built its
manager by hand and never did. So every engine page started with an empty jar
and discarded its cookies on exit — while the profile directory filled up with
`cache/`, `storage/` and `mediakeys/` and looked perfectly alive.

**The row above passed anyway**, because it set a cookie on one page and read it
back on another *in the same process*, where an in-memory store is shared. A
round trip inside one process was never evidence of a jar.

Measured differential, identical steps under two private `HOME`s:

| | `cookies` file after setting one | survives `daemon restart` |
|---|---|---|
| binary before the fix | **absent** | n/a |
| binary after | present, Netscape text | **yes** |

Consequence, and the reason this sat under a Cloudflare investigation: a
bot-check clearance cookie (`cf_clearance`) could never be kept, so a site that
issues one re-challenges every navigation and the loop never ends.

**Two honest scope limits.** The visible yggterm GUI was NOT driven — the owner
is working at the GUI host — so "zero re-auth" is proven as *the engine reads and writes
the very directory `profile_dir` gives the surface, and a cookie survives across
pages*, not as a GUI login replayed headlessly. And **SponsorBlock was not
exercised**: it is not installed on dev, so there was no state to read. The
userscript plane itself is proven, including world placement, by the probe
script.

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
*AC: the three recipe scripts run green on the GUI host/dev/a headless host; skill documented.*

✅ **DONE 2026-07-31. All three recipes GREEN on dev, the GUI host AND a headless host.**
`assets/engine-recipes/{crawl-and-extract,form-fill,watch-page-until}.sh` plus
`run-all.sh`; the SKILL doc gained an agent-engine section.

- **`/engine/batch` now streams NDJSON**, closing the Phase D placeholder: one
  JSON object per page as it finishes, flushed per line, and a final
  `"summary": true` line so a reader can tell a finished stream from a dropped
  connection. `ychrome ctl batch` prints each line as it arrives.
- **`ychrome ctl <verb> key=value …` exists.** Deliberately thin per §3: a value
  that parses as JSON is JSON, anything else is a string, so a new engine verb
  needs no CLI change and every verb stays curl-able.
- **The daemon-socket transport is now live-proven**, which it was not through
  Phases B-D: `ctl` drives `/engine/*` over the real `daemon.sock` on three
  hosts. That gap is closed.

**The recipes teach the wait rule, they do not merely mention it.** Each one
waits for the state it expects after every input, and `form-fill.sh` carries the
comment saying why. A recipe that races is worse than no recipe.

### ⚠ SponsorBlock: the Phase C gap is NOT closed, and here is exactly where it stops

Installed into an isolated `HOME` and driven at `https://www.youtube.com/`: the
page really loaded (`location.host === "www.youtube.com"`) and
`window.__ysb` was **`undefined`** after a 15 s wait. The script declares
`@match https://*.youtube.com/*`, so the match is not the problem — its runtime
state appears to be watch-page-only, and a watch page was not exercised. So:
the userscript PLANE is proven (Phase C's probe script runs in its declared
world on an engine page), and **SponsorBlock's own state remains unproven**.
Do not read Phase C's green as covering it.

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
| Debian packages absent on a fleet host | `apt list` on dev/a headless host before Phase B | `sudo apt install libwpewebkit-2.0-1 libwpe-1.0-1` is a documented one-time prereq (a headless host precedent: libwebkit2gtk) |
| GPU-less hosts (a headless host) render slowly | bench in Phase D | swrast is fine for agent work; record numbers, don't guess |
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

**Proven on the GUI host too, 2026-07-31.** The AC names the GUI host as the gate host, so the
gate was re-run there: all five proofs PASS, exit 0, on display `:90` (the
picker correctly skipped `:1`, `:95`, `:96` and `:98`, which other processes
held). `ychrome engine flow` passes there as well, so Phase B's verbs are
proven on both hosts.

Run **non-invasively while the owner was working at the GUI host's GUI**, and that is
the pattern to reuse: the binary went to a private `/tmp` path and was never
installed; `HOME` was pointed at a private directory so nothing touched
`~/.yggterm`; the engine owns its own Xvfb on a display nobody is looking at.
Verified afterwards: no Xvfb of ours left running, the X lock files unchanged
from before the run, three yggterm GUI processes and two ychrome daemons still
up and untouched, and the private path removed. Nothing was deployed, restarted
or retired.

Worth noting from the the GUI host pixel: the same page renders with a different font
stack than on dev. That is the machine, not the engine, and it is why proof 3
counts INK inside the heading's own rect rather than comparing against a
reference image — a golden-image check would have gone red across hosts for a
reason that has nothing to do with the substrate.


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
