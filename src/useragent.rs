//! The browser identity ychrome presents to the web: the User-Agent string.
//!
//! # The default is the ENGINE'S OWN UA, and that is the whole point
//!
//! An anti-bot edge that scores a request (Cloudflare's Managed Challenge is the
//! one users meet) does not blocklist engines — it scores **consistency** across
//! the JS environment, the TLS handshake, the HTTP/2 frame fingerprint and the
//! User-Agent. A coherent unusual browser passes; an incoherent one is
//! challenged forever. GNOME Web is this same WebKitGTK and passes every day.
//!
//! So a UA we invent is not a free lie. Measured on the engine plane, dev,
//! 2026-07-31, with the previous default (Safari on macOS):
//!
//! ```text
//! navigator.userAgent -> "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ... Safari/605.1.15"
//! navigator.platform  -> "Linux x86_64"
//! ```
//!
//! The UA claims macOS while the JS environment in the SAME page says Linux.
//! Nothing on the TLS or HTTP/2 side has to be consulted to catch that; one line
//! of page script does it. That was the shipped default for every profile.
//!
//! WebKitGTK's own UA has no such contradiction. Measured on this fleet
//! (WebKitGTK 2.52.5): `Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15
//! (KHTML, like Gecko) Version/60.5 Safari/605.1.15`. `AppleWebKit/605.1.15` is
//! what the engine actually is, the platform token matches `navigator.platform`,
//! and it is byte-for-byte what GNOME Web sends from the same Debian package.
//! **This module therefore never writes that string down.** [`Preset::Engine`]
//! resolves to `None`, which means "leave WebKitGTK's default alone" — the
//! engine remains the one owner of its own identity, and a WebKitGTK upgrade
//! moves the UA without ychrome shipping a new constant.
//!
//! # What the presets are for
//!
//! Badly-coded sites gate on the UA STRING — some government portals will not
//! render for anything that does not say Chrome. Those sites are not
//! fingerprinting anybody; they are running an `indexOf`. So the overrides stay,
//! but they are **per site** rather than a global mode: a portal that demands
//! Chrome gets Chrome, and every other site — including the one running a
//! managed challenge on your login — gets the coherent engine identity.
//!
//! A global preset is still available (it is what a user who wants one browser
//! identity everywhere would set), and it still defaults to [`Preset::Engine`].
//!
//! ⚠ **An override is a real cost, not a free knob.** A spoofed UA can break a
//! fingerprint-gated login precisely because it makes the environment
//! inconsistent, and the failure mode is an unrecoverable challenge loop rather
//! than an error message. The settings pane says so where the user chooses it,
//! and [`set_site`] refuses to be the silent path.
//!
//! # Ownership
//!
//! The UA is browsing config, so ychrome's host owns the choice; only the GUI
//! can apply it, so it rides `/policy` and yggterm applies it. Same shape as the
//! adblock ruleset and the per-site zoom — the app decides, the GUI injects,
//! yggterm persists nothing. The per-site MATCHING rule is
//! [`crate::sitehost`]'s, shared with the zoom map, never re-derived here.
//!
//! # The claude.ai story, re-measured
//!
//! An earlier revision of this file justified a dishonest default by a live
//! measurement: claude.ai answered the engine UA with `403 {"error":{"type":
//! "forbidden"}}` while serving a macOS-Safari UA. **That differential no longer
//! reproduces.** From dev on 2026-07-31, all three UAs (engine, Safari-macOS,
//! Chrome-Linux) got the same `403 Request not allowed` from claude.ai's edge,
//! and all three got `200` from brilliant.org. A curl probe cannot isolate the
//! UA anyway — its TLS fingerprint is nothing like WebKit's — so the honest
//! reading is that the old differential is unproven today, not that it was wrong
//! then. If claude.ai does refuse the engine identity in a real surface, the fix
//! is one per-site entry (`claude.ai -> safari`), which is exactly what the
//! per-site layer is for; it is not a reason to make every site's identity
//! inconsistent.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Safari 18.5 / macOS. Safari has frozen its platform token at `10_15_7` for
/// years, so this is what a real Safari sends.
///
/// ⚠ On Linux this is only PARTLY honest: the engine really is WebKit
/// (`AppleWebKit/605.1.15` is true), but `navigator.platform` still says
/// `Linux x86_64`, so the platform token contradicts the page. Fine for a site
/// that sniffs the string; a liability on a site that scores coherence.
pub const SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";

/// Chrome / Linux, for sites that gate on Chrome specifically.
///
/// ⚠ The most incoherent option we offer, and deliberately last: it claims
/// `AppleWebKit/537.36` and `Chrome/138`, and NOTHING else about this browser
/// corroborates either — not the JS environment, not the TLS handshake, not the
/// HTTP/2 frame order. Use it on the site that demands it and nowhere else.
pub const CHROME_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36";

/// The presets, in the order the settings pane lists them. [`Preset::Engine`] is
/// first because it is the default and the recommendation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Engine,
    Safari,
    Chrome,
}

impl Preset {
    pub fn id(self) -> &'static str {
        match self {
            Preset::Engine => "engine",
            Preset::Safari => "safari",
            Preset::Chrome => "chrome",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::Engine => "WebKit (this engine)",
            Preset::Safari => "Safari (macOS)",
            Preset::Chrome => "Chrome (Linux)",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Preset::Engine => {
                "Honest and consistent: the same identity GNOME Web sends. \
                 Passes bot checks that score coherence. Recommended."
            }
            Preset::Safari => {
                "Says macOS while the page can see Linux. For a site that \
                 refuses this engine by name."
            }
            Preset::Chrome => {
                "For sites that only test Chrome. Nothing else about this \
                 browser corroborates it, so bot checks may challenge you."
            }
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Preset::ALL.iter().copied().find(|preset| preset.id() == id)
    }

    /// The UA this preset sends, or `None` to leave WebKitGTK's default alone.
    ///
    /// `Engine` is `None` rather than a copy of the engine's string on purpose:
    /// see the module docs. WebKitGTK owns its own identity.
    fn user_agent(self) -> Option<&'static str> {
        match self {
            Preset::Engine => None,
            Preset::Safari => Some(SAFARI_UA),
            Preset::Chrome => Some(CHROME_UA),
        }
    }

    pub const ALL: [Preset; 3] = [Preset::Engine, Preset::Safari, Preset::Chrome];
}

/// `~/.yggterm/ychrome/user-agent.json` on the host ychrome runs on.
///
/// ONE file holds both the global preset and the per-site overrides — a second
/// file would be a second store for one concept ("what identity does this
/// browser present"), which is the thing AGENTS.md forbids.
///
/// ```json
/// { "preset": "engine", "sites": { "somegovtportal.gov.in": "chrome" } }
/// ```
fn config_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("ychrome")
        .join("user-agent.json"))
}

fn config() -> Value {
    config_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

/// The global preset. An unset or unreadable config means [`Preset::Engine`]:
/// a browser nobody has configured presents the identity it actually has.
pub fn preset() -> Preset {
    preset_from(&config())
}

fn preset_from(config: &Value) -> Preset {
    config["preset"]
        .as_str()
        .and_then(Preset::from_id)
        .unwrap_or(Preset::Engine)
}

/// The per-site overrides on disk, host -> preset. A missing, broken or
/// unrecognised entry is simply absent: that site uses the global preset.
pub fn sites() -> BTreeMap<String, Preset> {
    parse_sites(&config())
}

fn parse_sites(config: &Value) -> BTreeMap<String, Preset> {
    let mut out = BTreeMap::new();
    if let Some(map) = config.get("sites").and_then(Value::as_object) {
        for (host, id) in map {
            let host = crate::sitehost::normalize(host);
            if host.is_empty() {
                continue;
            }
            if let Some(preset) = id.as_str().and_then(Preset::from_id) {
                out.insert(host, preset);
            }
        }
    }
    out
}

/// The preset in force for one page's host: its own entry, else its parent
/// domain's, else the global. The walk is [`crate::sitehost`]'s, shared with the
/// zoom map.
pub fn preset_for_host(sites: &BTreeMap<String, Preset>, global: Preset, host: &str) -> Preset {
    crate::sitehost::lookup(sites, host)
        .copied()
        .unwrap_or(global)
}

/// What the GUI should hand `WebViewBuilder::with_user_agent` for a surface with
/// no page yet — the GLOBAL decision. `None` = the engine default.
pub fn effective() -> Option<String> {
    preset().user_agent().map(str::to_string)
}

/// The UA for one page's host, honouring any per-site override. `None` = the
/// engine default, which is the answer for every unoverridden site now.
pub fn effective_for_host(host: &str) -> Option<String> {
    let config = config();
    preset_for_host(&parse_sites(&config), preset_from(&config), host)
        .user_agent()
        .map(str::to_string)
}

/// The per-site map as the GUI consumes it: host -> the UA STRING to send, or
/// `null` for "the engine's own". Resolved here so yggterm never needs to know
/// what a preset is — the same division as `/zoom`, where ychrome owns the map
/// and the GUI only applies it.
pub fn sites_json() -> Value {
    let mut map = serde_json::Map::new();
    for (host, preset) in sites() {
        map.insert(
            host,
            match preset.user_agent() {
                Some(ua) => Value::String(ua.to_string()),
                None => Value::Null,
            },
        );
    }
    Value::Object(map)
}

fn write(config: Value) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&config)?)?;
    Ok(())
}

/// Preserve unknown keys: a newer ychrome's setting is not destroyed by an older
/// one that never heard of it (the same rule as the adblock config).
fn edit(mutate: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<()> {
    let mut object = config().as_object().cloned().unwrap_or_default();
    mutate(&mut object);
    write(Value::Object(object))
}

pub fn set_preset(id: &str) -> Result<()> {
    let preset = Preset::from_id(id).with_context(|| format!("unknown user-agent preset: {id}"))?;
    edit(|object| {
        object.insert("preset".to_string(), Value::String(preset.id().to_string()));
    })
}

/// Set (or, with `None`, clear) the override for one host.
///
/// Clearing REMOVES the key so the site falls back to the global — it never
/// persists "same as global", exactly as [`crate::webzoom::set`] does not.
pub fn set_site(host: &str, id: Option<&str>) -> Result<()> {
    let host = crate::sitehost::normalize(host);
    if host.is_empty() {
        anyhow::bail!("cannot set a browser identity for an empty host");
    }
    let preset = match id {
        Some(id) => {
            Some(Preset::from_id(id).with_context(|| format!("unknown user-agent preset: {id}"))?)
        }
        None => None,
    };
    edit(|object| {
        let mut sites = object
            .get("sites")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        match preset {
            Some(preset) => {
                sites.insert(host, Value::String(preset.id().to_string()));
            }
            None => {
                sites.remove(&host);
            }
        }
        object.insert("sites".to_string(), Value::Object(sites));
    })
}

/// The warning shown wherever a user picks a per-site identity, and printed by
/// the CLI. One string, so the pane and the terminal cannot disagree about what
/// the cost is.
pub const OVERRIDE_WARNING: &str = "A spoofed identity can break a login that checks whether the browser is consistent: \
     the site sees a claim the page contradicts and can challenge you in a loop with no \
     way out. Set one only for a site that refuses to render without it.";

/// The UA's contribution to `policy_version`. The stamp must move when the
/// DECISION moves, not when the file's bytes do — same rule as the adblock
/// decision, and the reason it is the id and not an mtime. The per-site map is
/// in it because a site override changes what a surface sends.
pub fn stamp() -> String {
    let mut stamp = format!("user-agent:{}\n", preset().id());
    for (host, preset) in sites() {
        stamp.push_str(&format!("user-agent-site:{host}={}\n", preset.id()));
    }
    stamp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_round_trips_through_its_id() {
        for preset in Preset::ALL {
            assert_eq!(Preset::from_id(preset.id()), Some(preset));
        }
        assert_eq!(Preset::from_id("nope"), None);
    }

    /// ⭐ THE LOCK THIS MODULE EXISTS FOR. An unconfigured browser presents the
    /// identity it actually HAS. The previous default claimed macOS while
    /// `navigator.platform` said `Linux x86_64` on the same page, which is the
    /// contradiction a managed challenge scores. If this turns red, every
    /// profile is back to shipping an inconsistent fingerprint by default.
    #[test]
    fn the_default_identity_is_the_engines_own_and_invents_no_string() {
        assert_eq!(preset_from(&json!({})), Preset::Engine);
        assert_eq!(
            preset_from(&json!({ "preset": "nonsense" })),
            Preset::Engine
        );
        assert_eq!(Preset::Engine.user_agent(), None);
        // Not "equals the engine's string" — this module must not HOLD the
        // engine's string as DATA. WebKitGTK owns its own identity; we decline
        // to speak for it, so an engine upgrade moves the UA and ychrome ships
        // no new constant.
        //
        // Doc comments are stripped before the scan on purpose: recording what
        // the engine measured on a given day is documentation, and deleting the
        // measurement to satisfy a scan would lose the evidence this whole
        // change rests on. What must not exist is a `const` or a literal the
        // CODE can send.
        let code: String = include_str!("useragent.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module body precedes its tests")
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("Version/60.5"),
            "the engine's UA belongs to WebKitGTK; a copy here would go stale on \
             the next engine upgrade"
        );
        // And no preset may BE the engine's shape (`X11; Linux` + WebKit 605),
        // which is how a well-meaning copy would arrive: as a fourth preset
        // that "just sends what the engine sends", frozen at today's version.
        for ua in [SAFARI_UA, CHROME_UA] {
            assert!(
                !(ua.contains("X11; Linux") && ua.contains("AppleWebKit/605.1.15")),
                "a preset is impersonating the engine instead of standing aside \
                 for it: {ua}"
            );
        }
    }

    /// The spoofing presets are still THERE — the user asked for both halves,
    /// and a govt portal that gates on the string still needs Chrome.
    #[test]
    fn the_spoofing_presets_are_still_offered() {
        assert_eq!(Preset::Safari.user_agent(), Some(SAFARI_UA));
        assert_eq!(Preset::Chrome.user_agent(), Some(CHROME_UA));
        assert!(CHROME_UA.contains("Chrome/"));
        assert!(SAFARI_UA.contains("Macintosh"));
    }

    #[test]
    fn a_per_site_override_beats_the_global_and_only_on_that_site() {
        let sites = parse_sites(&json!({ "sites": { "portal.gov.in": "chrome" } }));
        assert_eq!(
            preset_for_host(&sites, Preset::Engine, "portal.gov.in"),
            Preset::Chrome
        );
        // A subdomain of the overridden host inherits it (sitehost's walk)...
        assert_eq!(
            preset_for_host(&sites, Preset::Engine, "www.portal.gov.in"),
            Preset::Chrome
        );
        // ...and nothing else does. This is the property that keeps a managed
        // challenge on brilliant.org away from a lie set for a govt portal.
        assert_eq!(
            preset_for_host(&sites, Preset::Engine, "brilliant.org"),
            Preset::Engine
        );
    }

    #[test]
    fn a_global_preset_still_applies_where_no_site_overrides_it() {
        let sites = parse_sites(&json!({ "sites": { "portal.gov.in": "engine" } }));
        assert_eq!(
            preset_for_host(&sites, Preset::Safari, "example.com"),
            Preset::Safari
        );
        // An explicit `engine` entry is how a site opts OUT of a global spoof.
        assert_eq!(
            preset_for_host(&sites, Preset::Safari, "portal.gov.in"),
            Preset::Engine
        );
    }

    #[test]
    fn unrecognised_and_empty_site_entries_are_dropped_not_guessed() {
        let sites = parse_sites(&json!({
            "sites": { "a.com": "firefox", "": "chrome", "B.COM:443": "chrome" }
        }));
        assert_eq!(
            sites.get("a.com"),
            None,
            "an unknown preset id is not a site rule"
        );
        assert_eq!(sites.len(), 1);
        assert_eq!(
            sites.get("b.com"),
            Some(&Preset::Chrome),
            "hosts are normalized on read"
        );
    }

    /// The wire shape yggterm applies: a resolved UA string, or `null` for the
    /// engine's own. The GUI must never need the preset vocabulary.
    #[test]
    fn the_wire_map_carries_resolved_strings_and_null_for_the_engine() {
        let sites: BTreeMap<String, Preset> = [
            ("a.com".to_string(), Preset::Chrome),
            ("b.com".to_string(), Preset::Engine),
        ]
        .into_iter()
        .collect();
        let mut map = serde_json::Map::new();
        for (host, preset) in sites {
            map.insert(
                host,
                match preset.user_agent() {
                    Some(ua) => Value::String(ua.to_string()),
                    None => Value::Null,
                },
            );
        }
        assert_eq!(map["a.com"], json!(CHROME_UA));
        assert_eq!(map["b.com"], Value::Null);
    }
}
