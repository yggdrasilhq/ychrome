# ychrome — pending bugs

Entries are removed in the same commit as their verified fix. Newest first.

> **This file is the ONE answer to "what is open" for ychrome.** Open items
> only; an entry is deleted in the same commit as its verified fix and git
> remembers it. The law, the owner table for every other question, and how to
> search the archive are in `yggterm/docs/docs-ssot.md`.

## ★★★ THE VAULT PANE IS NOT AT PARITY: no stored values, no copy, no last-sync

**Status:** OPEN

User-tested the sidebar against the real Bitwarden extension side by side,
2026-08-02, with screenshots. Three distinct gaps.

### 1. Edit cannot SHOW a stored value, so it cannot be copied or verified

Bitwarden's Edit Login renders the stored **password** and **authenticator key**
as masked fields with a REVEAL eye (and a regenerate control), plus the
passkey's creation date and per-URI rows. Ours renders a "Replace a secret"
block whose boxes are empty by design, explained as *"These boxes are empty
because this pane never carries a stored secret."*

The user's words: *"I cannot edit the existing fields or see them to manually
copy paste."* That is the whole complaint — the pane is a REPLACE form, not an
EDIT form, so you cannot check what is stored or copy it by hand, which is the
fallback every other client gives you when autofill misses.

⛔ The empty-box rule was a real invariant, not laziness: a schema travels
app → OSC → GUI and is re-sent on every refresh, so a secret placed in it is
broadcast repeatedly. **The fix is not to prefill the form.** It is an explicit
per-field REVEAL action — the user presses the eye, the pane fetches that ONE
value and renders it for that one render, exactly as Bitwarden's eye does — plus
a COPY action. The invariant becomes "a secret is never in a schema AT REST or
in a LISTING", which is the property that actually matters.

### 2. No copy actions on a row

Bitwarden's row overflow offers **Copy username · Copy password · Copy
verification code**, plus launch-URI, Edit, Clone, Archive, Delete. Ours offers
fill ⧉, totp ⏱ and edit ✎ only — every affordance is "put it in the page", none
is "give it to me". A user who needs the value anywhere else (another app, a
terminal, a form we mis-detect) has no route.

### 3. The sync line is a warning where a fact belongs — and the cause is ours

The pane shows: *"This host's vault agent does not report when it last synced,
so nothing here can tell you whether it is current. Install the current
ychrome-vault and hand the agent over."* The user: *"our sync now option should
have last synced time and then sync now button … A warning text is
non-confidence inspiring."*

**Root cause, measured 2026-08-02 — and the advice in that warning does not
work.** `status_json` (the ONE status builder, `agent.rs:1772`) always sets
`last_sync_unix`. The RUNNING agent's status has no such key at all — proven by
asking `~/.yggterm/vault/agent.sock` directly, not the CLI. So the agent
predates the field. But:

- `agent_stale` reads **false**, because it compares the agent against the
  INSTALLED binary and cannot see a missing FIELD;
- ⛔ **`ychrome-vault handover` REFUSES**: *"is the binary this agent is ALREADY
  running — nothing to hand over"*. It compares the PATH, and an in-place binary
  replace leaves the path identical while the code differs. **So the one
  documented zero-cost remedy is unavailable exactly when it is needed**, and
  the pane's own advice sends the user in a circle.

**Fixes owed:** ~~(a) `handover` must compare the installed binary's IDENTITY~~
✅ **(a) DONE 2026-08-02** (`ce0a7ec`): `exe_stamp` now captures the running
binary's stamp ONCE, pinned at `serve_on`, instead of re-reading the path's
mtime on every call — which after an in-place install described the SUCCESSOR
and made the identity check blind to the change it exists to detect.
⚠ **Honesty about this one: the reported refusal did NOT reproduce.** On Linux a
replaced binary readlinks as `<path> (deleted)`, so the stale agent stamped
itself `(deleted)@0`, `agent_stale` read **true**, and `ychrome-vault handover`
went through **with the unlock preserved** (verified live on guihost: 1120 items,
no master password re-entry). The fix removes the dependency on that procfs
detail, which does not hold if an installer overwrites in place and keeps the
inode.
✅ **The agent-side half of (b) is also cleared**: the post-handover agent
reports `last_sync_unix` (measured `1785682003`). What remains of (b) is purely
the PANE — show **"Last synced <time> · Sync now"** as a plain fact and keep the
warning for the genuine failure case only.
**(c) still owed:** sync on a schedule/staleness rule rather than making the
user press it.

⚠ Same family as three other findings today: a version-gated hot-restart that
cannot swap a same-version binary, `ctl --help` exiting 0 on a build without the
verb, and `agent_stale`. **An identity check that cannot see the change it
exists to detect.**


## ★★ A CLIENT-SIDE VIEW SWAP STALLS: the URL changes, the screen does not

**Status:** OPEN

Found 2026-08-02 driving the Google sign-in end to end. After each step Google
navigates its own URL (`/v3/signin/challenge/pwd`, then the 2SV screens) and the
**visible view never swaps**: the body still renders the PREVIOUS screen with
Google's own "Loading" prefix, while the next screen's inputs are present in the
DOM at **`0x0` with `offsetParent: null`**.

**This is layout, not paint** — the measurements come from `web eval`, so the
element genuinely has no box; a stale-pixel explanation is falsified. Google's
client-side swap did not complete.

`web wait --until load:finished` answers `met: true, elapsed_ms: 1`, which is
correct and unhelpful: the document load DID finish. Waiting does not help
(measured: still `0x0` after 60 s of polling).

**`location.reload()` renders the real screen every time**, so the flow is
drivable — at the cost of one reload per step, which is why it reads as "the
Google auth flow cannot be completed" to anyone who does not know the trick.

⚠ Downstream, this makes an HONEST refusal look like a selector bug:
`fill-vault` answers `no_hittable_match (… matched 1 element(s) and NONE could
receive a click)`. That is the tool correctly refusing to dispatch into a 0x0
target — do not "fix" it by loosening hittability.

**Worth knowing before diagnosing:** the passkey shim is NOT the cause. It was
the first hypothesis (Google's identifier field carries
`autocomplete="username webauthn"`, so a conditional-mediation call into a shim
that ignores `options.mediation` would block on the ceremony timeout) and it is
FALSIFIED: `performance.getEntriesByType("resource")` shows **zero** `/fido2/*`
and zero control-endpoint requests on the stalled page. The shim was never
called. ⚠ The `mediation` gap is real anyway —
`isConditionalMediationAvailable()` answers `false` while `shim.get()` ignores
`options.mediation` entirely, so a site that asks regardless would hang on a
ceremony no agent surface can approve. Worth closing on its own merits.


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

### IT REACHED THE USER, 2026-08-01 — measured, and the cause is NOT what it looked like

**Report:** a Forgejo 2FA page answered *"Could not read your security key. Your
browser does not currently support WebAuthn."* — *"How am I supposed to use the
passkey?"*

**Two independent faults, both measured on the GUI host. Neither was the documented
"the vault holds no passkey for this host" case:** `ychrome-vault passkeys
<host>` returns a real credential for it.

1. **The running agent does not know `passkey-hosts`.** Asked directly on
   `~/.yggterm/vault/agent.sock` it answers `unknown op "passkey-hosts"`, in
   0.2 ms, five times out of five. That is the documented deploy-ordering
   hazard below, and it happened: the browser shipped ahead of its agent.

   ⚠ **The stderr announce was not evidence either way, and reasoning from its
   absence sent a first diagnosis in the wrong direction.** ychrome's stderr is
   the PTY it was launched in — it is not in `~/.yggterm`, not in the GUI's
   `app-launch-logs`, and not anywhere anyone greps. **FIXED:** the vault pane
   now renders the state (`sidebar::passkey_shim_widgets`) with a one-click
   `handover`, so the browser says it where the user already is when a passkey
   login fails.

   ⚠ **`agent_stale` is structurally incapable of catching this**, which is why
   the pane was silent: it compares the agent against the INSTALLED
   `ychrome-vault`, and both were the same six-day-old binary. `status` read
   `unlocked`, `agent_stale: false`, 1116 items — a perfectly healthy vault —
   on the same socket that refused the op. Only ASKING for the op finds it.
   The remedy is also ordered: `handover` execs the *installed* binary, so a
   stale installed binary must be replaced FIRST or the handover is a no-op.

2. **The policy stamp was blind to the shim, which made it permanent.**
   `webpolicy::policy_version` stamped adblock, the UA, SponsorBlock and
   userscript FILES — nothing about the vault — while `/policy` prepends a shim
   whose scope comes from the vault. The GUI refetches only when that stamp
   moves, and yggterm applies userscripts at surface CREATION. So a surface kept
   whatever shim decision was true when it opened, for life.

   Measured, one unchanged `policy_version` (`ebc219f7d40ddc53`):
   `sidebar_contribution/policy` recorded `userscripts: 6` at 14:53 and
   `userscripts: 5` from 16:07 onward, across the deploy. **FIXED:**
   `passkey_shim_stamp()` folds `agent.pid` (rewritten by `serve_on` on every
   start AND every handover, since an `execve` keeps the pid) and the installed
   binary's stamp into `policy_version`, stat-only so it stays off the socket.

   ⚠ **Still open:** a plain lock → unlock touches no file, so a surface opened
   over a locked vault still needs REOPENING. The pane says so in words. A
   vault-published scope stamp would close it properly.

### ⚠ ENROLMENT ON A SITE WITH NO PASSKEY — THE ARM IS BUILT, THE PROOF IS OWED

The shim's match patterns come only from rpIds the vault ALREADY holds
credentials for, so a site you have no passkey for sees a pristine `navigator`
— exactly right for the fingerprinting fix, and it also meant
`navigator.credentials.create()` could never be called there. Every passkey in
this vault was enrolled in some other browser. The user hit this on a Google
sign-in: *"I cannot enter the passkey when anyone requests me … there is no
clicking to give passkey or save a passkey."*

⛔ The fix is NOT to widen the scope back, and it was not: **the user arms one
host from the vault pane** ("Enrol a passkey here"), the anomaly exists only on
a page a human deliberately armed, and arming is per-process so a browser
restart forgets it. Built 2026-08-02, unit-locked and mutation-proven (an armed
host must reach the shim's real match patterns; a wildcard or empty host is
refused at the door).

**Owed:** the end-to-end proof on a real page — arm a site, reopen the tab,
and watch `navigator.credentials.create()` succeed. ⚠ The pane says the tab must
be REOPENED, because the shim is installed when a surface is built; if that
turns out to be wrong on this path, the notice is what needs correcting.

### IMPLEMENTED 2026-08-01, NOT YET PROVEN ON A REAL PAGE

All four steps are done and unit-locked: the `passkey-hosts` agent op (metadata
only — rpIds, no credential ids, no keys), the `sidebar.rs` call through
`ychrome-vault-proto`, no-shim-on-a-locked-vault, and the probe kept off the
`policy_version` heartbeat. Match patterns are `*://<rp>/*` PLUS `*://*.<rp>/*`,
because WebKit's `*://*.example.com/*` does not admit the bare host while
WebAuthn scopes a credential to the rpId and its subdomains. The user-presence
invariant is untouched and locked: scoping decides only WHERE
`navigator.credentials` is patched.

**What is NOT yet demonstrated, and why this entry stays open:**

1. **No end-to-end proof on a real page.** Nobody has loaded a site the vault
   holds a passkey for and confirmed the shim is present, then loaded one it
   does not and confirmed `navigator.credentials` is pristine. That is the
   observation that would actually close this.
2. **The running vault agent predates the op**, so on dev today the browser
   installs the shim NOWHERE. Verified live: the agent answers `unknown op
   "passkey-hosts"`. The code says so loudly on stderr, once, rather than
   letting it look like the healthy case — but until the agent is handed over
   (`ychrome-vault handover`) passkeys are off.
3. ⚠ **DEPLOY ORDERING IS PART OF THIS FIX.** The vault agent must be handed
   over before, or with, the browser. Shipping the browser alone turns passkey
   logins off everywhere.

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
profile on the GUI host, which needs a yggterm GUI + daemon deploy — and
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

### The mechanism, traced 2026-08-02 (the expensive half is done)

Read on the yggterm side, `crates/yggterm-shell/src/shell.rs`:

- A contribution holds `policy`, `policy_attempts` and `declared_control_url`.
- `sidebar_contribution_matches_declare` ALREADY handles a moved endpoint: a
  declare carrying a different control url returns false, and the caller tears
  the contribution down so the declare re-creates it. **So re-resolution exists
  for a fresh declare** — which is why a session whose ychrome CLI is cycled
  recovers, and why cycling the clients is the working remedy today.
- What has no owner is the REPAIR. `fail_sidebar_policy` counts to
  `MAX_POLICY_FETCH_ATTEMPTS` (3) and `web_surface_policy_gate` then answers
  `Abandoned`, so the reconciler builds the surface with no userscripts and no
  ruleset. When the policy later becomes reachable, `apply_sidebar_policy` fills
  `contribution.policy` — and **nothing rebuilds the webview**, which still
  holds the empty `init_script` set it was created with. That is the "for its
  whole life" clause, and it is a missing edge rather than a wrong decision.

**Shape of the fix:** when `apply_sidebar_policy` lands a policy for a session
whose surface was created under `Abandoned`, reclaim that session's tabs so the
reconciler recreates them with the scripts attached —
`selecting_a_reclaimed_tab_recreates_its_webview_with_the_saved_url` is the
existing proof that a reclaimed tab comes back at its saved url, so the
machinery is already there and only the trigger is missing.

⚠ A rebuild is visible (the page reloads), and that is unavoidable:
`init_script` binds at webview creation, so there is no silent repair. Prefer
rebuilding a BACKGROUND tab immediately and an active one at the next
opportunity, rather than yanking the page the user is reading.

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
  friction: it is LOCKED on a headless host and dev (as root it answers `not_configured`) and
  resolves only on the GUI host, and `fill-vault` needs a mapped surface so it is
  unavailable on exactly the detached surfaces agents are told to prefer. Every
  run pays this toll by hand.
- **§3 OTP from the data-fabric** and **§5 Extract surface** — the same shape.

**Ask:** promote each numbered section into this file with a reproduction and an
acceptance test, or state explicitly which are declined and why. The `dream-*`
documents should hold *design*, never *the only record that something is
missing*. Same applies to `dream-detached-agent-surfaces.md`, whose §5 control
and §7 table are already bug-shaped.

## ⚠ FLAKY TEST — `daemon_staleness` fails on a busy host, and it is NOT the code

**Characterised 2026-08-01 on dev, after it cost a session real time twice.**
Two tests in `tests/daemon_staleness.rs` fail intermittently:

```
a_pre_gate_client_is_named_as_such_by_the_refusal_and_by_status
a_gated_route_survives_a_daemon_handover_on_what_the_client_re_declares
  → panicked at tests/daemon_staleness.rs:326: a control response:
    Os { code: 11, kind: WouldBlock }        ← a 5s read timeout, not a bug
```

`control_get` gives a spawned real binary **5 s** to answer over TCP. dev is a
32-core LXC that regularly sits at load 45-112 (the yggterm test binaries alone
were measured at 1208% and 1144% CPU), and under that the budget is simply not
enough.

**Proven to be load, not a regression, by a controlled A/B under the same load:**
at load 112 the *baseline* (HEAD with the change stashed) failed **3/3**; at
load ~44 the same tree passed **8/8 in 8.2 s**. Anything that changes the
daemon's startup path will look like it caused this. It did not.

**Want:** make the budget generous (or adaptive) rather than a wall clock a
loaded CI host cannot meet — a timing test that fails on a busy machine teaches
agents to ignore red, which is worse than the flake.

---

## ⚠ MINOR — `ychrome ctl --help` asks the engine to run `--help` as a verb

Found while verifying the subcommand-swallow fix, 2026-08-01. The important
half is closed (a bare unknown word exits non-zero and opens nothing), but:

```
$ ychrome ctl --help ; echo $?
{"error":"unknown engine verb \"--help\"","ok":false}
ychrome: engine replied 404
1                                ← non-zero, so no false capability report
```

`ctl` forwards `--help` as if it were a verb rather than printing the usage that
bare `ychrome ctl` already prints correctly. Cosmetic — it exits non-zero, so it
cannot resurrect the false-deploy-report failure — but `--help` should not
become a 404 from the engine.
