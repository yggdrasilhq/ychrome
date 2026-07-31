# ychrome — pending bugs

Entries are removed in the same commit as their verified fix. Newest first.

---

## ★★★ A DAEMON HANDOVER STRANDS THE GUI ON A DEAD CONTROL PORT, AND SURFACES SILENTLY LOSE ADBLOCK

**User-reported 2026-07-31, immediately after a deploy + `ychrome daemon
restart`.** The GUI raised:

> **Web policy unavailable** — The app did not serve its ad-block and userscript
> policy (connect 127.0.0.1:42687: Connection refused (os error 111)). Its
> surfaces open unprotected.

**The notice is honest and correct — that part is good design.** The defect is
what it is honest *about*.

### What is actually true, measured on the live host

- The GUI was trying **42687**, a port from a daemon generation that no longer
  exists. Nothing listens there.
- **The policy was available the whole time.** The current daemon (pid 486744)
  serves the session's control endpoint on the port recorded in the journal's
  latest `register` — for `oi cfa` that is **34419**, and `curl
  127.0.0.1:34419/policy` returns the full new ruleset (`/ping` → 200).
- ⚠ **Do not misread the `ss` output**, as I did first: the ports the journal
  registers are owned by the **daemon process**, not by the per-session
  `ychrome --profile <name>` CLI. That is the design — the daemon hosts the
  control endpoint. The CLI's own listener answers **404** and is not it.

### The bug

**A daemon handover invalidates every control port the GUI has cached, and the
GUI does not re-resolve — it caches the dead port, warns once, and leaves the
surface unprotected.** The remedy exists in the daemon's own journal (the new
`register` line carries the live port); nothing consults it.

Consequence, and why this is ★★★: the user's browsing continues with **no
adblock and no userscripts** while the ruleset sits correctly installed and
served. The failure is silent after the single toast, and it survives for the
life of the surface.

### PROVEN IN A SHADOW SESSION, 2026-07-31 — and the consequence is worse than "no rules"

Probed with `server app web eval` against a real YouTube page in a backgrounded
probe session (the user's viewport never moved). The page contained **every one
of yggterm's own shims** — `__yggtermClipboardImagePasteShim`,
`__yggtermCloseShim`, `__yggtermScrollNavShim`, `__yggtermThemeColorShim` — and
**not one ychrome userscript**: `window.__ytAdDefense` `undefined`,
`window.__ysb` `undefined`, `window.fetch` still native. The surface reported
`policy_gate: "absent"`.

So the userscript plane is not merely late, it is **absent for the life of that
surface**, which is exactly what `yggterm/docs/web-surfaces.md:341-346` already
says happens:

> *"A surface opened before its contribution exists is created unblocked and
> runs without userscripts for its whole life. After
> `MAX_POLICY_FETCH_ATTEMPTS` failed fetches the gate opens anyway — a page with
> no adblock beats no page — and the user is notified."*

**That is the whole chain**: binary swap → daemon handover → GUI holds a stale
control port → policy fetch fails → gate opens unblocked → **no userscripts,
permanently** → YouTube pre-rolls play (they are served from googlevideo.com,
the same host as the video, so no network rule can touch them — the main-world
`adPlacements` strip is the only defence) and SponsorBlock never runs.

**Two fixes are owed, not one:**
1. Re-resolve the control port (above). Necessary, not sufficient.
2. ⭐ **"For its whole life" is too harsh a punishment for a transient fetch
   failure.** A surface that opened unblocked must be REPAIRED when the policy
   later becomes reachable — recreate it, or re-run the userscript attach —
   rather than staying degraded until the user happens to close the tab.
   Reloading the page does NOT fix it: `init_script` is bound at webview
   creation, so the surface must be rebuilt.

⚠ Interim remedy for a user in this state: **close and reopen the ychrome tab**
(not reload) once the policy endpoint answers. Verify with
`curl 127.0.0.1:<registered-port>/policy`.

⚠ Honest limit: a follow-up attempt to prove the cure by recreating the surface
was INCONCLUSIVE — the second `ychrome` invocation did not mint a new surface
(the href never changed), so "a fresh surface gets its scripts" is expected but
**not yet demonstrated**. Demonstrate it before closing this entry.

### The fix

On a policy-fetch connection failure the GUI must **re-resolve the endpoint**
(ask the daemon, or honour the next OSC re-declare) and retry, rather than
pinning the port it first learned. A refusal is only honest when re-resolution
is genuinely impossible. Consider also having the daemon push a policy-changed
signal on handover so surfaces refresh without a fetch failing first.

### Operator note — the deploy step that was skipped

`docs/pending-bugs.md` in **yggterm** already warns that after any ychrome
deploy the daemon is stale by definition and must be handed over **together with
its clients** ("clients and daemon together per the round-29 mixed-version
note"). The 2026-07-31 deploy handed over the daemon and did **not** cycle the
clients, which is exactly how this surfaced. Until the fix lands, the manual
remedy is to cycle each session's ychrome CLI so it re-declares its endpoint —
at a moment the user is not mid-task, because it reloads their page.

---

## ★★★ `/input` DISPATCHES A SELECTOR CLICK TO A ZERO-SIZE ELEMENT AND REPORTS SUCCESS

**Cost three wrong conclusions in one session, one of which blamed the operator's
vault for a failure that never happened.**

`document.querySelector` returns the FIRST match, and real pages carry hidden
duplicates. IBKR's login page has six-plus `button[type=submit]`, five of them
hidden at `0x0`, the live one third in document order:

```
[{txt:"Login", vis:false, wh:[0,0]},   ← querySelector returns THIS
 {txt:"Login", vis:false, wh:[0,0]},
 {txt:"Login", vis:true,  wh:[414,48]},  ← the real one
 {txt:"Authenticate", vis:false, wh:[0,0]}, …]
```

`ychrome ctl input page_id=… events='[{"type":"click","selector":"button[type=submit]"}]'`
resolved that to a `0x0` rect, dispatched at **(0,0)**, and answered:

    {"dispatched":3,"ok":true}

Nothing was clicked. The page appeared to "reset", and I concluded a brokerage
2FA had rejected a TOTP that had in fact never been submitted — then said so to
the operator, about his vault.

**The old surface plane already gets this right:** `web do` refuses with
`"<selector> matched a zero-size element"`, alongside its siblings
`detached_node` and `target_moved`. §4 of `agent-engine.md` says selector clicks
are sugar that "resolves center, scrolls into view" — the resolver needs the same
refusals the surface plane's has.

**Fix:** refuse a `0x0` or offscreen target by default (`zero_size_element`),
after attempting `scrollIntoView` and re-measuring. Consider preferring the first
VISIBLE match over the first match, or requiring the caller to opt in to
ambiguity. **`{"dispatched":n,"ok":true}` must never describe a click that hit
nothing** — an engine that reports events dispatched into the void is the exact
lie-of-success shape this codebase refuses everywhere else. Lock it with a
fixture page carrying a hidden duplicate ahead of the real control.

## ★★ THE ENGINE ONLY EXISTS ON the GUI host, SO AGENT BROWSING STILL BURNS THE OPERATOR'S LAPTOP

The engine's whole purpose is that agent browsing stops costing the human. It is
half-delivered: **headless means off-screen, not off-host.** `Xvfb :90` and every
WebKitWebProcess run on **the GUI host — the operator's machine** — so the CPU, RAM and
thermal cost are still his; only the pixels are hidden.

`dev` and `oc` still have the OLD binary (`ychrome ctl pool --json` →
`unexpected argument 'pool'`), so there is nowhere else to run it. dev is a
container on mains power and is the documented preference for anything that
renders (`data-fabric`: *"Prefer dev over the GUI host… the GUI host is the laptop the user is
working on"*).

**This is a bug, not a deployment chore**, because the feature's stated benefit
does not exist until it lands: an engine that can only run on the operator's
laptop has moved the problem, not solved it. It also blocks the fleet framing in
`agent-engine.md` §5 (budgets, "hundreds of pages") — none of which belongs on a
14 GB laptop that has already been driven into swap exhaustion and an OOM kill by
agent browsing.

**Wants:** ychrome deployed fleet-wide (it "deploys as a fleet" per standing
practice), and the engine verified reachable from dev/oc, so `ctl` runs where
nobody is sitting.

## ★★ THE `dream-control-surfaces` ITEMS ARE BUGS, NOT ASPIRATIONS

`docs/dream-control-surfaces.md` is filed as a dream document. The operator's
ruling, 2026-07-31: **these are bugs.** Framing a missing capability as a dream
puts it outside every process that would fix it — it is not in a bug list, it has
no reproduction, and nobody triages it. Several of its items have now cost real
sessions:

- **§2 Headless surface-create — the OSC must not depend on window focus.** This
  is precisely the gap that sent an agent toward revealing surfaces on the
  operator's screen; it is the subject of `dream-detached-agent-surfaces.md` and
  of the detached-by-default rule in `yggterm/docs/agent-surface-attachment.md`.
- **§1 Unlock request** and **§6 Autofill-from-vault.** The vault is the standing
  friction: it is LOCKED on oc and dev (as root it answers `not_configured`) and
  resolves only on the GUI host, and `fill-vault` needs a mapped surface so it is
  unavailable on exactly the detached surfaces agents are told to prefer. Every
  run pays this toll by hand.
- **§3 OTP from the data-fabric** and **§5 Extract surface** — the same shape.

**Ask:** promote each numbered section into this file with a reproduction and an
acceptance test, or state explicitly which are declined and why. The `dream-*`
documents should hold *design*, never *the only record that something is
missing*. Same applies to `dream-detached-agent-surfaces.md`, whose §5 control
and §7 table are already bug-shaped.

## ★★★ THE ENGINE IS NOT HEADLESS — IT OPENS REAL WINDOWS ON THE OPERATOR'S DESKTOP

**Found on the GUI host, 2026-07-31, by the operator, who watched an IBKR login window
appear over the video he was watching.** He sent a screenshot. This is the
feature's central promise inverted: the engine exists so agent browsing stops
touching the human's screen, and instead it puts a titled `ychrome` toplevel in
front of them — **with a filled-in brokerage login visible on it.**

### Reproduction

```
ychrome ctl open url=https://… profile=finance viewport='{"w":1400,"h":950}'
  → {"ok":true,"page_id":"pg_00000N","state":"live"}       ← reports success
  → and a real window appears on the operator's Wayland session
```

`ychrome ctl close page_id=…` removes the window, so the lifecycle is at least
honest; the window simply should never have existed.

### Cause (diagnosed, not guessed)

The daemon's environment has **no `WAYLAND_DISPLAY` and no `DISPLAY`**:

```
$ tr '\0' '\n' < /proc/$(pgrep -f 'ychrome --daemon')/environ | grep -icE 'wayland|display'
0
$ ls /run/user/1000/wayland-*
/run/user/1000/wayland-0   /run/user/1000/wayland-0.lock   /run/user/1000/wayland-1
```

**GDK's Wayland backend defaults to `wayland-0` when `WAYLAND_DISPLAY` is
unset**, and `XDG_RUNTIME_DIR` still points at the operator's runtime dir — so
"no display configured" does not mean "no display". It means *the operator's
compositor*. Unsetting the variable is not isolation; it is the default path to
their screen.

This is the same family as two failures already recorded in this fleet's memory:
x11vnc refusing to start because it saw an inherited `WAYLAND_DISPLAY`, and the
daemon's frozen environment poisoning every session it spawns. **An inherited —
or absent — variable that describes a different world.**

### What `docs/agent-engine.md` promises

§9.1 Decision 1 settles the substrate as *"WebKitGTK on an engine-owned headless
display."* The **engine-owned headless display is the half that is missing.**
Either it was never wired, or it is not reached on this path.

### Shape of a fix

- The engine must **own its display**, explicitly and positively: start (or
  attach to) a headless compositor — `wlheadless`/`cage`/a nested sway, or
  `WAYLAND_DISPLAY=<engine-owned>` — and set it in the environment of the
  webviews it creates. Never inherit, never default.
- **Fail closed.** If the engine cannot get its own display it must refuse to
  open a page, with a named error, rather than silently borrowing the seat. An
  engine that quietly renders on the operator's compositor is worse than an
  engine that does not start: the whole point of the plane is that the human's
  screen is not ours.
- **Lock it**: assert the webview's display is not the session's — compare
  against `XDG_RUNTIME_DIR`'s `wayland-0`, or assert an engine-owned value is
  set — and mutate it to prove the test red.

### Until it is fixed

**Agent browsing through `ctl` is NOT safe to run on a host the operator is
using.** It is not a background row (the earlier `--no-activate` complaint) — it
is a focused window over their work. Treat the engine as usable only on a
headless host until this closes.

## ★★ A MISTYPED OR NOT-YET-BUILT SUBCOMMAND IS SILENTLY SWALLOWED AS A URL

**Found on oc, 2026-07-31, while checking whether the engine had been deployed.
It produced a false deployment report.**

`ychrome` takes a positional `[URL]`, so **any bare word in argv position 1 is
accepted as a URL** — including a word that is obviously a subcommand. On a
binary that predates `ctl`:

```
$ ychrome ctl --help          ; echo $?
<prints the plain ychrome usage>
0                              ← EXIT ZERO. `ctl` was swallowed as the URL.

$ ychrome ctl pool --json     ; echo $?
error: unexpected argument 'pool' found
2                              ← only errors because there are now TWO positionals

$ timeout 8 ychrome ctl       ; echo $?
124                            ← HUNG. It is trying to open a surface for
                                 a "url" literally named `ctl`.
```

### Why this is worth fixing rather than living with

1. **It defeats the obvious capability probe.** `cmd sub --help; echo $?` is how
   everyone checks whether a build has a feature. Here it answers *yes* on a
   binary that has never heard of the subcommand. I ran exactly that across the
   fleet, recorded `CTL_PRESENT` for a host with the old binary, and reported a
   deploy state that was wrong. The real probe has to be a subcommand **plus an
   argument** (`ychrome ctl pool --json` → `unexpected argument 'pool'`), which
   nobody would guess.
2. **The bare form does not fail — it hangs.** `ychrome ctl` opens a surface for
   a nonsense URL. A typo (`ychrome clt`, `ychrome stauts`) is a launched
   browser session, not an error message. In a script it is a hang.
3. It gets worse as `ctl` grows: every new verb is another word that means
   something on a new binary and silently means "browse to this" on an old one.

### Shape of a fix

The positional is genuinely useful (`ychrome example.com` should work), so the
answer is not to remove it — it is to stop a **subcommand-shaped** token from
reaching it. Options, cheapest first:

- **Reserve the subcommand names.** If argv[1] exactly matches a known verb
  (`ctl`, `daemon`, `status`, `update`, …) treat it as a subcommand, and if this
  build cannot serve it, **fail loudly** — `unknown subcommand 'ctl' (this build
  is 0.1.0; the engine landed in <version>)`. That sentence alone would have
  saved the false report.
- **Require the positional to look like a URL** — a scheme, a dot, `localhost`,
  or an existing path. A bare `ctl` matches none of those. Reject with
  `not a URL and not a known subcommand: 'ctl'`.

Either way the test is one line and locks the real defect: *a bare unknown word
in argv[1] must exit non-zero without opening anything.*

### Related, unverified — do not treat as filed

When a surface already exists for a profile, `ychrome <new-url> --profile <p>`
is documented to route the new URL into it (`--here` opts out). In one
observation the surface **stayed on the previous URL** instead. I could not
re-test it before running out of room, so this is a sighting, not a bug report.
It matters because it is what blocked building a page-of-my-own control for the
injection question — see `dream-detached-agent-surfaces.md` §5.

**Deploy state at time of writing:** the GUI host has the new binary with a **stale
daemon** (pid 3857563, up 54,310 s, answering `{"error":"unknown op","stale":
true}`); dev and oc still have the old binary. `ychrome status` diagnoses this
correctly and refuses to retire under the operator's 3 live surfaces —
`ychrome daemon restart` is the handover and is the operator's call.
