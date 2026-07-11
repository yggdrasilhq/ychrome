//! The browser identity ychrome presents to the web: the User-Agent string.
//!
//! WebKitGTK's default UA describes a browser that does not exist — Safari on
//! X11/Linux — and UA-allowlisting edges reject it outright. claude.ai answers
//! it with a bare `403 {"error":{"type":"forbidden","message":"Request not
//! allowed"}}` (verified against the live edge: the SAME request from a
//! macOS-Safari UA is served, and so is Chrome-on-Linux; only the nonexistent
//! Safari-on-Linux pair is refused). Any site behind that class of bot rule does
//! the same, so a daily browser cannot ship the engine default.
//!
//! Ownership: the UA is browsing config, so ychrome's host owns the choice; only
//! the GUI can apply it (WebKit fixes the UA at webview creation), so it rides
//! `/policy` and yggterm applies it. The same shape as the adblock ruleset — the
//! app decides, the GUI injects, yggterm persists nothing.
//!
//! The default is **Safari on macOS**, which is the smallest honest-ish lie: the
//! engine really is WebKit, so a site that UA-sniffs serves WebKit-compatible
//! code and anti-bot fingerprinting finds an engine that matches the claim. A
//! Chrome UA over a WebKit engine is the *inconsistent* one — it invites
//! Blink-only code paths and challenge loops. Chrome remains available as a
//! preset for the sites that only ever test Chrome.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Safari 18.5 / macOS. Safari has frozen its platform token at `10_15_7` for
/// years, so this is what a real Safari sends.
pub const SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";

/// Chrome / Linux, for sites that gate on Chrome specifically.
pub const CHROME_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36";

/// The presets, in the order the settings pane lists them. `Engine` is the
/// escape hatch: WebKitGTK's own UA, which is honest and gets you 403'd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Safari,
    Chrome,
    Engine,
}

impl Preset {
    pub fn id(self) -> &'static str {
        match self {
            Preset::Safari => "safari",
            Preset::Chrome => "chrome",
            Preset::Engine => "engine",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::Safari => "Safari (macOS)",
            Preset::Chrome => "Chrome (Linux)",
            Preset::Engine => "WebKitGTK default",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Preset::Safari => "Matches the engine. Recommended.",
            Preset::Chrome => "For sites that only test Chrome.",
            Preset::Engine => "Honest, and refused by claude.ai and friends.",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Preset::ALL.iter().copied().find(|preset| preset.id() == id)
    }

    /// The UA this preset sends, or `None` to leave WebKitGTK's default alone.
    fn user_agent(self) -> Option<&'static str> {
        match self {
            Preset::Safari => Some(SAFARI_UA),
            Preset::Chrome => Some(CHROME_UA),
            Preset::Engine => None,
        }
    }

    pub const ALL: [Preset; 3] = [Preset::Safari, Preset::Chrome, Preset::Engine];
}

/// `~/.yggterm/ychrome/user-agent.json` on the host ychrome runs on.
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

/// The chosen preset. An unset or unreadable config means Safari: a browser
/// nobody has configured must still be able to open claude.ai.
pub fn preset() -> Preset {
    config()["preset"]
        .as_str()
        .and_then(Preset::from_id)
        .unwrap_or(Preset::Safari)
}

/// What the GUI should hand `WebViewBuilder::with_user_agent`. `None` = the
/// engine default.
pub fn effective() -> Option<String> {
    preset().user_agent().map(str::to_string)
}

pub fn set_preset(id: &str) -> Result<()> {
    let preset = Preset::from_id(id).with_context(|| format!("unknown user-agent preset: {id}"))?;
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Preserve unknown keys: a newer ychrome's setting is not destroyed by an
    // older one that never heard of it (the same rule as the adblock config).
    let mut object = config().as_object().cloned().unwrap_or_default();
    object.insert("preset".to_string(), Value::String(preset.id().to_string()));
    std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(object))?)?;
    Ok(())
}

/// The UA's contribution to `policy_version`. The stamp must move when the
/// DECISION moves, not when the file's bytes do — same rule as the adblock
/// decision, and the reason it is the id and not an mtime.
pub fn stamp() -> String {
    format!("user-agent:{}\n", preset().id())
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

    /// The whole point of the module: the default identity is NOT the engine's,
    /// because the engine's is the one the web refuses.
    #[test]
    fn the_default_preset_sends_a_real_browsers_ua() {
        assert_eq!(Preset::Safari.user_agent(), Some(SAFARI_UA));
        assert!(SAFARI_UA.contains("Macintosh"));
        assert!(!SAFARI_UA.contains("X11"));
        assert_eq!(Preset::Engine.user_agent(), None);
    }
}
