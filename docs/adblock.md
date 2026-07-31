# Ad blocking, cosmetic filtering and the annoyance scripts

Two planes, one owner each, and a reconciler that keeps a host's copy of both
from quietly rotting.

| plane | owner | what it can do |
|---|---|---|
| WebKit content blocker (`~/.yggterm/web-adblock/rules.json`) | `src/abp.rs` + `src/adblock.rs` | block a request, hide an element by CSS selector, block cookies, exempt a page |
| userscripts (`~/.yggterm/web-userscripts/*.js`) | `src/extensions.rs` + `src/webpolicy.rs` | anything JavaScript can do, on the pages `@match` names |

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
| **the shipped one** | **146,748** | **15.7 s** | **476 MB** |

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

218,312 source lines in. **146,748 rules out.**

| | count |
|---|---|
| network blocks | 122,547 |
| network exceptions (`ignore-previous-rules`) | 2,211 |
| `$important` blocks | 52 |
| document exceptions (`$document`/`$elemhide`) | 597 |
| generic cosmetic selectors | 35,535 (in 18 batched rules) |
| domain-scoped cosmetic selectors | 27,588 (in 20,792 `if-domain` rules) |
| `#@#` un-hides honoured | 545 |
| duplicates collapsed | 191 |
| `$domain=` entries dropped from otherwise-kept filters | 873 |

**Cosmetic selectors went from 8 to 63,123.**

Per-domain `##` rules turned out to be perfectly expressible with `if-domain`,
which is worth stating because the assumption going in was that they were not.

### What could not be translated: 7,706 filters, every one counted

`rules.meta.json` carries the counts with five verbatim samples each, and
`ychrome adblock status` prints it.

| reason | count | why it is impossible, not merely unimplemented |
|---|---|---|
| `scriptlet` | 5,059 | `##+js(...)` needs a surrogate library and per-domain JS injection |
| `procedural-selector` | 238 | the forms beyond `:has-text`/`:style` (see §4) |
| `untranslatable-domain` | 641 | a wildcard, regex or IDN domain where dropping it would widen the filter |
| `request-rewrite` | 438 | `$removeparam`, `$replace`, `$urltransform` — a blocker decides, it never edits |
| `redirect` | 322 | there is **no redirect action** in the content-blocker JSON |
| `unsupported-option` | 248 | `$webrtc`, `$header=`, `$method=`, `$denyallow=`, `$strict3p` … |
| `unsupported-regex` | 218 | `/(a\|b)/`, `{n,m}`, `\w` — emitting one fails the whole compile |
| `html-filtering` | 50 | `##^...` edits the response body |
| `unprovable-selector` | 37 | mostly `##sel {decls}`, a style filter written with `##` |
| `header-injection` | 38 | `$csp=`, `$permissions=` |
| `badfilter` | 2 | upstream asked for these to be disabled; honouring it is not a loss |

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

## 5. Provisioning: no bundled asset may sit dead on a host

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

## 6. The annoyance scripts

### `youtube-adblock` (v1.1.0)

YouTube's ads are first-party, so no URL filter can reach them. The script
deletes `adPlacements`, `adSlots`, `playerAds` and `adBreakHeartbeatParams` out
of the `/youtubei/v1/player` response before the player reads it, hooking
`window.fetch`, `XMLHttpRequest` and a setter on `window.ytInitialPlayerResponse`.

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
console and distinguishes the two failures: `pruned === 0` means the hooks never
bit (renamed fields, or the isolated world), `pruned > 0` means the prune works
and the break came by a route `AD_BEARING_PATHS` does not list.

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

## 7. Commands

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

## 8. What is proven, and what is not

**Proven here.** The ceiling, the regex and selector dialects, the valid trigger
keys, the compile cost, and that the shipped ruleset compiles — all by running
`WebKitUserContentFilterStore` on this host. The reconciler's five verdicts, end
to end over a scratch `$HOME`, including the GUI host's exact header-less
`youtube-adblock` and a hand-copied 60-rule `rules.json`. The YouTube prune,
against a real captured response. The generated cosmetic script, against a stub
DOM. The SponsorBlock hash-prefix endpoint, against the live API.

**Not proven.** None of it has been exercised in the yggterm GUI on the GUI host: no
faithful screenshot of a page with the new ruleset attached, no confirmation
that YouTube ads are gone on the live host, no measurement of the GUI's startup
with a 146,748-rule filter. That needs a deploy, and the deploy needs the
`load`-first change in §1 first.
