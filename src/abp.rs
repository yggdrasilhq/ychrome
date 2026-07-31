//! ABP/uBO filter syntax -> WebKit content-blocker JSON.
//!
//! ychrome's ad blocking used to be 60 rules somebody typed by hand: 59
//! third-party domain blocks and one `css-display-none` with 8 selectors. No
//! upstream list, no update path, and nothing in the program that referenced
//! it — a human had to copy the file to `~/.yggterm/web-adblock/rules.json`.
//! This module is the replacement: real filter lists in, a content-blocker
//! ruleset out, with everything it could not translate COUNTED and NAMED.
//!
//! # What WebKit actually accepts (measured, not remembered)
//!
//! Every constraint below was measured against the installed engine
//! (WebKitGTK **2.52.5** on both dev and the GUI host) by compiling probe rulesets
//! through `WebKitUserContentFilterStore`. The numbers are in
//! `docs/adblock.md`; the ones that shape this code:
//!
//! - **150,000 rules is the hard ceiling.** 150,000 compiles; 150,001 fails
//!   with *"Too many rules in JSON array."* See [`WEBKIT_RULE_CEILING`].
//! - **The regex dialect has no alternation.** `(a|b)` is rejected outright, so
//!   the textbook translation of ABP's `^` separator — *"a separator character
//!   OR the end of the URL"* — is not expressible. Neither is `{2,4}`,
//!   lookahead, `\w`, `\d`, or a `^` anywhere but the start.
//!   Accepted: `.` `*` `+` `?` `[...]` `(...)` `(?:...)` `$` and a leading `^`.
//! - **One bad network rule kills the WHOLE ruleset.** An invalid regex or an
//!   unknown `resource-type` string fails the entire compile, which means no ad
//!   blocking at all. So this module emits only constructs it has proven valid,
//!   and validates each one it builds ([`webkit_regex_ok`]).
//! - **A bad SELECTOR is dropped silently.** The compile still succeeds, minus
//!   that rule. Silent degradation is the failure class this whole lane exists
//!   to close, so selectors are validated here too and the rejects are counted.
//! - `resource-type: object` is NOT a valid string on this engine, whatever
//!   older Safari documentation says. `document`, `image`, `style-sheet`,
//!   `script`, `font`, `raw`, `svg-document`, `media`, `popup`, `ping`,
//!   `fetch`, `websocket`, `other`, `top-document` all are.
//!
//! # Rule ORDER is semantics
//!
//! WebKit evaluates rules in array order and `ignore-previous-rules` cancels
//! everything before it *for that load*. The sections below are therefore
//! ordered, not grouped for tidiness:
//!
//! 1. **blocks** — every `||ads.example^` style rule.
//! 2. **network exceptions** — `@@` rules, as `ignore-previous-rules`. After
//!    the blocks, so they can cancel one.
//! 3. **`$important` blocks** — after the exceptions, because `$important` in
//!    ABP means "an exception may not cancel this".
//! 4. **cosmetic** — `css-display-none`. After the subresource exceptions so a
//!    `@@||cdn.example^$script` cannot switch off a page's element hiding,
//!    which is not what it says.
//! 5. **document exceptions** — `@@...$document` / `$elemhide` /
//!    `$generichide`, last, because these ones DO mean "this page is exempt
//!    from all of it", cosmetic included.
//!
//! # What cannot be translated at all
//!
//! WebKit has no redirect action, no scriptlet injection, no request rewriting
//! and no per-selector cosmetic exception mechanism. `$redirect=`,
//! `$removeparam`, `$csp=`, `##+js(...)` and friends are therefore
//! **impossible**, not merely unimplemented. They are counted by reason in
//! [`Report::dropped`] with verbatim samples, following the same
//! name-what-you-refused shape as `webpolicy::promote_or_refuse`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

/// The measured hard ceiling of `WebKitUserContentFilterStore` on WebKitGTK
/// 2.52.5: 150,000 rules compile, 150,001 fails the whole ruleset with "Too
/// many rules in JSON array." Exceeding it does not degrade blocking, it
/// ABOLISHES it, so the converter trims and says so rather than shipping a
/// ruleset that cannot load.
pub const WEBKIT_RULE_CEILING: usize = 150_000;

/// How many verbatim examples to keep per drop reason. Enough to see the shape
/// of what was refused; few enough that the report stays a report.
const SAMPLES_PER_REASON: usize = 5;

/// WebKit `resource-type` strings, all measured valid on 2.52.5. Used as the
/// universe for a negated ABP type (`$~script`) — deliberately WITHOUT
/// `document`, `top-document` and `popup`, because ABP's `~type` never means
/// "and also block the page itself".
const SUBRESOURCE_TYPES: [&str; 11] = [
    "fetch",
    "font",
    "image",
    "media",
    "other",
    "ping",
    "raw",
    "script",
    "style-sheet",
    "svg-document",
    "websocket",
];

/// Why a filter did not become a rule. Every one of these is COUNTED; none is
/// silently discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DropReason {
    /// `$redirect=`, `$redirect-rule=`, `$rewrite=`: substitute a resource for
    /// the blocked one. WebKit's content blocker has no redirect action at all
    /// — the only actions are block, block-cookies, css-display-none,
    /// ignore-previous-rules and make-https. IMPOSSIBLE, not unimplemented.
    Redirect,
    /// `$removeparam=`, `$replace=`, `$urltransform=`, `$urlskip=`: rewrite the
    /// request. Same story: a declarative blocker decides yes/no, it never
    /// edits.
    RequestRewrite,
    /// `$csp=`, `$permissions=`: inject a response header. No action for it.
    HeaderInjection,
    /// `##+js(...)` naming a scriptlet the runtime does not implement, or one
    /// with no domain scope. The scriptlet PLANE exists now
    /// (`generate_scriptlet_script`); this counts what it still cannot run, so
    /// the size of the remaining gap stays visible instead of shrinking to
    /// "we support scriptlets".
    Scriptlet,
    /// `#$#` / `#%#` / `#$?#`: CSS injection and snippet filters. `css-display-none`
    /// can only hide; it cannot set an arbitrary property.
    StyleOrSnippet,
    /// `##^...`: AdGuard HTML filtering, which edits the response body.
    HtmlFiltering,
    /// A procedural cosmetic selector (`:has-text()`, `:xpath()`, `:upward()`,
    /// `:-abp-contains()`, `#?#`). Not CSS, so WebKit rejects it — measured:
    /// the rule is dropped and the compile still succeeds, which is precisely
    /// why it is counted here instead.
    ProceduralSelector,
    /// A selector that is not something this converter can prove is valid CSS.
    /// Conservative: WebKit drops an invalid selector silently, so a false
    /// reject costs one selector and a false accept costs a silent lie.
    UnprovableSelector,
    /// A `/regex/` filter using constructs WebKit's dialect does not have
    /// (alternation, `\w`, `{n,m}`, lookahead). Emitting it would fail the
    /// WHOLE ruleset compile.
    UnsupportedRegex,
    /// `$domain=a.com|~b.com`: positive and negative domains on one filter.
    /// WebKit's trigger has `if-domain` and `unless-domain` but honours only
    /// one of them, so neither half alone is the filter that was written.
    MixedDomainOption,
    /// A `$domain=` / cosmetic domain entry this cannot hand WebKit, and
    /// dropping it would WIDEN the filter (an `unless-domain` entry, or a
    /// positive list that ends up empty). Wildcards (`tripadvisor.*`), regex
    /// entries, and non-ASCII hosts all land here — the last measured, not
    /// guessed: WebKit answers `"Domains must be lower case ASCII. Use
    /// punycode to encode non-ASCII characters."` and fails the whole ruleset.
    UntranslatableDomain,
    /// `$object`, `$webrtc`, `$strict3p`, `$header=`, `$method=`, `$to=`,
    /// `$denyallow=` and the rest of the modern option surface that has no
    /// declarative equivalent.
    UnsupportedOption,
    /// The filter's pattern translated to something that would match every URL
    /// (`.*` and nothing else). Emitting it would block the web.
    TooBroad,
    /// Disabled by a `$badfilter` line in one of the source lists. Upstream
    /// asked for this; honouring it is not a loss.
    BadFilter,
    /// Trimmed to stay under [`WEBKIT_RULE_CEILING`]. The only drop reason that
    /// is ychrome's own budget rather than a property of the filter.
    OverCeiling,
}

impl DropReason {
    /// The stable key this reason is reported under.
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::Redirect => "redirect",
            DropReason::RequestRewrite => "request-rewrite",
            DropReason::HeaderInjection => "header-injection",
            DropReason::Scriptlet => "scriptlet",
            DropReason::StyleOrSnippet => "style-or-snippet",
            DropReason::HtmlFiltering => "html-filtering",
            DropReason::ProceduralSelector => "procedural-selector",
            DropReason::UnprovableSelector => "unprovable-selector",
            DropReason::UnsupportedRegex => "unsupported-regex",
            DropReason::MixedDomainOption => "mixed-domain-option",
            DropReason::UntranslatableDomain => "untranslatable-domain",
            DropReason::UnsupportedOption => "unsupported-option",
            DropReason::TooBroad => "too-broad",
            DropReason::BadFilter => "badfilter",
            DropReason::OverCeiling => "over-ceiling",
        }
    }

    /// One sentence on why WebKit cannot express this, for the report a human
    /// reads.
    pub fn explain(self) -> &'static str {
        match self {
            DropReason::Redirect => {
                "WebKit's content blocker has no redirect action; a rule can only allow or deny"
            }
            DropReason::RequestRewrite => {
                "a declarative blocker cannot edit a request URL, only allow or deny it"
            }
            DropReason::HeaderInjection => "there is no action that injects a response header",
            DropReason::Scriptlet => {
                "no implementation for this scriptlet in the runtime library, or the filter \
                 named no domain to scope it to"
            }
            DropReason::StyleOrSnippet => {
                "css-display-none can only hide an element; it cannot set an arbitrary property"
            }
            DropReason::HtmlFiltering => "editing the response body is not a blocker action",
            DropReason::ProceduralSelector => {
                "not a CSS selector; WebKit drops it silently, so it is counted rather than shipped"
            }
            DropReason::UnprovableSelector => {
                "could not be proven to be valid CSS, and an invalid selector is dropped silently"
            }
            DropReason::UnsupportedRegex => {
                "uses regex WebKit's dialect lacks (no alternation, no {n,m}, no lookahead, no \\w); \
                 emitting it would fail the entire ruleset compile"
            }
            DropReason::MixedDomainOption => {
                "WebKit honours if-domain or unless-domain, never both, so neither half is the \
                 filter that was written"
            }
            DropReason::UntranslatableDomain => {
                "if-domain takes a lower-case ASCII domain — not a pattern, a regex or an IDN — \
                 and a bad one fails the ENTIRE ruleset compile"
            }
            DropReason::UnsupportedOption => {
                "no declarative equivalent in the content-blocker JSON"
            }
            DropReason::TooBroad => "would match every URL",
            DropReason::BadFilter => "disabled upstream by a $badfilter line",
            DropReason::OverCeiling => {
                "trimmed to stay under WebKit's measured 150,000-rule ceiling, above which the \
                 whole ruleset fails to compile"
            }
        }
    }
}

/// What one conversion did, in numbers a human can check.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub source_lines: usize,
    pub comments: usize,
    /// Filters that became a `block` rule.
    pub network_block: usize,
    /// `@@` filters that became an `ignore-previous-rules` rule.
    pub network_exception: usize,
    /// `$important` blocks, emitted after the exceptions.
    pub network_important: usize,
    /// `@@...$document`/`$elemhide` exceptions, emitted last.
    pub document_exception: usize,
    /// Generic `##` selectors that made it into the batched rules.
    pub cosmetic_generic_selectors: usize,
    /// How many rules those selectors were batched into.
    pub cosmetic_generic_rules: usize,
    /// Domain-scoped `##` selectors kept, and the rules they grouped into.
    pub cosmetic_domain_selectors: usize,
    pub cosmetic_domain_rules: usize,
    /// `#@#` exceptions that were honoured by removing a selector or adding an
    /// `unless-domain`.
    pub cosmetic_unhide_applied: usize,
    /// Procedural cosmetic filters routed to the generated userscript rather
    /// than dropped.
    pub cosmetic_procedural_rules: usize,
    /// `##+js(...)` filters routed to the generated scriptlet userscript.
    pub scriptlet_rules: usize,
    /// `#@#+js(...)` exceptions honoured by removing a scriptlet from a domain.
    pub scriptlet_unhide_applied: usize,
    /// Rules emitted, total.
    pub emitted: usize,
    /// Identical rules collapsed. Overlapping lists produce a lot of these.
    pub deduplicated: usize,
    /// Individual `$domain=` / cosmetic domain ENTRIES WebKit could not take,
    /// on filters that were otherwise kept. Each one narrows its filter by one
    /// site; none of them widens it. Counted separately from `dropped` because
    /// no rule was lost.
    pub domain_entries_dropped: usize,
    pub dropped: BTreeMap<&'static str, usize>,
    pub samples: BTreeMap<&'static str, Vec<String>>,
}

impl Report {
    fn drop(&mut self, reason: DropReason, filter: &str) {
        *self.dropped.entry(reason.as_str()).or_insert(0) += 1;
        let samples = self.samples.entry(reason.as_str()).or_default();
        if samples.len() < SAMPLES_PER_REASON {
            samples.push(filter.to_string());
        }
    }

    /// Everything that did not become a rule, whatever the reason.
    pub fn dropped_total(&self) -> usize {
        self.dropped.values().sum()
    }

    /// The report as JSON, for `rules.meta.json` and `ychrome adblock status`.
    pub fn to_json(&self) -> Value {
        let dropped: BTreeMap<&str, Value> = self
            .dropped
            .iter()
            .map(|(reason, count)| {
                (
                    *reason,
                    json!({
                        "count": count,
                        "why": DropReason::ALL
                            .iter()
                            .find(|candidate| candidate.as_str() == *reason)
                            .map(|candidate| candidate.explain())
                            .unwrap_or_default(),
                        "samples": self.samples.get(reason).cloned().unwrap_or_default(),
                    }),
                )
            })
            .collect();
        json!({
            "source_lines": self.source_lines,
            "comments": self.comments,
            "emitted": self.emitted,
            "deduplicated": self.deduplicated,
            "domain_entries_dropped": self.domain_entries_dropped,
            "network_block": self.network_block,
            "network_exception": self.network_exception,
            "network_important": self.network_important,
            "document_exception": self.document_exception,
            "cosmetic_generic_selectors": self.cosmetic_generic_selectors,
            "cosmetic_generic_rules": self.cosmetic_generic_rules,
            "cosmetic_domain_selectors": self.cosmetic_domain_selectors,
            "cosmetic_domain_rules": self.cosmetic_domain_rules,
            "cosmetic_unhide_applied": self.cosmetic_unhide_applied,
            "cosmetic_procedural_rules": self.cosmetic_procedural_rules,
            "scriptlet_rules": self.scriptlet_rules,
            "scriptlet_unhide_applied": self.scriptlet_unhide_applied,
            "dropped_total": self.dropped_total(),
            "dropped": dropped,
        })
    }
}

impl DropReason {
    /// Every reason, so a report can look one up by key without a second table
    /// that could drift from the enum.
    pub const ALL: [DropReason; 15] = [
        DropReason::Redirect,
        DropReason::RequestRewrite,
        DropReason::HeaderInjection,
        DropReason::Scriptlet,
        DropReason::StyleOrSnippet,
        DropReason::HtmlFiltering,
        DropReason::ProceduralSelector,
        DropReason::UnprovableSelector,
        DropReason::UnsupportedRegex,
        DropReason::MixedDomainOption,
        DropReason::UntranslatableDomain,
        DropReason::UnsupportedOption,
        DropReason::TooBroad,
        DropReason::BadFilter,
        DropReason::OverCeiling,
    ];
}

/// Translate one `$domain=` / cosmetic domain entry into the spelling WebKit's
/// `if-domain` takes, or `None` when it cannot be one.
///
/// WebKit's `*` PREFIX means "this domain and its subdomains", which is what an
/// ABP domain entry means — it is not a wildcard and must not be confused with
/// one. A `*` anywhere else (`tripadvisor.*`, `*.foo`) is an ABP wildcard with
/// no equivalent, and a non-ASCII host is refused by the engine outright:
/// measured, `"Domains must be lower case ASCII. Use punycode to encode
/// non-ASCII characters."`, which fails the whole ruleset rather than that one
/// rule. Case is folded rather than refused, because a canonical host is
/// lowercase and a list that shouted one still meant it.
pub fn translate_domain(entry: &str) -> Option<String> {
    let host = entry.trim().trim_start_matches('.').to_ascii_lowercase();
    if host.is_empty() || !host.is_ascii() || host.starts_with('/') {
        return None;
    }
    if host
        .bytes()
        .any(|byte| !(byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-' || byte == b'_'))
    {
        return None;
    }
    Some(format!("*{host}"))
}

/// Split an ABP domain list into WebKit's two, dropping the entries WebKit
/// cannot take.
///
/// The asymmetry is the point. An entry dropped from the POSITIVE list makes
/// the filter apply to fewer sites — the conservative direction, and much
/// better than losing all 250 domains of a list because one of them said
/// `tripadvisor.*`. An entry dropped from the NEGATIVE list makes the filter
/// apply to MORE sites, including one its author excluded, so a bad negative
/// entry refuses the whole filter.
fn split_domain_list<'a>(
    entries: impl Iterator<Item = &'a str>,
    dropped_entries: &mut usize,
) -> Result<(Vec<String>, Vec<String>), DropReason> {
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    let mut saw_positive = false;
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (negated, host) = match entry.strip_prefix('~') {
            Some(rest) => (true, rest),
            None => (false, entry),
        };
        match (translate_domain(host), negated) {
            (Some(host), false) => {
                saw_positive = true;
                positive.push(host);
            }
            (Some(host), true) => negative.push(host),
            (None, false) => {
                saw_positive = true;
                *dropped_entries += 1;
            }
            (None, true) => return Err(DropReason::UntranslatableDomain),
        }
    }
    if saw_positive && positive.is_empty() {
        // Every positive entry was untranslatable: emitting the filter with no
        // if-domain would apply it EVERYWHERE, which is the opposite of what it
        // said.
        return Err(DropReason::UntranslatableDomain);
    }
    if !positive.is_empty() && !negative.is_empty() {
        return Err(DropReason::MixedDomainOption);
    }
    positive.sort();
    positive.dedup();
    negative.sort();
    negative.dedup();
    Ok((positive, negative))
}

/// One source list.
pub struct Source<'a> {
    /// The list's id, as it appears in the generated ruleset's provenance
    /// sidecar. Carried on the source so a future per-list statistic has one
    /// place to hang off, and so a caller cannot pass an anonymous blob.
    #[allow(dead_code)]
    pub name: &'a str,
    pub text: &'a str,
}

/// The output: the ruleset, the generated cosmetic userscript, and the account
/// of how both were reached.
pub struct Conversion {
    pub rules: Vec<Value>,
    /// Procedural cosmetic rules WebKit cannot express, keyed by domain, ready
    /// for [`generate_cosmetic_script`]. Deterministic order.
    pub procedural: BTreeMap<String, BTreeSet<ProceduralRule>>,
    /// `##+js(...)` scriptlet invocations the runtime implements, keyed by
    /// domain, ready for [`generate_scriptlet_script`]. Deterministic order.
    pub scriptlets: BTreeMap<String, BTreeSet<ScriptletRule>>,
    pub report: Report,
}

/// One procedural cosmetic rule, in the only two forms this converter
/// implements.
///
/// 646 procedural rules across the shipped corpus, all domain-scoped, over 804
/// distinct domains. 422 of them are `:has-text()` and 159 are `:style()` —
/// 90% between them. The remaining 65 (`:upward()`, `:xpath()`, `:remove()`,
/// `:matches-css`, `:matches-attr()`, `:matches-path()`, `:others()`) are
/// counted and dropped, because each needs its own semantics and the tail is
/// not worth a second engine.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProceduralRule {
    /// `<prefix>:has-text(TEXT)` — hide elements matching `prefix` whose text
    /// contains `text`. uBO's own most common procedural form.
    HasText { prefix: String, text: String },
    /// `<prefix>:style(DECLS)` — apply `decls` to elements matching `prefix`.
    /// `css-display-none` can only hide; this is how the consent lists give a
    /// page its scrolling back
    /// (`body.didomi-popup-open:style(overflow: auto !important;)` alone rides
    /// on 205 domains).
    Style { prefix: String, decls: String },
}

// ---------------------------------------------------------------------------
// Scriptlets (`##+js(...)`)
// ---------------------------------------------------------------------------

/// One scriptlet invocation from a filter: the CANONICAL name and its
/// arguments, already alias-resolved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScriptletRule {
    pub name: &'static str,
    pub args: Vec<String>,
}

/// The scriptlet library: one canonical name, every alias the lists spell it
/// with, and the measured number of filters it unlocks.
///
/// ⚠ **This table and `assets/web-scriptlets/runtime.js` are ONE contract.**
/// A canonical name here with no implementation there would route filters into
/// a runtime that silently ignores them, which is the exact silent-degradation
/// shape this converter exists to end.
/// `extensions.rs::the_scriptlet_table_and_the_runtime_are_one_contract` fails
/// if either side moves.
///
/// ⚠ **The implementations are OURS, written from the documented behaviour of
/// the filter syntax.** uBlock Origin is GPLv3 and this project ships Apache.
/// A scriptlet's NAME, its argument grammar and what it observably does are not
/// copyrightable; an implementation is. Nothing here was transcribed.
///
/// `filters` is what the 2026-07-31 snapshot of the nine shipped lists actually
/// uses, counted rather than guessed — it is why the order is what it is.
pub struct Scriptlet {
    pub canonical: &'static str,
    pub aliases: &'static [&'static str],
    /// Filters in the shipped corpus, measured 2026-07-31.
    pub filters: usize,
}

/// Ordered by measured coverage, highest first, so the reason each one is here
/// is legible. `scriptlet_coverage_is_ordered_by_what_it_unlocks` keeps it that
/// way.
pub static SCRIPTLETS: &[Scriptlet] = &[
    Scriptlet {
        canonical: "set-cookie",
        aliases: &["trusted-set-cookie", "set-cookie.js"],
        filters: 1070,
    },
    Scriptlet {
        canonical: "abort-on-property-read",
        aliases: &["aopr", "abort-on-property-read.js"],
        filters: 637,
    },
    Scriptlet {
        canonical: "abort-current-script",
        aliases: &[
            "acs",
            "abort-current-inline-script",
            "acis",
            "abort-current-script.js",
            "abort-current-inline-script.js",
        ],
        filters: 363,
    },
    Scriptlet {
        canonical: "set-constant",
        aliases: &["set", "set-constant.js"],
        filters: 345,
    },
    Scriptlet {
        canonical: "set-local-storage-item",
        aliases: &["trusted-set-local-storage-item"],
        filters: 237,
    },
    Scriptlet {
        canonical: "no-setTimeout-if",
        aliases: &["nostif", "prevent-setTimeout", "setTimeout-defuser"],
        filters: 211,
    },
    Scriptlet {
        canonical: "addEventListener-defuser",
        aliases: &[
            "aeld",
            "prevent-addEventListener",
            "addEventListener-defuser.js",
        ],
        filters: 205,
    },
    Scriptlet {
        canonical: "abort-on-property-write",
        aliases: &["aopw", "abort-on-property-write.js"],
        filters: 183,
    },
    Scriptlet {
        canonical: "no-window-open-if",
        aliases: &["nowoif", "prevent-window-open", "window.open-defuser"],
        filters: 183,
    },
    Scriptlet {
        canonical: "remove-cookie",
        aliases: &["cookie-remover", "cookie-remover.js"],
        filters: 67,
    },
    Scriptlet {
        canonical: "href-sanitizer",
        aliases: &[],
        filters: 62,
    },
    Scriptlet {
        canonical: "adjust-setInterval",
        aliases: &["nano-sib", "nano-setInterval-booster"],
        filters: 51,
    },
    Scriptlet {
        canonical: "remove-node-text",
        aliases: &["rmnt", "remove-node-text.js"],
        filters: 49,
    },
    Scriptlet {
        canonical: "nowebrtc",
        aliases: &["nowebrtc.js"],
        filters: 47,
    },
    Scriptlet {
        canonical: "no-xhr-if",
        aliases: &["prevent-xhr"],
        filters: 46,
    },
    Scriptlet {
        canonical: "no-fetch-if",
        aliases: &["prevent-fetch"],
        filters: 40,
    },
    Scriptlet {
        canonical: "adjust-setTimeout",
        aliases: &["nano-stb", "nano-setTimeout-booster"],
        filters: 32,
    },
    Scriptlet {
        canonical: "remove-attr",
        aliases: &["ra", "remove-attr.js"],
        filters: 31,
    },
    Scriptlet {
        canonical: "noeval-if",
        aliases: &["noeval", "noeval.js", "silent-noeval", "prevent-eval-if"],
        filters: 30,
    },
    Scriptlet {
        canonical: "no-setInterval-if",
        aliases: &["nosiif", "prevent-setInterval", "setInterval-defuser"],
        filters: 28,
    },
    Scriptlet {
        canonical: "json-prune",
        aliases: &["json-prune.js"],
        filters: 27,
    },
    Scriptlet {
        canonical: "set-session-storage-item",
        aliases: &["trusted-set-session-storage-item"],
        filters: 9,
    },
    Scriptlet {
        canonical: "remove-class",
        aliases: &["rc"],
        filters: 1,
    },
];

/// Canonical name for a spelling the lists use, or `None` when the runtime has
/// no implementation.
pub fn canonical_scriptlet(name: &str) -> Option<&'static str> {
    let name = name.trim();
    SCRIPTLETS
        .iter()
        .find(|entry| entry.canonical == name || entry.aliases.contains(&name))
        .map(|entry| entry.canonical)
}

/// Split `+js(name, a, b)`'s inside into its comma-separated arguments,
/// honouring `\,` as a literal comma. uBO's grammar is positional and
/// unquoted; a value that needs a comma escapes it, and that is the only
/// escape there is.
fn split_scriptlet_args(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&',') => {
                chars.next();
                current.push(',');
            }
            ',' => {
                out.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    out.push(current.trim().to_string());
    out
}

/// Parse a cosmetic body of the form `+js(name, args...)`.
///
/// Returns `None` for anything that is not a scriptlet invocation, and for a
/// scriptlet whose name the runtime does not implement — the caller counts that
/// as a drop rather than shipping a rule nothing will run.
pub fn parse_scriptlet(body: &str) -> Option<ScriptletRule> {
    let inner = body.strip_prefix("+js(")?.strip_suffix(')')?;
    let mut args = split_scriptlet_args(inner);
    if args.is_empty() || args[0].is_empty() {
        return None;
    }
    let raw = args.remove(0);
    let name = canonical_scriptlet(raw.trim_matches(|c| c == '\'' || c == '"'))?;
    // Trailing empty positional arguments carry no meaning and would otherwise
    // make two spellings of the same filter into two different rules.
    while args.last().is_some_and(String::is_empty) {
        args.pop();
    }
    Some(ScriptletRule {
        name,
        args: args
            .into_iter()
            .map(|arg| {
                arg.trim_matches(|c| c == '\'' || c == '"')
                    .replace("\\'", "'")
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// The regex dialect
// ---------------------------------------------------------------------------

/// ABP's `^` separator, as far as WebKit's dialect can express it: any
/// character that is not a letter, digit, `_`, `-`, `.` or `%`.
///
/// ABP's actual definition also matches the END of the URL, which needs
/// alternation (`[...]|$`) — measured REJECTED by this engine. The consequence
/// is strictly narrower matching, never wider: a filter ending `/ads^` misses a
/// URL that ends exactly at `/ads`. It does not affect the overwhelmingly
/// common `||host^` form at all, because a canonical URL always carries a `/`
/// or a `:port` after its authority.
const SEPARATOR_CLASS: &str = "[^a-zA-Z0-9_%.-]";

/// The domain anchor `||`. Scheme-agnostic (`[^:]+:`) so `ws://` and `ftp://`
/// are covered like ABP means them, and `([^/]+\.)?` gives "this host or any
/// subdomain of it" without ever crossing into the path.
const DOMAIN_ANCHOR: &str = "^[^:]+:(//)?([^/]+\\.)?";

/// Whether a regex uses only constructs measured to compile on this engine.
///
/// This is a GATE, not a nicety: one rejected regex fails the entire ruleset,
/// so a rule that cannot pass here must never be emitted. Rejects, each
/// measured: `|` alternation, `{` counted repetition, `(?` anything but
/// `(?:`, backslash escapes outside a small allow-list, and `^` anywhere but
/// position zero.
pub fn webkit_regex_ok(pattern: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    // Measured: a url-filter containing any non-ASCII character is rejected
    // outright ("Invalid or unsupported regular expression"), IDN hosts
    // included. Filter lists do carry them, so this is a real gate and not a
    // theoretical one.
    if !pattern.is_ascii() {
        return false;
    }
    let bytes: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    let mut depth_paren = 0i32;
    let mut in_class = false;
    while index < bytes.len() {
        let ch = bytes[index];
        match ch {
            '\\' => {
                let Some(next) = bytes.get(index + 1) else {
                    return false;
                };
                // Escapes WebKit's dialect accepts: a literal metacharacter.
                // `\w`, `\d`, `\s`, `\b` are measured REJECTED.
                if next.is_ascii_alphanumeric() {
                    return false;
                }
                index += 2;
                continue;
            }
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            _ if in_class => {}
            '|' => return false,
            '{' | '}' => return false,
            '^' if index != 0 => return false,
            '(' => {
                depth_paren += 1;
                // `(?:` is fine; every other `(?` form (lookahead, named
                // groups) is rejected.
                if bytes.get(index + 1) == Some(&'?') && bytes.get(index + 2) != Some(&':') {
                    return false;
                }
            }
            ')' => {
                depth_paren -= 1;
                if depth_paren < 0 {
                    return false;
                }
            }
            _ => {}
        }
        index += 1;
    }
    depth_paren == 0 && !in_class
}

/// Escape one literal character for WebKit's regex dialect.
///
/// `/` is deliberately NOT escaped: measured, the engine takes it plain, and
/// every URL pattern in a filter list is full of them — escaping would add
/// hundreds of kilobytes to the ruleset for nothing. Escaping every other
/// metacharacter (including `{`, which is rejected raw and accepted escaped)
/// is measured too.
fn escape_literal(ch: char, out: &mut String) {
    if "\\^$.[]|()*+?{}".contains(ch) {
        out.push('\\');
    }
    out.push(ch);
}

/// Translate an ABP pattern (everything before `$options`) into a WebKit
/// `url-filter`, or say why it cannot be.
pub fn translate_pattern(pattern: &str) -> Result<String, DropReason> {
    // A /regex/ filter is already a regex, in a dialect that is mostly not
    // this one. Pass through only what compiles.
    if pattern.len() >= 2 && pattern.starts_with('/') && pattern.ends_with('/') {
        let inner = &pattern[1..pattern.len() - 1];
        return if webkit_regex_ok(inner) {
            Ok(inner.to_string())
        } else {
            Err(DropReason::UnsupportedRegex)
        };
    }

    let mut rest = pattern;
    let mut out = String::new();
    if let Some(tail) = rest.strip_prefix("||") {
        out.push_str(DOMAIN_ANCHOR);
        rest = tail;
    } else if let Some(tail) = rest.strip_prefix('|') {
        out.push('^');
        rest = tail;
    }
    let anchored_end = rest.ends_with('|');
    if anchored_end {
        rest = &rest[..rest.len() - 1];
    }

    for ch in rest.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '^' => out.push_str(SEPARATOR_CLASS),
            _ => escape_literal(ch, &mut out),
        }
    }
    if anchored_end {
        out.push('$');
    }

    // An EMPTY pattern is `$popup,domain=a.test`: no URL constraint at all,
    // only options. WebKit rejects an empty url-filter, so it is spelled `.*`
    // — which is honest, and which `matches_everything` then makes the caller
    // justify with a domain or a resource type.
    if out.is_empty() {
        out.push_str(".*");
    }
    if !webkit_regex_ok(&out) {
        return Err(DropReason::UnsupportedRegex);
    }
    Ok(out)
}

/// Whether a translated url-filter matches every URL. Such a filter is fine
/// when something ELSE narrows the rule (`$popup,domain=a.test` is a real,
/// common filter), and blocks the web when nothing does.
fn matches_everything(url_filter: &str) -> bool {
    url_filter
        .chars()
        .zip(url_filter.chars().skip(1).chain(std::iter::once(' ')))
        .all(|(ch, next)| matches!(ch, '^' | '$' | '*') || (ch == '.' && next == '*'))
}

// ---------------------------------------------------------------------------
// Network filters
// ---------------------------------------------------------------------------

/// The trigger half of a content-blocker rule, before serialization.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Trigger {
    url_filter: String,
    case_sensitive: bool,
    resource_types: BTreeSet<&'static str>,
    load_type: Option<&'static str>,
    load_context: Option<&'static str>,
    if_domain: Vec<String>,
    unless_domain: Vec<String>,
}

impl Trigger {
    fn to_json(&self) -> Value {
        let mut trigger = serde_json::Map::new();
        trigger.insert("url-filter".into(), json!(self.url_filter));
        if self.case_sensitive {
            trigger.insert("url-filter-is-case-sensitive".into(), json!(true));
        }
        if !self.resource_types.is_empty() {
            trigger.insert(
                "resource-type".into(),
                json!(self.resource_types.iter().collect::<Vec<_>>()),
            );
        }
        if let Some(load_type) = self.load_type {
            trigger.insert("load-type".into(), json!([load_type]));
        }
        if let Some(load_context) = self.load_context {
            trigger.insert("load-context".into(), json!([load_context]));
        }
        if !self.if_domain.is_empty() {
            trigger.insert("if-domain".into(), json!(self.if_domain));
        }
        if !self.unless_domain.is_empty() {
            trigger.insert("unless-domain".into(), json!(self.unless_domain));
        }
        Value::Object(trigger)
    }
}

/// Where an emitted rule belongs in the ordered sections. The discriminant IS
/// the order, and the order IS the semantics (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Block = 0,
    NetworkException = 1,
    ImportantBlock = 2,
    Cosmetic = 3,
    DocumentException = 4,
}

/// Split `filter` into its pattern and its raw option string.
///
/// The `$` that starts the options is the LAST one, except that a `/regex/`
/// filter may contain any number of them: for that shape the options can only
/// begin after the closing slash.
fn split_options(filter: &str) -> (&str, Option<&str>) {
    if filter.starts_with('/') {
        // `/re/$opts` — the closing slash is the last one before a `$`.
        if let Some(dollar) = filter.rfind("/$") {
            return (&filter[..dollar + 1], Some(&filter[dollar + 2..]));
        }
        return (filter, None);
    }
    match filter.rfind('$') {
        Some(index) => (&filter[..index], Some(&filter[index + 1..])),
        None => (filter, None),
    }
}

/// Map one ABP type option to WebKit resource types. `None` = this option is
/// not a type at all.
fn resource_types_for(option: &str) -> Option<&'static [&'static str]> {
    Some(match option {
        "script" => &["script"],
        "image" => &["image"],
        "stylesheet" | "css" => &["style-sheet"],
        "font" => &["font"],
        "media" => &["media"],
        // WebKit splits what ABP calls xmlhttprequest across `fetch` and the
        // older `raw`; emitting both is the only way to cover every build.
        "xmlhttprequest" | "xhr" => &["fetch", "raw"],
        "ping" | "beacon" => &["ping"],
        "websocket" => &["websocket"],
        "other" => &["other"],
        "popup" | "popunder" => &["popup"],
        // ABP's $object is plugin content, which this engine reports as
        // `other`; there is no `object` resource-type (measured: the string is
        // rejected and takes the whole ruleset with it).
        "object" | "object-subrequest" => &["other"],
        _ => return None,
    })
}

/// A parsed network filter, ready to become one or more rules.
struct NetworkRule {
    trigger: Trigger,
    section: Section,
    exception: bool,
}

/// Parse one network filter line. `Err` names why it could not become a rule.
/// `dropped_entries` counts individual `$domain=` entries WebKit could not take
/// while the filter itself survived — a narrowing, which is reported as a
/// number rather than pretended away.
fn parse_network(line: &str, dropped_entries: &mut usize) -> Result<NetworkRule, DropReason> {
    let (body, exception) = match line.strip_prefix("@@") {
        Some(rest) => (rest, true),
        None => (line, false),
    };
    let (pattern, options) = split_options(body);
    let mut trigger = Trigger {
        url_filter: translate_pattern(pattern)?,
        ..Trigger::default()
    };
    let mut important = false;
    let mut document_scope = false;
    let mut negated_types: BTreeSet<&'static str> = BTreeSet::new();

    for option in options
        .unwrap_or_default()
        .split(',')
        .filter(|o| !o.is_empty())
    {
        let (negated, option) = match option.strip_prefix('~') {
            Some(rest) => (true, rest),
            None => (false, option),
        };
        let (key, value) = match option.split_once('=') {
            Some((key, value)) => (key, Some(value)),
            None => (option, None),
        };
        match key {
            _ if resource_types_for(key).is_some() => {
                let types = resource_types_for(key).expect("just checked");
                if negated {
                    negated_types.extend(types.iter().copied());
                } else {
                    trigger.resource_types.extend(types.iter().copied());
                }
            }
            "subdocument" | "frame" => {
                trigger.resource_types.insert("document");
                trigger.load_context = Some(if negated { "top-frame" } else { "child-frame" });
            }
            // ABP's $document means the top-level page load. Spelled as
            // "a document, in the top frame" rather than `top-document`,
            // because both halves are transparent and independently measured.
            "document" | "doc" => {
                trigger.resource_types.insert("document");
                trigger.load_context = Some("top-frame");
                document_scope = true;
            }
            // Cosmetic-disabling exceptions. Only meaningful on an @@ rule,
            // where they mean "this page is exempt from element hiding" — which
            // is why they land in the LAST section, after the cosmetic rules.
            "elemhide" | "ehide" | "generichide" | "ghide" | "specifichide" | "shide" => {
                if !exception {
                    return Err(DropReason::UnsupportedOption);
                }
                trigger.resource_types.insert("document");
                trigger.load_context = Some("top-frame");
                document_scope = true;
            }
            "third-party" | "3p" => {
                trigger.load_type = Some(if negated {
                    "first-party"
                } else {
                    "third-party"
                });
            }
            "first-party" | "1p" => {
                trigger.load_type = Some(if negated {
                    "third-party"
                } else {
                    "first-party"
                });
            }
            "match-case" => trigger.case_sensitive = !negated,
            "important" => important = true,
            // `$all` widens to every type, which is the absence of a
            // resource-type restriction.
            "all" => {}
            "domain" | "from" => {
                let list = value.ok_or(DropReason::UnsupportedOption)?;
                let (positive, negative) = split_domain_list(list.split('|'), dropped_entries)?;
                trigger.if_domain = positive;
                trigger.unless_domain = negative;
            }
            "redirect" | "redirect-rule" | "rewrite" => return Err(DropReason::Redirect),
            "removeparam" | "queryprune" | "replace" | "urltransform" | "uritransform"
            | "urlskip" => return Err(DropReason::RequestRewrite),
            "csp" | "permissions" => return Err(DropReason::HeaderInjection),
            _ => return Err(DropReason::UnsupportedOption),
        }
    }

    // `$~script` and friends: the complement over subresource types. Never
    // includes `document` or `popup`, because "everything except scripts" has
    // never meant "and also the page itself".
    if !negated_types.is_empty() && trigger.resource_types.is_empty() {
        trigger.resource_types = SUBRESOURCE_TYPES
            .iter()
            .copied()
            .filter(|kind| !negated_types.contains(kind))
            .collect();
    }

    // A pattern that matches every URL is only safe when the options narrowed
    // the rule to a domain or a resource type. `$popup,domain=a.test` with an
    // empty pattern is exactly that, and there are hundreds of them.
    if matches_everything(&trigger.url_filter)
        && trigger.if_domain.is_empty()
        && trigger.resource_types.is_empty()
    {
        return Err(DropReason::TooBroad);
    }

    let section = match (exception, document_scope, important) {
        (true, true, _) => Section::DocumentException,
        (true, false, _) => Section::NetworkException,
        (false, _, true) => Section::ImportantBlock,
        (false, _, false) => Section::Block,
    };
    Ok(NetworkRule {
        trigger,
        section,
        exception,
    })
}

// ---------------------------------------------------------------------------
// Cosmetic filters
// ---------------------------------------------------------------------------

/// The cosmetic separators, longest first so `#@$?#` is never read as `#@#`.
const COSMETIC_SEPARATORS: [&str; 9] = [
    "#@$?#", "#$?#", "#@$#", "#@?#", "#@#", "#?#", "#$#", "#%#", "##",
];

/// Markers that make a selector procedural rather than CSS. Measured: WebKit
/// rejects every one of them, and does so SILENTLY (the compile succeeds, the
/// rule vanishes).
const PROCEDURAL_MARKERS: [&str; 18] = [
    ":has-text(",
    ":-abp-contains(",
    ":-abp-has(",
    ":-abp-properties(",
    ":contains(",
    ":upward(",
    ":xpath(",
    ":matches-css",
    ":matches-path(",
    ":matches-attr(",
    ":matches-media(",
    ":min-text-length(",
    ":watch-attr(",
    ":remove(",
    ":style(",
    ":others(",
    ":nth-ancestor(",
    ":if(",
];

/// Parse a procedural selector into the subset this converter implements, or
/// `None` when it is one of the forms that stay dropped.
///
/// Deliberately strict about NESTING: `a:has(p:has-text(Sponsored))` puts the
/// procedural part inside a `:has()`, and hiding `a:has(p)` instead would hide
/// far more than the filter asked. Only a procedural pseudo-class in the LAST
/// position, with a plain-CSS prefix, is accepted.
pub fn parse_procedural(selector: &str) -> Option<ProceduralRule> {
    for (marker, build) in [
        (":has-text(", 0u8),
        (":-abp-contains(", 0),
        (":contains(", 0),
        (":style(", 1),
    ] {
        let Some(at) = selector.rfind(marker) else {
            continue;
        };
        if !selector.ends_with(')') {
            continue;
        }
        let prefix = &selector[..at];
        let arg = &selector[at + marker.len()..selector.len() - 1];
        // The argument must be the LAST thing: a nested form leaves an
        // unbalanced prefix or trailing content.
        if arg.contains('(') || arg.contains(')') || arg.is_empty() {
            continue;
        }
        if prefix.is_empty() || !selector_ok(prefix) {
            continue;
        }
        // A /regex/ argument is uBO's other language; not implemented.
        if build == 0 && arg.starts_with('/') && arg.ends_with('/') {
            continue;
        }
        return Some(if build == 0 {
            ProceduralRule::HasText {
                prefix: prefix.to_string(),
                text: arg.to_string(),
            }
        } else {
            ProceduralRule::Style {
                prefix: prefix.to_string(),
                decls: arg.to_string(),
            }
        });
    }
    None
}

/// Whether a selector is one this converter is willing to hand WebKit.
///
/// Conservative on purpose: WebKit drops an invalid selector without failing
/// the compile, so a selector this rejects costs one hidden element, while one
/// it wrongly accepts is counted as shipped and silently is not. The check is
/// structural (balanced delimiters, no procedural pseudo-class, no `{`) rather
/// than a CSS parser, which would be a second implementation of something the
/// engine already owns.
pub fn selector_ok(selector: &str) -> bool {
    let selector = selector.trim();
    if selector.is_empty() || selector.len() > 4096 {
        return false;
    }
    if PROCEDURAL_MARKERS
        .iter()
        .any(|marker| selector.contains(marker))
    {
        return false;
    }
    // A declaration block means this is a style filter, not a hide filter.
    if selector.contains('{') || selector.contains('}') {
        return false;
    }
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in selector.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            _ if quote == Some(ch) => quote = None,
            _ if quote.is_some() => {}
            '"' | '\'' => quote = Some(ch),
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            _ => {}
        }
        if paren < 0 || bracket < 0 {
            return false;
        }
    }
    paren == 0 && bracket == 0 && quote.is_none() && !escaped
}

/// A cosmetic filter's domain scope, already split into WebKit's two lists.
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DomainScope {
    if_domain: Vec<String>,
    unless_domain: Vec<String>,
}

fn parse_domain_scope(list: &str, dropped_entries: &mut usize) -> Result<DomainScope, DropReason> {
    let (if_domain, unless_domain) = split_domain_list(list.split(','), dropped_entries)?;
    Ok(DomainScope {
        if_domain,
        unless_domain,
    })
}

/// How many selectors ride in one `css-display-none` rule. Measured: a single
/// rule carrying 20,000 selectors (249 KB) compiles in 23 ms, so batching costs
/// nothing and saves tens of thousands of rules against the 150,000 ceiling.
/// Kept well under that so one unusable selector cannot take a whole continent
/// of hiding down with it.
const SELECTORS_PER_RULE: usize = 2_000;

// ---------------------------------------------------------------------------
// The conversion
// ---------------------------------------------------------------------------

/// Convert every source list into one ordered content-blocker ruleset.
///
/// Deterministic: the same inputs always produce the same bytes. Domain scopes
/// and selectors are sorted, sections are emitted in a fixed order, and
/// duplicates collapse in first-seen order.
pub fn convert(sources: &[Source<'_>]) -> Conversion {
    let mut report = Report::default();

    // Pass 1: collect $badfilter directives, which disable a filter written
    // elsewhere in the same corpus. They have to be known before any rule is
    // built, or a disabled filter ships anyway.
    let mut badfilters: BTreeSet<String> = BTreeSet::new();
    for source in sources {
        for line in source.text.lines() {
            let line = line.trim();
            if line.contains("badfilter") {
                let (pattern, options) = split_options(line);
                if let Some(options) = options {
                    let kept: Vec<&str> = options
                        .split(',')
                        .filter(|option| *option != "badfilter")
                        .collect();
                    let rebuilt = if kept.is_empty() {
                        pattern.to_string()
                    } else {
                        format!("{pattern}${}", kept.join(","))
                    };
                    if options.split(',').any(|option| option == "badfilter") {
                        badfilters.insert(rebuilt);
                    }
                }
            }
        }
    }

    // Sectioned accumulators. `sections[n]` holds the rules for `Section n`.
    let mut sections: Vec<Vec<Value>> = vec![Vec::new(); 5];
    let mut seen: BTreeSet<String> = BTreeSet::new();

    // Cosmetic accumulators, keyed so the output is grouped and deterministic.
    let mut generic_hide: BTreeSet<String> = BTreeSet::new();
    let mut domain_hide: BTreeMap<DomainScope, BTreeSet<String>> = BTreeMap::new();
    let mut generic_unhide: BTreeSet<String> = BTreeSet::new();
    let mut domain_unhide: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Procedural rules go to the userscript plane, keyed by bare domain (the
    // generated script matches a host to its rules itself).
    let mut procedural: BTreeMap<String, BTreeSet<ProceduralRule>> = BTreeMap::new();
    // Scriptlets take the same plane, the same key, and the same rule: a
    // filter with no domain is refused rather than run on every page.
    let mut scriptlets: BTreeMap<String, BTreeSet<ScriptletRule>> = BTreeMap::new();
    let mut scriptlet_unhide: Vec<(String, ScriptletRule)> = Vec::new();
    let mut take_scriptlet = |domains: &str, body: &str, report: &mut Report, line: &str| {
        // A scriptlet with no domain scope would patch page globals on EVERY
        // site in the browser. Nothing in the shipped corpus is written that
        // way, and one that is gets counted rather than paid for everywhere.
        if domains.is_empty() {
            report.drop(DropReason::Scriptlet, line);
            return;
        }
        let Some(rule) = parse_scriptlet(body) else {
            report.drop(DropReason::Scriptlet, line);
            return;
        };
        let mut landed = false;
        for entry in domains.split(',').map(str::trim) {
            if entry.is_empty() || entry.starts_with('~') {
                continue;
            }
            let Some(host) = translate_domain(entry) else {
                continue;
            };
            scriptlets
                .entry(host.trim_start_matches('*').to_string())
                .or_default()
                .insert(rule.clone());
            landed = true;
        }
        if landed {
            report.scriptlet_rules += 1;
        } else {
            report.drop(DropReason::Scriptlet, line);
        }
    };
    let mut take_procedural = |domains: &str, body: &str, report: &mut Report, line: &str| {
        // A procedural rule with no domain would run its text scan on every
        // page in the browser. Every one in the shipped corpus is scoped; an
        // unscoped one is refused rather than paid for everywhere.
        if domains.is_empty() {
            report.drop(DropReason::ProceduralSelector, line);
            return;
        }
        let Some(rule) = parse_procedural(body) else {
            report.drop(DropReason::ProceduralSelector, line);
            return;
        };
        let mut landed = false;
        for entry in domains.split(',').map(str::trim) {
            if entry.is_empty() || entry.starts_with('~') {
                continue;
            }
            // The generated script matches hosts itself, so it wants the bare
            // domain, not WebKit's `*`-prefixed spelling.
            let Some(host) = translate_domain(entry) else {
                continue;
            };
            procedural
                .entry(host.trim_start_matches('*').to_string())
                .or_default()
                .insert(rule.clone());
            landed = true;
        }
        if landed {
            report.cosmetic_procedural_rules += 1;
        } else {
            report.drop(DropReason::ProceduralSelector, line);
        }
    };

    for source in sources {
        for raw in source.text.lines() {
            report.source_lines += 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
                report.comments += 1;
                continue;
            }

            // Cosmetic filters are identified by their separator, which must be
            // found before the network path claims the line (a `##` line can
            // contain a `$`).
            if let Some((index, separator)) = COSMETIC_SEPARATORS
                .iter()
                .filter_map(|separator| line.find(separator).map(|index| (index, *separator)))
                .min_by_key(|(index, separator)| (*index, std::cmp::Reverse(separator.len())))
            {
                let domains = &line[..index];
                let body = &line[index + separator.len()..];
                match separator {
                    "#$#" | "#%#" | "#$?#" | "#@$#" | "#@$?#" => {
                        report.drop(DropReason::StyleOrSnippet, line);
                    }
                    "#?#" => take_procedural(domains, body, &mut report, line),
                    "#@#" | "#@?#" if body.starts_with("+js(") => {
                        // A scriptlet exception. Recorded and applied after
                        // every scriptlet is known, because a `#@#+js` may
                        // precede the `##+js` it cancels.
                        if let Some(rule) = parse_scriptlet(body) {
                            for entry in domains.split(',').map(str::trim) {
                                if entry.is_empty() {
                                    continue;
                                }
                                if let Some(host) = translate_domain(entry.trim_start_matches('~'))
                                {
                                    scriptlet_unhide.push((
                                        host.trim_start_matches('*').to_string(),
                                        rule.clone(),
                                    ));
                                }
                            }
                        } else {
                            report.drop(DropReason::Scriptlet, line);
                        }
                    }
                    "#@#" | "#@?#" => {
                        // Record the unhide; it is applied after every hide is
                        // known, because a `#@#` may precede its `##`.
                        if body.is_empty() {
                            report.drop(DropReason::UnprovableSelector, line);
                        } else if domains.is_empty() {
                            generic_unhide.insert(body.to_string());
                        } else {
                            for entry in domains.split(',').map(str::trim) {
                                if entry.is_empty() {
                                    continue;
                                }
                                if let Some(host) = translate_domain(entry.trim_start_matches('~'))
                                {
                                    domain_unhide
                                        .entry(host)
                                        .or_default()
                                        .insert(body.to_string());
                                }
                            }
                        }
                    }
                    "##" if body.starts_with("+js(") => {
                        take_scriptlet(domains, body, &mut report, line)
                    }
                    "##" if body.starts_with('^') => report.drop(DropReason::HtmlFiltering, line),
                    "##" => {
                        if PROCEDURAL_MARKERS.iter().any(|m| body.contains(m)) {
                            take_procedural(domains, body, &mut report, line);
                        } else if !selector_ok(body) {
                            report.drop(DropReason::UnprovableSelector, line);
                        } else if domains.is_empty() {
                            generic_hide.insert(body.to_string());
                        } else {
                            match parse_domain_scope(domains, &mut report.domain_entries_dropped) {
                                Ok(scope) => {
                                    domain_hide
                                        .entry(scope)
                                        .or_default()
                                        .insert(body.to_string());
                                }
                                Err(reason) => report.drop(reason, line),
                            }
                        }
                    }
                    _ => report.drop(DropReason::UnsupportedOption, line),
                }
                continue;
            }

            // Network filter.
            if badfilters.contains(line) {
                report.drop(DropReason::BadFilter, line);
                continue;
            }
            if line.contains("$badfilter") || line.ends_with(",badfilter") {
                // The directive itself is not a rule.
                report.comments += 1;
                continue;
            }
            match parse_network(line, &mut report.domain_entries_dropped) {
                Ok(rule) => {
                    let action = if rule.exception {
                        json!({ "type": "ignore-previous-rules" })
                    } else {
                        json!({ "type": "block" })
                    };
                    let value = json!({ "trigger": rule.trigger.to_json(), "action": action });
                    let key = value.to_string();
                    if !seen.insert(key) {
                        report.deduplicated += 1;
                        continue;
                    }
                    match rule.section {
                        Section::Block => report.network_block += 1,
                        Section::NetworkException => report.network_exception += 1,
                        Section::ImportantBlock => report.network_important += 1,
                        Section::DocumentException => report.document_exception += 1,
                        Section::Cosmetic => {}
                    }
                    sections[rule.section as usize].push(value);
                }
                Err(reason) => report.drop(reason, line),
            }
        }
    }

    // Apply the unhides now that every hide is known. A generic `#@#` removes
    // the selector outright; a domain-scoped one pulls the selector out of the
    // shared batch into its own rule with an `unless-domain`, which is the only
    // shape WebKit can express.
    for selector in &generic_unhide {
        if generic_hide.remove(selector) {
            report.cosmetic_unhide_applied += 1;
        }
        for selectors in domain_hide.values_mut() {
            if selectors.remove(selector) {
                report.cosmetic_unhide_applied += 1;
            }
        }
    }
    let mut carved: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (domain, selectors) in &domain_unhide {
        for selector in selectors {
            if generic_hide.contains(selector) {
                carved
                    .entry(selector.clone())
                    .or_default()
                    .insert(domain.clone());
            }
            for (scope, hides) in domain_hide.iter_mut() {
                if scope.if_domain.contains(domain) && hides.remove(selector) {
                    report.cosmetic_unhide_applied += 1;
                }
            }
        }
    }
    let mut cosmetic: Vec<Value> = Vec::new();
    for (selector, domains) in &carved {
        generic_hide.remove(selector);
        report.cosmetic_unhide_applied += 1;
        report.cosmetic_generic_selectors += 1;
        cosmetic.push(json!({
            "trigger": {
                "url-filter": ".*",
                "unless-domain": domains.iter().collect::<Vec<_>>(),
            },
            "action": { "type": "css-display-none", "selector": selector },
        }));
    }

    // Generic hides, batched. One rule per SELECTORS_PER_RULE selectors.
    let generic: Vec<&String> = generic_hide.iter().collect();
    report.cosmetic_generic_selectors += generic.len();
    for chunk in generic.chunks(SELECTORS_PER_RULE) {
        report.cosmetic_generic_rules += 1;
        cosmetic.push(json!({
            "trigger": { "url-filter": ".*" },
            "action": {
                "type": "css-display-none",
                "selector": chunk.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            },
        }));
    }
    // Domain-scoped hides, one rule per distinct domain scope.
    for (scope, selectors) in &domain_hide {
        if selectors.is_empty() {
            continue;
        }
        report.cosmetic_domain_selectors += selectors.len();
        for chunk in selectors
            .iter()
            .collect::<Vec<_>>()
            .chunks(SELECTORS_PER_RULE)
        {
            report.cosmetic_domain_rules += 1;
            let mut trigger = serde_json::Map::new();
            trigger.insert("url-filter".into(), json!(".*"));
            if !scope.if_domain.is_empty() {
                trigger.insert("if-domain".into(), json!(scope.if_domain));
            }
            if !scope.unless_domain.is_empty() {
                trigger.insert("unless-domain".into(), json!(scope.unless_domain));
            }
            cosmetic.push(json!({
                "trigger": Value::Object(trigger),
                "action": {
                    "type": "css-display-none",
                    "selector": chunk.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                },
            }));
        }
    }
    sections[Section::Cosmetic as usize] = cosmetic;

    // Assemble in section order, then enforce the ceiling. Cosmetic is trimmed
    // first because losing a hidden element degrades the page; losing a block
    // rule lets a tracker through, and losing an exception BREAKS a site.
    //
    // The bot-check guard is held back and appended AFTER the trim, and the
    // ceiling budget is reduced by its size, so a ruleset that arrives at the
    // limit can never be the one that drops it: `ignore-previous-rules` only
    // cancels what precedes it, so the guard is worth nothing unless it is last
    // AND present.
    let guard = bot_check_guard_rules();
    let budget = WEBKIT_RULE_CEILING - guard.len();
    let mut rules: Vec<Value> = Vec::new();
    for section in &sections {
        rules.extend(section.iter().cloned());
    }
    if rules.len() > budget {
        let over = rules.len() - budget;
        let cosmetic_len = sections[Section::Cosmetic as usize].len();
        let trim = over.min(cosmetic_len);
        sections[Section::Cosmetic as usize].truncate(cosmetic_len - trim);
        rules.clear();
        for section in &sections {
            rules.extend(section.iter().cloned());
        }
        for _ in 0..over {
            report.drop(DropReason::OverCeiling, "(cosmetic rule trimmed)");
        }
        rules.truncate(budget);
    }
    rules.extend(guard);
    report.emitted = rules.len();
    // `#@#+js(...)`: honoured last, because an exception may be written above
    // the filter it cancels. Dropping the whole domain would be wrong — the
    // exception names ONE scriptlet.
    for (host, rule) in scriptlet_unhide {
        if let Some(rules) = scriptlets.get_mut(&host)
            && rules.remove(&rule)
        {
            report.scriptlet_unhide_applied += 1;
            report.scriptlet_rules = report.scriptlet_rules.saturating_sub(1);
        }
    }
    scriptlets.retain(|_, rules| !rules.is_empty());
    Conversion {
        rules,
        procedural,
        scriptlets,
        report,
    }
}

/// ⭐ THE BOT-CHECK GUARD: OURS, appended after every upstream rule.
///
/// A bot check is the last thing on the web that may be blocked by accident.
/// When it is, the failure has no symptom a user can read: the login page comes
/// back, and comes back, and comes back, with nothing on screen saying an asset
/// was refused. The user hit exactly this on a login and filed a ticket with
/// Cloudflare rather than with us, which is the shape of a failure nobody can
/// attribute.
///
/// Audited on the shipped ruleset (146,817 rules, 2026-07-31): **nothing blocks
/// the challenge platform today.** Of the 79 rules mentioning cloudflare or
/// `cdn-cgi`, the four that touch a challenge are already `ignore-previous-rules`
/// (upstream's own allowlist), and the blocking ones are analytics on different
/// paths — `/cdn-cgi/rum`, `/cdn-cgi/beacon/`, `/cdn-cgi/zaraz/` — which stay
/// blocked and should. So this is not a fix for a live break; it is the lock
/// that stops the next list update from becoming one, and it costs two rules.
///
/// The two paths, and nothing wider:
///
/// - `/cdn-cgi/challenge-platform/` — the first-party orchestrator, JS detector
///   and its XHRs, served from the challenged site's OWN origin;
/// - `challenges.cloudflare.com` — Turnstile's script and its iframe document.
///
/// Deliberately NOT `/cdn-cgi/` wholesale: that would un-block the RUM and
/// beacon endpoints that have nothing to do with getting a user logged in.
fn bot_check_guard_rules() -> Vec<Value> {
    vec![
        json!({
            "trigger": { "url-filter": "/cdn-cgi/challenge-platform/" },
            "action": { "type": "ignore-previous-rules" },
        }),
        json!({
            "trigger": {
                "url-filter": "^[^:]+:(//)?([^/]+\\.)?challenges\\.cloudflare\\.com[^a-zA-Z0-9_%.-]"
            },
            "action": { "type": "ignore-previous-rules" },
        }),
    ]
}

/// The stem the generated cosmetic userscript is installed under, in the
/// catalog and on disk. Named once, here, beside the generator.
pub const COSMETIC_SCRIPT_STEM: &str = "cosmetic-filters";

/// The stem the generated scriptlet userscript is installed under.
pub const SCRIPTLET_SCRIPT_STEM: &str = "scriptlets";

/// The runtime library, embedded. It is a FUNCTION EXPRESSION so the node
/// harness can drive the real thing rather than a copy of it; see the header of
/// `assets/web-scriptlets/runtime.js`.
pub const SCRIPTLET_RUNTIME: &str = include_str!("../assets/web-scriptlets/runtime.js");

/// The markers the generated body wraps the runtime in, so the harness can cut
/// the real implementation out of the SHIPPED artefact and drive it with its
/// own rules. Testing a copy of the runtime would prove nothing about the file
/// the catalog serves.
pub const RUNTIME_BEGIN: &str = "ychrome-scriptlet-runtime:begin";
pub const RUNTIME_END: &str = "ychrome-scriptlet-runtime:end";

/// Render the scriptlet rules as a userscript body.
///
/// **The plane is reused, not reinvented.** Per-domain JS injection was named
/// as the blocker on `##+js(...)` for a whole release, and it is a matching
/// problem this repo already solved: `@match` lines scope the script to exactly
/// the domains that have rules, WebKit does the matching in the engine, and on
/// every other page the script does not exist. `cosmetic-filters.js` holds the
/// same contract.
///
/// **The world is MAIN, and that is not a detail.** A scriptlet's whole job is
/// to edit page globals — `window.open`, `JSON.parse`, a property the page's
/// own script reads. In an isolated world every one of those edits is invisible
/// to the page, and the script would run, report success, and change nothing.
/// That exact mistake cost this project a release with `youtube-adblock`
/// (docs/adblock.md §6), and a scriptlet plane is strictly more exposed to it.
///
/// Deterministic by construction: `BTreeMap` of `BTreeSet`, serialized as JSON.
pub fn generate_scriptlet_script(
    scriptlets: &BTreeMap<String, BTreeSet<ScriptletRule>>,
    version: &str,
) -> String {
    // THE RULES ARE INTERNED. One filter names dozens of domains — a single
    // `set-cookie` line rides on 2,191 of them — so a domain-keyed table of
    // whole invocations repeats the same row thousands of times. Measured on
    // the 2026-07-31 corpus: 8,736 instances over 2,428 DISTINCT rules, and
    // spelling each one out cost 845 KB of the generated file. `TABLE` holds
    // each rule once; `RULES` maps a domain to indices into it.
    //
    // Deterministic: the table is built by walking a `BTreeMap` of `BTreeSet`s,
    // so a rule's index is a pure function of the corpus.
    let mut table: Vec<Value> = Vec::new();
    let mut index_of: BTreeMap<&ScriptletRule, usize> = BTreeMap::new();
    let mut payload: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (domain, rules) in scriptlets {
        let mut indices = Vec::with_capacity(rules.len());
        for rule in rules {
            let next = index_of.len();
            let at = *index_of.entry(rule).or_insert_with(|| {
                let mut row = vec![Value::from(rule.name)];
                row.extend(rule.args.iter().map(|arg| Value::from(arg.as_str())));
                table.push(Value::Array(row));
                next
            });
            indices.push(at);
        }
        payload.insert(domain.as_str(), indices);
    }
    let table = serde_json::to_string(&table).unwrap_or_else(|_| "[]".to_string());
    let matches: String = scriptlets
        .keys()
        .map(|domain| format!("// @match       *://*.{domain}/*\n"))
        .collect();
    let rule_count: usize = scriptlets.values().map(BTreeSet::len).sum();
    let domain_count = scriptlets.len();
    let payload = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    let runtime = SCRIPTLET_RUNTIME;
    let begin = RUNTIME_BEGIN;
    let end = RUNTIME_END;

    format!(
        "// ==UserScript==\n\
         // @name        ychrome scriptlets (GENERATED)\n\
         // @version     {version}\n\
         {matches}\
         // @world       main\n\
         // @run-at      document-start\n\
         // ==/UserScript==\n\
         // GENERATED by `ychrome adblock update` from the upstream filter lists.\n\
         // DO NOT EDIT: regenerate it. {rule_count} scriptlet invocations over\n\
         // {domain_count} domains.\n\
         //\n\
         // `##+js(name, args...)` asks the blocker to run a named piece of\n\
         // JavaScript on a page. A declarative content blocker cannot: it decides\n\
         // yes or no about a request and never runs anything. So the filters become\n\
         // DATA — a domain-keyed table of invocations — and the library below runs\n\
         // them. A new site costs a line in an upstream list; only a new PRIMITIVE\n\
         // costs code.\n\
         //\n\
         // @world main is load-bearing: these edit the page's own globals, and in\n\
         // an isolated world every edit would be invisible to the page while the\n\
         // script reported success.\n\
         (function () {{\n\
         \x20   'use strict';\n\
         \x20   if (window.__yggScriptlets) return;\n\
         \x20   var TABLE = {table};\n\
         \x20   var RULES = {payload};\n\
         \x20   /* {begin} */\n\
         \x20   var run = {runtime};\n\
         \x20   /* {end} */\n\
         \x20   run(RULES, TABLE, window);\n\
         }})();\n"
    )
}

/// Render the procedural rules as a userscript body.
///
/// This is the SECOND half of cosmetic filtering and it reuses the userscript
/// plane rather than inventing an injection path: `@match` lines scope it to
/// exactly the domains that have rules, so WebKit does the matching in the
/// engine and the script costs literally nothing on every other page in the
/// browser. That scoping is the whole reason a text-scanning MutationObserver
/// is affordable at all.
///
/// Deterministic by construction: the rules arrive in a `BTreeMap` of
/// `BTreeSet`s and the payload is serialized as JSON, so the same corpus always
/// renders the same bytes.
pub fn generate_cosmetic_script(
    procedural: &BTreeMap<String, BTreeSet<ProceduralRule>>,
    version: &str,
) -> String {
    // domain -> [["t"|"s", prefix, arg], ...]. A compact positional shape: the
    // payload is the bulk of the file and a verbose one would triple it.
    let payload: BTreeMap<&str, Vec<Value>> = procedural
        .iter()
        .map(|(domain, rules)| {
            (
                domain.as_str(),
                rules
                    .iter()
                    .map(|rule| match rule {
                        ProceduralRule::HasText { prefix, text } => json!(["t", prefix, text]),
                        ProceduralRule::Style { prefix, decls } => json!(["s", prefix, decls]),
                    })
                    .collect(),
            )
        })
        .collect();
    let matches: String = procedural
        .keys()
        .map(|domain| format!("// @match       *://*.{domain}/*\n"))
        .collect();
    let rule_count: usize = procedural.values().map(BTreeSet::len).sum();

    format!(
        "// ==UserScript==\n\
         // @name        ychrome cosmetic filters (GENERATED)\n\
         // @version     {version}\n\
         {matches}\
         // @world       isolated\n\
         // @run-at      document-start\n\
         // ==/UserScript==\n\
         // GENERATED by `ychrome adblock update` from the upstream filter lists.\n\
         // DO NOT EDIT: regenerate it. {rule_count} procedural cosmetic rules over\n\
         // {domain_count} domains.\n\
         //\n\
         // These are the cosmetic filters WebKit's content blocker cannot express.\n\
         // `css-display-none` takes a CSS selector and can only hide; `:has-text()`\n\
         // is not CSS, and `:style()` sets a property rather than hiding. Measured:\n\
         // WebKit drops such a rule SILENTLY, compile still succeeding, which is the\n\
         // silent-degradation shape this whole lane exists to close.\n\
         //\n\
         // The @match list above is the performance story. WebKit matches in the\n\
         // engine, so on any page not named there this script does not exist.\n\
         (function () {{\n\
         \x20   'use strict';\n\
         \x20   if (window.__yggCosmetic) return;\n\
         \x20   window.__yggCosmetic = true;\n\
         \x20   var RULES = {payload};\n\
         \x20   // Longest-suffix host match, the same rule webzoom uses: a rule for\n\
         \x20   // `example.com` covers `www.example.com`, and a bare TLD never matches.\n\
         \x20   var host = String(location.hostname || '').toLowerCase();\n\
         \x20   var mine = [];\n\
         \x20   for (var key in RULES) {{\n\
         \x20       if (host === key || (host.length > key.length\n\
         \x20           && host.slice(-(key.length + 1)) === '.' + key)) {{\n\
         \x20           mine = mine.concat(RULES[key]);\n\
         \x20       }}\n\
         \x20   }}\n\
         \x20   if (!mine.length) return;\n\
         \x20   var state = {{ hidden: 0, styled: 0, passes: 0 }};\n\
         \x20   window.__yggCosmeticState = state;\n\
         \x20\n\
         \x20   function apply() {{\n\
         \x20       state.passes += 1;\n\
         \x20       for (var i = 0; i < mine.length; i++) {{\n\
         \x20           var rule = mine[i];\n\
         \x20           var nodes;\n\
         \x20           try {{\n\
         \x20               nodes = document.querySelectorAll(rule[1]);\n\
         \x20           }} catch (e) {{ continue; }}\n\
         \x20           for (var j = 0; j < nodes.length; j++) {{\n\
         \x20               var el = nodes[j];\n\
         \x20               if (rule[0] === 't') {{\n\
         \x20                   // :has-text(): hide when the element's text carries it.\n\
         \x20                   if (el.__yggHidden) continue;\n\
         \x20                   var text = el.textContent || '';\n\
         \x20                   if (text.toLowerCase().indexOf(rule[2].toLowerCase()) === -1) continue;\n\
         \x20                   el.__yggHidden = true;\n\
         \x20                   el.style.setProperty('display', 'none', 'important');\n\
         \x20                   state.hidden += 1;\n\
         \x20               }} else {{\n\
         \x20                   // :style(): apply the declarations verbatim. This is how a\n\
         \x20                   // consent banner's `overflow:hidden` scroll lock comes off.\n\
         \x20                   if (el.__yggStyled === rule[2]) continue;\n\
         \x20                   el.__yggStyled = rule[2];\n\
         \x20                   var decls = rule[2].split(';');\n\
         \x20                   for (var k = 0; k < decls.length; k++) {{\n\
         \x20                       var at = decls[k].indexOf(':');\n\
         \x20                       if (at === -1) continue;\n\
         \x20                       var name = decls[k].slice(0, at).trim();\n\
         \x20                       var value = decls[k].slice(at + 1).trim();\n\
         \x20                       var important = '';\n\
         \x20                       if (value.slice(-10).toLowerCase() === '!important') {{\n\
         \x20                           value = value.slice(0, -10).trim();\n\
         \x20                           important = 'important';\n\
         \x20                       }}\n\
         \x20                       if (name) el.style.setProperty(name, value, important);\n\
         \x20                   }}\n\
         \x20                   state.styled += 1;\n\
         \x20               }}\n\
         \x20           }}\n\
         \x20       }}\n\
         \x20   }}\n\
         \x20\n\
         \x20   // Coalesced: a page that rewrites its DOM in a loop must not make this\n\
         \x20   // run in that loop. One pass per animation frame at most.\n\
         \x20   var queued = false;\n\
         \x20   function schedule() {{\n\
         \x20       if (queued) return;\n\
         \x20       queued = true;\n\
         \x20       var run = function () {{ queued = false; apply(); }};\n\
         \x20       if (typeof requestAnimationFrame === 'function') requestAnimationFrame(run);\n\
         \x20       else setTimeout(run, 50);\n\
         \x20   }}\n\
         \x20\n\
         \x20   function start() {{\n\
         \x20       apply();\n\
         \x20       new MutationObserver(schedule).observe(document.documentElement,\n\
         \x20           {{ childList: true, subtree: true }});\n\
         \x20   }}\n\
         \x20   if (document.readyState === 'loading') {{\n\
         \x20       document.addEventListener('DOMContentLoaded', start, {{ once: true }});\n\
         \x20   }} else {{\n\
         \x20       start();\n\
         \x20   }}\n\
         }})();\n",
        version = version,
        matches = matches,
        payload = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        rule_count = rule_count,
        domain_count = procedural.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse_network` with the entry counter the tests do not care about.
    fn parse_network_for_test(line: &str) -> Result<NetworkRule, DropReason> {
        let mut dropped = 0;
        parse_network(line, &mut dropped)
    }

    fn convert_one(text: &str) -> Conversion {
        convert(&[Source { name: "test", text }])
    }

    // THE GATE. Every construct here was measured against WebKitGTK 2.52.5 by
    // compiling a one-rule ruleset through WebKitUserContentFilterStore. A
    // regression that starts accepting an alternation ships a ruleset that
    // fails to compile, which turns ad blocking OFF entirely — the loudest
    // possible version of the silent-degradation bug this lane is closing.
    #[test]
    fn the_regex_gate_matches_what_webkit_measurably_accepts() {
        for accepted in [
            "^https?://ads\\.example\\.net/",
            "^[^:]+:(//)?([^/]+\\.)?ads\\.example\\.net[^a-zA-Z0-9_%.-]",
            "^https?://[a-z0-9]+\\.example\\.net/",
            "^https?://a+\\.example\\.net/",
            "^https?://(?:ads)\\.example\\.net/",
            "^https?://ads\\.example\\.net/x$",
            ".*",
            "ads\\.example\\.net",
        ] {
            assert!(webkit_regex_ok(accepted), "{accepted} must be accepted");
        }
        for rejected in [
            "^https?://(a|b)\\.example\\.net/",  // no alternation
            "^https?://a{2,4}\\.example\\.net/", // no counted repetition
            "^https?://(?=ads)x\\.net/",         // no lookahead
            "^https?://\\w+\\.example\\.net/",   // no \w
            "^https?://\\d+\\.example\\.net/",   // no \d
            "^https?://ads\\.net([/:?#]|$)",     // alternation again
            "^https?://a^b/",                    // ^ only at position 0
            "",                                  // an empty url-filter is invalid
            "^https?://(unbalanced/",
            "^https?://[unbalanced/",
            "^https?://ads\\.tëst/", // measured: non-ASCII is rejected outright
        ] {
            assert!(
                !webkit_regex_ok(rejected),
                "{rejected} must be REJECTED — emitting it fails the whole compile"
            );
        }
    }

    // The three anchors, and the separator that cannot be expressed exactly.
    #[test]
    fn patterns_translate_with_the_anchors_abp_declares() {
        assert_eq!(
            translate_pattern("||ads.example.net^").unwrap(),
            "^[^:]+:(//)?([^/]+\\.)?ads\\.example\\.net[^a-zA-Z0-9_%.-]"
        );
        assert_eq!(
            translate_pattern("|http://ads.example.net/").unwrap(),
            "^http://ads\\.example\\.net/"
        );
        assert_eq!(translate_pattern("/banner.gif|").unwrap(), "/banner\\.gif$");
        assert_eq!(
            translate_pattern("/ads/*/banner").unwrap(),
            "/ads/.*/banner"
        );
        // Everything emitted must itself pass the gate.
        for pattern in [
            "||ads.example.net^",
            "|http://ads.example.net/",
            "/ads/*/banner",
            "||a.example.net/x?y=1",
        ] {
            let filter = translate_pattern(pattern).expect("translates");
            assert!(webkit_regex_ok(&filter), "{pattern} -> {filter}");
        }
    }

    // A filter that reduces to "match everything" would block the web —
    // UNLESS something else narrows it. `$popup,domain=a.test` with an empty
    // pattern is a real and common filter (381 of them in the shipped corpus),
    // and refusing those was throwing away work the lists had done.
    #[test]
    fn a_filter_that_matches_everything_is_refused_unless_something_narrows_it() {
        for wide in ["*", "**", "|*|", "$third-party"] {
            assert_eq!(
                parse_network_for_test(wide).err(),
                Some(DropReason::TooBroad),
                "{wide} matches every URL with nothing to narrow it"
            );
        }
        for narrowed in ["$popup,domain=a.test", "*$script,domain=a.test"] {
            let rule = parse_network_for_test(narrowed)
                .unwrap_or_else(|reason| panic!("{narrowed} refused as {reason:?}"));
            assert!(matches_everything(&rule.trigger.url_filter));
            assert!(!rule.trigger.if_domain.is_empty() || !rule.trigger.resource_types.is_empty());
        }
    }

    // A domain list is not all-or-nothing. Dropping an entry WebKit cannot take
    // narrows a positive filter (fine, and much better than losing the other
    // 249 domains beside it) and WIDENS a negative one (never allowed).
    // Non-ASCII is not a style question: measured, WebKit answers "Domains must
    // be lower case ASCII" and fails the WHOLE ruleset.
    #[test]
    fn an_untranslatable_domain_entry_narrows_but_never_widens() {
        let mut dropped = 0;
        let rule = parse_network(
            "||ads.test^$domain=good.test|tripadvisor.*|other.test",
            &mut dropped,
        )
        .expect("the translatable entries survive");
        assert_eq!(rule.trigger.if_domain, vec!["*good.test", "*other.test"]);
        assert_eq!(dropped, 1, "the wildcard entry must be COUNTED, not hidden");

        assert_eq!(
            parse_network_for_test("||ads.test^$domain=~exempt.*").err(),
            Some(DropReason::UntranslatableDomain),
            "dropping a NEGATIVE entry would run the filter on a site its author excluded"
        );
        assert_eq!(
            parse_network_for_test("||ads.test^$domain=only.*").err(),
            Some(DropReason::UntranslatableDomain),
            "an empty positive list would apply the filter everywhere"
        );
        assert_eq!(
            translate_domain("Example.COM").as_deref(),
            Some("*example.com")
        );
        assert_eq!(
            translate_domain("bücher.test"),
            None,
            "a non-ASCII domain fails the entire ruleset compile — measured"
        );
    }

    // A /regex/ filter passes through only if it is already in the dialect.
    #[test]
    fn regex_filters_pass_through_only_when_the_dialect_allows() {
        assert_eq!(
            translate_pattern("/ads[0-9]+\\.js/").unwrap(),
            "ads[0-9]+\\.js"
        );
        assert_eq!(
            translate_pattern("/(ad|banner)\\.js/"),
            Err(DropReason::UnsupportedRegex),
            "alternation must be refused, not shipped into a failing compile"
        );
    }

    // The options that map, mapped. Each field here is one WebKit trigger key
    // measured valid on 2.52.5.
    #[test]
    fn network_options_map_onto_the_trigger_keys() {
        let rule = parse_network_for_test("||ads.example.net^$third-party,script,image").unwrap();
        assert_eq!(rule.trigger.load_type, Some("third-party"));
        assert_eq!(
            rule.trigger.resource_types.iter().collect::<Vec<_>>(),
            vec![&"image", &"script"]
        );
        assert_eq!(rule.section, Section::Block);

        let first = parse_network_for_test("||ads.example.net^$~third-party").unwrap();
        assert_eq!(first.trigger.load_type, Some("first-party"));

        let domained = parse_network_for_test("||ads.example.net^$domain=a.test|b.test").unwrap();
        assert_eq!(domained.trigger.if_domain, vec!["*a.test", "*b.test"]);
        assert!(domained.trigger.unless_domain.is_empty());

        let excluded = parse_network_for_test("||ads.example.net^$domain=~a.test").unwrap();
        assert_eq!(excluded.trigger.unless_domain, vec!["*a.test"]);

        let framed = parse_network_for_test("||ads.example.net^$subdocument").unwrap();
        assert_eq!(framed.trigger.load_context, Some("child-frame"));

        let cased = parse_network_for_test("||ads.example.net/A^$match-case").unwrap();
        assert!(cased.trigger.case_sensitive);
    }

    // A negated type becomes the COMPLEMENT over subresource types only.
    // Including `document` would turn "block everything except scripts" into
    // "and also refuse to load the page", which is a broken browser.
    #[test]
    fn a_negated_type_never_widens_to_the_document_itself() {
        let rule = parse_network_for_test("||ads.example.net^$~script").unwrap();
        assert!(!rule.trigger.resource_types.contains("script"));
        assert!(rule.trigger.resource_types.contains("image"));
        assert!(
            !rule.trigger.resource_types.contains("document"),
            "a negated type must never block the page itself"
        );
        assert!(!rule.trigger.resource_types.contains("popup"));
    }

    // Every option with no declarative equivalent must be NAMED, not guessed
    // at. `$redirect=` in particular: WebKit has no redirect action, so a
    // converter that quietly emitted a plain block would substitute "break the
    // page" for "swap in a stub".
    #[test]
    fn impossible_options_are_refused_with_the_reason() {
        for (filter, reason) in [
            ("||a.test/ads.js$redirect=noopjs", DropReason::Redirect),
            ("||a.test^$redirect-rule=noopjs", DropReason::Redirect),
            (
                "||a.test^$removeparam=utm_source",
                DropReason::RequestRewrite,
            ),
            ("||a.test^$replace=/a/b/", DropReason::RequestRewrite),
            (
                "||a.test^$csp=script-src 'none'",
                DropReason::HeaderInjection,
            ),
            (
                "||a.test^$permissions=camera=()",
                DropReason::HeaderInjection,
            ),
            (
                "||a.test^$domain=a.test|~b.test",
                DropReason::MixedDomainOption,
            ),
            ("||a.test^$domain=*.test", DropReason::UntranslatableDomain),
            ("||a.test^$webrtc", DropReason::UnsupportedOption),
            ("||a.test^$header=x:y", DropReason::UnsupportedOption),
        ] {
            assert_eq!(
                parse_network_for_test(filter).err(),
                Some(reason),
                "{filter} must be refused as {reason:?}"
            );
        }
    }

    // ORDER IS SEMANTICS. `ignore-previous-rules` cancels what came before it,
    // so an exception that lands before its block does nothing, and a
    // subresource exception that lands after the cosmetic rules silently
    // switches off element hiding for the whole page.
    #[test]
    fn the_sections_are_emitted_in_the_order_that_makes_them_mean_what_they_say() {
        let conversion = convert_one(
            "||ads.test^\n\
             @@||ads.test/ok.js$script\n\
             ||tracker.test^$important\n\
             ##.generic-ad\n\
             @@||whitelisted.test^$document\n",
        );
        let kinds: Vec<&str> = conversion
            .rules
            .iter()
            .map(|rule| rule["action"]["type"].as_str().unwrap())
            .collect();
        // The upstream sections, in order — then OUR bot-check guard, which is
        // appended after all of them (see `bot_check_guard_rules`). It is part
        // of the ordering contract, not noise on the end: an allowlist entry
        // that is not last does not protect what follows it.
        let mut expected = vec![
            "block",                 // 1. the block
            "ignore-previous-rules", // 2. the subresource exception
            "block",                 // 3. the $important block, past the exceptions
            "css-display-none",      // 4. cosmetic, after subresource exceptions
            "ignore-previous-rules", // 5. the document exception, last of the upstream ones
        ];
        expected.extend(
            bot_check_guard_rules()
                .iter()
                .map(|_| "ignore-previous-rules"),
        );
        assert_eq!(kinds, expected);
        assert_eq!(conversion.report.network_block, 1);
        assert_eq!(conversion.report.network_exception, 1);
        assert_eq!(conversion.report.network_important, 1);
        assert_eq!(conversion.report.document_exception, 1);
    }

    // Cosmetic: generic selectors batch into one rule, domain-scoped ones group
    // by their domain set, and `if-domain` carries the scope. This is the half
    // the old ruleset had EIGHT selectors of.
    #[test]
    fn cosmetic_filters_batch_generically_and_group_by_domain() {
        let conversion = convert_one(
            "##.ad-one\n\
             ##.ad-two\n\
             a.test,b.test##.site-ad\n\
             a.test,b.test##.site-ad-two\n\
             c.test##.other\n",
        );
        let cosmetic: Vec<&Value> = conversion
            .rules
            .iter()
            .filter(|rule| rule["action"]["type"] == "css-display-none")
            .collect();
        assert_eq!(cosmetic.len(), 3, "one generic batch + two domain scopes");
        let generic = cosmetic
            .iter()
            .find(|rule| rule["trigger"].get("if-domain").is_none())
            .expect("a generic batch");
        assert_eq!(generic["action"]["selector"], ".ad-one, .ad-two");
        let scoped = cosmetic
            .iter()
            .find(|rule| rule["trigger"]["if-domain"] == json!(["*a.test", "*b.test"]))
            .expect("the a.test+b.test scope");
        assert_eq!(scoped["action"]["selector"], ".site-ad, .site-ad-two");
        assert_eq!(conversion.report.cosmetic_generic_selectors, 2);
        assert_eq!(conversion.report.cosmetic_domain_selectors, 3);
    }

    // A procedural selector is not CSS. WebKit drops it SILENTLY (measured:
    // the compile still succeeds), which is why it must be counted here — a
    // number in a report is the only thing standing between the user and a
    // rule they think is running.
    #[test]
    fn procedural_and_scriptlet_filters_are_counted_never_silently_dropped() {
        let conversion = convert_one(
            "a.test##div:upward(2)\n\
             a.test#?#div:xpath(//div)\n\
             a.test##+js(trusted-click-element, .accept)\n\
             a.test#$#body { display: block; }\n\
             a.test##^script:has-text(ads)\n",
        );
        let dropped = &conversion.report.dropped;
        assert_eq!(dropped.get("procedural-selector"), Some(&2));
        // A scriptlet the runtime does not implement is still COUNTED. The
        // plane existing is not the same as the plane covering everything, and
        // collapsing the two would hide the remaining gap.
        assert_eq!(dropped.get("scriptlet"), Some(&1));
        assert_eq!(dropped.get("style-or-snippet"), Some(&1));
        assert_eq!(dropped.get("html-filtering"), Some(&1));
        assert_eq!(conversion.report.dropped_total(), 5);
        // And each one keeps a verbatim sample, so the report names what it
        // refused rather than only counting it.
        assert!(
            conversion.report.samples["scriptlet"][0].contains("+js(trusted-click-element"),
            "a drop must name the filter verbatim"
        );
    }

    // THE SCRIPTLET PLANE. `##+js(...)` was counted as impossible for a whole
    // release ("needs a surrogate library and per-domain JS injection"); the
    // per-domain injection was the userscript plane all along, and the library
    // is `assets/web-scriptlets/runtime.js`.
    #[test]
    fn implemented_scriptlets_route_to_the_userscript_plane() {
        let conversion = convert_one(
            "a.test,b.test##+js(set-constant, adsEnabled, false)\n\
             a.test##+js(aopr, blockAdBlock)\n\
             a.test##+js(no-such-scriptlet-exists, x)\n\
             ##+js(set-constant, everywhere, true)\n",
        );
        assert_eq!(conversion.report.scriptlet_rules, 2);
        // Aliases resolve to ONE canonical name, so `aopr` and
        // `abort-on-property-read` can never become two rules for one filter.
        let a = &conversion.scriptlets["a.test"];
        assert!(a.contains(&ScriptletRule {
            name: "set-constant",
            args: vec!["adsEnabled".into(), "false".into()],
        }));
        assert!(a.contains(&ScriptletRule {
            name: "abort-on-property-read",
            args: vec!["blockAdBlock".into()],
        }));
        assert_eq!(conversion.scriptlets["b.test"].len(), 1);
        // An unimplemented name and an UNSCOPED filter are both refused. The
        // second matters most: a scriptlet with no domain would patch page
        // globals on every site in the browser.
        assert_eq!(conversion.report.dropped.get("scriptlet"), Some(&2));
    }

    // `#@#+js(...)` disables ONE scriptlet on a domain, not the domain. It is
    // applied after the whole corpus is read, because an exception is regularly
    // written above the filter it cancels.
    #[test]
    fn a_scriptlet_exception_removes_exactly_one_rule() {
        let conversion = convert_one(
            "a.test#@#+js(set-constant, adsEnabled, false)\n\
             a.test,b.test##+js(set-constant, adsEnabled, false)\n\
             a.test##+js(aopr, blockAdBlock)\n",
        );
        assert_eq!(conversion.report.scriptlet_unhide_applied, 1);
        let a = &conversion.scriptlets["a.test"];
        assert_eq!(a.len(), 1, "only the excepted rule is gone");
        assert_eq!(a.iter().next().unwrap().name, "abort-on-property-read");
        assert_eq!(
            conversion.scriptlets["b.test"].len(),
            1,
            "the exception names one domain and must not reach another"
        );
    }

    // The generated body is the artefact the catalog ships, so its SHAPE is a
    // contract: main world, one @match per domain, the runtime between its
    // markers, and rules interned rather than repeated.
    #[test]
    fn the_generated_scriptlet_body_is_scoped_interned_and_main_world() {
        let conversion = convert_one(
            "a.test,b.test##+js(set-constant, adsEnabled, false)\n\
             b.test##+js(aopr, blockAdBlock)\n",
        );
        let body = generate_scriptlet_script(&conversion.scriptlets, "1.0.0");
        assert!(body.contains("// @world       main"));
        assert!(body.contains("// @match       *://*.a.test/*"));
        assert!(body.contains("// @match       *://*.b.test/*"));
        assert!(body.contains(RUNTIME_BEGIN) && body.contains(RUNTIME_END));
        // Interning: the shared rule appears ONCE in the table even though two
        // domains use it. Spelling it out per domain cost 845 KB on the real
        // corpus (8,736 instances over 2,428 distinct rules).
        assert_eq!(
            body.matches("[\"set-constant\",\"adsEnabled\",\"false\"]")
                .count(),
            1,
            "a rule shared by two domains must be stored once and referenced twice"
        );
        // Deterministic: same corpus, same bytes.
        assert_eq!(
            body,
            generate_scriptlet_script(&conversion.scriptlets, "1.0.0")
        );
    }

    // The order of `SCRIPTLETS` is a CLAIM — "highest coverage first" is why
    // this list is the length it is and why `trusted-click-element` is missing
    // from it. A claim a reader cannot check is decoration.
    #[test]
    fn scriptlet_coverage_is_ordered_by_what_it_unlocks() {
        let mut previous = usize::MAX;
        for entry in SCRIPTLETS {
            assert!(
                entry.filters <= previous,
                "{:?} ({} filters) is listed after something smaller — the table claims to be \
                 ordered by measured coverage",
                entry.canonical,
                entry.filters
            );
            previous = entry.filters;
        }
        // 3,954 of the corpus's 5,057 `##+js(...)` filters, measured 2026-07-31.
        // A change that moves this number should move docs/adblock.md §5 with it.
        let total: usize = SCRIPTLETS.iter().map(|entry| entry.filters).sum();
        assert_eq!(total, 3954);
    }

    // The argument grammar, which is positional and has exactly one escape.
    #[test]
    fn scriptlet_arguments_parse_the_way_the_lists_write_them() {
        let rule = parse_scriptlet("+js(set-constant, a.b.c, noopFunc)").expect("parses");
        assert_eq!(rule.name, "set-constant");
        assert_eq!(rule.args, vec!["a.b.c", "noopFunc"]);
        // `\,` is a literal comma — the only escape the syntax has, and the
        // reason a regex argument can contain one at all.
        let rule = parse_scriptlet("+js(nostif, /ad\\,break/, 1000)").expect("parses");
        assert_eq!(rule.name, "no-setTimeout-if");
        assert_eq!(rule.args, vec!["/ad,break/", "1000"]);
        // Trailing empty positionals carry no meaning; keeping them would make
        // two spellings of one filter into two rules.
        assert_eq!(
            parse_scriptlet("+js(aopr, x, , )").expect("parses").args,
            vec!["x"]
        );
        // An unimplemented name is refused HERE, so the caller counts it rather
        // than shipping a rule nothing will run.
        assert!(parse_scriptlet("+js(trusted-click-element, .ok)").is_none());
        assert!(parse_scriptlet("##.not-a-scriptlet").is_none());
    }

    // THE OTHER HALF OF COSMETIC FILTERING. `:has-text()` and `:style()` are
    // not CSS, so WebKit drops them SILENTLY (measured: the compile still
    // succeeds). 90% of the corpus's procedural rules are these two, so they go
    // to the userscript plane instead of the count column.
    #[test]
    fn the_two_procedural_forms_that_matter_route_to_the_userscript_plane() {
        let conversion = convert_one(
            "a.test,b.test##div.promo:has-text(Sponsored)\n\
             a.test#?#body.locked:style(overflow: auto !important;)\n\
             a.test##nav:-abp-contains(Advertisement)\n",
        );
        assert_eq!(conversion.report.cosmetic_procedural_rules, 3);
        let a = conversion
            .procedural
            .get("a.test")
            .expect("a.test has procedural rules");
        assert!(a.contains(&ProceduralRule::HasText {
            prefix: "div.promo".to_string(),
            text: "Sponsored".to_string(),
        }));
        assert!(a.contains(&ProceduralRule::Style {
            prefix: "body.locked".to_string(),
            decls: "overflow: auto !important;".to_string(),
        }));
        assert!(
            conversion.procedural.contains_key("b.test"),
            "every domain a scoped rule names must get it"
        );
        // And none of it leaked into the content blocker, where WebKit would
        // have discarded it without a word.
        assert!(
            !conversion
                .rules
                .iter()
                .any(|rule| rule["action"]["selector"]
                    .as_str()
                    .is_some_and(|sel| sel.contains(":has-text"))),
            "a procedural selector reached the content blocker"
        );
    }

    // NESTING is where a naive splitter does damage: `a:has(p:has-text(X))`
    // means "an <a> containing a <p> that says X", and treating the prefix
    // `a:has(p` as the thing to hide would hide every such <a> on the page.
    // Refusing is the only honest answer.
    #[test]
    fn a_nested_or_unimplemented_procedural_form_is_refused_not_guessed_at() {
        assert!(parse_procedural("a[role=\"button\"]:has(p:has-text(Sponsored))").is_none());
        assert!(
            parse_procedural(":has-text(Sponsored)").is_none(),
            "no prefix"
        );
        assert!(parse_procedural("div:has-text()").is_none(), "no argument");
        assert!(
            parse_procedural("div:has-text(/ad[0-9]+/)").is_none(),
            "a /regex/ argument is uBO's other language"
        );
        assert!(parse_procedural("div:upward(2)").is_none());
        assert!(parse_procedural("div:xpath(//x)").is_none());
        // …and the ones that ARE implemented still parse.
        assert!(parse_procedural("div.a:has-text(Ad)").is_some());
        assert!(parse_procedural("body:style(overflow: auto)").is_some());
    }

    // The generated script is a build artefact this repo commits, so it must be
    // byte-stable for a given corpus, and it must declare the placement that
    // makes it affordable: @match-scoped to exactly the domains with rules, so
    // WebKit skips it entirely everywhere else.
    #[test]
    fn the_generated_cosmetic_script_is_deterministic_and_scoped() {
        let corpus =
            "z.test##div:has-text(Z)\na.test##div:has-text(A)\na.test#?#p:style(color: red)\n";
        let first = convert_one(corpus);
        let second = convert_one(corpus);
        let one = generate_cosmetic_script(&first.procedural, "1.20260731");
        let two = generate_cosmetic_script(&second.procedural, "1.20260731");
        assert_eq!(one, two, "the generated script must be byte-stable");

        let parsed = crate::userscript::parse(&one);
        assert_eq!(
            parsed.version,
            crate::userscript::ScriptVersion::parse("1.20260731"),
            "the generated script must carry the version the reconciler compares"
        );
        assert_eq!(
            parsed.matches,
            vec!["*://*.a.test/*".to_string(), "*://*.z.test/*".to_string()],
            "the @match list is what keeps this off every other page in the browser"
        );
        assert_eq!(parsed.world, crate::userscript::ScriptWorld::Isolated);
        assert!(
            parsed.untranslatable_includes.is_empty(),
            "a generated script that the promotion gate refuses would ship nowhere"
        );
        // The payload really carries the rules, not an empty object.
        assert!(one.contains("[\"t\",\"div\",\"A\"]"));
        assert!(one.contains("[\"s\",\"p\",\"color: red\"]"));
    }

    // `#@#` is an un-hide. Honouring it matters: leaving the selector in place
    // hides something the list's own authors said to leave alone.
    #[test]
    fn an_unhide_removes_the_selector_it_names() {
        // Generic unhide: the selector goes away entirely.
        let conversion = convert_one("##.shared-ad\n##.kept\n#@#.shared-ad\n");
        let selectors: Vec<&str> = conversion
            .rules
            .iter()
            .filter_map(|rule| rule["action"]["selector"].as_str())
            .collect();
        assert_eq!(selectors, vec![".kept"]);
        assert_eq!(conversion.report.cosmetic_unhide_applied, 1);

        // Domain unhide of a GENERIC selector: carved out into its own rule
        // with an unless-domain, which is the only shape WebKit can express.
        let carved = convert_one("##.shared-ad\nexempt.test#@#.shared-ad\n");
        let rule = carved
            .rules
            .iter()
            .find(|rule| rule["action"]["selector"] == ".shared-ad")
            .expect("the carved rule");
        assert_eq!(rule["trigger"]["unless-domain"], json!(["*exempt.test"]));

        // Domain unhide of a DOMAIN-scoped selector: removed from that scope.
        let scoped = convert_one("a.test##.x\na.test#@#.x\n");
        assert!(
            !scoped
                .rules
                .iter()
                .any(|rule| rule["action"]["selector"] == ".x"),
            "a domain-scoped unhide must remove the selector from that domain"
        );
    }

    // Overlapping lists produce the same filter many times. Collapsing them is
    // the difference between fitting under the ceiling and not.
    #[test]
    fn identical_rules_from_different_lists_collapse() {
        let conversion = convert(&[
            Source {
                name: "one",
                text: "||ads.test^$third-party\n",
            },
            Source {
                name: "two",
                text: "||ads.test^$third-party\n",
            },
        ]);
        assert_eq!(conversion.report.network_block, 1);
        assert_eq!(conversion.report.deduplicated, 1);
    }

    // `$badfilter` is upstream saying "disable that other rule". Ignoring it
    // ships a filter its own maintainers withdrew.
    #[test]
    fn a_badfilter_disables_the_rule_it_names() {
        let conversion =
            convert_one("||ads.test^$third-party\n||ads.test^$third-party,badfilter\n");
        assert_eq!(conversion.report.network_block, 0);
        assert_eq!(conversion.report.dropped.get("badfilter"), Some(&1));
    }

    // DETERMINISM, which this repo forbids breaking. Two conversions of the
    // same corpus must be byte-identical, whatever order the maps iterated in.
    #[test]
    fn the_same_corpus_always_produces_the_same_bytes() {
        let corpus = "||z.test^\n||a.test^\nz.test,a.test##.x\n##.b\n##.a\nb.test##.y\n";
        let first = serde_json::to_string(&convert_one(corpus).rules).unwrap();
        let second = serde_json::to_string(&convert_one(corpus).rules).unwrap();
        assert_eq!(first, second);
    }

    // Every rule this converter emits must be a shape WebKit accepts. This is
    // the invariant that keeps one bad filter from abolishing ad blocking.
    #[test]
    fn every_emitted_rule_is_a_shape_webkit_accepts() {
        let conversion = convert_one(
            "||ads.test^$third-party\n\
             @@||ok.test^$script\n\
             /banner[0-9]+\\.gif/\n\
             a.test##.scoped\n\
             ##.generic\n\
             @@||white.test^$document\n",
        );
        assert!(!conversion.rules.is_empty());
        for rule in &conversion.rules {
            let filter = rule["trigger"]["url-filter"]
                .as_str()
                .expect("every trigger has a url-filter");
            assert!(
                webkit_regex_ok(filter),
                "emitted an invalid regex: {filter}"
            );
            if let Some(types) = rule["trigger"]["resource-type"].as_array() {
                for kind in types {
                    let kind = kind.as_str().unwrap();
                    assert!(
                        SUBRESOURCE_TYPES.contains(&kind)
                            || ["document", "popup", "top-document"].contains(&kind),
                        "emitted an unknown resource-type: {kind}"
                    );
                }
            }
            if let Some(selector) = rule["action"]["selector"].as_str() {
                for one in selector.split(", ") {
                    assert!(selector_ok(one), "emitted an unusable selector: {one}");
                }
            }
        }
    }

    // The ceiling is a hard engine limit, not a preference: one rule past it
    // and NOTHING compiles. Cosmetic is what gets trimmed, because a lost hide
    // degrades a page while a lost exception breaks one.
    #[test]
    fn the_ceiling_trims_cosmetic_first_and_says_how_much() {
        // A corpus that overshoots on cosmetic alone.
        let mut corpus = String::new();
        for index in 0..(WEBKIT_RULE_CEILING + 10) {
            corpus.push_str(&format!("||ads{index}.test^\n"));
        }
        for index in 0..50 {
            corpus.push_str(&format!("scope{index}.test##.ad\n"));
        }
        let conversion = convert_one(&corpus);
        assert!(
            conversion.rules.len() <= WEBKIT_RULE_CEILING,
            "shipped {} rules, over the measured ceiling",
            conversion.rules.len()
        );
        assert!(
            conversion.report.dropped.get("over-ceiling").is_some(),
            "a trim must be reported, not silent"
        );
        assert!(
            !conversion
                .rules
                .iter()
                .any(|rule| rule["action"]["type"] == "css-display-none"),
            "cosmetic must be trimmed before any network rule is"
        );
    }

    /// ⭐ THE BOT-CHECK GUARD, locked three ways.
    ///
    /// A user could not get through a Cloudflare challenge on a login and had no
    /// way to tell whether an asset had been eaten — this is the rule that makes
    /// the answer permanently "no". All three assertions are load-bearing:
    ///
    /// 1. the guard is PRESENT (an upstream refresh cannot lose it);
    /// 2. it is LAST (`ignore-previous-rules` cancels only what precedes it, so
    ///    a guard in the middle protects nothing that follows);
    /// 3. it survives the CEILING (the trim runs against a reduced budget, so a
    ///    ruleset that arrives at 150,000 cannot be the one that drops it).
    #[test]
    fn a_bot_check_is_allowlisted_last_and_survives_the_ceiling() {
        // Overshoot on network rules so the trim path definitely runs.
        let mut corpus = String::new();
        for index in 0..(WEBKIT_RULE_CEILING + 10) {
            corpus.push_str(&format!("||ads{index}.test^\n"));
        }
        // ...and a rule that WOULD block the challenge platform, so the guard
        // has something real to cancel.
        corpus.push_str("||brilliant.org/cdn-cgi/challenge-platform/\n");
        let conversion = convert_one(&corpus);

        let guard = bot_check_guard_rules();
        assert!(
            conversion.rules.len() <= WEBKIT_RULE_CEILING,
            "the guard pushed the ruleset over the ceiling, which compiles to \
             NOTHING: {} rules",
            conversion.rules.len()
        );
        let tail = &conversion.rules[conversion.rules.len() - guard.len()..];
        assert_eq!(
            tail,
            guard.as_slice(),
            "the bot-check guard must be the LAST rules in the array — \
             `ignore-previous-rules` cancels only what comes before it"
        );
        for rule in &guard {
            assert_eq!(rule["action"]["type"], "ignore-previous-rules");
        }
        // And it is narrow: the analytics endpoints on the same `/cdn-cgi/`
        // prefix are a different path and stay blockable.
        let filters: Vec<&str> = guard
            .iter()
            .filter_map(|rule| rule["trigger"]["url-filter"].as_str())
            .collect();
        assert!(filters.iter().any(|f| f.contains("challenge-platform")));
        assert!(filters.iter().any(|f| f.contains("challenges")));
        assert!(
            !filters.iter().any(|f| *f == "/cdn-cgi/"),
            "allowlisting /cdn-cgi/ wholesale would un-block RUM and beacon too"
        );
    }

    // The selector gate. Conservative by design: WebKit drops what it cannot
    // parse without complaining, so anything this cannot prove is CSS is
    // counted as refused rather than shipped as a lie.
    #[test]
    fn the_selector_gate_refuses_what_webkit_would_drop_in_silence() {
        for ok in [
            ".ad",
            "div[data-ad-slot]",
            "#banner > .ad",
            "div:not(.keep)",
            "div:has(> .ad)",
            "li:nth-child(2)",
            "a[href*=\"ads\"]",
            "div.\\31 23",
        ] {
            assert!(selector_ok(ok), "{ok} is valid CSS and must be kept");
        }
        for bad in [
            "div:has-text(Ads)",
            "div:-abp-contains(Ads)",
            "div:upward(2)",
            "div:xpath(//div)",
            "div:remove()",
            "div:style(display: none)",
            "body { display: block }",
            "div[unbalanced",
            "",
        ] {
            assert!(!selector_ok(bad), "{bad} must be refused");
        }
    }
}
