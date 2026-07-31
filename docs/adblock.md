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
(the version on dev and on guihost, checked with `pkg-config --modversion
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
guihost predated the script's own metadata block, so it parsed to the documented
defaults, so it ran in the **isolated** world, where its `window.fetch` and
`ytInitialPlayerResponse` patches are invisible to the page. Only the DOM
fallback ran — which used to force `playbackRate = 16`, clamped by WebKit to
about 2x. The user saw every ad, at double speed. Separately, the adblock
ruleset had to be copied by hand; it was on guihost and had never been on dev,
where ad blocking was therefore off with nothing anywhere saying so.

Every bundled asset now declares a version — `@version` in a userscript's
metadata block, `ruleset_version` in `rules.meta.json`. A content hash cannot
tell "an old release of ours" from "the user's edit"; a declared version can.

| installed | verdict | what happens |
|---|---|---|
| absent | `Absent` | install |
| older, **or unversioned** | `Superseded` | replace, keeping `<name>.superseded` |
| same version, same bytes | `Current` | nothing |
| same version, other bytes | `Forked` | **keep it**, and say so in the pane |
| newer than the bundle | `Ahead` | **keep it**, and say so |

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

### `sponsorblock` (v1.1.0)

Auto-skips sponsor/selfpromo/interaction segments. It asks by **hash prefix**,
never by video id: `SHA-256(videoID)` truncated to four hex characters, which
returns every video sharing that prefix (77 rows in 27 KB for a sample prefix,
all 77 verified to really hash to it) and the match happens in the browser.
Asking `?videoID=<id>` would tell sponsor.ajay.app exactly what you are
watching, every time.

There is deliberately no fallback to the by-id endpoint: without
`crypto.subtle` it makes no request at all.

It honours `actionType` (only `skip` means seek past; a `mute` or `full` segment
is left alone) and ignores segments with `votes < -1`.

**Not built, and named rather than implied:** segment submission, voting,
chapter names, highlight jump, the unsubmitted-segment queue, mute-action
support, and a per-category configuration UI.

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
cargo test
```

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
compiles in 14.8 s). The reconciler's five verdicts, end to end over a scratch
`$HOME`, including guihost's exact header-less `youtube-adblock` and a hand-copied
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

**Not proven.** Nothing has been exercised in the yggterm GUI on guihost: no
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
