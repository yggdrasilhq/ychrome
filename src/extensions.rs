//! ychrome's bundled userscript CATALOG — the "add an extension" list.
//!
//! WebKitGTK cannot run Chrome extensions (`.crx`), so ychrome's substitute is
//! userscripts. A user can drop their own `*.js` into
//! `~/.yggterm/web-userscripts/`, but that assumes they have one to hand. This
//! module ships a small, curated set ychrome can install with one click, written
//! to that same host-resident directory (the app owns its config).
//!
//! The catalog is deliberately SMALL and each entry is a script simple enough to
//! be obviously correct — a broken bundled userscript is worse than none. The
//! bodies live under `assets/web-userscripts/` and are embedded at build time, so
//! an install works on any host without shipping the files separately.
//!
//! `stem` is the filename without `.js` and doubles as the install action id and
//! the toggle id once installed. It must be a bare name (enforced in
//! `webpolicy::install_userscript`).

/// One installable userscript.
pub struct Extension {
    /// Filename stem (`sponsorblock` -> `sponsorblock.js`). Also its id.
    pub stem: &'static str,
    /// A short human name for the card.
    pub name: &'static str,
    /// One line on what it does, shown as the card's subtitle.
    pub description: &'static str,
    /// The script body, embedded at build time.
    pub body: &'static str,
}

/// The `sponsorblock` stem is special-cased by the settings pane into its own
/// section, so keep the id stable.
pub const SPONSORBLOCK_STEM: &str = "sponsorblock";

/// The full catalog, in display order.
pub fn catalog() -> &'static [Extension] {
    &CATALOG
}

/// Look one up by stem, for the install action.
pub fn find(stem: &str) -> Option<&'static Extension> {
    CATALOG.iter().find(|ext| ext.stem == stem)
}

static CATALOG: [Extension; 6] = [
    Extension {
        stem: crate::abp::COSMETIC_SCRIPT_STEM,
        name: "Cosmetic filters",
        description: "Hide the ad shapes WebKit's content blocker cannot express \
                      (:has-text, :style) — generated from the upstream filter lists.",
        body: include_str!("../assets/web-userscripts/cosmetic-filters.js"),
    },
    Extension {
        stem: crate::abp::SCRIPTLET_SCRIPT_STEM,
        name: "Scriptlets",
        description: "Run the per-site fixes the filter lists ask for with `##+js(...)` — \
                      a declarative blocker can only allow or deny, never run anything.",
        body: include_str!("../assets/web-userscripts/scriptlets.js"),
    },
    Extension {
        stem: SPONSORBLOCK_STEM,
        name: "SponsorBlock",
        description: "Auto-skip sponsor and self-promo segments on YouTube.",
        body: include_str!("../assets/web-userscripts/sponsorblock.js"),
    },
    Extension {
        stem: "youtube-adblock",
        name: "YouTube ad defense",
        description: "Strip YouTube's ad breaks out of the player's own response, \
                      and skip anything that still reaches the screen.",
        body: include_str!("../assets/web-userscripts/youtube-adblock.js"),
    },
    Extension {
        stem: "idcac",
        name: "I still don't care about cookies",
        description: "Reject cookie banners where there is a reject button, hide the \
                      rest, and give the page its scrolling back. Never accepts.",
        body: include_str!("../assets/web-userscripts/idcac.js"),
    },
    Extension {
        stem: "unblock-select",
        name: "Re-enable selection & right-click",
        description: "Restore copy, text selection and the context menu on sites that block them.",
        body: include_str!("../assets/web-userscripts/unblock-select.js"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    // Every catalog entry must have a bare-name stem (it becomes a filename and
    // an action id) and a non-empty body (a blank install is a silent no-op).
    #[test]
    fn catalog_entries_are_well_formed() {
        for ext in catalog() {
            assert!(!ext.stem.is_empty());
            assert!(!ext.stem.contains('/') && !ext.stem.contains(".."));
            assert!(!ext.name.is_empty());
            assert!(
                !ext.body.trim().is_empty(),
                "{} has an empty body",
                ext.stem
            );
        }
    }

    #[test]
    fn find_resolves_a_known_stem_and_rejects_the_rest() {
        assert!(find(SPONSORBLOCK_STEM).is_some());
        assert!(find("does-not-exist").is_none());
    }

    /// The part of a bundled userscript that RUNS: the inside of its IIFE, from
    /// `(function ()` to the closing `})();`. Every source assertion below is
    /// made against this slice, so neither a header comment above it nor a
    /// needle bolted on below it satisfies anything — only code does. Both
    /// anchors are FIRST occurrences: text added at either end cannot move them,
    /// and a script that grew a second IIFE fails loudly rather than widening
    /// the window.
    fn running_body(ext: &Extension) -> &str {
        let open = ext
            .body
            .find("(function ()")
            .unwrap_or_else(|| panic!("{}: not the expected IIFE (no opener)", ext.stem));
        let close = ext
            .body
            .find("})();")
            .unwrap_or_else(|| panic!("{}: not the expected IIFE (no terminator)", ext.stem));
        assert!(open < close, "{}: IIFE anchors are out of order", ext.stem);
        &ext.body[open..close]
    }

    /// Index of `needle` inside the running body, or a failure naming the script.
    fn code_index(ext: &Extension, needle: &str) -> usize {
        running_body(ext).find(needle).unwrap_or_else(|| {
            panic!(
                "{}: the running body no longer contains {needle:?} — if this moved on \
                 purpose, move the lock with it",
                ext.stem
            )
        })
    }

    fn require(ext: &Extension, needle: &str) {
        let _ = code_index(ext, needle);
    }

    // Every entry must serve the script it CLAIMS to serve. Non-empty is not
    // enough: an `include_str!` pointed at the wrong asset still yields a fat,
    // plausible body, and the user would install the wrong thing silently. Each
    // needle is code only that script would ever contain.
    #[test]
    fn every_catalog_body_is_the_script_it_claims_to_be() {
        let fingerprints = [
            (crate::abp::COSMETIC_SCRIPT_STEM, "window.__yggCosmetic"),
            (
                crate::abp::SCRIPTLET_SCRIPT_STEM,
                "W.__yggScriptlets = state;",
            ),
            (SPONSORBLOCK_STEM, "sponsor.ajay.app"),
            ("youtube-adblock", "'/youtubei/v1/player'"),
            ("idcac", "window.__yggIdcac"),
            ("unblock-select", "-webkit-user-select: text !important"),
        ];
        assert_eq!(
            fingerprints.len(),
            catalog().len(),
            "a catalog entry was added without a fingerprint to hold it honest"
        );
        for (stem, needle) in fingerprints {
            let ext = find(stem).unwrap_or_else(|| panic!("{stem} in catalog"));
            assert!(
                running_body(ext).contains(needle),
                "{stem} does not serve its own script (missing {needle:?}) — check its \
                 include_str! target"
            );
        }
        // Two entries pointing at one file is the other way this goes wrong.
        for (i, a) in catalog().iter().enumerate() {
            for b in catalog().iter().skip(i + 1) {
                assert_ne!(
                    a.body, b.body,
                    "{} and {} serve the same body",
                    a.stem, b.stem
                );
            }
        }
    }

    // The YouTube script's whole job is editing the player's answer, and the
    // field names and hook points it edits are the parts that rot when YouTube
    // moves. Locking them here means a maintainer who deletes one gets a test
    // naming it, not a user who quietly starts seeing ads again.
    #[test]
    fn youtube_adblock_keeps_the_shape_it_depends_on() {
        let ext = find("youtube-adblock").expect("youtube-adblock in catalog");

        // The ad fields pruned out of the player response.
        for field in [
            "'adPlacements'",
            "'adSlots'",
            "'playerAds'",
            "'adBreakHeartbeatParams'",
        ] {
            require(ext, field);
        }
        // The endpoints those fields arrive on.
        require(ext, "'/youtubei/v1/player'");
        require(ext, "'/playlist'");
        // The inline copy a cold page load ships instead of fetching: pruned
        // eagerly if it is already there, and through a setter if it is not.
        require(ext, "window.ytInitialPlayerResponse;");

        // Both network hooks, and the original captured before the page can wrap
        // it. Losing either one leaves half the player builds unpruned.
        require(ext, "var origFetch = window.fetch;");
        require(ext, "window.fetch = function");
        require(ext, "xhrProto.open = function");
        require(ext, "xhrProto.send = function");

        // THE TWO PARSE FUNNELS, and they are not optional. Measured on one
        // cold watch-page load in the ychrome engine on 2026-07-31: the fetch
        // hook saw ONE `/youtubei/v1/player` call and it was not the video's
        // player response, while `JSON.parse` was handed the real one 30 times
        // and `Response.prototype.json` twice more — every copy still carrying
        // all four ad fields. With only the network hooks, `getPlayerResponse()`
        // on the SECOND video of a session still answered
        // `["playerAds","adPlacements","adBreakHeartbeatParams"]`. That is the
        // whole of "the script runs and the user still sees ads".
        require(ext, "JSON.parse = function");
        require(ext, "responseProto.json = function");
        // …and the trap that makes the two layers cancel out. `pruneText` has
        // to parse with the parser it captured BEFORE hooking: with the hooked
        // one, the parse hook cleans the object first, `pruneAds` then finds
        // nothing to remove, and the fetch hook hands back the ORIGINAL TEXT to
        // any caller reading the body as text.
        require(ext, "var nativeParse = JSON.parse;");
        require(ext, "data = nativeParse(text);");
        assert!(
            !running_body(ext).contains("data = JSON.parse(text)"),
            "pruneText is parsing with the HOOKED JSON.parse. The parse hook will clean the \
             object first, the rewrite will decide nothing was removed, and the body will go \
             back to the page unedited — two working layers cancelling out, silently."
        );

        // The DOM belt for the day the response shape shifts.
        require(ext, "new MutationObserver(");
        require(ext, ".ytp-skip-ad-button");
        require(ext, ".ytp-ad-skip-button");
        require(ext, "video.currentTime = video.duration");
        // The belt must SAY it fired. An ad on screen means the network prune
        // missed, and a silent fallback turns that into a mystery — which is
        // exactly how "I still see youtube ads, sped up to 2x" reached the
        // user instead of "the blocker needs attention".
        require(ext, "function warnLayerOneMissed");
        require(ext, "console.warn(");
        assert!(
            !running_body(ext).contains("playbackRate ="),
            "the forced playbackRate is back. WebKit clamps it to about 2x, so it never \
             skipped an ad — it made the user watch every one of them at double speed while \
             hiding that layer 1 was dead."
        );
        // SPA navigation: without this the hooks bind once and never rebind.
        require(ext, "'yt-navigate-finish'");
    }

    // Ordering, not just presence: the host guard has to run BEFORE any global
    // is replaced. A fetch hook installed above the guard would sit on every
    // site the user browses, which is a different product than this one.
    #[test]
    fn youtube_adblock_guards_the_host_before_it_touches_a_global() {
        let ext = find("youtube-adblock").expect("youtube-adblock in catalog");
        let guard = code_index(ext, "test(location.hostname)");
        assert!(
            guard < code_index(ext, "window.fetch = function"),
            "the fetch hook is installed before the youtube.com guard"
        );
        assert!(
            guard < code_index(ext, "xhrProto.send = function"),
            "the XHR hook is installed before the youtube.com guard"
        );
        assert!(
            guard
                < code_index(
                    ext,
                    "Object.defineProperty(window, 'ytInitialPlayerResponse'"
                ),
            "the inline-response hook is installed before the youtube.com guard"
        );
        // The parse hooks are the WIDEST thing this script installs. `fetch` and
        // XHR are per-request; `JSON.parse` runs thousands of times on every
        // page in the browser. Installed above the guard, this blocker becomes a
        // tax on the whole web instead of a fix for one site.
        assert!(
            guard < code_index(ext, "JSON.parse = function"),
            "the JSON.parse hook is installed before the youtube.com guard"
        );
        assert!(
            guard < code_index(ext, "responseProto.json = function"),
            "the Response.prototype.json hook is installed before the youtube.com guard"
        );
    }

    // SponsorBlock's PRIVACY property, locked in the running body. Asking
    // `/api/skipSegments?videoID=<id>` tells sponsor.ajay.app exactly what the
    // user is watching, every single video. The hash-prefix endpoint tells it
    // four hex characters, which name thousands of videos and identify none —
    // and there must be no fallback to the by-id form, because a privacy
    // property that silently degrades is not one.
    #[test]
    fn sponsorblock_never_asks_by_video_id() {
        let ext = find(SPONSORBLOCK_STEM).expect("sponsorblock in catalog");
        let body = running_body(ext);
        assert!(
            !body.contains("videoID="),
            "sponsorblock builds a by-id query — that leaks the exact video to a third party \
             on every watch"
        );
        require(ext, "crypto.subtle");
        require(ext, "'SHA-256'");
        require(ext, "api/skipSegments/");
        require(ext, "HASH_PREFIX_LENGTH");
        // The match happens in the browser, over the prefix answer.
        require(ext, "row.videoID !== videoId");
        // And the filters that stop it skipping things the community did not
        // mean. ⚠ These three locks were REWRITTEN for v2 rather than dropped:
        // v1 refused every non-`skip` actionType outright, which is why a
        // `mute` segment was silently discarded; v2 routes each action type to
        // its own behaviour, so the property worth locking is that a `full`
        // segment (which labels the WHOLE video) can never become a seek.
        require(ext, "if (action === 'full') return 'label';");
        require(ext, "seg.votes < MIN_VOTES");
        require(ext, "var MIN_VOTES = -1;");
    }

    // v1 asked for three categories and took the API's default `actionTypes`,
    // so eight categories and every mute/full/poi/chapter segment were
    // invisible — measured against the live API, 48.7% of videos that HAVE
    // community segments had nothing in those three, which is what "SponsorBlock
    // does not work" looked like from the sofa. The query must name what it
    // wants, both halves.
    #[test]
    fn sponsorblock_asks_for_more_than_the_api_default() {
        let ext = find(SPONSORBLOCK_STEM).expect("sponsorblock in catalog");
        require(ext, "'?categories='");
        require(ext, "'&actionTypes='");
        let body = running_body(ext);
        for action in ["'skip'", "'mute'", "'full'", "'poi'", "'chapter'"] {
            assert!(
                body.contains(action),
                "sponsorblock never asks for the {action} action type"
            );
        }
    }

    // The cookie script must never consent on the user's behalf. `REJECT_TEXT`
    // is the ONLY list it clicks from, so that array is exactly where a wrong
    // phrase would have to be added — check it rather than the whole file, which
    // says the words "accept all" in the comment that promises not to click it.
    #[test]
    fn idcac_clicks_nothing_that_consents() {
        let ext = find("idcac").expect("idcac in catalog");
        let body = running_body(ext);
        let start = code_index(ext, "var REJECT_TEXT = [");
        let end = start
            + body[start..]
                .find("];")
                .expect("REJECT_TEXT is an array literal");
        // Entry by entry, not substring: "continue without accepting" is a
        // REJECT phrase that contains the word "accepting", and "reject cookies"
        // contains the letters "ok". A consent button says its verb FIRST.
        let phrases: Vec<String> = body[start..end]
            .split('\'')
            .skip(1)
            .step_by(2)
            .map(|phrase| phrase.trim().to_lowercase())
            .collect();
        assert!(
            phrases.len() >= 10,
            "REJECT_TEXT did not parse: {phrases:?}"
        );
        // A consent WORD anywhere disqualifies a phrase — unless the phrase also
        // negates it. `starts_with` was not enough: "i accept" starts with "i",
        // so prepending it to REJECT_TEXT left this lock green while the script
        // would click the very button it exists to avoid (found by adversarial
        // review). "continue without accepting" is the case the negation clause
        // exists for: it carries "accepting" and is a REJECT phrase.
        // "do not consent" and "do not sell my personal information" are
        // REJECT phrases that carry a consent word; "not " is what makes them
        // legible, exactly as "without" does for "continue without accepting".
        // "sans" is French for "without" and carries the same weight in
        // "continuer sans accepter"; a list that says no in six languages needs
        // its negations in six languages too.
        const NEGATIONS: [&str; 11] = [
            "without", "sans ", "non-", "only", "reject", "decline", "refuse", "deny", "no ",
            "not ", "never",
        ];
        for phrase in &phrases {
            let negated = NEGATIONS.iter().any(|negation| phrase.contains(negation));
            // English, plus the consent verbs of every language REJECT_TEXT
            // now speaks. A list that says no in six languages and only
            // recognises yes in one is a lock with a hole in it: "alle
            // akzeptieren" is German for "accept all" and used to pass.
            for consent in [
                "accept",
                "agree",
                "allow",
                "consent",
                "enable all",
                "understood",
                "got it",
                "okay",
                "yes",
                // German
                "akzept",
                "zustimm",
                "einverstanden",
                "erlauben",
                // Spanish / Portuguese
                "acept",
                "aceit",
                "permitir",
                // Italian
                "accett",
                "accordo",
                // Dutch
                "akkoord",
                "toestaan",
                // Nordic
                "godta",
                "godkann",
                "godkänn",
                "tillad",
                "tillat",
                // Polish
                "akcept",
                "zgadzam",
            ] {
                // A consent word counts when it stands as its own word, or
                // when it BEGINS one — "acceptera alla" is Swedish for
                // "accept all" and must be caught. It does not count when a
                // letter runs into it from the left: "disagree" carries the
                // letters of "agree" and means the opposite. This is a
                // sharper rule than a bare substring, not a looser one; the
                // adversarial case the comment above names ("i accept",
                // which starts with "i") is still caught, because "accept"
                // there begins a word.
                let carries = phrase.match_indices(consent).any(|(at, _)| {
                    at == 0
                        || !phrase[..at]
                            .chars()
                            .next_back()
                            .is_some_and(|ch| ch.is_alphanumeric())
                });
                assert!(
                    !(carries && !negated),
                    "idcac would click a consent button: REJECT_TEXT has {phrase:?} \
                     (consent word {consent:?}, and nothing in the phrase negates it)"
                );
            }
            // "ok" as its own WORD only — "reject cookies" contains the letters
            // o-k, and a substring rule here would forbid the list's own spine.
            assert!(
                !phrase
                    .split(|ch: char| !ch.is_alphanumeric())
                    .any(|word| word == "ok"),
                "idcac would click an OK button: REJECT_TEXT has {phrase:?}"
            );
        }
        assert!(
            phrases.iter().any(|phrase| phrase == "reject all"),
            "REJECT_TEXT lost 'reject all': {phrases:?}"
        );
        // The list's whole value is that it says no in more ways than the
        // ruleset ever can, so it must stay broad and must stay multilingual.
        assert!(
            phrases.len() >= 45,
            "REJECT_TEXT shrank to {} phrases — this list is the one job no \
             content-blocker rule can do",
            phrases.len()
        );
        for must in [
            "decline all",
            "alle ablehnen",
            "tout refuser",
            "rechazar todo",
            "do not consent",
        ] {
            assert!(
                phrases.iter().any(|phrase| phrase == must),
                "REJECT_TEXT lost {must:?}"
            );
        }
        // The precision rule above, pinned in both directions.
        assert!(
            phrases.iter().any(|phrase| phrase == "disagree"),
            "'disagree' is a REJECT phrase; a substring rule that reads it as \
             'agree' is the rule that is wrong"
        );
        // Hiding a banner without undoing its scroll lock leaves an unreadable
        // page — the third thing it does, and the easy one to drop.
        require(ext, "function unlockScrolling");
    }

    /// A private temp dir for one test, removed on the way in so a rerun starts
    /// clean. Keyed by test name — no randomness, no shared state.
    fn scratch_dir(test: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ychrome-extensions-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    // Source needles prove a line still exists; they cannot prove it still
    // WORKS. This runs the body the catalog actually serves against a fixture
    // `/youtubei/v1/player` response under node, and checks the ad fields come
    // out gone, the video comes out intact, a non-player URL comes back
    // byte-identical, and an off-YouTube host gets no hooks at all.
    //
    // node is a TEST-TIME dependency (it is on every host in this fleet). If it
    // is genuinely absent, `YCHROME_ALLOW_NO_NODE=1` skips — an explicit opt-out,
    // so this lock can never quietly stop running.
    #[test]
    fn youtube_adblock_actually_prunes_a_player_response() {
        let ext = find("youtube-adblock").expect("youtube-adblock in catalog");
        let node_ok = std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !node_ok {
            assert!(
                std::env::var_os("YCHROME_ALLOW_NO_NODE").is_some(),
                "node is needed to run the youtube-adblock behaviour lock; install it, or \
                 set YCHROME_ALLOW_NO_NODE=1 to knowingly ship without this proof"
            );
            return;
        }

        let dir = scratch_dir("youtube-adblock");
        // The body from the CATALOG, not the asset file: this way a mis-pointed
        // include_str! fails the behaviour lock too.
        let script = dir.join("youtube-adblock.js");
        std::fs::write(&script, ext.body).expect("write the script under test");
        let harness = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/youtube-adblock-harness.js");

        // Two YouTube hosts (the guard must admit both) and two that must be
        // left alone.
        for host in [
            "www.youtube.com",
            "music.youtube.com",
            "example.com",
            "youtube.com.attacker.net",
        ] {
            let out = std::process::Command::new("node")
                .arg(&harness)
                .arg(&script)
                .arg(host)
                .output()
                .expect("run the node harness");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && stdout.contains("ALL OK"),
                "youtube-adblock harness failed on {host}:\n{stdout}\n{stderr}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The GENERATED cosmetic script, DRIVEN. Source needles prove `:has-text`
    // is mentioned; only running it proves the element gets hidden. Three
    // hosts: one with a :has-text rule, one with a :style rule, and one that is
    // not in the list at all — the last is the performance contract, because a
    // text-scanning observer on every page in the browser is a different
    // product than this one.
    #[test]
    fn the_generated_cosmetic_script_actually_hides_and_styles() {
        let ext = find(crate::abp::COSMETIC_SCRIPT_STEM).expect("cosmetic-filters in catalog");
        let node_ok = std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !node_ok {
            assert!(
                std::env::var_os("YCHROME_ALLOW_NO_NODE").is_some(),
                "node is needed to run the cosmetic-filters behaviour lock; install it, or \
                 set YCHROME_ALLOW_NO_NODE=1 to knowingly ship without this proof"
            );
            return;
        }

        // Pick the hosts out of the script's OWN payload, so the lock never
        // encodes a domain that a regeneration might drop.
        let payload = ext
            .body
            .split("var RULES = ")
            .nth(1)
            .and_then(|rest| rest.split(";\n").next())
            .expect("the generated script carries a RULES payload");
        let rules: serde_json::Value =
            serde_json::from_str(payload).expect("the RULES payload is JSON");
        let map = rules.as_object().expect("RULES is an object");
        assert!(
            map.len() > 100,
            "the generated script covers only {} domains — a regeneration that produced \
             almost nothing",
            map.len()
        );
        let host_with = |kind: &str| -> String {
            map.iter()
                .find(|(_, list)| {
                    list.as_array()
                        .is_some_and(|rules| rules.iter().any(|rule| rule[0] == kind))
                })
                .map(|(domain, _)| domain.clone())
                .unwrap_or_else(|| panic!("no domain carries a {kind:?} rule"))
        };

        let dir = scratch_dir("cosmetic-filters");
        let script = dir.join("cosmetic-filters.js");
        std::fs::write(&script, ext.body).expect("write the script under test");
        let harness = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cosmetic-filters-harness.js");
        for host in [
            host_with("t"),
            host_with("s"),
            "not-in-the-list.example".to_string(),
        ] {
            let out = std::process::Command::new("node")
                .arg(&harness)
                .arg(&script)
                .arg(&host)
                .output()
                .expect("run the node harness");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && stdout.contains("ALL OK"),
                "cosmetic-filters harness failed on {host}:\n{stdout}\n{stderr}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // THE ANTI-DRIFT LOCK, and the most important test in this file.
    //
    // `abp::SCRIPTLETS` decides which `##+js(...)` filters get ROUTED into the
    // generated script; `assets/web-scriptlets/runtime.js` decides which ones
    // that script can RUN. A canonical name in the table with no implementation
    // in the runtime routes filters into a runtime that ignores them — the
    // ruleset would report thousands of scriptlets "supported" and the user
    // would see the ads anyway. That is exactly the silent-degradation shape
    // this converter was built to end, so it is a test, not a comment.
    #[test]
    fn the_scriptlet_table_and_the_runtime_are_one_contract() {
        let runtime = crate::abp::SCRIPTLET_RUNTIME;
        for entry in crate::abp::SCRIPTLETS {
            assert!(
                runtime.contains(&format!("'{}': function", entry.canonical)),
                "abp::SCRIPTLETS routes {:?} but runtime.js has no implementation for it — \
                 every filter naming it would be counted as supported and then ignored",
                entry.canonical
            );
        }
        // …and the other direction: an implementation nothing routes to is dead
        // weight in a body injected on 5,000 domains.
        let mut implemented: Vec<&str> = Vec::new();
        for line in runtime.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix('\'')
                && let Some((name, tail)) = rest.split_once('\'')
                && tail.starts_with(": function")
            {
                implemented.push(name);
            }
        }
        for name in &implemented {
            assert!(
                crate::abp::SCRIPTLETS
                    .iter()
                    .any(|entry| entry.canonical == *name),
                "runtime.js implements {name:?} but abp::SCRIPTLETS never routes a filter to it"
            );
        }
        assert_eq!(
            implemented.len(),
            crate::abp::SCRIPTLETS.len(),
            "the table and the runtime disagree on how many scriptlets exist"
        );
        // Aliases must be unambiguous: two entries claiming one spelling would
        // make the winner depend on table order.
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for entry in crate::abp::SCRIPTLETS {
            assert!(
                seen.insert(entry.canonical),
                "duplicate {:?}",
                entry.canonical
            );
            for alias in entry.aliases {
                assert!(
                    seen.insert(alias),
                    "{alias:?} is claimed by more than one scriptlet"
                );
            }
        }
    }

    // The GENERATED scriptlet script, DRIVEN. Source needles prove a name is
    // mentioned; only running it proves `window.open` is actually refused and
    // that a page with no rules pays nothing.
    #[test]
    fn the_generated_scriptlet_script_actually_runs_its_scriptlets() {
        let ext = find(crate::abp::SCRIPTLET_SCRIPT_STEM).expect("scriptlets in catalog");
        let node_ok = std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !node_ok {
            assert!(
                std::env::var_os("YCHROME_ALLOW_NO_NODE").is_some(),
                "node is needed to run the scriptlet behaviour lock; install it, or set \
                 YCHROME_ALLOW_NO_NODE=1 to knowingly ship without this proof"
            );
            return;
        }
        let dir = scratch_dir("scriptlets");
        let script = dir.join("scriptlets.js");
        std::fs::write(&script, ext.body).expect("write the script under test");
        let harness = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scriptlets-harness.js");
        // The probe host drives one synthetic rule per implemented scriptlet;
        // the second is the performance contract, on a host with no rules.
        for host in ["ychrome-probe.test", "not-in-the-list.example"] {
            let out = std::process::Command::new("node")
                .arg(&harness)
                .arg(&script)
                .arg(host)
                .output()
                .expect("run the node harness");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && stdout.contains("ALL OK"),
                "scriptlets harness failed on {host}:\n{stdout}\n{stderr}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The world is the whole ballgame for this one. A scriptlet edits the
    // PAGE's globals; in an isolated world every edit is invisible to the page
    // and the script reports success while changing nothing. That is not
    // hypothetical here — it is exactly how `youtube-adblock` shipped broken
    // (docs/adblock.md §6), and a scriptlet plane is strictly more exposed.
    #[test]
    fn the_scriptlet_script_runs_in_the_main_world() {
        let ext = find(crate::abp::SCRIPTLET_SCRIPT_STEM).expect("scriptlets in catalog");
        let parsed = crate::userscript::parse(ext.body);
        assert_eq!(
            parsed.world,
            crate::userscript::ScriptWorld::Main,
            "the scriptlet script must run in the MAIN world; an isolated one cannot touch \
             a single page global it exists to edit"
        );
        assert!(
            !parsed.matches.is_empty(),
            "the scriptlet script must be @match-scoped to the domains that have rules — \
             unscoped, it patches page globals on every site in the browser"
        );
    }

    // What the SHIPPED bodies declare, read back through the parser that will
    // read them on a user's disk. These bodies are `include_str!`d, so this is a
    // lock on the asset files themselves: edit a header wrongly and the build
    // that embeds it fails here.
    #[test]
    fn the_bundled_scripts_declare_the_placement_they_need() {
        use crate::userscript::{ScriptWorld, parse};

        // SponsorBlock is a YouTube script: it must be scoped, or every tab in
        // the browser pays for it. Isolated is enough — it talks to the DOM and
        // the network, never to a page global.
        let sponsorblock = parse(find(SPONSORBLOCK_STEM).expect("sponsorblock").body);
        assert!(
            sponsorblock
                .matches
                .iter()
                .any(|pattern| pattern.contains("youtube.com")),
            "sponsorblock must be @match-scoped to YouTube, got {:?}",
            sponsorblock.matches
        );
        assert_eq!(sponsorblock.world, ScriptWorld::Isolated);

        // The selection unblocker is deliberately global and deliberately
        // frame-crossing: the text you cannot select is usually in an iframe.
        let unblock = parse(find("unblock-select").expect("unblock-select").body);
        assert!(
            unblock.matches.is_empty(),
            "unblock-select applies everywhere by design"
        );
        assert!(unblock.all_frames, "the blocked text is usually in a frame");
        assert_eq!(unblock.world, ScriptWorld::Isolated);
    }

    // Every bundled body must carry a metadata block at all. A catalog entry
    // added without one silently inherits "every URL, isolated world", which for
    // a site-specific script is a per-tab cost on every page the user opens.
    #[test]
    fn every_bundled_script_carries_a_metadata_block() {
        for ext in catalog() {
            assert!(
                ext.body.contains("==UserScript=="),
                "{} ships with no metadata block",
                ext.stem
            );
        }
    }
}
