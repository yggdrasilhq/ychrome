//! ychrome's PER-ORIGIN CAMERA AND MICROPHONE memory — the only place a
//! capture decision is remembered.
//!
//! The engine mechanics live in yggterm (`vendor/dioxus-desktop/src/web_surface.rs`):
//! it turns `enable-media-stream` on per web surface, answers WebKitGTK's
//! `permission-request`, and shows the native prompt. What it deliberately does
//! NOT do is remember the answer. Site policy is ychrome's — the same ownership
//! that puts ad blocking, per-site zoom and per-site browser identity here — so
//! yggterm asks this module before it prompts and tells it what the human said.
//!
//! ```text
//! page --getUserMedia()--> WebKitGTK --permission-request--> yggterm
//! yggterm --GET  <control>/media-permission?origin=&audio=&video=--> ychrome
//!                                    { "decision": "allow" | "deny" | "ask" }
//! (ask) yggterm --native prompt--> the human
//! yggterm --POST <control>/media-permission {origin, camera, microphone}--> ychrome
//! ```
//!
//! ## The key is an ORIGIN, and the match is EXACT
//!
//! ⛔ This module deliberately does NOT use [`crate::sitehost`]'s longest-suffix
//! walk, even though zoom and browser identity both do. Those are readability
//! and fingerprinting preferences, where "an entry for `youtube.com` also covers
//! `music.youtube.com`" is a convenience. A capture grant is not a preference —
//! it is a capability over hardware in the room the user is sitting in, and
//! suffix reach would mean that granting a camera to one site silently granted
//! it to every sub-domain anyone can host under that domain, including a
//! user-content sub-domain. Chrome and Firefox both key media permissions to an
//! exact origin; so does this.
//!
//! The key is the full origin (`https://example.com`, port included when it is
//! not the scheme's default), not the bare host, for the same reason: `http://`
//! and `https://` are different security contexts and a grant must not cross
//! between them.
//!
//! ## The tri-state
//!
//! `allow` / `deny` / `ask`, per device class, per origin. `ask` is the DEFAULT
//! and is stored as absence — an origin with nothing recorded asks every time.
//! That makes "revoke" a deletion rather than a third stored value, so there is
//! no way to have a stale `ask` row disagreeing with an empty one.
//!
//! The file is host-global (all profiles share it), like `web-zoom.json`: the
//! camera is a property of the machine, not of the browsing identity looking
//! through it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// What is remembered for one origin and one device class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decision {
    /// No memory: prompt the human every time. Stored as ABSENCE — see the
    /// module docs.
    #[default]
    Ask,
    /// The human said yes and asked to be remembered.
    Allow,
    /// The human blocked this origin. Never prompts again; the page's
    /// `getUserMedia()` is rejected outright.
    Deny,
}

impl Decision {
    /// The wire word. ONE table, read by the control endpoint, the settings
    /// pane and the on-disk file, so a decision cannot be spelled two ways.
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Ask => "ask",
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        }
    }

    /// Parse a wire word. ⛔ Anything unrecognised is `Ask`, never `Allow`: a
    /// corrupted file, a future value, or a typo must fall back to asking the
    /// human, not to handing out a camera.
    pub fn from_str(raw: &str) -> Decision {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allow" => Decision::Allow,
            "deny" => Decision::Deny,
            _ => Decision::Ask,
        }
    }
}

/// Which device a decision is about. The two are independent: a site may hold a
/// microphone grant and no camera grant, which is the common case for a call
/// the user joins with video off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Camera,
    Microphone,
}

impl Device {
    pub fn as_str(self) -> &'static str {
        match self {
            Device::Camera => "camera",
            Device::Microphone => "microphone",
        }
    }

    pub fn from_str(raw: &str) -> Option<Device> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "camera" => Some(Device::Camera),
            "microphone" | "mic" => Some(Device::Microphone),
            _ => None,
        }
    }

    /// The human word for a prompt or a settings row.
    pub fn label(self) -> &'static str {
        match self {
            Device::Camera => "Camera",
            Device::Microphone => "Microphone",
        }
    }
}

/// What one origin is remembered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SiteDecisions {
    pub camera: Decision,
    pub microphone: Decision,
}

impl SiteDecisions {
    pub fn get(&self, device: Device) -> Decision {
        match device {
            Device::Camera => self.camera,
            Device::Microphone => self.microphone,
        }
    }

    pub fn set(&mut self, device: Device, decision: Decision) {
        match device {
            Device::Camera => self.camera = decision,
            Device::Microphone => self.microphone = decision,
        }
    }

    /// Nothing remembered ⇒ the row is deleted rather than stored. See the
    /// module docs on why `ask` is absence.
    pub fn is_empty(&self) -> bool {
        self.camera == Decision::Ask && self.microphone == Decision::Ask
    }

    fn to_json(self) -> Value {
        json!({
            "camera": self.camera.as_str(),
            "microphone": self.microphone.as_str(),
        })
    }
}

/// `~/.yggterm` on the host ychrome runs on — the app's host, which over ssh is
/// the remote one, not the GUI's. Same rule as [`crate::webzoom`].
fn yggterm_home() -> Result<PathBuf> {
    Ok(dirs::home_dir().context("no home dir")?.join(".yggterm"))
}

fn media_path() -> Result<PathBuf> {
    Ok(yggterm_home()?.join("web-media-permissions.json"))
}

/// The remembered decisions on disk, origin -> decisions.
///
/// A missing or broken file is an EMPTY map, which means every origin asks. That
/// is the safe direction for this particular failure: losing the file costs the
/// user a prompt, while any other default would either grant or block silently.
pub fn sites() -> BTreeMap<String, SiteDecisions> {
    let Ok(path) = media_path() else {
        return BTreeMap::new();
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<Value>(&raw)
        .map(|value| parse_sites(&value))
        .unwrap_or_default()
}

fn parse_sites(value: &Value) -> BTreeMap<String, SiteDecisions> {
    let mut out = BTreeMap::new();
    let Some(map) = value.get("sites").and_then(Value::as_object) else {
        return out;
    };
    for (origin, entry) in map {
        let Some(origin) = normalize_origin(origin) else {
            continue;
        };
        let read = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .map(Decision::from_str)
                .unwrap_or_default()
        };
        let decisions = SiteDecisions {
            camera: read("camera"),
            microphone: read("microphone"),
        };
        // A row that says "ask, ask" is the same as no row. Dropping it on read
        // keeps one representation of "nothing is remembered here".
        if decisions.is_empty() {
            continue;
        }
        out.insert(origin, decisions);
    }
    out
}

/// The JSON the GUI fetches from `GET /media-permission` with no query: the
/// whole map, for the settings pane's review-and-revoke list.
pub fn to_json() -> Value {
    let sites: serde_json::Map<String, Value> = sites()
        .into_iter()
        .map(|(origin, decisions)| (origin, decisions.to_json()))
        .collect();
    json!({ "sites": Value::Object(sites) })
}

/// What is remembered for one origin. ⛔ EXACT match — see the module docs.
pub fn decisions_for(sites: &BTreeMap<String, SiteDecisions>, origin: &str) -> SiteDecisions {
    normalize_origin(origin)
        .and_then(|origin| sites.get(&origin).copied())
        .unwrap_or_default()
}

/// The answer to ONE `getUserMedia()` ask: given what the page wants, is this
/// origin already allowed, already blocked, or does a human have to decide?
///
/// The rules, in order:
///
/// 1. A page that asked for no device at all gets `Deny` — there is nothing to
///    grant, and it must never read as "ask".
/// 2. Any WANTED device that is blocked blocks the whole request. `getUserMedia`
///    is all-or-nothing (it rejects unless every requested track is available),
///    so a site blocked from the camera cannot be handed a half stream.
/// 3. `Allow` only when EVERY wanted device is already allowed. One remembered
///    grant does not carry the other: a site with a microphone grant that now
///    wants the camera too gets a fresh prompt.
/// 4. Otherwise a human decides.
pub fn verdict(
    sites: &BTreeMap<String, SiteDecisions>,
    origin: &str,
    audio: bool,
    video: bool,
) -> Decision {
    if !audio && !video {
        return Decision::Deny;
    }
    let decisions = decisions_for(sites, origin);
    let mut wanted = Vec::new();
    if audio {
        wanted.push(decisions.microphone);
    }
    if video {
        wanted.push(decisions.camera);
    }
    if wanted.iter().any(|decision| *decision == Decision::Deny) {
        return Decision::Deny;
    }
    if wanted.iter().all(|decision| *decision == Decision::Allow) {
        return Decision::Allow;
    }
    Decision::Ask
}

/// Set (or, with [`Decision::Ask`], forget) one device's decision for one
/// origin. Forgetting the last decision deletes the row — `ask` is absence.
pub fn set(origin: &str, device: Device, decision: Decision) -> Result<()> {
    let Some(origin) = normalize_origin(origin) else {
        anyhow::bail!("not an origin a decision can be keyed to: {origin:?}");
    };
    let mut sites = sites();
    let mut entry = sites.get(&origin).copied().unwrap_or_default();
    entry.set(device, decision);
    if entry.is_empty() {
        sites.remove(&origin);
    } else {
        sites.insert(origin, entry);
    }
    write(&sites)
}

/// Forget everything remembered for one origin — the settings pane's Revoke.
/// Revoking must actually take effect: the next ask prompts again.
pub fn forget(origin: &str) -> Result<()> {
    let Some(origin) = normalize_origin(origin) else {
        anyhow::bail!("not an origin a decision can be keyed to: {origin:?}");
    };
    let mut sites = sites();
    sites.remove(&origin);
    write(&sites)
}

/// A URL (or a bare origin) reduced to the key a decision is stored under:
/// `scheme://host[:port]`, lowercased, with the scheme's default port elided.
///
/// `None` when the input has no origin a decision could safely be keyed to — a
/// `file://` URL, a `data:` URL, a blank page, garbage. Those NEVER get a
/// remembered decision; they can still be allowed once through the prompt, which
/// is the honest behaviour for a document with no origin to remember.
///
/// Only `http` and `https` are accepted. WebKit gates `navigator.mediaDevices`
/// on a secure context anyway, so in practice a real capture ask arrives as
/// `https://`, or as `http://` on loopback (a secure context by definition).
pub fn normalize_origin(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // Authority = everything before the first `/`, `?` or `#`.
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Credentials never key a decision.
    let authority = authority.rsplit('@').next().unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        // An IPv6 literal keeps its brackets and has no port here.
        Some((host, port)) if !host.ends_with(']') && port.chars().all(|c| c.is_ascii_digit()) => {
            (host, Some(port))
        }
        _ => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    let default_port = if scheme == "https" { "443" } else { "80" };
    match port.filter(|port| !port.is_empty() && *port != default_port) {
        Some(port) => Some(format!("{scheme}://{host}:{port}")),
        None => Some(format!("{scheme}://{host}")),
    }
}

fn write(sites: &BTreeMap<String, SiteDecisions>) -> Result<()> {
    let path = media_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let map: serde_json::Map<String, Value> = sites
        .iter()
        .map(|(origin, decisions)| (origin.clone(), decisions.to_json()))
        .collect();
    let body = json!({ "sites": Value::Object(map) });
    std::fs::write(&path, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(rows: &[(&str, Decision, Decision)]) -> BTreeMap<String, SiteDecisions> {
        rows.iter()
            .map(|(origin, camera, microphone)| {
                (
                    origin.to_string(),
                    SiteDecisions {
                        camera: *camera,
                        microphone: *microphone,
                    },
                )
            })
            .collect()
    }

    /// ⛔ THE lock this module exists for: a grant reaches its own origin and
    /// NOTHING else. If this ever passes for a sub-domain, granting a camera to
    /// one page granted it to every page anyone can host under that domain.
    #[test]
    fn a_grant_never_reaches_another_origin() {
        let sites = map(&[("https://example.com", Decision::Allow, Decision::Allow)]);
        assert_eq!(
            verdict(&sites, "https://example.com", true, true),
            Decision::Allow,
        );
        // A sub-domain is a different origin.
        assert_eq!(
            verdict(&sites, "https://user-content.example.com", false, true),
            Decision::Ask,
        );
        // A parent domain is a different origin.
        assert_eq!(verdict(&sites, "https://com", false, true), Decision::Ask);
        // A different scheme is a different origin.
        assert_eq!(
            verdict(&sites, "http://example.com", false, true),
            Decision::Ask,
        );
        // A different port is a different origin.
        assert_eq!(
            verdict(&sites, "https://example.com:8443", false, true),
            Decision::Ask,
        );
        // An unrelated site is an unrelated site.
        assert_eq!(
            verdict(&sites, "https://evil.test", false, true),
            Decision::Ask,
        );
    }

    #[test]
    fn nothing_remembered_means_ask() {
        let sites = map(&[]);
        assert_eq!(
            verdict(&sites, "https://example.com", true, true),
            Decision::Ask,
        );
    }

    /// One grant does not carry the other, and one block sinks the request.
    #[test]
    fn each_device_is_decided_on_its_own() {
        let sites = map(&[("https://example.com", Decision::Ask, Decision::Allow)]);
        // Microphone only: already allowed.
        assert_eq!(
            verdict(&sites, "https://example.com", true, false),
            Decision::Allow,
        );
        // Camera too: the camera has no grant, so a human decides again.
        assert_eq!(
            verdict(&sites, "https://example.com", true, true),
            Decision::Ask,
        );
        let sites = map(&[("https://example.com", Decision::Deny, Decision::Allow)]);
        // A blocked camera blocks the whole all-or-nothing request.
        assert_eq!(
            verdict(&sites, "https://example.com", true, true),
            Decision::Deny,
        );
        // ...but the microphone alone is still allowed.
        assert_eq!(
            verdict(&sites, "https://example.com", true, false),
            Decision::Allow,
        );
    }

    #[test]
    fn an_ask_for_no_device_is_denied_not_prompted() {
        let sites = map(&[("https://example.com", Decision::Allow, Decision::Allow)]);
        assert_eq!(
            verdict(&sites, "https://example.com", false, false),
            Decision::Deny,
        );
    }

    /// ⛔ A decision word this build does not understand must fall back to
    /// asking a human, never to a grant.
    #[test]
    fn an_unknown_decision_word_falls_back_to_ask() {
        assert_eq!(Decision::from_str("allow-forever"), Decision::Ask);
        assert_eq!(Decision::from_str(""), Decision::Ask);
        assert_eq!(Decision::from_str("true"), Decision::Ask);
        assert_eq!(Decision::from_str("ALLOW"), Decision::Allow);
        assert_eq!(Decision::from_str("Deny"), Decision::Deny);
        let parsed = parse_sites(&json!({
            "sites": { "https://a.test": { "camera": "yes-please", "microphone": "allow" } }
        }));
        assert_eq!(
            parsed.get("https://a.test").map(|row| row.camera),
            Some(Decision::Ask),
        );
        assert_eq!(
            parsed.get("https://a.test").map(|row| row.microphone),
            Some(Decision::Allow),
        );
    }

    #[test]
    fn a_broken_file_remembers_nothing_rather_than_granting() {
        assert!(parse_sites(&json!({})).is_empty());
        assert!(parse_sites(&json!({ "sites": "nope" })).is_empty());
        assert!(parse_sites(&json!({ "sites": { "not-an-origin": { "camera": "allow" } } })).is_empty());
        // An all-ask row is the same as no row.
        assert!(
            parse_sites(&json!({ "sites": { "https://a.test": { "camera": "ask" } } })).is_empty()
        );
    }

    #[test]
    fn origins_are_normalized_the_same_way_on_both_sides() {
        assert_eq!(
            normalize_origin("HTTPS://Example.COM/some/path?q=1#frag").as_deref(),
            Some("https://example.com"),
        );
        // The scheme's default port is elided, so `:443` and bare agree.
        assert_eq!(
            normalize_origin("https://example.com:443").as_deref(),
            Some("https://example.com"),
        );
        assert_eq!(
            normalize_origin("http://example.com:80").as_deref(),
            Some("http://example.com"),
        );
        // A non-default port is part of the origin.
        assert_eq!(
            normalize_origin("http://127.0.0.1:8099/page.html").as_deref(),
            Some("http://127.0.0.1:8099"),
        );
        // Credentials never key a decision.
        assert_eq!(
            normalize_origin("https://user:pw@example.com/").as_deref(),
            Some("https://example.com"),
        );
    }

    /// A document with no origin gets no memory — it can still be allowed once
    /// through the prompt, but nothing about it is written down.
    #[test]
    fn an_originless_document_is_never_remembered() {
        for raw in [
            "file:///home/user/test.html",
            "data:text/html,<p>hi",
            "about:blank",
            "",
            "https://",
            "ftp://example.com",
        ] {
            assert_eq!(normalize_origin(raw), None, "{raw} should have no origin key");
        }
        assert!(set("file:///home/user/test.html", Device::Camera, Decision::Allow).is_err());
        assert!(forget("about:blank").is_err());
    }

    #[test]
    fn forgetting_the_last_decision_deletes_the_row() {
        let mut row = SiteDecisions {
            camera: Decision::Allow,
            microphone: Decision::Ask,
        };
        assert!(!row.is_empty());
        row.set(Device::Camera, Decision::Ask);
        assert!(row.is_empty());
    }

    #[test]
    fn device_words_round_trip() {
        assert_eq!(Device::from_str("camera"), Some(Device::Camera));
        assert_eq!(Device::from_str("microphone"), Some(Device::Microphone));
        assert_eq!(Device::from_str("mic"), Some(Device::Microphone));
        assert_eq!(Device::from_str("speaker"), None);
        assert_eq!(Device::Camera.as_str(), "camera");
        assert_eq!(Device::Microphone.as_str(), "microphone");
    }
}
