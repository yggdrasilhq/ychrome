# ychrome — pending bugs

Entries are removed in the same commit as their verified fix. Newest first.

---

## ★★ (yggterm) THE PANE THROWS AWAY THE 403 THAT EXPLAINS ITSELF

**Not in this repo. Filed here because it is the remaining half of the
`control endpoint returned 403` report of 2026-07-31**, whose ychrome half is
fixed (see `docs/protocol.md` §"The third mixed case").

ychrome's GUI-only refusals now answer with the cause and the remedy in the body:

```json
{
  "cause": "client_predates_control_token",
  "error": "forbidden: /pane/vault is GUI-only and the ychrome CLI serving this
            session predates the control-token gate, so it declares no token and
            this route can never answer. restarting the daemon does not fix it
            and neither does restarting the GUI: press Ctrl+C in that session's
            terminal and run ychrome again. …"
}
```

`yggterm-shell/src/shell.rs:2149` discards all of it:

```rust
if !(200..300).contains(&status) {
    return Err(format!("control endpoint returned {status}"));
}
```

That string is what `app_pane_apply_error` renders, so the user reads a bare
status code while the app is answering in full sentences one function call away.
**The fix is to carry the body's `error` when the response has one** (and it may
key off `cause` for a structured recovery later). Until that lands, the actionable
text is reachable only from the daemon journal, `ychrome status`, or curl.

⚠ An ychrome-side workaround was considered and rejected: answering `GET
/pane/<id>` with a 200 whose schema explains the failure would put the message
where the user is, but it would also make a page-reachable GUI-only route answer
200, which is the invariant the gate exists to hold. The 403 is right; the GUI
should read it.

---

## ★★★ THE PASSKEY SHIM PATCHES `navigator.credentials` ON EVERY PAGE, INCLUDING A CHALLENGE PAGE

**Found 2026-07-31 while investigating why a Cloudflare challenge on a
brilliant.org login would not clear.** Not the cause of that report (the UA and
the engine's cookie jar were, both fixed) but a real, measured incoherence that
a bot check can read.

`sidebar.rs`'s `/policy` prepends `passkey::shim_userscript()` to EVERY
surface's userscripts, main world, document-start, unconditionally. On a page it
touches:

- `navigator.credentials` becomes a plain `Object.create(native || {})`, whose
  methods stringify to JS source rather than `[native code]`;
- `window.PublicKeyCredential` is DEFINED, and
  `isUserVerifyingPlatformAuthenticatorAvailable()` answers `true`.

**Measured on the engine plane (which does not install the shim), WebKitGTK
2.52.5:** `typeof window.PublicKeyCredential === "undefined"` and
`navigator.credentials === undefined`. WebKitGTK has no WebAuthn at all. So on
the visible surface we claim a platform authenticator that this engine cannot
have — an anomaly no real GNOME Web ever shows.

**Why top-frame-only does not save it.** The shim is already `all_frames:
false`, so the `challenges.cloudflare.com` iframe is clean. But an interstitial
managed challenge is served as the TOP-FRAME document at the site's own URL, and
its collector runs in exactly the environment the shim has already patched.

**Why there is no cheap fix, stated so it is not re-attempted.** Since the
engine genuinely lacks WebAuthn, ANY presence of these APIs is the anomaly —
making the shim "look native" cannot work, and a URL-pattern exclusion cannot
help because the challenge is served at the normal page URL. The only correct
shape is **per-origin installation**: build the shim's `Userscript::matches`
from the set of rpIds the vault actually holds passkeys for, so a page for a
site you have no passkey for sees a pristine `navigator`.

That needs a vault op this client does not have — `passkeys <item>` resolves one
item and there is no way to enumerate rpIds. The work is:

1. add a `passkey-hosts` op to `ychrome-vault`'s agent (metadata only: rpIds, no
   credential ids, no keys);
2. call it from `sidebar.rs` through `ychrome-vault-proto` (already linked, no
   subprocess) and set `matches` to `*://*.<rpId>/*` per host;
3. a LOCKED vault answers nothing, and installing no shim is then CORRECT rather
   than a regression — a ceremony needs an unlocked agent anyway;
4. ⚠ do not put the rpId probe on the `policy_version` path: that stamp is
   recomputed on the ~4 s heartbeat and must not grow a socket round trip.

---

## ★★ (yggterm) A PROFILE WHOSE WRITE-LOCK IS HELD ELSEWHERE OPENS WITH NO JAR

**Not in this repo — filed here because it is the other half of the cookie-jar
failure ychrome fixed on its own engine plane, and an ychrome user meets it as
"the login will not stick".**

`yggterm-shell/src/shell.rs` commented that a surface whose profile write-lock
is held elsewhere "opens READ-ONLY (ephemeral, no jar)". Ephemeral is not
read-only: an ephemeral `WebContext` reads NOTHING from the jar and writes
nothing back. A second surface on a profile another surface already holds
therefore starts logged out and cannot keep a cookie — including a bot-check
clearance cookie, which is why a challenged login can loop forever.

Two things were owed. **One is done** (yggterm `lane/dev/ychrome-bugs-docs`):
the silence. `WebSurfaceJarMode` now owns the decision, the spelling and the
words — `persistent` / `ephemeral_by_request` / `no_jar_lock_held_elsewhere` —
the mode is on the `profile_write_lock` trace line, and a degraded profile
raises ONE notice (per profile, not per tab) that talks about being logged out
and about the bot-check cookie rather than about write locks. The misleading
"READ-ONLY" comment is gone. A `debug_assert` pins the mode to the jar it
describes.

⚠ **The other half is a design call, not a mechanical fix, and that is why it
is still here.** WebKitGTK has no read-only jar mode, so "genuinely read-only"
means giving the loser a private COPY of the profile's cookies (a Netscape text
file — mechanically easy) and its local storage. Editorially it is not easy:
**every shadow surface an agent opens on a profile the user holds would then
duplicate that profile's live session cookies to a second place on disk.** In a
browser that carries the operator's brokerage sessions, spreading cookie jars is
his decision to make. Options, so the next reader does not start from scratch:

1. copy-on-open into a scratch dir, wiped at teardown, with a startup sweep for
   crash leftovers — full fidelity, maximum secret spread;
2. copy the `cookies` file ONLY, which fixes the reported symptom (the login
   sticks for the surface's life) at a fraction of the exposure;
3. decline, and keep the notice as the whole answer.

⚠ **Not live-proven.** The notice needs two live clients contending for one
profile on jojo, which needs a yggterm GUI + daemon deploy — and
`yggterm/docs/pending-bugs.md` still carries "REMOTE ROWS WEDGE IN
`RemoteBootstrap` AFTER A DAEMON VERSION HANDOVER", which wedged 15 rows on the
last bump. The rule and the words are unit-locked and mutation-proven; the
pixel is owed.

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

⚠ **Measured while fixing the sibling token bug, 2026-07-31 — one fact that
narrows this one.** The CLIENT does re-declare the moved control url on every
~4s heartbeat, and has since 2026-07-18; a real client was driven through a real
`ychrome daemon restart` on dev and followed the new port every time. So the
dead-port window is one heartbeat, not the life of the session, and the durable
damage named above ("no userscripts, permanently") comes from the SURFACE that
was created unblocked during that window rather than from a declare that never
arrives. That makes fix (2) — repair a surface that opened unblocked — the
load-bearing half. Which sessions are still on a pre-gate CLI is now visible in
`ychrome status`; that marker is about the token, not about this port, but it
answers "did anyone forget to cycle the clients" in one command.

---

## ★★ SEVERAL DAEMONS RACE FOR ONE SOCKET, AND EVERY ENGINE PAGE DIES WITH THE LOSER

**Measured on dev 2026-07-31**, while live-verifying SponsorBlock. Symptom, from
an agent's side:

```
$ ychrome ctl open profile=sbv2 url=…        → {"ok":true,"page_id":"pg_000001"}
$ ychrome ctl eval page_id=pg_000001 …       → {"error":"no page \"pg_000001\""}
```

The journal explains it: `daemon_start pid 2797950` at T, then a **second**
`daemon_start pid 2801025` fifteen seconds later, then `engine.start` on
`display :101` where the first had been on `:100`. `pgrep -f "ychrome --daemon"`
showed **five** live daemons. The page really was opened; it belonged to an
engine whose daemon no longer owns `daemon.sock`.

**Why it compounds.** Every surface CLI calls `daemon::ensure` on its heartbeat,
so a single failed connect makes each of them spawn a daemon, and they then
alternate ownership of the socket. Displays climbed `:100 → :105` across one
session; the orphaned `Xvfb` processes from `:90`-`:99` were still resident from
earlier rounds.

**Why it matters beyond tidiness:** an agent cannot hold a page across two `ctl`
calls, which is the entire premise of the engine's verb surface. Every recipe in
`assets/engine-recipes/` is a sequence of `ctl` calls against one `page_id`. The
workaround that got the SponsorBlock proof through was to poll `ctl status`
until the `display` stopped changing, then run the whole drive inside one script.

**Wants:** one owner of `daemon.sock` (an advisory lock or an atomic bind, so a
loser exits instead of rebinding), a reaped `Xvfb` when a daemon retires, and
`ctl` reporting which daemon generation answered so "no page" can say *why*.

⚠ Distinct from the ★★★ handover entry above, which is about the GUI caching a
dead control port. This one is about the daemons multiplying in the first place.

---

## ★★ THE ENGINE ONLY EXISTS ON jojo, SO AGENT BROWSING STILL BURNS THE OPERATOR'S LAPTOP

The engine's whole purpose is that agent browsing stops costing the human. It is
half-delivered: **headless means off-screen, not off-host.** `Xvfb :90` and every
WebKitWebProcess run on **jojo — the operator's machine** — so the CPU, RAM and
thermal cost are still his; only the pixels are hidden.

**HALF-CLOSED 2026-07-31 (oc).** `fleet-binary-sync` pulled the engine binary to
**oc**, where `ychrome ctl pool --json` now answers for real and the whole IBKR
login flow was driven off the operator's laptop. **`dev` is still uncovered, and
worse than before: `ychrome` is `command not found` there** — not an old binary,
no binary. dev is a container on mains power and is the documented preference for
anything that renders (`data-fabric`: *"Prefer dev over jojo… jojo is the laptop
the user is working on"*), so it is the one host that most needs it.

⚠ **oc can run the engine but CANNOT mint a TOTP** — see the clock-skew entry
below. Any flow that needs a second factor has to source the code from jojo even
when the browsing itself happens on oc.

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
  resolves only on jojo, and `fill-vault` needs a mapped surface so it is
  unavailable on exactly the detached surfaces agents are told to prefer. Every
  run pays this toll by hand.
- **§3 OTP from the data-fabric** and **§5 Extract surface** — the same shape.

**Ask:** promote each numbered section into this file with a reproduction and an
acceptance test, or state explicitly which are declined and why. The `dream-*`
documents should hold *design*, never *the only record that something is
missing*. Same applies to `dream-detached-agent-surfaces.md`, whose §5 control
and §7 table are already bug-shaped.

## ★★★ THE ENGINE IS NOT HEADLESS — IT OPENS REAL WINDOWS ON THE OPERATOR'S DESKTOP

**Found on jojo, 2026-07-31, by the operator, who watched an IBKR login window
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

**Deploy state at time of writing:** jojo has the new binary with a **stale
daemon** (pid 3857563, up 54,310 s, answering `{"error":"unknown op","stale":
true}`); dev and oc still have the old binary. `ychrome status` diagnoses this
correctly and refuses to retire under the operator's 3 live surfaces —
`ychrome daemon restart` is the handover and is the operator's call.
