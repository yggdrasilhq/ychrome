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
    /// `##+js(...)`: uBO scriptlet injection. Needs a library of surrogate
    /// implementations and a JS injection plane keyed per domain. Out of scope
    /// here, and named so the size of the gap is visible.
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
                "uBO scriptlet injection needs a surrogate library and per-domain JS injection"
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

/// The output: the ruleset, and the account of how it was reached.
pub struct Conversion {
    pub rules: Vec<Value>,
    pub report: Report,
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
        .all(|(ch, next)| {
            matches!(ch, '^' | '$' | '*') || (ch == '.' && next == '*')
        })
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
                    "#?#" => report.drop(DropReason::ProceduralSelector, line),
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
                    "##" if body.starts_with("+js(") => report.drop(DropReason::Scriptlet, line),
                    "##" if body.starts_with('^') => report.drop(DropReason::HtmlFiltering, line),
                    "##" => {
                        if PROCEDURAL_MARKERS.iter().any(|m| body.contains(m)) {
                            report.drop(DropReason::ProceduralSelector, line);
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
    let mut rules: Vec<Value> = Vec::new();
    for section in &sections {
        rules.extend(section.iter().cloned());
    }
    if rules.len() > WEBKIT_RULE_CEILING {
        let over = rules.len() - WEBKIT_RULE_CEILING;
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
        rules.truncate(WEBKIT_RULE_CEILING);
    }
    report.emitted = rules.len();
    Conversion { rules, report }
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
        assert_eq!(
            kinds,
            vec![
                "block",                 // 1. the block
                "ignore-previous-rules", // 2. the subresource exception
                "block",                 // 3. the $important block, past the exceptions
                "css-display-none",      // 4. cosmetic, after subresource exceptions
                "ignore-previous-rules", // 5. the document exception, last of all
            ]
        );
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
            "a.test##div:has-text(Sponsored)\n\
             a.test#?#div:-abp-contains(Ad)\n\
             a.test##+js(set-constant, x, true)\n\
             a.test#$#body { display: block; }\n\
             a.test##^script:has-text(ads)\n",
        );
        let dropped = &conversion.report.dropped;
        assert_eq!(dropped.get("procedural-selector"), Some(&2));
        assert_eq!(dropped.get("scriptlet"), Some(&1));
        assert_eq!(dropped.get("style-or-snippet"), Some(&1));
        assert_eq!(dropped.get("html-filtering"), Some(&1));
        assert_eq!(conversion.report.dropped_total(), 5);
        // And each one keeps a verbatim sample, so the report names what it
        // refused rather than only counting it.
        assert!(
            conversion.report.samples["scriptlet"][0].contains("+js(set-constant"),
            "a drop must name the filter verbatim"
        );
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
