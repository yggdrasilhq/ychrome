# ychrome — pending bugs

Entries are removed in the same commit as their verified fix. Newest first.

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

## ★★ (yggterm) A PROFILE WHOSE WRITE-LOCK IS HELD ELSEWHERE OPENS EPHEMERAL, SILENTLY

**Not in this repo — filed here because it is the other half of the cookie-jar
failure ychrome just fixed on its own engine plane, and an ychrome user meets it
as "the login will not stick".**

`yggterm-shell/src/shell.rs` (~:10672) comments that a surface whose profile
write-lock is held elsewhere "opens READ-ONLY (ephemeral, no jar)". Ephemeral is
not read-only: an ephemeral `WebContext` reads NOTHING from the jar and writes
nothing back. A second surface on a profile another surface already holds
therefore starts logged out and cannot keep a cookie — including a bot-check
clearance cookie, which is why a challenged login can loop forever with nothing
on screen explaining it.

Two things are owed there: make the degradation match its comment (a genuinely
read-only jar), and **stop degrading silently** — the surface has to say which
mode it opened in.

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

## ★★ THE ENGINE ONLY EXISTS ON guihost, SO AGENT BROWSING STILL BURNS THE OPERATOR'S LAPTOP

The engine's whole purpose is that agent browsing stops costing the human. It is
half-delivered: **headless means off-screen, not off-host.** `Xvfb :90` and every
WebKitWebProcess run on **guihost — the operator's machine** — so the CPU, RAM and
thermal cost are still his; only the pixels are hidden.

**HALF-CLOSED 2026-07-31 (oc).** `fleet-binary-sync` pulled the engine binary to
**oc**, where `ychrome ctl pool --json` now answers for real and the whole IBKR
login flow was driven off the operator's laptop.

**dev IS NOW COVERED TOO — re-measured 2026-08-01.** The claim above that
`ychrome` is `command not found` on dev is stale: `/home/user/.local/bin/ychrome`
is present on dev, guihost and oc with the same mtime, and the whole OpenWrt
attended-sysupgrade verification of 2026-08-01 was driven through `ychrome ctl`
on **dev**, off the operator's laptop entirely. What is left of this entry is
keeping it that way — a deploy question, not a code defect.

⚠ **The TOTP caveat is also stale.** oc and dev can both mint a TOTP now; see
the clock entry below, whose infra half is closed.

**This is a bug, not a deployment chore**, because the feature's stated benefit
does not exist until it lands: an engine that can only run on the operator's
laptop has moved the problem, not solved it. It also blocks the fleet framing in
`agent-engine.md` §5 (budgets, "hundreds of pages") — none of which belongs on a
14 GB laptop that has already been driven into swap exhaustion and an OOM kill by
agent browsing.

**Wants:** ychrome deployed fleet-wide (it "deploys as a fleet" per standing
practice), and the engine verified reachable from dev/oc, so `ctl` runs where
nobody is sitting.

## ★★ `ychrome-vault totp` GUESSES ON A WRONG CLOCK INSTEAD OF REFUSING

**The infra half of this entry is CLOSED — manin's clock is fixed.** Re-measured
2026-08-01: `chronyc tracking` on manin reads `System time : 0.000222107 seconds
fast of NTP time`, guihost/dev/oc agree on the epoch to the second, and drift
against a real server's `Date` header is **−1 s on dev and 0 s on oc** (it was
72 s). `ychrome-vault totp` was exercised end to end on dev: six digits, stable
within its window, and it advances exactly on the 30 s boundary.

**What remains is the code half, and it is the part that made the failure
expensive.** When the host clock was 72 s out, `totp` emitted a six-digit code
that was *always* wrong while looking perfectly well-formed — no error, no
warning. It is a lie-of-success: the instrument reports a confident answer that
cannot be right, and the operator spends it on a live-brokerage 2FA prompt
before anyone suspects the clock. Waiting never helps, because a constant skew
never drifts into the correct window.

**Want:** `totp` should **refuse, not guess**, when the host clock is further
than one window from a trusted reference — and say that the clock is why.

⚠ **Keep this trap written down, because it is what hid the skew for so long.**
chrony reported `Last offset : -0.000112349 s` and `RMS offset : 0.0003 s` — it
believed it was tracking *perfectly* while system time was 72 s out. Reading
`chronyc tracking`'s offset lines alone tells you the clock is healthy. **Only
the `System time :` line, or a comparison against a real server's `Date` header,
shows the truth.**

**Also worth remembering:** `fleet-memory-sync.sh` is newest-**mtime**-wins with
no `--delete`, so any future clock skew between guihost and manin silently resolves
two edits inside the skew window by the wrong winner.

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
  resolves only on guihost, and `fill-vault` needs a mapped surface so it is
  unavailable on exactly the detached surfaces agents are told to prefer. Every
  run pays this toll by hand.
- **§3 OTP from the data-fabric** and **§5 Extract surface** — the same shape.

**Ask:** promote each numbered section into this file with a reproduction and an
acceptance test, or state explicitly which are declined and why. The `dream-*`
documents should hold *design*, never *the only record that something is
missing*. Same applies to `dream-detached-agent-surfaces.md`, whose §5 control
and §7 table are already bug-shaped.

## ⚠ UNVERIFIED SIGHTING — a second invocation may not route into an existing surface

Carried over from the (now closed) subcommand-swallow entry, because it was
never reproduced and should not be lost with its parent.

When a surface already exists for a profile, `ychrome <new-url> --profile <p>`
is documented to route the new URL into it (`--here` opts out). In one
observation the surface **stayed on the previous URL** instead. It has not been
re-tested, so this is a sighting, not a bug report. It matters because it is
what blocked building a page-of-my-own control for the injection question — see
`dream-detached-agent-surfaces.md` §5.

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
