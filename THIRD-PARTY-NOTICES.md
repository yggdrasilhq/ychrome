# Third-party notices

This file is the attribution that travels with any redistribution of this
repository, or of a binary built from it. It describes **what this repository
actually contains** — claims about files that are not here have been removed
rather than carried over.

## Filter lists — the committed ruleset is a derivative work

`assets/web-adblock/rules.json.gz` and `assets/web-userscripts/cosmetic-filters.js`
are **generated artefacts**. They are compiled by `src/abp.rs` (ABP/uBO filter
syntax to WebKit content-blocker JSON) under the policy in `src/adblock.rs`, and
they are committed so a fresh host has real blocking before it has ever been
online. `src/adblock.rs` embeds the ruleset into the binary with
`include_bytes!`, so a built binary carries the derivative too.

They are **derivative works of the upstream filter lists below**, and this
notice is the attribution owed for them.

None of these lists is vendored as source in this repository; the generated
ruleset is the derivative. The generator, its drop accounting, and the measured
engine limits are documented in `docs/adblock.md`. The exact source set and line
counts for the committed build are recorded in
`assets/web-adblock/rules.meta.json` under `sources`.

| List (`sources` name) | Lines | Upstream | Licence as published upstream |
|---|---|---|---|
| `easylist` | 84,946 | https://easylist.to/ | GPL-3.0 **or** CC BY-SA 3.0 |
| `easyprivacy` | 56,423 | https://easylist.to/ | GPL-3.0 **or** CC BY-SA 3.0 |
| `easylist-cookie` (Fanboy's Cookiemonster) | 25,838 | https://easylist.to/ | follows EasyList |
| `idcac` | 24,339 | https://www.i-dont-care-about-cookies.eu/ | GPL-3.0 |
| `ubo-filters` | 10,937 | https://github.com/uBlockOrigin/uAssets | GPL-3.0 |
| `ubo-badware` | 6,373 | https://github.com/uBlockOrigin/uAssets | GPL-3.0 |
| `ubo-annoyances-cookies` | 5,707 | https://github.com/uBlockOrigin/uAssets | GPL-3.0 |
| `ubo-privacy` | 3,224 | https://github.com/uBlockOrigin/uAssets | GPL-3.0 |
| `ubo-quick-fixes` | 525 | https://github.com/uBlockOrigin/uAssets | GPL-3.0 |

⚠ **EasyList and EasyPrivacy are dual-licensed.** This project takes them under
**GPL-3.0**, which is why the browser is GPL-3.0-or-later: CC BY-SA 3.0 is not
one-way compatible with the GPL, and mixing the two arms would have been the
harder story to defend.

## uBO scriptlets — not implemented, and not copied

This repository ships **no scriptlet runtime**. uBlock Origin's `##+js(...)`
scriptlet injection is refused by the converter, which drops those filters and
counts them: `src/abp.rs` names the reason (`DropReason::Scriptlet`), and the
drop is reported in `docs/adblock.md` and in `rules.meta.json`. WebKit's content
blocker has no scriptlet-injection action, so these are impossible here rather
than merely unimplemented.

**No uBlock Origin source has been copied into this repository.**

Note for anyone grepping: `src/userscript.rs` calls itself "the scriptlet plane",
but it is the Greasemonkey metadata-block parser (`@match`, `@world`, `@run-at`),
not a uBO scriptlet runtime.

## `idcac.js` — shares the upstream name, not its code

`assets/web-userscripts/idcac.js` is a small independently written cosmetic
sweeper: a list of reject-button phrases and a list of generic consent-manager
container selectors. It contains **no compiled upstream rule table**, and its own
header says the hiding work belongs to the ruleset rather than to the script.

The i-dont-care-about-cookies **attribution owed by this project is for its ABP
list**, which is compiled into the committed ruleset — that is covered in the
filter-list table above, not by this file.

## SponsorBlock

**Upstream:** https://github.com/ajayyy/SponsorBlock — the browser extension,
**GPL-3.0**. Directly compatible with this repository's GPL-3.0-or-later.

Two separately-licensed things travel under that name, and they are not the same
question.

### 1. The extension — GPL-3.0

What exists here is `assets/web-userscripts/sponsorblock.js`, embedded into the
binary by `src/extensions.rs` with `include_str!`. There is no Rust-side
implementation of the protocol; the Rust code is catalog, settings toggle and
tests.

**Adopted from upstream, and marked as such:** the hash-prefix query protocol,
and the category names it uses. This build queries **three** categories only —
`sponsor`, `selfpromo`, `interaction`.

**Written here, not copied:** all of the code. It was written against the API's
documented behaviour.

**Deliberately absent**, so this notice does not overclaim: there is no seek-bar
rendering and no adoption of upstream's bar colours, no chapter or highlight
support, no per-category configuration UI, and no segment submission or voting.
The only feedback is a text toast.

The userscript asks by **SHA-256 hash prefix** and refuses to fall back to
querying by video id.

### 2. The segment database — CC BY-NC-SA 4.0

The crowd-sourced segments served by `https://sponsor.ajay.app` are licensed
**CC BY-NC-SA 4.0**, which is non-commercial.

- ✅ **Querying the API at runtime** is the user's own browser using a public
  service, exactly as every SponsorBlock user does.
- ✅ **Caching what a user fetched, for that user** is not distribution.
- ⛔ **Segment data must never travel in a released binary.** No bundled segment
  file, no pre-seeded artefact, nothing derived from the database baked into the
  crate. No segment data is present in this repository.
- ✅ **Attribution (BY) is owed for any use** and this section is it.

**On the non-commercial clause.** ychrome is GPL-3.0-or-later and is installed by
the user, and no NC-licensed material is distributed by it. ⚠ That reasoning
rests on ychrome staying separately installed rather than being bundled into a
paid product. Anyone who bundles it has changed the licensing question, not just
the packaging, and owes this paragraph a fresh answer.

## Rust dependencies

`Cargo.lock` records no licence fields, so the classification below comes from
the upstream crates themselves, not from anything in this tree. This repository
does not ship a generated per-crate manifest.

The only copyleft dependencies in the tree are **MPL-2.0**:

| Crate | Reached via |
|---|---|
| `cssparser`, `cssparser-macros`, `dtoa-short`, `selectors`, `servo_arc` | Servo's CSS/DOM stack, pulled in through the webview and `dom_query` layer |
| `option-ext` | `dirs` → `dirs-sys` |

**MPL-2.0 is file-level ("weak") copyleft and conflicts with neither licence
here.** Its §3.3 explicitly permits distributing a larger work under a Secondary
License (GPL/LGPL/AGPL), and its file scope means our own sources keep their own
licence. Depending on these crates *unmodified* is compatible with the GPLv3
browser and with the Apache-2.0 vault crates alike.

**What it obliges:** preserve those crates' licence notices, and make their
source available. Normal crates.io distribution satisfies the second; this file
satisfies the first. ⚠ **If anyone ever vendors and MODIFIES one of these, the
modified files stay MPL-2.0 and their source must be published.**

## The licence boundary inside this repository

`crates/ychrome-vault` and `crates/ychrome-vault-proto` are **Apache-2.0** —
reusable infrastructure, carved out deliberately from the GPL-3.0-or-later
browser around them. Each carries its own `LICENSE`.

⛔ **GPL code must never land in those two crates**, and no uBO or SponsorBlock
derivation may migrate into them. Re-run the per-crate dependency audit before
adding a dependency to either — a GPL crate landing there would silently make
the Apache declaration false.
