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
