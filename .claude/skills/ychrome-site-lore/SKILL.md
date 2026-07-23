---
name: ychrome-site-lore
description: Read BEFORE agent-driven browsing of any non-trivial site through ychrome (login flows, form filling, data extraction, co-browsing a bank / income-tax / ombudsman / cybercrime / PG-portal / Amazon / Flipkart / Meta property). A per-site memory of methods that WORKED and traps that BROKE, logged by model + date, shared across the fleet through git. Check it first so you don't re-derive a flow another agent already solved; append what you learn so the next agent (or the next you) starts ahead. Triggers on: site-lore, known working method, how do I log into, selector broke, co-browse recipe, "does an agent already know this site".
---

# ychrome site-lore

Every site is different, and what it takes to drive one with an agent — the exact
selectors, the login dance, the interstitial that eats the first click, the DOM
that only settles after a scroll — is hard-won and easily lost. This skill is the
shared memory: **known working methods for a particular website, logged by which
model and on what date.** Read a site's lore before you browse it; write back what
you discovered so retrieval is easier next time and the knowledge compounds across
the fleet.

This exists because agent browsing of real sites (banks, income-tax, ombudsman,
cybercrime, PG-portal, Amazon, Flipkart, Meta) is the bottleneck for the
dossierGraph triage runs and for automated trademark filing. Every solved flow
logged here is one less hurdle next time.

## The store (and why it is shaped this way)

- **Source of truth: one Markdown file per domain, `lore/<domain>.md`, committed
  to this repo.** Sharing across the fleet is a plain `git pull`: an agent on one
  host logs a Facebook method, pushes, and the next agent on another host reads it.
  The domain is
  the registrable host with `www.` stripped (`facebook.com`, `incometax.gov.in`).
- **Retrieval: a derived SQLite cache** (`~/.yggterm/ychrome/site-lore.db`,
  rebuildable, gitignored) for fast cross-site queries. It is NEVER the source of
  truth. The fleet syncs files newest-wins; a single committed binary DB would
  silently clobber one host's writes with another's and cannot be diffed or
  reviewed. Markdown-per-site keeps writes local, mergeable, and legible. The user
  asked for "a sqlite file"; this is that idea made fleet-safe — the SQLite is the
  index, the Markdown is the record.

## Automatic recall at launch (you cannot forget this)

A skill an agent must remember to load is a skill an agent forgets. So the recall
also lives in the **tool's own output**: `ychrome <url>` prints this site's lore to
stderr the moment the surface opens (the `── site-lore for <domain> ──` banner),
matching the host with `www.` stripped. If there is no lore yet, it prints the exact
`lore.py log` command to record one. You will see it whether or not you loaded this
skill — reading it before you drive is then automatic, and logging after is a
one-line copy-paste. Override the lookup dir with `YCHROME_SITE_LORE_DIR` (defaults
to this skill dir under `~/gh/ychrome`). The commands below are still how you query
across sites and write entries.

## Use it

```sh
LORE=~/gh/ychrome/.claude/skills/ychrome-site-lore/lore.py
export YCHROME_LORE_MODEL=claude-fable-5     # so log stamps the right model

# BEFORE browsing a site — what does the fleet already know?
python3 "$LORE" get facebook.com
python3 "$LORE" search "otp"                 # methods across every site mentioning otp
python3 "$LORE" list                         # every site, entry counts, last touched

# AFTER you get something working (or find something reliably broken) — log it:
python3 "$LORE" log incometax.gov.in --slug login-pan-otp --status WORKS \
  --task "log in with PAN + OTP" --tags "login,otp" \
  --body "1. ychrome --profile itr https://eportal.incometax.gov.in ...
2. #userId <- PAN; Continue; then #loginOtp after SMS ...
Gotcha: the Continue button is disabled until a blur event fires on #userId —
dispatch it via app web eval, a synthetic .value= alone leaves it disabled."

# Fast structured retrieval (auto-builds the cache on first use):
python3 "$LORE" reindex
python3 "$LORE" query "SELECT domain,slug,status,model,date FROM lore WHERE status='WORKS' ORDER BY date DESC"
```

## What makes a good entry

Log the thing that was NOT obvious and cost you time. An entry earns its place if
a future agent, reading only it, could reproduce the flow without rediscovering
the trap. Aim for:

- **The exact handle** — a CSS selector, an `aria-label`, a URL that skips a
  redirect, the tab order. Names drift; prefer stable attributes over nth-child.
- **The trap** — the interstitial that steals focus, the button disabled until a
  `blur`/`InputEvent` fires, the field that a synthetic `.value=` does not commit
  (Dioxus/React controlled inputs need the prototype setter + `InputEvent`; see
  the yggui skill's trust caveat), the rate-limit, the CAPTCHA point.
- **The proof** — how you confirmed it worked (a faithful `--backend os` pixel, a
  DOM assertion via `app web eval`), and the date, so a reader knows how stale it
  might be. Sites change; a WORKS from six months ago is a hypothesis, not a fact.

Set STATUS honestly: `WORKS` (reproduced end to end), `PARTIAL` (got most of the
way, one step still manual), `BROKEN` (a previously-working method that now
fails — say what changed), `BLOCKED` (needs something an agent may not do, e.g.
a human CAPTCHA, a vault the user must unlock, a passkey user-presence tap).

## The browsing primitives an entry describes

ychrome gives agents unusual control over the page; site-lore is where the
site-specific *use* of these primitives lives. The primitives themselves are in
the yggui and ychrome skills — don't duplicate them here, reference them:

- `yggterm server app web eval '<js>'` — run JS in the ychrome page DOM (read
  state, dispatch events, fill fields). The workhorse.
- `yggterm server app web screenshot` / `app screenshot --backend os` — the
  faithful page pixel (default backends are blind to the native webview).
- `yggterm server app grid show --target surface` — labeled click grid over the
  page for canvas / unfamiliar layouts.
- `ychrome [--profile P] <url>` — open/route a tab; profiles isolate identity
  (one per persona: `meta`, `itr`, a bank). Credentials come from `ychrome-vault`
  and are injected as an `eval` script, never written to lore.

**Never write a secret into lore.** No passwords, OTPs, cookies, or tokens. Log
the *method* to obtain and inject them (which vault item, which selector), not the
values. Lore is committed to git and shared fleet-wide.

## Bootstrapping a site with no lore

`get` on an unknown domain prints a starter hint. First time through a site,
narrate your steps as you go and log the winning path once, rather than logging
every attempt. If you hit a wall that needs the user (CAPTCHA, unlock, passkey tap),
log it `BLOCKED` with exactly what is needed — that is itself valuable: it tells
the next agent to line the human up first instead of burning a run discovering it.
