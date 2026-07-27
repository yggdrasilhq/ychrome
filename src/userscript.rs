//! The SCRIPTLET PLANE: a userscript is a body plus the four facts that decide
//! where it runs.
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
//! ## The four facts
//!
//! - **`@match` / `@include`** — WebKit match patterns, passed through
//!   VERBATIM. WebKit's `WebKitUserScript` takes an allow-list of patterns and
//!   does the matching itself in the engine, so ychrome never interprets a
//!   pattern and cannot disagree with the engine about what one means. No
//!   pattern at all = every URL, which is what a header-less script already got.
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
}

impl Userscript {
    /// A script with the documented defaults: every URL, isolated world, top
    /// frame, document-start. What a body with no metadata block means, and the
    /// base every parsed header overrides.
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            matches: Vec::new(),
            all_frames: false,
            world: ScriptWorld::Isolated,
            run_at: RUN_AT_DOCUMENT_START.to_string(),
        }
    }

    /// Force the page's own world. For scripts ychrome itself injects (the
    /// passkey shim), whose whole purpose is to be visible to the page.
    pub fn in_main_world(mut self) -> Self {
        self.world = ScriptWorld::Main;
        self
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "body": self.body,
            "matches": self.matches,
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
/// corresponding default from [`Userscript::new`] in place. There is no error
/// path: a userscript with a typo'd header is still a userscript, and refusing
/// to run it would be a worse answer than running it with the safe defaults.
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
            "@match" | "@include" if !value.is_empty() => script.matches.push(value.to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "\
// ==UserScript==
// @name        Demo
// @match       https://*.youtube.com/*
// @include     https://m.youtube.com/*
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
        // Keys we do not implement are ignored, not fatal.
        let unknown = parse("// ==UserScript==\n// @grant none\n// @match https://a/*\n");
        assert_eq!(unknown.matches, vec!["https://a/*".to_string()]);
    }

    // A block that forgets its closing line must still yield the @match the
    // author wrote. Dropping it would silently widen a YouTube script to every
    // page on the internet — the opposite of a safe default.
    #[test]
    fn an_unclosed_block_still_honours_what_it_declared_and_stops_at_the_code() {
        let script = parse(
            "// ==UserScript==\n// @match https://*.youtube.com/*\n(function () { var at = '@world main'; })();\n",
        );
        assert_eq!(script.matches, vec!["https://*.youtube.com/*".to_string()]);
        // The line of CODE mentioning `@world main` is past the end of the
        // block and must not have been read as a declaration.
        assert_eq!(script.world, ScriptWorld::Isolated);
    }

    // Bare `@all-frames` is the idiom; the flag parser must not need a value.
    #[test]
    fn a_bare_flag_means_yes_and_an_explicit_no_means_no() {
        assert!(flag_value(""));
        assert!(flag_value("true"));
        assert!(!flag_value("false"));
        assert!(!flag_value("nonsense"));
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
        assert_eq!(value["run_at"], RUN_AT_DOCUMENT_START);
        assert!(value["body"].as_str().is_some_and(|b| b.contains("@match")));
    }
}
