# Ad blocking, cosmetic filtering and the annoyance scripts

Two planes, one owner each, and a reconciler that keeps a host's copy of both
from quietly rotting.

| plane | owner | what it can do |
|---|---|---|
| WebKit content blocker (`~/.yggterm/web-adblock/rules.json`) | `src/abp.rs` + `src/adblock.rs` | block a request, hide an element by CSS selector, block cookies, exempt a page |
| userscripts (`~/.yggterm/web-userscripts/*.js`) | `src/extensions.rs` + `src/webpolicy.rs` | anything JavaScript can do, on the pages `@match` names |

**The second plane does more than it looks like.** Two of the three things this
converter used to call impossible — procedural cosmetic selectors and
`##+js(...)` scriptlets — were only ever blocked on "per-domain JS injection",
and the userscript plane IS per-domain JS injection. `cosmetic-filters.js` (§4)
and `scriptlets.js` (§5) are both generated from the same `abp::convert` call
that produces the ruleset, and both are scoped by `@match` so WebKit matches
them in the engine.

Everything below that is stated as a number was measured on **WebKitGTK 2.52.5**
(the version on dev and on the GUI host, checked with `pkg-config --modversion
webkit2gtk-4.1`) by compiling probe rulesets through
`WebKitUserContentFilterStore`, and on the **2026-07-31** snapshot of the
upstream filter lists.

## 1. What WebKit's content blocker actually accepts

This section is the reason `src/abp.rs` is shaped the way it is. None of it is
read off documentation.

### The ceiling is 150,000 rules

150,000 compiles. 150,001 fails with **"Too many rules in JSON array."** The
failure mode is not "some rules are missing" — the whole ruleset is rejected and
there is no ad blocking at all. `abp::WEBKIT_RULE_CEILING` is that number, and
the converter trims cosmetic rules (never network ones) to stay under it, and
reports the trim.

### One bad network rule kills everything; one bad selector kills nothing

| ruleset | result |
|---|---|
| 50 good rules + 1 invalid regex | **whole compile fails**: "Invalid or unsupported regular expression." |
| 50 good rules + 1 unknown `resource-type` | **whole compile fails**: "Invalid string in the trigger flags array." |
| 50 good rules + 1 `:has-text()` selector | compiles fine, that rule silently vanishes |
| 50 good rules + 1 non-ASCII `if-domain` | **whole compile fails**: "Domains must be lower case ASCII. Use punycode…" |

So the converter validates every network construct it emits (a false negative
costs one filter; a false positive costs the user all ad blocking), and
validates selectors too, because a silently dropped selector is exactly the
kind of invisible degradation this lane exists to end.

The non-ASCII case is not hypothetical: the first generated ruleset failed to
compile on it.

### The regex dialect

Accepted: `.` `*` `+` `?` `[...]` `(...)` `(?:...)` `$`, a leading `^`, escaped
metacharacters (including `\{`), a plain `/`, and any ASCII literal.

**Rejected:** `|` alternation, `{n,m}`, lookahead `(?=`, `\w`, `\d`, a `^`
anywhere but position 0, an empty `url-filter`, and any non-ASCII character.

The missing alternation has one visible consequence. ABP's `^` separator means
"a separator character **or** the end of the URL", which needs `([...]|$)`. It
is therefore translated to the character class alone
(`[^a-zA-Z0-9_%.-]`), which is strictly **narrower**: a filter ending `/ads^`
misses a URL that ends exactly at `/ads`. It does not affect the overwhelmingly
common `||host^` form at all, because a canonical URL always carries a `/` or a
`:port` after its authority.

### Trigger keys and actions, verified one at a time

Valid `resource-type`: `document`, `image`, `style-sheet`, `script`, `font`,
`raw`, `svg-document`, `media`, `popup`, `ping`, `fetch`, `websocket`, `other`,
`top-document`. **`object` is not valid** whatever older Safari documentation
says.

Valid also: `load-type` (`first-party`/`third-party`), `load-context`
(`top-frame`/`child-frame`), `if-domain`/`unless-domain` (with or without a `*`
prefix), `if-top-url`/`unless-top-url`, `url-filter-is-case-sensitive`.

Actions: `block`, `block-cookies`, `ignore-previous-rules`, `make-https`,
`css-display-none`.

Selectors: `:has()`, `:not()`, `:nth-child()`, attribute selectors and CSS
escapes all work. `:has-text()`, `:-abp-contains()`, `:upward()`, `:xpath()` and
an empty selector do not.

### Cost

| ruleset | rules | compile | peak RSS |
|---|---|---|---|
| the old hand-written one | 60 | 0.15 s | 85 MB |
| synthetic | 50,000 | 0.59 s | 229 MB |
| synthetic | 150,000 | 1.9 s | 463 MB |
| **the shipped one, network only** | 125,407 | 11.3 s | 386 MB |
| **the shipped one, cosmetic only** | 21,341 | 5.2 s | 269 MB |
| **the shipped one** | **146,817** | **14.8 s** | **476 MB** |

One 20,000-selector `css-display-none` rule compiles in 23 ms, which is why
generic cosmetic filters are batched into 18 rules rather than 35,535.

> ### ⚠ The yggterm-side change this ruleset needs
>
> `vendor/dioxus-desktop/src/web_surface.rs` calls
> `webkit_user_content_filter_store_save` on **every GUI process start**, and
> `save` recompiles unconditionally: measured twice into the same store,
> 15.7 s and 16.2 s. `webkit_user_content_filter_store_load` against the
> already-populated store returns the same filter in **0.011 s**, and the
> compiled bytecode is 91 MB on disk.
>
> Load-first, keyed by a content stamp, with `save` only on a miss, turns a
> 16-second window of unfiltered browsing at every launch into eleven
> milliseconds. Until that lands, the comment in `web_surface.rs` — "page loads
> are slower than the compile, so the first navigation is still covered in
> practice" — is no longer true.

## 2. The filter lists

`src/adblock.rs` `LISTS` is the only roster. `ychrome adblock lists` prints it.

| list | why |
|---|---|
| EasyList | the baseline ad list |
| EasyPrivacy | trackers, which EasyList leaves out |
| uBO `filters` / `privacy` / `badware` / `quick-fixes` | uBO's own additions, including breakage fixes that land days early |
| EasyList Cookie (fanboy-cookiemonster) | consent banners |
| uBO `annoyances-cookies` | uBO's cookie-notice list |
| i-dont-care-about-cookies (ABP form) | the upstream IDCAC database |

**AdGuard Base is deliberately excluded.** It is ~151,000 lines, mostly
EasyList's own rules re-hosted, and would alone exceed the 150,000 ceiling —
where the failure is total, not partial. Its genuinely additive content is
AdGuard syntax (`$$`, `$replace`, `#%#//scriptlet`) that a declarative blocker
cannot express anyway.

## 3. What the conversion produced

Measured on the **2026-07-31** snapshot: 218,380 source lines in,
**146,817 rules out**, plus two generated userscripts.

| | count |
|---|---|
| network blocks | 122,614 |
| network exceptions (`ignore-previous-rules`) | 2,212 |
| `$important` blocks | 52 |
| document exceptions (`$document`/`$elemhide`) | 597 |
| generic cosmetic selectors | 35,535 (in 18 batched rules) |
| domain-scoped cosmetic selectors | 27,589 (in 20,793 `if-domain` rules) |
| `#@#` un-hides honoured | 545 |
| procedural cosmetic rules → `cosmetic-filters.js` (§4) | 415 |
| **scriptlet invocations → `scriptlets.js` (§5)** | **3,341** |
| `#@#+js` scriptlet exceptions honoured | 1 |
| duplicates collapsed | 191 |
| `$domain=` entries dropped from otherwise-kept filters | 869 |

**Cosmetic selectors went from 8 to 63,124.**

Per-domain `##` rules turned out to be perfectly expressible with `if-domain`,
which is worth stating because the assumption going in was that they were not.

### What could not be translated: 3,947 filters, every one counted

`rules.meta.json` carries the counts with five verbatim samples each, and
`ychrome adblock status` prints it.

| reason | count | why |
|---|---|---|
| `scriptlet` | 1,718 | a `##+js(...)` naming a scriptlet the runtime does not implement (1,103), or one whose every domain is untranslatable (§5) |
| `untranslatable-domain` | 637 | a wildcard, regex or IDN domain where dropping it would widen the filter |
| `request-rewrite` | 438 | `$removeparam`, `$replace`, `$urltransform` — a blocker decides, it never edits |
| `redirect` | 322 | there is **no redirect action** in the content-blocker JSON |
| `unsupported-option` | 248 | `$webrtc`, `$header=`, `$method=`, `$denyallow=`, `$strict3p` … |
| `procedural-selector` | 238 | the forms beyond `:has-text`/`:style` (see §4) |
| `unsupported-regex` | 218 | `/(a\|b)/`, `{n,m}`, `\w` — emitting one fails the whole compile |
| `html-filtering` | 50 | `##^...` edits the response body |
| `header-injection` | 38 | `$csp=`, `$permissions=` |
| `unprovable-selector` | 37 | mostly `##sel {decls}`, a style filter written with `##` |
| `badfilter` | 2 | upstream asked for these to be disabled; honouring it is not a loss |
| `style-or-snippet` | 1 | `css-display-none` cannot set an arbitrary property |

**The `scriptlet` row was 5,059 until 2026-07-31, and it was wrong about why.**
It read "needs a surrogate library and per-domain JS injection" — but per-domain
JS injection was `cosmetic-filters.js`'s plane the whole time, and the library
is a thing we can write. See §5.

### Order is semantics

WebKit evaluates in array order and `ignore-previous-rules` cancels everything
before it for that load. The sections are therefore:

1. blocks
2. network exceptions (`@@`) — after the blocks, so they can cancel one
3. `$important` blocks — after the exceptions, because `$important` means an
   exception may not cancel this
4. cosmetic — after the subresource exceptions, so `@@||cdn^$script` cannot
   silently switch off a page's element hiding, which is not what it says
5. document exceptions (`$document`, `$elemhide`, `$generichide`) — last,
   because these ones **do** mean the page is exempt from all of it
6. **the bot-check guard — OURS, after every upstream rule** (see below)

### The bot-check guard: two rules that must always be last

A bot check is the last thing on the web that may be blocked by accident,
because the failure has no symptom a user can read: the login page comes back,
and comes back, and comes back. A user hit exactly this shape on a login in July
2026 and filed a ticket with Cloudflare rather than with us.

`abp::bot_check_guard_rules` appends two `ignore-previous-rules` entries at the
very end of every generated ruleset:

| path | what it is |
|---|---|
| `/cdn-cgi/challenge-platform/` | the first-party orchestrator, JS detector and its XHRs, served from the challenged site's OWN origin |
| `challenges.cloudflare.com` | Turnstile's script and its iframe document |

Three properties, all locked by
`a_bot_check_is_allowlisted_last_and_survives_the_ceiling`:

- **present** — a list refresh cannot lose it;
- **last** — `ignore-previous-rules` cancels only what precedes it, so a guard in
  the middle protects nothing after it;
- **ceiling-proof** — the trim runs against `WEBKIT_RULE_CEILING - guard.len()`,
  so a ruleset that arrives at 150,000 can never be the one that drops it.

Deliberately NOT `/cdn-cgi/` wholesale: `/cdn-cgi/rum`, `/cdn-cgi/beacon/` and
`/cdn-cgi/zaraz/` are analytics on the same prefix and stay blocked.

**Audit of the shipped ruleset, 2026-07-31 (146,817 rules):** nothing blocked a
challenge then either. Of the 79 rules mentioning cloudflare or `cdn-cgi`, the
four touching a challenge were already upstream's own `ignore-previous-rules`,
and every blocking one was analytics on a different path. So the guard is a lock
against the next list update, not a fix for a live break — and the committed
`rules.json.gz` predates it, so it arrives at the next `ychrome adblock update`.

## 4. Cosmetic filtering's other half: `cosmetic-filters.js`

646 procedural rules in the corpus, all domain-scoped, over 804 domains:
`:has-text(` 422, `:style(` 159, `:upward(` 31, `:xpath(` 20, `:remove(` 16,
`:matches-css` 16, and a tail of 12.

The first two are 90% of it and both are cheap and exact, so 414 of them
generate `assets/web-userscripts/cosmetic-filters.js` — **1,122 rules over 704
domains, 102 KB** — from the same `abp::convert` call that produces the ruleset.
The remaining 238 are counted.

`:style()` is load-bearing, not decorative: `css-display-none` can only hide,
and `body.didomi-popup-open:style(overflow: auto !important;)` — one rule, 205
domains — is the cookie-banner **scroll unlock**. Hiding a banner without it
leaves a page that cannot be read.

**The performance contract.** It reuses the userscript plane rather than
inventing an injection path, so 704 `@match` lines scope it to exactly the
domains with rules and WebKit matches in the engine: on any other page the
script does not exist. The runtime holds the same contract itself for a GUI too
old to apply `@match`, and its MutationObserver is coalesced to one pass per
animation frame.

**Nesting is refused, not guessed at.** `a:has(p:has-text(Sponsored))` means
"an `<a>` containing a `<p>` that says Sponsored"; splitting on the marker would
leave `a:has(p` and hiding that would hide every such link on the page. Only a
procedural pseudo-class in the last position with a plain-CSS prefix is
accepted.

The script is installed alongside the ruleset (it is the ruleset's other half,
not an extension the user picked) on the launches where the ruleset itself is
written, and is **not** reinstated on every launch — which is what keeps the
pane's Delete button honest.

## 5. The scriptlet plane: `##+js(...)` → `scriptlets.js`

`##+js(name, args...)` asks a blocker to run a named piece of JavaScript on a
page. It is the single biggest category the content blocker cannot express —
**5,057 filters**, more than every other untranslatable category put together —
and until 2026-07-31 all of them were counted and dropped, with the reason
"needs a surrogate library and per-domain JS injection".

Half of that reason was wrong. **Per-domain JS injection is the userscript
plane, which `cosmetic-filters.js` had been using for months.** The other half
was real and is now `assets/web-scriptlets/runtime.js`.

### The shape

`abp::convert` parses `##+js(...)` into `ScriptletRule { name, args }`, resolves
every alias to one canonical name, and keys it by domain — the same walk, the
same domain translation and the same "no domain means refused" rule the
procedural cosmetic path uses. `abp::generate_scriptlet_script` renders that into
a userscript with one `@match` per domain and the runtime spliced in.

**The world is `main`, and that is the load-bearing line in the file.** A
scriptlet's entire job is to edit page globals — `window.open`, `JSON.parse`, a
property the page's own script reads. In an isolated world every one of those
edits is invisible to the page, so the script would run, report success, and
change nothing. That is not hypothetical: it is exactly how `youtube-adblock`
shipped broken (§6), and `extensions.rs::the_scriptlet_script_runs_in_the_main_world`
exists so it cannot recur here.

**Rules are interned.** One upstream filter names thousands of domains — a single
`set-cookie` line rides on 2,191 of them — so a domain-keyed table of whole
invocations repeated 8,736 rows over 2,428 distinct rules and cost 845 KB.
`TABLE` holds each rule once and `RULES` maps a domain to indices into it.

### What it covers, counted

3,954 of the 5,057 scriptlet filters (**78.2%**) name something the runtime
implements; 3,341 actually route, the difference being filters whose every
domain is a wildcard or IDN the ruleset already refuses. Ordered by what they
unlock:

| scriptlet (canonical) | filters | aliases the lists use |
|---|---|---|
| `set-cookie` | 1,070 | `trusted-set-cookie` |
| `abort-on-property-read` | 637 | `aopr` |
| `abort-current-script` | 363 | `acs`, `abort-current-inline-script` |
| `set-constant` | 345 | `set` |
| `set-local-storage-item` | 237 | `trusted-set-local-storage-item` |
| `no-setTimeout-if` | 211 | `nostif`, `prevent-setTimeout` |
| `addEventListener-defuser` | 205 | `aeld`, `prevent-addEventListener` |
| `abort-on-property-write` | 183 | `aopw` |
| `no-window-open-if` | 183 | `nowoif`, `prevent-window-open` |
| `remove-cookie` | 67 | `cookie-remover` |
| `href-sanitizer` | 62 | |
| `adjust-setInterval` | 51 | `nano-sib` |
| `remove-node-text` | 49 | `rmnt` |
| `nowebrtc` | 47 | |
| `no-xhr-if` | 46 | `prevent-xhr` |
| `no-fetch-if` | 40 | `prevent-fetch` |
| `adjust-setTimeout` | 32 | `nano-stb` |
| `remove-attr` | 31 | `ra` |
| `noeval-if` | 30 | `noeval` |
| `no-setInterval-if` | 28 | `nosiif` |
| `json-prune` | 27 | |
| `set-session-storage-item` | 9 | |
| `remove-class` | 1 | `rc` |

### What it does NOT cover, and why

1,103 filters name something else. **886 of them are one scriptlet:
`trusted-click-element`**, which clicks page elements a filter names — overwhelmingly
consent dialogs. It is deliberately not implemented. `idcac.js` holds a hard rule,
enforced by `extensions.rs::idcac_clicks_nothing_that_consents` in six languages,
that this browser never clicks a button that CONSENTS to anything; a generic
filter-driven auto-clicker would drive a coach and horses through that rule with
data nobody in this repo reviewed. Implementing it needs the consent guard
extended to cover it first, and that is a decision, not a chore.

The rest is a long tail: `aost` (abort on stack trace, 34), `rpnt` (23),
`popads-dummy` (18), `nobab`/`nofab` (25), and 26 more names under 20 filters
each.

### ⚠ A pattern argument is not a literal, and getting that wrong is silent

Found on the live host on 2026-07-31, after the plane was already passing its
tests: `soundcloud.com##+js(set-local-storage-item, /sc_tracking_anonymous_id|statsig/, $remove$)`
reported success and removed nothing, because the key was used verbatim and
`removeItem('/sc_.../')` removes a key nobody ever wrote. Storage keys can be
regular expressions and must be matched against `store.key(i)`. The harness now
carries a regex-key case with a `length`/`key(i)` stub, because a stub without
them lets a literal-only implementation pass.

### Proof

`tests/fixtures/scriptlets-harness.js` cuts the runtime out of the **shipped
generated body** (between two markers the generator emits) and drives it with one
synthetic rule per implemented scriptlet — 36 checks — plus a no-rules host that
must get no state object and no replaced global at all. `abp.rs`'s own tests
cover alias resolution, `\,` escaping, `#@#+js` exceptions removing exactly one
rule, interning, and determinism.

`extensions.rs::the_scriptlet_table_and_the_runtime_are_one_contract` is the
important one: `abp::SCRIPTLETS` decides what gets ROUTED and `runtime.js`
decides what can RUN, and a name in one and not the other would report thousands
of filters as supported and then ignore them.

### Licensing

**The implementations are ours, written from the documented behaviour of the
filter syntax.** uBlock Origin is GPLv3 and this project ships Apache, so no uBO
scriptlet body, surrogate or resource was read into this repo. A scriptlet's
name, its argument grammar and what it observably does are facts about a filter
format; an implementation is not. If a future behaviour genuinely cannot be
reproduced without transcribing someone's code, **stop and ask** rather than
deciding the licensing question in a commit.

**The filter LISTS are a separate question and this document did not answer it
until now.** EasyList and EasyPrivacy are dual-licensed GPLv3 / CC BY-SA 3.0;
uBlock Origin's own lists (`filters`, `privacy`, `badware`, `quick-fixes`,
`annoyances-cookies`) are GPLv3; fanboy-cookiemonster follows EasyList's terms;
i-dont-care-about-cookies is GPLv3. All of them carry attribution obligations,
and `rules.json.gz` is a derived work of all nine. **Nothing in this repo
currently reproduces those notices** — not the ruleset, not the sidecar, not
`ychrome adblock lists`. That is an open compliance gap, not a solved one, and it
needs settling before any public release: at minimum a `NOTICE`-style file
naming each list, its licence and its source URL, and ideally the same text in
`ychrome adblock lists`.

## 6. Provisioning: no bundled asset may sit dead on a host

`src/provision.rs` is the one owner of "is this host's copy current?".

Two bugs made it necessary, and both were silent. The `youtube-adblock.js` on
the GUI host predated the script's own metadata block, so it parsed to the documented
defaults, so it ran in the **isolated** world, where its `window.fetch` and
`ytInitialPlayerResponse` patches are invisible to the page. Only the DOM
fallback ran — which used to force `playbackRate = 16`, clamped by WebKit to
about 2x. The user saw every ad, at double speed. Separately, the adblock
ruleset had to be copied by hand; it was on the GUI host and had never been on dev,
where ad blocking was therefore off with nothing anywhere saying so.

Every bundled asset now declares a version — `@version` in a userscript's
metadata block, `ruleset_version` in `rules.meta.json`. A content hash cannot
tell "an old release of ours" from "the user's edit"; a declared version can.

| installed | verdict | what happens |
|---|---|---|
| absent | `Absent` | install |
| older, **or unversioned** | `Superseded` | replace, keeping `<name>.superseded` |
| same version, same bytes | `Current` | nothing, but RECORD the delivery |
| same version, other bytes, **is what we last wrote** | `Stale` | replace, keeping `<name>.superseded` |
| same version, other bytes | `Forked` | **keep it**, and say so in the pane |
| newer than the bundle | `Ahead` | **keep it**, and say so |

### ⛔⛔ GENERATING IS NOT PLACING — one conversion, two directories

`ychrome adblock update` writes **four** files from one conversion: `rules.json`,
its sidecar, and the two companion userscripts. They land in ONE directory
because two owners of "what the filter lists say" is the divergence this repo
forbids — and that directory is `web-adblock/`.

⛔ **But `webpolicy` injects userscripts from `web-userscripts/`, and only from
there.** So for as long as the placement step was missing, every `adblock
update` refreshed the network ruleset, reported success, and left the cosmetic
filters and the scriptlets exactly as stale as it found them. The visible half
really had been updated, which is why nothing looked wrong.

⇒ `update` now places both companions through `provision::place_generated_companion`,
which runs the SAME verdict the reconciler does — against the generated body
instead of the bundled one. It is not a copy: a script the user deleted stays
deleted, and a script they edited is kept and said so. A regenerated body is
newer than the bundle by construction, so an ordinary host reads `Superseded`
and moves.

⚠ `--out` skips the placement: that spelling regenerates the committed baseline
into a scratch directory and must not reach into the operator's live profile.

### ⛔⛔ A GENERATED ASSET CAN DRIFT FROM ITS OWN GENERATOR

The reconciler compares the HOST's copy against the BUNDLED asset. **Nothing
compared the bundled asset against the CODE that emits it**, and on 2026-08-21
they had diverged: `generate_cosmetic_script` had gained a DOM-published state
attribute — the fix for "this script reports into a world no probe can read" —
weeks before, and the checked-in asset did not have it. Every host was injecting
an undiagnosable script while the repo contained the fix.

⇒ `the_committed_cosmetic_script_is_not_older_than_its_generator` closes the
seam: generating from an EMPTY rule set yields the pure template, and every
template line must appear in the committed asset. When it fails, regenerate
(below) — never hand-edit an asset that says DO NOT EDIT.

### ⭐ THE FOUR SCRIPTS AND HOW TO READ EACH ONE

They publish their state four different ways, and an agent should not have to
read four scripts to learn that. `@world` decides what a page-world `ctl eval`
can see:

| script | `@world` | published as | read it with |
|---|---|---|---|
| `youtube-adblock.js` | `main` | `window.__yga_state` | `ctl eval js='JSON.stringify(window.__yga_state)'` |
| `scriptlets.js` | `main` | `window.__yggScriptlets` | `ctl eval js='typeof window.__yggScriptlets'` |
| `cosmetic-filters.js` | **isolated** | `data-ycf` **on `<html>`** | `ctl eval js='document.documentElement.getAttribute("data-ycf")'` |
| `sponsorblock.js` | **isolated** | `data-ysb` **on `<html>`** | `ctl eval js='document.documentElement.getAttribute("data-ysb")'` |

⛔ **An isolated-world global is invisible to a page-world `eval`.** Reading
`window.__yggCosmeticState` or `window.__ysb` returns `undefined` on a perfectly
healthy script. That misread has cost two investigations. The two isolated
scripts publish to the DOM because it is the one thing both worlds share.

⚠ **`rules: 0` is a RESULT, not an absence.** The cosmetic script publishes
before its early return, so a host with no rules for it reads `rules:0,passes:0`
— *it ran and had nothing to do*. A script that never loaded has no attribute at
all, and the two readings must never collapse into one.

### ⛔ The declared version was not enough: the delivery ledger

A version only decides this if **a version identifies a body**, and twice it did
not. The generated assets stamp `@version` from the wall clock at generation
time, so a same-day regeneration ships different bytes under an identical stamp;
and a hand-edited asset kept its hand-written stamp, so an edit that forgot the
bump did the same with no regeneration involved. Both landed in the last arm —
version equal, bytes differ — and returned `Forked`, which does not write.

That is worse than failing to update. `Forked` is the one verdict that reads as
*a deliberate user choice*, so a human sees it and leaves it alone: the asset
becomes **undeployable forever while reporting as the user's own edit**. It has
happened to this repo's own shipped work (`sponsorblock.js` at an unchanged
`2.0.0`, 1,886 bytes different, unable to reach any host).

A content hash still cannot break the tie. **A hash of what the provisioner
itself wrote can**, because it records our own act instead of inferring about the
file. Every write, and every `Current` verdict, appends `<id> <sha256>` to a
`.delivered` ledger in the asset's own directory — a dotfile, not a `.js`, for
the same reason `.deleted` is. The ambiguous arm then splits: bytes differ and
match what we last wrote ⇒ ours, superseded ⇒ **write**; bytes differ and do not
⇒ a real edit ⇒ **keep**.

⭐ **`Current` records too, and that is what protects a host that is already in
sync** — the common case, and the one the trap springs on next. `Current` means
the installed body is byte-for-byte the bundled body, so recording it is not an
inference; it costs no write to the asset and arms the host immediately.

⚠ **A host already stuck at `Forked` cannot be rescued by this.** With nothing
recorded and the bytes already diverged, `Forked` stays the only honest answer;
unsticking one is still a version bump or a removal on that host.

Unversioned reads as older than every release, because every body ychrome has
shipped declares one — so the only unstamped bodies predate the stamp, which is
exactly the population needing a heal. `rules.meta.json` carries an FNV-1a of
the ruleset it describes, so a sidecar that has drifted (or is missing, which is
every hand-copied ruleset) reads as unversioned too.

Every decision prints to stderr. `Forked` and `Ahead` also reach the settings
pane as a row note. `ychrome provision --json` runs the same call the browser
makes at launch, so what it prints is what a launch does.

**The backstop.** A body carrying one of ychrome's own stems but declaring **no
metadata block** — meaning the heal could not run — is refused rather than
injected with the wrong placement, through `webpolicy::promote_or_refuse`, the
gate that already existed. Injecting `youtube-adblock` into the isolated world
is strictly worse than not injecting it.

## 7. The annoyance scripts

### `youtube-adblock` (v1.2.0)

YouTube's ads are first-party, so no URL filter can reach them. The script
deletes `adPlacements`, `adSlots`, `playerAds` and `adBreakHeartbeatParams` out
of the player response before the player reads it.

#### ⚠ v1.1.0 hooked the wrong things, and the user could see it

The user's report was "youtube still shows ads", with the script demonstrably
running: `__yga_loaded` true, `window.fetch` patched, `adPlacements` gone from
`ytInitialPlayerResponse`. Measured in the ychrome engine on 2026-07-31, one
cold `www.youtube.com/watch` load, every entry point instrumented:

| entry point | player responses carrying ad fields |
|---|---|
| `window.fetch` → `/youtubei/v1/player` | **0.** The one call it saw returned 328 bytes: `{"adPlacements":true,"playerAds":true,…}`, a field probe, not a video's answer |
| `JSON.parse(<60–260 KB string>)` | **30**, all four ad fields intact, all from `kevlar_base_module` |
| `Response.prototype.json()` | **2**, same fields, on Responses whose `.url` is the **empty string** — built in JS by the page, so no URL hook can ever see them |

The differential, on the SECOND video of a session (an SPA navigation, which is
the case the initial-response hook cannot reach at all):

```
v1.1.0   player.getPlayerResponse()  ->  ["playerAds","adPlacements","adBreakHeartbeatParams"]
v1.2.0   player.getPlayerResponse()  ->  []
```

with `hooks: {fetch:1, xhr:0, inline:1, json_parse:37, response_json:2}`. The
server had scheduled **17 ad placements** for that video — a pre-roll, five
mid-rolls, a post-roll, a `linearAdSequenceRenderer` and a companion ad — so
there was something real to remove.

So `fetch`, `XMLHttpRequest` and the inline `ytInitialPlayerResponse` are three
ways bytes ARRIVE, and the player reads its answer somewhere else entirely.
v1.2.0 adds the two PARSE funnels — `JSON.parse` and `Response.prototype.json` —
which every route has to pass through whatever carried the bytes. (This is the
same thing `json-prune` does, §5; YouTube is the site that needed it first.)

**The trap inside the fix.** `pruneText` decides whether to rewrite a body by
parsing it and seeing whether anything was removed. Parsed with the HOOKED
`JSON.parse`, the parse hook cleans the object first, the rewrite finds nothing
left to remove, and the ORIGINAL TEXT — ad fields and all — goes back to any
caller that reads the body as text. Two working layers, cancelling out, in
silence. It parses with a `nativeParse` captured before hooking, and the harness
reads the raw response text (never the hooked parser) so the cancellation is
visible: mutating that one line turns the lock red.

**The parse hooks sit BELOW the host guard**, and there is a test for it.
`JSON.parse` runs thousands of times on every page in the browser; installed
above the guard, this blocker would be a tax on the whole web.

**AD_FIELDS is verified against reality.** Four watch pages captured on
2026-07-31: three of four carry `adPlacements` at the top level, all four carry
`adBreakHeartbeatParams`. One capture is now
`tests/fixtures/youtube-player-response-captured.json` (scrubbed of tokens and
URLs, key structure untouched) and the node harness prunes it, checking that
every ad field is gone and that `streamingData`, `videoDetails`,
`playabilityStatus`, `captions`, `playerConfig`, `microformat`, `annotations`
and `storyboards` all survive.

**The forced playback rate is gone.** WebKit clamps it to about 2x, so it never
skipped an ad — it made the user watch every one at double speed while
disguising a dead layer 1. An ad reaching the player now warns once on the
console, and `state.hooks` says WHICH funnel bit, which separates the three
failures worth chasing: nothing pruned at all (renamed fields, or the isolated
world), the network hooks firing while the parse funnels stay at zero (a new
transport — measure which call the player parses through), or everything firing
and an ad still arriving (a field `AD_FIELDS` does not name).

### `idcac` (v1.1.0) — the honest answer to "is it feature complete?"

**No, and it never could be — but the gap is now closed from the other side.**

The three consent lists in the ruleset name **27,859 distinct domains**. This
script hard-codes 36 container selectors and knows zero domains. All 19 of its
original selectors are already in those lists, verbatim. The hiding job now
belongs to the ruleset, per-domain, maintained by people who do it full time.

What a declarative blocker cannot do, and this exists for:

1. **Press "reject all."** Hiding a banner leaves it unanswered, and an
   unanswered banner is asked again on the next page and the next site.
   Rejecting ends it.
2. **Undo the scroll lock** — handled by `cosmetic-filters.js`'s `:style()`
   support for the domains upstream knows, and by this script's
   `unlockScrolling()` for the ones it does not.

`REJECT_TEXT` therefore went from 19 phrases to 53 across six languages; it is
the one thing here with no upstream substitute. It never clicks a consent
button, and `extensions.rs::idcac_clicks_nothing_that_consents` enforces that in
six languages too.

**What it will not handle:** a banner that is neither in the ruleset nor one of
the 36 shapes it knows; a consent dialog inside a cross-origin iframe; a
per-purpose preference form.

### `sponsorblock` (v2.0.0)

Asks by **hash prefix**, never by video id: `SHA-256(videoID)` truncated to four
hex characters, which returns every video sharing that prefix and the match
happens in the browser. Asking `?videoID=<id>` would tell sponsor.ajay.app
exactly what you are watching, every time. There is deliberately no fallback to
the by-id endpoint: without `crypto.subtle` it makes no request at all.

#### Why v1 looked broken, measured rather than guessed

v1 was **not** failing to run. Driven in the ychrome engine on dev against
`youtube.com/watch?v=vax8FCuQUsE`, it injected, hashed, fetched, matched the
video out of the prefix answer, and seeked: `currentTime` went **157.88 →
208.28**, exactly the segment's end. It then followed a real SPA autoplay
navigation to the next video and re-fetched.

What it did wrong was **ask for almost nothing**. The API defaults to
`categories=["sponsor"]` and `actionTypes=["skip"]`, and v1 sent
`categories=["sponsor","selfpromo","interaction"]` with no `actionTypes` at all.
So eight of the eleven categories, and every `mute`/`full`/`poi`/`chapter`
segment, were invisible.

Measured against the live API over **881 videos that have community segments**
(six 4-char prefixes, all categories, all action types):

| | |
|---|---|
| videos where v1's three categories had no segment at all | **429 = 48.7%** |
| most-submitted category in the sample | **`intro`** (53 of 210 segments), which v1 never asked for |
| response size, default query vs all categories + all action types | 9.0 KB → 48.1 KB per prefix |

Half the time, on a video that SponsorBlock users would expect to be handled,
v1 correctly did nothing. That is what "SponsorBlock is not working" looked
like.

#### What v2 does

Eleven categories with **per-category behaviour**, set from the settings pane:

| category | ychrome default | why |
|---|---|---|
| `sponsor`, `selfpromo`, `interaction` | **auto-skip** | what v1 already did; no user loses behaviour |
| `intro`, `outro`, `preview`, `music_offtopic` | **skip button** | content some people want; a button adds an affordance, an automatic seek takes one away |
| `filler` | **off** | highly subjective |
| `poi_highlight` | **show** | a jump target, never a skip |
| `exclusive_access` | **show** | a whole-video label |
| `chapter` | **show** | named regions on the seek bar |

Every one is settable to auto-skip / skip button / mute / off (label categories
offer show / off). Upstream ships only `sponsor` auto-skipping and asks during
onboarding; ychrome has no onboarding, so it inherits its own previous
behaviour and offers the rest. `src/sponsorblock.rs` owns the catalogue.

Beyond the category set:

- **The skip is scheduled to the moment it is due**, not waited for on the next
  `timeupdate` (which fires ~4 Hz, so up to a quarter-second of sponsor). The
  `timeupdate` handler stays as the safety net.
- **A skip notice with Undo.** Undo seeks back and marks that segment ignored
  for the rest of the page's life. Undo is the *only* thing that disables a
  segment: seeking into one re-skips it, as it does upstream.
- **A manual skip button** while a `manual` segment is playing, and a **Jump to
  the highlight** button before a `poi_highlight`.
- **Seek-bar markers** in the extension's own colours, with the segment's
  description as the tooltip. Chapters are markers, never skips.
- **`mute` segments are muted** and the user's prior mute state restored after.
  A `full` segment is a notice and can never become a seek.
- **Duration matching** (±2 s) and **locked-segment precedence**: a segment
  submitted against a different cut of the video is discarded, and where the
  community has locked a category the unlocked submissions for it are dropped.
- **It binds to `#movie_player`'s video**, not `document.querySelector('video')`.
  YouTube spawns a `<video>` for every thumbnail you hover, and v1's
  capture-phase `timeupdate` listener on `document` heard all of them.
- **Nothing happens while an ad is showing** (`#movie_player.ad-showing`).
- **A bounded in-memory session cache** so the back button and a page reload do
  not re-ask. Not persisted: community segments get downvoted and retracted, and
  a cache on disk turns that into a TTL knob. The licence permits either (see
  `THIRD-PARTY-NOTICES.md`); this is a correctness choice.

#### Settings, and how they reach the page

`src/sponsorblock.rs` is the single owner of the catalogue, the defaults and the
stored choices, which live in
`~/.yggterm/web-userscripts/sponsorblock.config.json`. `webpolicy::policy()`
renders them into a **synthetic userscript** — a bare `window.__ysbConfig = {…}`
in the same isolated world, injected ahead of `sponsorblock.js`.

It is not a file, deliberately. Splicing settings into `sponsorblock.js` would
make the host's copy diverge from the bundled asset, which is exactly the state
`provision` reads as "the user edited this, leave it alone" — and the next
release would then never update the script. `policy_version` stamps the
*decisions*, so a settings click reaches a running surface.

#### Probing it

⚠ **An isolated-world global is invisible to a page-world `eval`.** Reading
`window.__ysb` through `ychrome ctl eval` returns `undefined` on a perfectly
healthy script; that misread has already cost one investigation. The script
publishes its state to the DOM instead, which both worlds share:

```sh
ychrome ctl eval page_id=<id> js='document.documentElement.getAttribute("data-ysb")'
```

#### Not built, named rather than implied

**Segment submission and voting.** They need a persistent pseudonymous user id
and they WRITE to a shared public database. If they are added they must be
explicit, off by default, and the privacy consequence stated where the user
turns them on. Also absent: the unsubmitted-segment queue, and renaming
YouTube's own chapter list (community chapters are drawn as markers beside it,
not merged into it).

## 8. Commands

```sh
ychrome provision [--json]     # the reconcile the browser runs at launch
ychrome adblock status         # what ruleset is installed, its provenance, its report
ychrome adblock lists          # the roster, with the reason each list is in it
ychrome adblock update         # fetch the lists and regenerate (needs curl)
ychrome adblock update --from-dir <dir> --out <dir>   # offline, reproducible
```

### Regenerating the committed baseline

```sh
mkdir -p /tmp/lists && cd /tmp/lists
# one <name>.txt per entry in `ychrome adblock lists`
ychrome adblock update --from-dir /tmp/lists --out /tmp/gen
gzip -9 -c /tmp/gen/rules.json > assets/web-adblock/rules.json.gz
cp /tmp/gen/rules.meta.json     assets/web-adblock/rules.meta.json
cp /tmp/gen/cosmetic-filters.js assets/web-userscripts/cosmetic-filters.js
cp /tmp/gen/scriptlets.js       assets/web-userscripts/scriptlets.js   # BOTH companions
cargo test
```

⚠ **Copy BOTH companion scripts.** They are one conversion's output; shipping a
regenerated cosmetic script beside a stale scriptlet script splits the pair that
`build_into` writes together for exactly that reason.

The ruleset is committed **gzipped** (19 MB → 1.9 MB): the raw form would add
19 MB to a repository whose entire history is about 4 MB, on every regeneration.

### Verifying a ruleset really compiles

The converter's own gates cannot prove WebKit agrees. Compile it:

```python
import gi, json, time
gi.require_version('WebKit2', '4.1')
from gi.repository import GLib, WebKit2
store = WebKit2.UserContentFilterStore.new('/tmp/wkstore')
loop, start = GLib.MainLoop(), time.monotonic()
def done(source, res, _):
    try:
        source.save_finish(res); print('ok', round(time.monotonic() - start, 2), 's')
    except GLib.Error as err:
        print('FAILED:', err.message)
    loop.quit()
store.save('probe', GLib.Bytes.new(open('rules.json','rb').read()), None, done, None)
loop.run()
```

Do this after **any** change to `abp.rs`'s emission. One bad rule is not one
missing filter, it is no ad blocking at all.

## 9. What is proven, and what is not

**Proven here.** The ceiling, the regex and selector dialects, the valid trigger
keys, the compile cost, and that the shipped ruleset compiles — all by running
`WebKitUserContentFilterStore` on this host (the 2026-07-31 regeneration
compiles in 14.8 s). The reconciler's six verdicts, end to end over a scratch
`$HOME`, including the GUI host's exact header-less `youtube-adblock` and a hand-copied
60-rule `rules.json`. The YouTube prune, against a real captured response. The
generated cosmetic script, against a stub DOM. The SponsorBlock hash-prefix
endpoint, against the live API.

**Proven on a REAL PAGE, in the ychrome engine on dev (2026-07-31).**

- The `youtube-adblock` transport measurement above, and the v1.1.0 → v1.2.0
  differential on `player.getPlayerResponse()` after an SPA navigation. Two
  pages, same daemon, same video, one variable.
- The scriptlet plane: `techcrunch.com` → `{applied:3, failed:0, by:{set-constant:2,
  abort-current-script:1}}` with `navigator.globalPrivacyControl === false` on
  the page, which is what its filters ask for; `soundcloud.com` → all five of
  its rules installed, `failed:0`; `example.com` → `window.__yggScriptlets`
  **undefined**, which is the performance contract holding on a real page.

**Not proven.** Nothing has been exercised in the yggterm GUI on the GUI host: no
faithful screenshot of a page with the new ruleset attached, no measurement of
the GUI's startup with a 146,817-rule filter. That needs a deploy, and the
deploy needs the `load`-first change in §1 first.

**Not proven, and worth saying precisely:** no ad was observed PLAYING and then
observed stopping. On a logged-out headless engine this video served an ad
schedule but never rolled a break in ~20 s of playback, with or without the
blocker, so "the ad stopped" was not an available observation. What was observed
is that the player's own copy of its answer went from carrying a 17-placement ad
schedule to carrying none. Server-stitched ads (SSAP), where the ad is spliced
into the video stream itself, would not be reachable by response pruning at all
— that case was not encountered here and remains untested.
