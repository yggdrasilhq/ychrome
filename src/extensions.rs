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

static CATALOG: [Extension; 2] = [
    Extension {
        stem: SPONSORBLOCK_STEM,
        name: "SponsorBlock",
        description: "Auto-skip sponsor and self-promo segments on YouTube.",
        body: include_str!("../assets/web-userscripts/sponsorblock.js"),
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

    // The bundled SponsorBlock is the real script, not a stub: it must reference
    // the SponsorBlock API, or "Install" would ship something inert.
    #[test]
    fn sponsorblock_body_is_the_real_script() {
        let ext = find(SPONSORBLOCK_STEM).expect("sponsorblock in catalog");
        assert!(ext.body.contains("sponsor.ajay.app"));
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
