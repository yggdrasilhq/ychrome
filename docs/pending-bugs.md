# ychrome — pending bugs

Entries are removed in the same commit as their verified fix. Newest first.

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
