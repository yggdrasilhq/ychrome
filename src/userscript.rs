//! The SCRIPTLET PLANE: a userscript is a body plus the placement facts that
//! decide where it runs.
//!
//! Before this module every userscript ran on every page, in the page's own
//! JavaScript world, in the top frame only. That is three wrong defaults in a
//! row: a YouTube script paid its cost on every tab, and any page could read or
//! clobber a script's globals (and the script could clobber the page's) because
//! they shared one world.
//!
//! The fix is the one userscript managers have always used — a Greasemonkey
//! metadata block at the top of the file:
//!
//! ```text
//! // ==UserScript==
//! // @match      https://*.youtube.com/*
//! // @world      main
//! // @run-at     document-start
//! // @all-frames
//! // ==/UserScript==
//! ```
//!
//! ychrome PARSES those headers; yggterm APPLIES them. Nothing in between
//! re-derives them, and a script with no header block gets the documented
//! defaults, so every `.js` already on a user's disk keeps working.
//!
//! ## The placement facts
//!
//! - **`@match`** — WebKit match patterns, passed through VERBATIM. WebKit's
//!   `WebKitUserScript` takes an allow-list of patterns and does the matching
//!   itself in the engine, so ychrome never interprets a `@match` and cannot
//!   disagree with the engine about what one means. No pattern at all = every
//!   URL, which is what a header-less script already got.
//! - **`@include`** — a DIFFERENT dialect: Greasemonkey globs (`*` matches
//!   anything, `/…/` is a regex), matched against the whole URL, not WebKit
//!   match patterns. Each glob goes through [`translate_include`], which
//!   accepts only the subset provably equivalent to a match pattern. One
//!   untranslatable `@include` refuses the WHOLE script's promotion —
//!   all-or-nothing, because promoting the translatable part would run the
//!   script on fewer pages than it declares, the same silent-wrong class as
//!   feeding the glob to WebKit verbatim (where it matches NOTHING and the
//!   script quietly never runs).
//! - **`@exclude-match`** — `@match`'s dialect, SUBTRACTED. The same
//!   `WebKitUserScript` takes a BLOCK-list beside its allow-list, and these
//!   patterns go there verbatim: a page matching any of them never gets the
//!   script, whatever the allow-list says.
//! - **`@exclude`** — `@include`'s glob dialect, subtracted. Each glob goes
//!   through the same [`translate_include`] proof and lands on the block-list;
//!   one untranslatable `@exclude` refuses the WHOLE script's promotion, the
//!   same all-or-nothing as `@include`. Silently ignoring an exclusion is
//!   forbidden outright — it runs the script on pages its author explicitly
//!   ruled out.
//! - **`@world`** — `isolated` (the DEFAULT) or `main`. An isolated script gets
//!   its own JavaScript world: same DOM, private globals. A script that must
//!   patch something the PAGE will call — `window.fetch`,
//!   `navigator.credentials` — has to declare `@world main`, because a patch
//!   applied in an isolated world is invisible to the page.
//! - **`@all-frames`** — inject into sub-frames too. Default off (top frame
//!   only), which is what shipped before.
//! - **`@run-at`** — recorded verbatim and carried on the wire, but see
//!   [`Userscript::run_at`]: today every script is injected at document-start
//!   regardless.

/// Which JavaScript world a script's globals live in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptWorld {
    /// A private world: the script shares the page's DOM but not its globals.
    /// The default, because it is the safe one — the page cannot read or
    /// overwrite the script, and the script cannot accidentally collide with a
    /// name the page uses.
    Isolated,
    /// The page's own world. Required for a script whose whole job is to patch
    /// an API the PAGE then calls (`window.fetch`, `navigator.credentials`): a
    /// patch installed in an isolated world is invisible from the page.
    Main,
}

impl ScriptWorld {
    /// The wire spelling. Also what a `@world` line must say to select it.
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptWorld::Isolated => "isolated",
            ScriptWorld::Main => "main",
        }
    }
}

/// The only `@run-at` any surface honours today.
pub const RUN_AT_DOCUMENT_START: &str = "document-start";

/// One `@include` or `@exclude` whose glob could not be PROVEN equivalent to a
/// WebKit match pattern: which directive carried it, the glob verbatim (the
/// refusal must name the line as the author wrote it), and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntranslatableInclude {
    /// `"@include"` or `"@exclude"`, exactly as it reads in the header.
    pub directive: &'static str,
    /// The glob value exactly as the author wrote it.
    pub glob: String,
    /// Which equivalence rule it failed, in the author's terms.
    pub why: &'static str,
}

/// One userscript, with every placement decision already made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Userscript {
    /// The script source, headers and all. The metadata block is a comment, so
    /// it is left in place rather than stripped — a userscript that arrives in
    /// the engine without its provenance is unreadable in the inspector.
    pub body: String,
    /// WebKit match patterns from `@match`/`@include`, in declaration order.
    /// EMPTY = every URL.
    pub matches: Vec<String>,
    /// WebKit match patterns from `@exclude-match`/`@exclude`, in declaration
    /// order: the engine's BLOCK-list. A page matching any of these never gets
    /// the script, whatever [`Userscript::matches`] says. EMPTY = exclude
    /// nothing.
    pub exclude_matches: Vec<String>,
    /// `@all-frames`: inject into sub-frames as well as the top frame.
    pub all_frames: bool,
    /// `@world`.
    pub world: ScriptWorld,
    /// `@run-at`, verbatim, defaulting to [`RUN_AT_DOCUMENT_START`].
    ///
    /// ⚠ NOTHING HONOURS ANY OTHER VALUE YET. Every script is injected at
    /// document-start, so a script declaring `document-end` runs EARLY and must
    /// still guard itself (`DOMContentLoaded`, an observer, its own retry). The
    /// field travels anyway so that the day a surface grows document-end
    /// injection, the declaration is already on the wire and no script has to be
    /// re-authored.
    pub run_at: String,
    /// Every `@include`/`@exclude` whose glob failed [`translate_include`],
    /// verbatim.
    ///
    /// NON-EMPTY = this script REFUSES promotion, whole. The one gate that
    /// enforces it is `webpolicy::promote_or_refuse` — nothing else decides —
    /// and a script that never came from `parse` (the passkey shim) is
    /// promotable by construction because this list is empty.
    pub untranslatable_includes: Vec<UntranslatableInclude>,
}

impl Userscript {
    /// A script with the documented defaults: every URL, isolated world, top
    /// frame, document-start. What a body with no metadata block means, and the
    /// base every parsed header overrides.
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            matches: Vec::new(),
            exclude_matches: Vec::new(),
            all_frames: false,
            world: ScriptWorld::Isolated,
            run_at: RUN_AT_DOCUMENT_START.to_string(),
            untranslatable_includes: Vec::new(),
        }
    }

    /// Force the page's own world. For scripts ychrome itself injects (the
    /// passkey shim), whose whole purpose is to be visible to the page.
    pub fn in_main_world(mut self) -> Self {
        self.world = ScriptWorld::Main;
        self
    }

    pub fn to_json(&self) -> serde_json::Value {
        // The wire never carries a refused script: the promotion gate
        // (`webpolicy::promote_or_refuse`) drops it whole before any Policy
        // holds it. This assert is that invariant, spelled where a bypass
        // would first do damage — a refused script serialized here would ship
        // with only its translatable patterns, running on fewer pages than it
        // declares.
        debug_assert!(
            self.untranslatable_includes.is_empty(),
            "a script with untranslatable @include lines reached the wire"
        );
        serde_json::json!({
            "body": self.body,
            "matches": self.matches,
            "exclude_matches": self.exclude_matches,
            "all_frames": self.all_frames,
            "world": self.world.as_str(),
            "run_at": self.run_at,
        })
    }
}

/// The opening and closing lines of a Greasemonkey metadata block, as they read
/// once `//` and surrounding whitespace are stripped.
const HEADER_OPEN: &str = "==UserScript==";
const HEADER_CLOSE: &str = "==/UserScript==";

/// `// @key value` -> `("@key", "value")`, or `None` for a line that is not a
/// comment at all. A comment with no `@` yields an empty key, which no rule
/// below matches — that is how prose inside the block is ignored.
fn metadata_line(line: &str) -> Option<(&str, &str)> {
    let comment = line.trim().strip_prefix("//")?.trim();
    match comment.split_once(char::is_whitespace) {
        Some((key, value)) => Some((key, value.trim())),
        None => Some((comment, "")),
    }
}

/// A bare `@all-frames` means yes. An explicit value is honoured, and ANYTHING
/// unrecognised means no — the narrower behaviour, and the one that shipped.
fn flag_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "true" | "yes" | "1"
    )
}

/// Read the metadata block off a script body.
///
/// The block starts at the first line that is exactly `// ==UserScript==` and
/// ends at `// ==/UserScript==`, at the first line that is NOT a `//` comment
/// (the code has started), or at end of file — whichever comes first. Ending on
/// the first non-comment is what makes an UNCLOSED block safe: the parser can
/// never walk into the script body and mistake a line of code for a
/// declaration, and the `@match` an author did write is still honoured rather
/// than silently widening the script to every page.
///
/// Every unrecognised key, malformed value, or absent block leaves the
/// corresponding default from [`Userscript::new`] in place: a userscript with
/// a typo'd header is still a userscript, and refusing to run it would be a
/// worse answer than running it with the safe defaults.
///
/// The deliberate exceptions are `@include` and `@exclude`. A glob that cannot
/// be proven equivalent to a WebKit match pattern is recorded in
/// [`Userscript::untranslatable_includes`], and any entry there refuses the
/// whole script's promotion (all-or-nothing, enforced at
/// `webpolicy::promote_or_refuse`). "Safe default" has no meaning for a bad
/// `@include`: every URL is wider than declared, the translatable subset is
/// narrower than declared, and verbatim is a pattern that matches nothing —
/// all three run the script somewhere other than where its author said. A bad
/// `@exclude` is worse still: dropping it runs the script on the very pages
/// its author ruled out, which is the loudest possible way to disobey a
/// declaration silently.
pub fn parse(body: &str) -> Userscript {
    let mut script = Userscript::new(body);
    let mut lines = body.lines();
    // Skip to the opening line. Anything before it — a copyright banner, a
    // blank line — is not metadata.
    let opened = lines.by_ref().any(|line| {
        metadata_line(line).is_some_and(|(key, value)| key == HEADER_OPEN && value.is_empty())
    });
    if !opened {
        return script;
    }
    for line in lines {
        let Some((key, value)) = metadata_line(line) else {
            // Code, not a comment: the block is over whether or not it was
            // ever closed.
            break;
        };
        match key {
            HEADER_CLOSE => break,
            // @match IS WebKit's dialect: verbatim, the engine interprets it.
            "@match" if !value.is_empty() => script.matches.push(value.to_string()),
            // @include is Greasemonkey's dialect. Translate or refuse — never
            // hand a glob to WebKit as if it were a match pattern, which is a
            // script that quietly runs nowhere.
            "@include" if !value.is_empty() => match translate_include(value) {
                Ok(pattern) => script.matches.push(pattern),
                Err(why) => script.untranslatable_includes.push(UntranslatableInclude {
                    directive: "@include",
                    glob: value.to_string(),
                    why,
                }),
            },
            // @exclude-match is @match's dialect, subtracted: verbatim onto the
            // engine's block-list.
            "@exclude-match" if !value.is_empty() => script.exclude_matches.push(value.to_string()),
            // @exclude is @include's glob dialect, subtracted: the same
            // translate-or-refuse, landing on the block-list. Swallowing one
            // (the pre-fix `_ => {}` arm) ran the script on pages its author
            // explicitly ruled out.
            "@exclude" if !value.is_empty() => match translate_include(value) {
                Ok(pattern) => script.exclude_matches.push(pattern),
                Err(why) => script.untranslatable_includes.push(UntranslatableInclude {
                    directive: "@exclude",
                    glob: value.to_string(),
                    why,
                }),
            },
            "@all-frames" => script.all_frames = flag_value(value),
            "@world" => {
                script.world = match value.trim().to_ascii_lowercase().as_str() {
                    "main" => ScriptWorld::Main,
                    // `isolated`, a typo, or an empty value all mean the
                    // default. Isolated is the conservative half: a script that
                    // meant `main` and lost the word fails visibly (its patch
                    // does not take) instead of silently gaining the ability to
                    // clobber the page.
                    _ => ScriptWorld::Isolated,
                }
            }
            "@run-at" if !value.is_empty() => script.run_at = value.to_string(),
            _ => {}
        }
    }
    script
}

/// Translate one Greasemonkey `@include` glob into a WebKit match pattern, or
/// refuse with the rule it failed.
///
/// # The two languages
///
/// A Greasemonkey `@include` is an ANCHORED GLOB over the page's whole
/// normalized URL string (`location.href`): `*` matches any run of characters
/// INCLUDING `/`, `?` and `#`, and a value spelled `/…/` is a regular
/// expression instead. A WebKit match pattern
/// (`WebCore::UserContentURLPattern`, the thing `webkit_user_script_new`'s
/// allow-list holds) is `scheme://host/path` where the scheme is literal (or
/// `*` = http|https), the host is literal (or `*`, or a leading `*.` which
/// ALSO matches the bare host), and the path is an anchored glob tested
/// against the URL's PATH ALONE — the query string, fragment, port and
/// userinfo are invisible to it.
///
/// # The equivalence rules (why the accepted subset is exact)
///
/// A glob is accepted only when it has the shape
/// `http(s)://<literal-host>/<literal>*` — literal lowercase scheme, literal
/// host with a terminating `/`, and a path whose only wildcard is one `*` at
/// the very end. On that shape the two languages read the SAME string the
/// same way, so the translation is the identity spelling; what the function
/// adds is the PROOF, argued fact by fact against a normalized href:
///
/// - **Scheme**: normalized hrefs spell the scheme lowercase; a literal
///   `http`/`https` compares equal on both sides. Any `*` touching the scheme
///   is refused: a glob `*` is not stopped by `://`, so `http*://a/` also
///   matches an `://a/` buried in some other URL's query.
/// - **Host**: in a normalized href the authority is everything between
///   `scheme://` and the first `/`. The accepted host is literal, lowercase,
///   and contains none of `* : @ ? #`, and the glob requires a `/` right
///   after it — so the glob fires exactly when the authority IS that host,
///   which is exactly the pattern's host test. Every wildcard host is
///   refused, because the two languages genuinely disagree in BOTH
///   directions: glob `https://*.a.example/*` also matches
///   `https://evil.example/x.a.example/` (its `*` crosses into the path),
///   while pattern `*.a.example` also matches the bare `a.example` (subdomain
///   syntax includes the apex), so neither is a subset of the other.
/// - **Path**: the glob tests `path + query + fragment`; the pattern tests
///   the path alone. With `/<literal>*` — no `?`/`#`/`*` inside the literal —
///   the glob matches iff the remainder STARTS WITH the literal, and because
///   the literal contains no `?`/`#` the match can never extend past the end
///   of the path; the pattern's trailing-`*` prefix test accepts exactly the
///   same paths, with query and fragment free on both sides. A path with no
///   trailing `*` is refused (the glob additionally demands NO query string,
///   which a pattern cannot express), and a `*` anywhere else is refused (it
///   can swallow the `?` and match pages whose path does not).
///
/// # The one asymmetry, named
///
/// A match pattern never sees the port or userinfo, so
/// `https://a.example/*` as a pattern also fires on
/// `https://a.example:8443/…` where the glob would not. Hrefs spell no
/// default port (normalization strips `:443`/`:80`), so the divergence exists
/// only for explicitly non-default-port URLs — and it is WIDER, never
/// narrower: the script can gain a same-host, same-path port variant, but can
/// never lose a page its glob declared. Refusing every host-bearing glob over
/// this would empty the translatable subset; accepting it is documented here
/// instead. Globs that PIN a port (`:` in the host) are still refused, since
/// the pattern could not honour the pin.
///
/// Read from the BLOCK-list side (`@exclude`), the same port-blindness flips
/// direction: a translated exclusion also excludes the explicit-port variants
/// of its host+path, so the script may additionally SKIP a port variant its
/// glob never ruled out — but it can never RUN on a page the author excluded.
/// More exclusion is the conservative error for an exclusion, exactly as more
/// inclusion is the accepted one for an `@include`; both err toward the
/// pattern dialect's port-blindness and neither can betray a declared page.
///
/// # Refused outright, by dialect
///
/// `/regex/` forms (a different language), the Greasemonkey `.tld` magic
/// suffix (dialect-specific rewriting), non-http(s) schemes (patterns and
/// globs disagree about `file:`'s shape), and uppercase scheme/host (a
/// normalized href never matches it case-sensitively, and whether a manager
/// matches globs case-insensitively varies — unprovable either way).
pub fn translate_include(glob: &str) -> Result<String, &'static str> {
    if glob.len() >= 2 && glob.starts_with('/') && glob.ends_with('/') {
        return Err(
            "a /regex/ @include is a regular expression, a different language \
                    with no match-pattern equivalent",
        );
    }
    let (scheme, rest) = if let Some(rest) = glob.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = glob.strip_prefix("https://") {
        ("https", rest)
    } else {
        return Err(
            "the scheme must be a literal lowercase `http://` or `https://`: a glob \
                    `*` is not stopped by `://`, so a wildcard scheme can swallow into the \
                    host and path",
        );
    };
    let Some((host, path_tail)) = rest.split_once('/') else {
        return Err(
            "the host must end at a literal `/`: without one the glob's last host \
                    character runs straight into the path",
        );
    };
    if host.is_empty() {
        return Err(
            "an empty host matches no URL at all; refusing beats installing a \
                    script that silently never runs",
        );
    }
    if host.contains('*') {
        return Err(
            "a glob `*` in the host also matches `/`, so it can reach into the \
                    path (`https://*.a.example/` fires on a `.a.example/` buried in \
                    another site's path), while a pattern's `*.host` also matches the \
                    bare host — the two languages disagree in both directions",
        );
    }
    if host.contains(':') {
        return Err(
            "an explicit port: a match pattern never sees the port, so it cannot \
                    honour the pin",
        );
    }
    if host.contains('@') {
        return Err("userinfo in the authority: a match pattern never sees it");
    }
    if host.contains('?') || host.contains('#') {
        return Err(
            "`?` or `#` before the first `/`: the glob is matching the query \
                    string or fragment, which a match pattern cannot see",
        );
    }
    if host.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(
            "uppercase in the host: a normalized URL's host is lowercase, so the \
                    glob's meaning depends on the manager's case rules — not provable",
        );
    }
    if host.ends_with(".tld") {
        return Err(
            "the Greasemonkey `.tld` magic suffix is dialect-specific rewriting \
                    with no match-pattern equivalent",
        );
    }
    let path = format!("/{path_tail}");
    match path.find('*') {
        None => {
            return Err(
                "no trailing `*`: the glob also demands that no query string or \
                        fragment follow, which a match pattern cannot express",
            );
        }
        Some(position) if position + 1 != path.len() => {
            return Err(
                "a `*` before the end of the path can swallow the `?` that starts \
                        the query string, matching pages whose path does not match the \
                        pattern",
            );
        }
        Some(_) => {}
    }
    let literal = &path[..path.len() - 1];
    if literal.contains('?') || literal.contains('#') {
        return Err(
            "`?` or `#` in the path: the glob is constraining the query string or \
                    fragment, which a match pattern cannot see",
        );
    }
    Ok(format!("{scheme}://{host}{path}"))
}

/// The loud half of the all-or-nothing rule: what the refusal of one script's
/// promotion says, naming EACH untranslatable `@include`/`@exclude` line
/// verbatim with the rule it failed. Built here, beside the translator whose
/// refusals it reports; printed by `webpolicy::promote_or_refuse`, the one
/// gate.
pub fn refusal_report(source: &str, refused: &[UntranslatableInclude]) -> String {
    use std::fmt::Write as _;
    let mut report = format!(
        "ychrome: REFUSING userscript {source}: its @include/@exclude lines cannot be \
         proven equivalent to WebKit match patterns\n"
    );
    for include in refused {
        let _ = writeln!(report, "ychrome:   {} {}", include.directive, include.glob);
        let _ = writeln!(report, "ychrome:     — {}", include.why);
    }
    let _ = writeln!(
        report,
        "ychrome:   the script was NOT injected anywhere: promoting only its translatable \
         part would run it on pages other than the ones its author declared. Rewrite each \
         line above as a @match / @exclude-match pattern."
    );
    report
}

/// The pane's one-line version of [`refusal_report`]: the same facts, sized
/// for a row subtitle. Names each refused directive and glob verbatim — the
/// settings pane must never present a refused script as running, and must say
/// which lines to fix, or the user's only clue is a daemon's stderr they will
/// never read.
pub fn refusal_summary(refused: &[UntranslatableInclude]) -> String {
    let lines: Vec<String> = refused
        .iter()
        .map(|include| format!("{} {}", include.directive, include.glob))
        .collect();
    format!("Refused — not injected: {}", lines.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "\
// ==UserScript==
// @name        Demo
// @match       https://*.youtube.com/*
// @include     https://m.youtube.com/*
// @exclude-match https://*.youtube.com/embed/*
// @exclude     https://m.youtube.com/embed*
// @world       main
// @all-frames
// @run-at      document-start
// ==/UserScript==
(function () { 'use strict'; })();
";

    #[test]
    fn a_full_header_yields_every_declared_fact() {
        let script = parse(FULL);
        assert_eq!(
            script.matches,
            vec![
                "https://*.youtube.com/*".to_string(),
                "https://m.youtube.com/*".to_string()
            ]
        );
        assert_eq!(
            script.exclude_matches,
            vec![
                "https://*.youtube.com/embed/*".to_string(),
                "https://m.youtube.com/embed*".to_string()
            ],
            "@exclude-match travels verbatim and a translatable @exclude joins it"
        );
        assert_eq!(script.world, ScriptWorld::Main);
        assert!(script.all_frames);
        assert_eq!(script.run_at, RUN_AT_DOCUMENT_START);
        // The body travels whole: the metadata is a comment, and stripping it
        // would make the script unreadable in the inspector.
        assert!(script.body.contains("==UserScript=="));
    }

    // The defaults ARE the contract for every `.js` already on a user's disk.
    // A header-less script must still run, everywhere, top frame — and now in
    // an isolated world, which is the one behaviour this plane changes.
    #[test]
    fn a_body_with_no_header_block_gets_the_documented_defaults() {
        let script = parse("console.log('hi');\n");
        assert!(script.matches.is_empty(), "no @match must mean every URL");
        assert!(
            script.exclude_matches.is_empty(),
            "no @exclude must mean exclude nothing"
        );
        assert_eq!(script.world, ScriptWorld::Isolated);
        assert!(!script.all_frames);
        assert_eq!(script.run_at, RUN_AT_DOCUMENT_START);
    }

    // Every malformed shape lands on the SAFE default, never on a wider one.
    #[test]
    fn malformed_headers_fall_back_to_safe_defaults() {
        // An unknown @world is isolated, not main.
        assert_eq!(
            parse("// ==UserScript==\n// @world page\n").world,
            ScriptWorld::Isolated
        );
        // An empty @world is isolated too.
        assert_eq!(
            parse("// ==UserScript==\n// @world\n").world,
            ScriptWorld::Isolated
        );
        // An unrecognised @all-frames value is top-frame-only.
        assert!(!parse("// ==UserScript==\n// @all-frames maybe\n").all_frames);
        assert!(!parse("// ==UserScript==\n// @all-frames false\n").all_frames);
        // A valueless @match contributes nothing rather than an empty pattern,
        // which WebKit would reject for the whole script.
        assert!(parse("// ==UserScript==\n// @match\n").matches.is_empty());
        // Valueless exclusions name no page: nothing to honour, nothing to
        // refuse over.
        let valueless = parse("// ==UserScript==\n// @exclude-match\n// @exclude\n");
        assert!(valueless.exclude_matches.is_empty());
        assert!(valueless.untranslatable_includes.is_empty());
        // Keys we do not implement are ignored, not fatal.
        let unknown = parse("// ==UserScript==\n// @grant none\n// @match https://a/*\n");
        assert_eq!(unknown.matches, vec!["https://a/*".to_string()]);
    }

    // A block that forgets its closing line must do BOTH halves: still yield
    // the @match the author wrote (dropping it would silently widen a YouTube
    // script to every page), and STOP at the first line of code rather than
    // merely skip past it. The discriminator for the second half is the pair
    // of declaration-shaped comment lines AFTER the code: a parser that ends
    // the block there never sees them, while one that keeps scanning
    // (`continue` where the `break` belongs) reads them as declarations —
    // handing the script the page's world and a second match pattern. The
    // code line's own '@world main' string literal is NOT the discriminator:
    // a non-comment line is skipped by both behaviours.
    #[test]
    fn an_unclosed_block_still_honours_what_it_declared_and_stops_at_the_code() {
        let script = parse(
            "// ==UserScript==\n\
             // @match https://*.youtube.com/*\n\
             (function () { var at = '@world main'; })();\n\
             // @world main\n\
             // @match https://*.example.test/*\n",
        );
        assert_eq!(
            script.matches,
            vec!["https://*.youtube.com/*".to_string()],
            "a comment past the first line of code was read as an @match declaration"
        );
        assert_eq!(
            script.world,
            ScriptWorld::Isolated,
            "a comment past the first line of code was read as an @world declaration"
        );
    }

    // Bare `@all-frames` is the idiom; the flag parser must not need a value.
    #[test]
    fn a_bare_flag_means_yes_and_an_explicit_no_means_no() {
        assert!(flag_value(""));
        assert!(flag_value("true"));
        assert!(!flag_value("false"));
        assert!(!flag_value("nonsense"));
    }

    // THE TRANSLATABLE DIRECTION. On the accepted subset the two languages
    // coincide, so the output is the identity spelling — but each row here is
    // a SHAPE the equivalence proof in `translate_include`'s doc covers, at
    // its boundary: bare host with `/*`, subpath prefix, literal path with a
    // trailing star, a one-letter host, dots and dashes in host and path.
    #[test]
    fn the_provably_equivalent_globs_translate_to_their_own_spelling() {
        for glob in [
            "https://m.example.test/*",
            "http://m.example.test/*",
            "https://a/*",
            "https://m.example.test/watch*",
            "https://m.example.test/a/b-c.d/e*",
        ] {
            assert_eq!(
                translate_include(glob).as_deref(),
                Ok(glob),
                "{glob} is inside the provable subset and must translate to itself"
            );
        }
    }

    // THE REFUSED DIRECTION, each rule at its boundary. Every row is a glob
    // whose Greasemonkey meaning provably differs from the same spelling read
    // as a match pattern (or has no pattern spelling at all) — the doc on
    // `translate_include` derives each divergence. A translator that started
    // accepting any of these would ship scripts that run on the wrong pages.
    #[test]
    fn the_unprovable_globs_are_refused_with_the_rule_they_failed() {
        for (glob, why_mentions) in [
            // Greasemonkey's other language entirely.
            ("/^https?://.*/", "regular expression"),
            // Wildcard or non-http(s) schemes: `*` swallows past `://`.
            ("*://m.example.test/*", "scheme"),
            ("http*://m.example.test/*", "scheme"),
            ("ftp://m.example.test/*", "scheme"),
            ("HTTPS://m.example.test/*", "scheme"),
            // The host never ends, or does not exist.
            ("https://m.example.test*", "literal `/`"),
            ("https://", "literal `/`"),
            ("https:///*", "empty host"),
            // Wildcard hosts: divergent in BOTH directions (glob `*` crosses
            // into the path; pattern `*.host` includes the bare host).
            ("https://*.example.test/*", "both directions"),
            ("https://*/*", "both directions"),
            // Authority facts a match pattern cannot see.
            ("https://m.example.test:8443/*", "port"),
            ("https://user@m.example.test/*", "userinfo"),
            ("https://m.example.test?x=1/*", "query"),
            // Case rules differ between managers; hrefs are lowercase.
            ("https://M.example.test/*", "uppercase"),
            // Dialect magic.
            ("https://example.tld/*", ".tld"),
            // Path shapes whose glob meaning constrains the query string.
            ("https://m.example.test/watch", "trailing"),
            ("https://m.example.test/a*b", "swallow"),
            ("https://m.example.test/a*b*", "swallow"),
            ("https://m.example.test/watch?v=*", "query"),
        ] {
            let refusal = translate_include(glob);
            let why = refusal.expect_err(&format!("{glob} must be refused, not translated"));
            assert!(
                why.contains(why_mentions),
                "{glob} was refused for the wrong rule: {why}"
            );
        }
    }

    // THE SILENTLY-IGNORED-EXCLUSION CLASS, locked with the review's exact
    // script: `@match https://*.youtube.com/*` + `@exclude
    // https://*.youtube.com/embed/*` used to promote cleanly and inject into
    // the very embeds its author excluded. A wildcard-host @exclude glob is
    // untranslatable, so the WHOLE script must refuse — and the two honourable
    // spellings of the same exclusion must land on the block-list, never the
    // allow-list, never the refusal list.
    #[test]
    fn an_exclusion_is_never_silently_ignored() {
        let script = parse(
            "// ==UserScript==\n\
             // @match https://*.youtube.com/*\n\
             // @exclude https://*.youtube.com/embed/*\n\
             // ==/UserScript==\n\
             x();\n",
        );
        assert_eq!(
            script
                .untranslatable_includes
                .iter()
                .map(|include| (include.directive, include.glob.as_str()))
                .collect::<Vec<_>>(),
            vec![("@exclude", "https://*.youtube.com/embed/*")],
            "an untranslatable @exclude must poison the script's promotion, not vanish"
        );

        // The match-pattern spelling of the same exclusion needs no proof:
        // verbatim onto the engine's block-list.
        let native = parse(
            "// ==UserScript==\n\
             // @match https://*.youtube.com/*\n\
             // @exclude-match https://*.youtube.com/embed/*\n\
             // ==/UserScript==\n\
             x();\n",
        );
        assert!(native.untranslatable_includes.is_empty());
        assert_eq!(
            native.exclude_matches,
            vec!["https://*.youtube.com/embed/*".to_string()]
        );

        // A TRANSLATABLE @exclude glob lands on the block-list translated.
        let translated = parse(
            "// ==UserScript==\n\
             // @match https://*.youtube.com/*\n\
             // @exclude https://m.youtube.com/embed*\n\
             // ==/UserScript==\n\
             x();\n",
        );
        assert!(translated.untranslatable_includes.is_empty());
        assert_eq!(
            translated.exclude_matches,
            vec!["https://m.youtube.com/embed*".to_string()]
        );
        assert_eq!(
            translated.matches,
            vec!["https://*.youtube.com/*".to_string()],
            "an @exclude leaked onto the ALLOW-list, widening where the script runs"
        );
    }

    // ALL-OR-NOTHING, the parse half: one bad @include OR @exclude poisons the
    // whole script even when good @match and @include lines sit beside it, and
    // EVERY bad one is recorded verbatim with its directive — the refusal must
    // name them all, not stop at the first.
    #[test]
    fn one_untranslatable_include_marks_the_whole_script_and_all_are_named() {
        let script = parse(
            "// ==UserScript==\n\
             // @match https://*.youtube.com/*\n\
             // @include https://m.example.test/*\n\
             // @include /^https?://.*music.*/\n\
             // @include https://*.example.test/*\n\
             // @exclude https://*.example.test/embed/*\n\
             // ==/UserScript==\n\
             x();\n",
        );
        let lines: Vec<(&str, &str)> = script
            .untranslatable_includes
            .iter()
            .map(|include| (include.directive, include.glob.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![
                ("@include", "/^https?://.*music.*/"),
                ("@include", "https://*.example.test/*"),
                ("@exclude", "https://*.example.test/embed/*"),
            ],
            "every untranslatable line must be recorded verbatim, in order, \
             with the directive that carried it"
        );
        // The translatable lines still parsed — the REFUSAL is the gate's job,
        // and the parse stays a faithful record of what the author wrote.
        assert_eq!(
            script.matches,
            vec![
                "https://*.youtube.com/*".to_string(),
                "https://m.example.test/*".to_string()
            ]
        );
    }

    // The refusal report is the loud half of the contract: it must name the
    // SOURCE and each untranslatable line VERBATIM — directive and glob — with
    // its rule. The pane's one-line summary carries the same names.
    #[test]
    fn the_refusal_report_names_the_source_and_every_glob_verbatim() {
        let refused = vec![
            UntranslatableInclude {
                directive: "@include",
                glob: "/^https?://.*music.*/".to_string(),
                why: "a /regex/ @include has no equivalent",
            },
            UntranslatableInclude {
                directive: "@exclude",
                glob: "https://*.example.test/*".to_string(),
                why: "wildcard host",
            },
        ];
        let report = refusal_report("music-cleaner.js", &refused);
        assert!(report.contains("REFUSING userscript music-cleaner.js"));
        for include in &refused {
            assert!(
                report.contains(&format!("{} {}", include.directive, include.glob)),
                "the report must name `{} {}` verbatim",
                include.directive,
                include.glob
            );
            assert!(report.contains(include.why));
        }
        assert!(
            report.contains("NOT injected"),
            "the report must say the whole script was withheld"
        );

        let summary = refusal_summary(&refused);
        assert!(summary.contains("Refused"));
        for include in &refused {
            assert!(
                summary.contains(&format!("{} {}", include.directive, include.glob)),
                "the pane summary must name `{} {}` verbatim",
                include.directive,
                include.glob
            );
        }
    }

    // The wire spelling is what yggterm matches on; a rename here silently
    // demotes every main-world script to isolated on the far side.
    #[test]
    fn the_world_wire_spellings_are_the_ones_yggterm_parses() {
        assert_eq!(ScriptWorld::Isolated.as_str(), "isolated");
        assert_eq!(ScriptWorld::Main.as_str(), "main");
        let value = parse(FULL).to_json();
        assert_eq!(value["world"], "main");
        assert_eq!(value["all_frames"], true);
        assert_eq!(value["matches"][0], "https://*.youtube.com/*");
        assert_eq!(
            value["exclude_matches"][0], "https://*.youtube.com/embed/*",
            "the block-list must travel on the wire, or the GUI injects into \
             pages the author excluded"
        );
        assert_eq!(value["run_at"], RUN_AT_DOCUMENT_START);
        assert!(value["body"].as_str().is_some_and(|b| b.contains("@match")));
    }
}
