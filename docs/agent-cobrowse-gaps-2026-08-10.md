# Co-browse gaps found driving two Indian bank logins (2026-08-10)

Found by **a peer campaign row** (the bank lane) driving `netbank.examplebank-a.example` and
`netbank.examplebank-b.example` headless on `ychrome ctl` from **dev**. Both logins were reached to their
2FA step, so the engine did its job — these are the places where an instrument's **own success
field disagreed with the page**.

Site recipes are in lore: `netbank.examplebank-a.example` ·
`netbank.examplebank-b.example`.

---

## D1 ⛔⛔ `ctl fill` writes into a hidden DECOY input and then verifies the decoy

**Severity: highest here — the failure mode spends a bank login attempt.**

Example Bank A's login form carries three password inputs, deliberately:

```
input[name=password0]   display:none   autocomplete="new-password"   ← DECOY
input#pass              visible        autocomplete="off"            ← the real one
input[name=password1]   display:none   autocomplete="new-password"   ← DECOY
```

Exact command and exact response:

```
$ ychrome ctl fill page_id=pg_000034 entry='netbank.examplebank-a.example'
{"filled":"filled","ok":true,"confirm":"present-but-unnamed","secret_field_count":3,
 "fields":[{"field":"username","present":true,"ok":true,"want":9,"got":9,
            "target":{"name":"custid","tag":"input","type":"text"}},
           {"field":"secret","present":true,"ok":true,"want":14,"got":14,
            "target":{"name":"password0","tag":"input","type":"password"}}]}
```

Independent page-side readback, same instant:

```
{"cust":"<customer id>", "passlen":0, "h0":14, "h1":9}
                          ↑ the REAL password box is EMPTY
```

**What it cost:** had I trusted `filled:"filled"` and clicked Login, Example Bank A would have received an
empty password — **one consumed attempt on a bank that locks netbanking after repeated failures.**
The owner's standing warning for this lane is *"the password is correct"*, so the natural next move
after such a rejection is to doubt the credential or rotate it. This defect manufactures exactly
the wrong diagnosis.

⚠ **Why the 2026-08-07 `unverified` hardening does not catch it.** That change added a length
readback — but it reads back *the field it wrote*. A wrong-target write verifies perfectly:
`want == got` proves the value survived transport, and says nothing about whether the target is the
field the form will submit. ⇒ **A readback aimed at the wrong subject is not a verification.**
(Same shape as the health check that ANDed facts about two processes and passed on a corpse.)

Note `h1:9` too — the *username* was also pushed into the second decoy.

**Suggested fix, in preference order:**
1. **Skip inputs that are not hittable** (`display:none`, `visibility:hidden`, zero-size) when
   choosing a fill target — `/input` already computes exactly this pool for selector clicks, so the
   two verbs would agree instead of disagreeing.
2. **Report the target's visibility in `fields[].target`** so a caller can refuse. Cheap, and it
   makes the existing reply honest without changing behaviour.
3. Prefer `autocomplete="off"`/visible over `autocomplete="new-password"`+hidden when several
   password inputs match — the honeypot convention is stable across Indian bank portals.

---

## D2 ⚠ `type`'s `landed` flag is a FALSE NEGATIVE on every masked/formatted input

Example Bank B's mobile field reformats as you type. One event carrying all ten digits:

```
$ ychrome ctl input page_id=pg_000035 events='[{"type":"type","text":"<10 digits>"}]'
{"dispatched":20,"ok":true,"resolved":[{"kind":"type","landed":false,
  "before":0,"after":12,"grew_by":12,"want":10,...}]}
page-side value: "NNN N       "     ← genuinely mangled. landed:false is CORRECT here.
```

Ten events, one character each — every digit lands correctly:

```
{"dispatched":20,...,"landed":[false × 10]}
page-side value: "NNN NNN NNNN"     ← CORRECT. landed:false is WRONG on all ten.
```

`landed` is computed from length growth vs `want`. A mask that inserts a space makes growth ≠ 1, so
the flag reports failure on a keystroke that landed perfectly.

**What it cost:** the flag is the natural retry signal. An agent that retries on `landed:false`
**doubles the input** into the field — and on an amount, card or account field that is not a
cosmetic bug. It also cost this run a round of doubt about a step that had actually worked.

**Suggested fix:** for a field whose value is reformatted, growth is the wrong measure. Either
compare *digits/characters retained* rather than raw length, or add a
`"reformatted": true` observation when `after != before + want` **but the typed characters are all
present in order** — and say plainly in the reply that `landed` is unreliable for masked inputs.
The honest fallback is already in the reply (`before`/`after`/`grew_by`); it is `landed`'s
boolean that overstates.

---

## D3 ⚠ `lore.py`'s private-data lock is ASYMMETRIC — it caught one identifier and passed another

The lock is genuinely good and it stopped me: a 10-digit phone number in the Example Bank B body was refused
with the right words (*"write 'the registered number'"*). **The same lock passed a 9-digit bank
customer ID** in the Example Bank A body, which was written to a PUBLIC lore file and only removed because I
grepped for it by hand afterwards.

**What it cost:** nothing this time, by luck. But the lock's existence is what makes an agent stop
checking — a partial filter that reads as a complete one is worse than none, because it transfers
the belief without the coverage.

**Suggested fix:** extend the pattern set to bank-shaped identifiers (customer id, CIF, account no,
IFSC, MICR, card PAN) and, more importantly, **say what it checked** — a refusal names its hit, but
a pass currently says nothing, so the caller cannot tell "clean" from "not looked for". Same law as
everywhere: an absence has to describe its own boundary.

---

## D4 ⛔ A FAILED `ctl open` STILL LEAVES A LIVE PAGE — and the error is why nobody reaps it

```
$ ychrome ctl open url=https://retail.examplebank-a.example/ profile=agent-fin-bank
{"error":"load of https://retail.examplebank-a.example/ failed: Error resolving \u201cretail.examplebank-a.example\u201d: Name or service not known","ok":false}
ychrome: engine replied 502
```

No `page_id` is returned. But the page **was created and stayed `live`**:

```
$ ychrome ctl pages     # much later, after closing the two pages I knew about
{"page_id":"pg_000033","profile":"agent-fin-bank",
 "url":"https://retail.examplebank-a.example/","state":"live","title":null}
```

**What it cost / why it is worse than an ordinary leak:** the reply is an *error with no
`page_id`*, so a correct caller concludes nothing was created and has nothing to reap it by. The
leak is therefore invisible to the only party positioned to clean it, and it accumulates silently
against `max_live` (12) until `ctl open` starts refusing with saturation. I only found mine because
I counted pages on my own profile after closing both pages I knew about, and one remained.

⚠ **This plausibly explains the pool pressure another lane is carrying.** At the start of this run
`ctl pool` reported `live 12/12, live_headroom: 0`, with **9 of 12 pages on one other profile —
described as "one app with different cache-busters"**. A retried load against a URL that fails
(or a cache-buster loop where some loads error) would leak exactly this way, one live page per
failed attempt, with the caller seeing only errors.

**Suggested fix:** a load failure should either (a) destroy the page before returning the error, or
(b) **return the `page_id` alongside the error** so the caller can close it. (b) is strictly better
— a page that reached DNS failure may still be worth navigating elsewhere with `goto`, and it makes
the reply honest about what exists. Either way the current shape — resource created, identifier
withheld — is the one combination that cannot be cleaned up.

---

## Dream — `/fill` needs a target selector, and the reason is secret hygiene, not convenience

**What it would do:** `POST /fill {page_id, entry, field_selector?}` (or `secret_selector` /
`user_selector`) so the caller can name the field the secret goes into.

**Why it matters beyond this one site:** honeypot-defended login forms are a *convention* on Indian
bank portals, not an Example Bank A quirk. Today, the moment `/fill` picks wrong, the only workaround is:

```sh
PW=$(ychrome-vault get "<item>" --field password)
ychrome ctl input page_id=$p events="$(... json with the plaintext ...)"
```

which puts the cleartext secret into **the agent's shell and its argv**. That is strictly worse
hygiene than the verb that exists — `/fill`'s whole value is that the secret goes vault-agent →
eval script and is never seen by the agent. ⇒ **The missing parameter does not just cost round
trips; it forces a downgrade in how the credential is handled**, on exactly the class of site where
that matters most.

**The concrete problem hit because it was missing:** on Example Bank A I could not use `/fill` at all. I
cleared its poisoned decoys by hand, typed the password through argv, and had to invent a
three-field readback gate (`{"pass":14,"h0":0,"h1":0}`) before it was safe to click Login.

**Rank:** 1 of 1 — it is the only feature ask here, and D1's fix (skip non-hittable targets) would
cover most cases without it.

---

## What worked and must not regress

- `ctl open` / `wait until={"load":"finished"}` / `eval` / `input` drove both portals headless from
  dev with no GUI host and no reveal. Both banks' credential steps were completed this way.
- `input`'s `resolved[]` report (`matches`/`hittable`/`ambiguous`/`nth`) is genuinely good and is
  what let me address unlabelled styled-component buttons safely.
- The `type` verb's `before`/`after`/`grew_by`/`want` fields are the honest part — keep them even if
  `landed` is reworked.
- `lore.py`'s refusal-with-a-suggested-rewrite is the right shape for a lock. D3 is about coverage,
  not about the design.
