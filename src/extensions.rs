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

static CATALOG: [Extension; 4] = [
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

        // The DOM belt for the day the response shape shifts.
        require(ext, "new MutationObserver(");
        require(ext, ".ytp-skip-ad-button");
        require(ext, ".ytp-ad-skip-button");
        require(ext, "video.playbackRate = 16");
        require(ext, "video.currentTime = video.duration");
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
        const NEGATIONS: [&str; 9] = [
            "without", "non-", "only", "reject", "decline", "refuse", "deny", "no ", "never",
        ];
        for phrase in &phrases {
            let negated = NEGATIONS
                .iter()
                .any(|negation| phrase.contains(negation));
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
            ] {
                let carries = phrase
                    .split(|ch: char| !ch.is_alphanumeric())
                    .any(|word| word == consent)
                    || phrase.contains(consent);
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
}
