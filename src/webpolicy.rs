//! ychrome's WEB-CONTENT POLICY: ad blocking and userscripts, owned by the host
//! ychrome runs on.
//!
//! yggterm used to read `~/.yggterm/web-adblock/*` and `~/.yggterm/web-userscripts/*`
//! **on the GUI host** and hardcode a `RightPanelMode::AppSidebar` to edit them.
//! That was app chrome in the platform, and worse, it was incoherent: an ychrome
//! running over ssh was editing files on the remote host that nothing ever read.
//!
//! Now the app's host owns the config, and the control endpoint ships the
//! *effective* policy to the GUI, which applies it to the webview and persists
//! nothing but the compiled-filter cache WebKit demands. Same shape as vault
//! fill: the app computes, the GUI injects.
//!
//! ```text
//! yggterm --GET <control>/policy--> ychrome   {adblock_rules, userscripts}
//! ```
//!
//! The enabled/disabled decision lives HERE, not in yggterm. `adblock_rules` is
//! `None` when the master switch is off, the profile opted out, or no ruleset is
//! installed — three reasons, one answer, and the GUI never re-derives it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// `~/.yggterm` on the host ychrome runs on. Deliberately NOT the GUI's home:
/// over ssh those are different machines, and the app's host owns its config.
fn yggterm_home() -> Result<PathBuf> {
    Ok(dirs::home_dir().context("no home dir")?.join(".yggterm"))
}

fn adblock_dir() -> Result<PathBuf> {
    Ok(yggterm_home()?.join("web-adblock"))
}

fn shared_userscript_dir() -> Result<PathBuf> {
    Ok(yggterm_home()?.join("web-userscripts"))
}

fn profile_userscript_dir(profile: &str) -> Result<PathBuf> {
    Ok(yggterm_home()?
        .join("web-profiles")
        .join(profile)
        .join("userscripts"))
}

/// The reserved ephemeral profile. It has no jar, so it has no per-profile
/// userscript directory either — only the shared ones apply.
const TEMP_PROFILE: &str = "temp";

/// The effective policy for one profile: exactly what the GUI should apply, with
/// every enable/disable decision already made.
pub struct Policy {
    /// WebKit content-blocker JSON, or `None` for "no ad blocking on this
    /// surface". yggterm does not know why.
    pub adblock_rules: Option<String>,
    /// Injected at document-start, shared scripts first then per-profile, each
    /// directory sorted by filename. Deterministic: the same host state always
    /// produces the same order.
    pub userscripts: Vec<String>,
}

impl Policy {
    pub fn to_json(&self) -> Value {
        json!({
            "adblock_rules": self.adblock_rules,
            "userscripts": self.userscripts,
        })
    }
}

/// `{ "enabled": bool, "disabled_profiles": ["name", ...] }`. A missing or
/// broken file means enabled with no opt-outs — ad blocking is a daily-browser
/// table stake, so it fails ON.
fn adblock_config() -> Value {
    adblock_dir()
        .ok()
        .and_then(|dir| std::fs::read_to_string(dir.join("config.json")).ok())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

fn adblock_enabled(config: &Value) -> bool {
    config["enabled"].as_bool().unwrap_or(true)
}

fn adblock_profile_disabled(config: &Value, profile: &str) -> bool {
    config["disabled_profiles"]
        .as_array()
        .is_some_and(|list| list.iter().any(|entry| entry.as_str() == Some(profile)))
}

fn rules_path() -> Result<PathBuf> {
    Ok(adblock_dir()?.join("rules.json"))
}

/// Every userscript directory that applies to `profile`, in injection order.
fn userscript_dirs(profile: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(shared) = shared_userscript_dir() {
        dirs.push(shared);
    }
    if profile != TEMP_PROFILE
        && let Ok(per_profile) = profile_userscript_dir(profile)
    {
        dirs.push(per_profile);
    }
    dirs
}

/// The `*.js` files in `dir`, sorted by filename. A script is disabled by
/// renaming it away from `.js` (to `.js.disabled`), so the loader's rule is
/// simply "ends in .js".
fn enabled_scripts(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("js") && path.is_file()
        })
        .collect();
    paths.sort();
    paths
}

/// The effective policy for `profile`, read fresh off this host's disk.
pub fn policy(profile: &str) -> Policy {
    let config = adblock_config();
    let adblock_rules = rules_path()
        .ok()
        .filter(|path| path.is_file())
        .filter(|_| adblock_enabled(&config) && !adblock_profile_disabled(&config, profile))
        .and_then(|path| std::fs::read_to_string(path).ok());

    let userscripts = userscript_dirs(profile)
        .iter()
        .flat_map(|dir| enabled_scripts(dir))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect();

    Policy {
        adblock_rules,
        userscripts,
    }
}

/// An opaque stamp over everything `policy()` would read: which files exist,
/// how long they are, and when they last changed — plus the adblock decision,
/// which lives in `config.json` and would otherwise be invisible to a stat.
///
/// STAT-ONLY on the bulk content. A `rules.json` is ~10 KB and the sidebar
/// re-declares every ~4s; reading it into a hash on every heartbeat would burn
/// the remote host's disk for nothing. yggterm refetches `/policy` only when
/// this string changes.
///
/// The hash is FNV-1a: a change detector, not a security primitive. Nothing
/// trusts it to be collision-resistant against an adversary — the adversary
/// here is your own text editor.
pub fn policy_version(profile: &str) -> String {
    let mut manifest = String::new();
    let config = adblock_config();
    // The decision, not just the bytes: flipping `enabled` off changes no
    // userscript file and may not even change config.json's length.
    manifest.push_str(&format!(
        "adblock:{}:{}\n",
        adblock_enabled(&config),
        adblock_profile_disabled(&config, profile)
    ));
    if let Ok(rules) = rules_path() {
        stamp(&mut manifest, &rules);
    }
    for dir in userscript_dirs(profile) {
        for script in enabled_scripts(&dir) {
            stamp(&mut manifest, &script);
        }
    }
    format!("{:016x}", fnv1a(manifest.as_bytes()))
}

/// Append one file's identity to the manifest: path, length, mtime. A missing
/// file contributes its absence, so deleting a userscript changes the stamp.
fn stamp(manifest: &mut String, path: &Path) {
    let meta = std::fs::metadata(path).ok();
    let len = meta.as_ref().map(|meta| meta.len()).unwrap_or(0);
    let mtime = meta
        .as_ref()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    manifest.push_str(&format!(
        "{}:{}:{}:{}\n",
        path.display(),
        meta.is_some(),
        len,
        mtime
    ));
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// What the settings pane renders, and what its toggles do.
// ---------------------------------------------------------------------------

/// The pane's view of this host's policy files.
pub struct PolicyState {
    pub adblock_rules_present: bool,
    pub adblock_rule_count: usize,
    pub adblock_enabled: bool,
    pub adblock_profile_disabled: bool,
    /// `(file stem, enabled)` for the SHARED userscripts: `name.js` = on,
    /// `name.js.disabled` = off. Per-profile scripts are applied but not
    /// toggled here — one owner per control.
    pub userscripts: Vec<(String, bool)>,
}

pub fn state(profile: &str) -> PolicyState {
    let config = adblock_config();
    let rules = rules_path().ok().filter(|path| path.is_file());
    let adblock_rule_count = rules
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_array().map(Vec::len))
        .unwrap_or(0);

    let mut userscripts: Vec<(String, bool)> = Vec::new();
    if let Ok(dir) = shared_userscript_dir()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".js") {
                userscripts.push((stem.to_string(), true));
            } else if let Some(stem) = name.strip_suffix(".js.disabled") {
                userscripts.push((stem.to_string(), false));
            }
        }
    }
    userscripts.sort();

    PolicyState {
        adblock_rules_present: rules.is_some(),
        adblock_rule_count,
        adblock_enabled: adblock_enabled(&config),
        adblock_profile_disabled: adblock_profile_disabled(&config, profile),
        userscripts,
    }
}

/// Rewrite `config.json` with `mutate` applied to the current (or default)
/// object. Unknown keys survive: a future ychrome's setting is not destroyed by
/// an older one that never heard of it.
fn mutate_adblock_config(mutate: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<()> {
    let dir = adblock_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.json");
    let mut config = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    mutate(&mut config);
    std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(config))?)?;
    Ok(())
}

pub fn set_adblock_enabled(enabled: bool) -> Result<()> {
    mutate_adblock_config(|config| {
        config.insert("enabled".to_string(), Value::Bool(enabled));
    })
}

pub fn set_adblock_profile_disabled(profile: &str, disabled: bool) -> Result<()> {
    mutate_adblock_config(|config| {
        let mut list: Vec<Value> = config
            .get("disabled_profiles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        list.retain(|entry| entry.as_str() != Some(profile));
        if disabled {
            list.push(Value::String(profile.to_string()));
        }
        config.insert("disabled_profiles".to_string(), Value::Array(list));
    })
}

/// Enable/disable a shared userscript by renaming `<stem>.js` ⇄
/// `<stem>.js.disabled`. The stem is a single path component by construction
/// (it came from `state()`), but a hostile action payload could carry `../`, so
/// reject anything that is not a bare name.
pub fn set_userscript_enabled(stem: &str, enabled: bool) -> Result<()> {
    if stem.is_empty() || stem.contains('/') || stem.contains("..") {
        anyhow::bail!("userscript name must be a plain name, not a path: {stem:?}");
    }
    let dir = shared_userscript_dir()?;
    let (from, to) = if enabled {
        (format!("{stem}.js.disabled"), format!("{stem}.js"))
    } else {
        (format!("{stem}.js"), format!("{stem}.js.disabled"))
    };
    std::fs::rename(dir.join(from), dir.join(to))
        .with_context(|| format!("toggling userscript {stem}"))?;
    Ok(())
}

/// Reject anything that is not a bare filename stem. Shared by delete and
/// install: an action payload is attacker-influenced, so `../` never reaches a
/// path join.
fn checked_stem(stem: &str) -> Result<()> {
    if stem.is_empty() || stem.contains('/') || stem.contains("..") {
        anyhow::bail!("userscript name must be a plain name, not a path: {stem:?}");
    }
    Ok(())
}

/// Remove a shared userscript outright — both the enabled `<stem>.js` and the
/// disabled `<stem>.js.disabled`, whichever exists. Deleting a file that is not
/// there is not an error (the pane may be a beat stale). A per-profile script of
/// the same name is left alone: this pane only manages the shared ones, one
/// owner per control.
pub fn delete_userscript(stem: &str) -> Result<()> {
    checked_stem(stem)?;
    let dir = shared_userscript_dir()?;
    let mut removed = false;
    for name in [format!("{stem}.js"), format!("{stem}.js.disabled")] {
        match std::fs::remove_file(dir.join(&name)) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("deleting {name}")),
        }
    }
    if !removed {
        anyhow::bail!("no userscript named {stem:?} to delete");
    }
    Ok(())
}

/// Whether a shared userscript with this stem is installed (enabled or not).
/// Used to filter the "add an extension" catalog down to what is NOT yet here.
pub fn userscript_installed(stem: &str) -> bool {
    let Ok(dir) = shared_userscript_dir() else {
        return false;
    };
    dir.join(format!("{stem}.js")).exists() || dir.join(format!("{stem}.js.disabled")).exists()
}

/// The current enabled state of a shared userscript: `Some(true)` for
/// `<stem>.js`, `Some(false)` for `<stem>.js.disabled`, `None` if absent. A
/// list-row Enable/Disable button carries no checkbox value, so the action reads
/// this to flip.
pub fn userscript_enabled(stem: &str) -> Option<bool> {
    let dir = shared_userscript_dir().ok()?;
    if dir.join(format!("{stem}.js")).exists() {
        Some(true)
    } else if dir.join(format!("{stem}.js.disabled")).exists() {
        Some(false)
    } else {
        None
    }
}

/// Install a bundled userscript body as `<stem>.js` in the shared directory,
/// enabled. Refuses to clobber an existing script of the same name (enabled or
/// disabled) — an install is additive, never a silent overwrite of what the user
/// may have edited.
pub fn install_userscript(stem: &str, body: &str) -> Result<()> {
    checked_stem(stem)?;
    if userscript_installed(stem) {
        anyhow::bail!("{stem} is already installed");
    }
    let dir = shared_userscript_dir()?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{stem}.js")), body)
        .with_context(|| format!("installing userscript {stem}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adblock_defaults_on_when_the_config_is_missing_or_broken() {
        assert!(adblock_enabled(&json!({})));
        assert!(adblock_enabled(&json!({ "enabled": "yes-ish" })));
        assert!(!adblock_enabled(&json!({ "enabled": false })));
    }

    #[test]
    fn a_profile_opt_out_is_read_off_the_config() {
        let config = json!({ "disabled_profiles": ["work", "temp"] });
        assert!(adblock_profile_disabled(&config, "work"));
        assert!(!adblock_profile_disabled(&config, "personal"));
    }

    // The temp profile has no jar, so it has no per-profile script directory —
    // only the shared scripts apply to an incognito surface.
    #[test]
    fn the_temp_profile_has_no_per_profile_userscripts() {
        assert_eq!(userscript_dirs(TEMP_PROFILE).len(), 1);
        assert_eq!(userscript_dirs("personal").len(), 2);
    }

    // The stamp must move when the adblock DECISION moves, even though no
    // userscript file changed and config.json may keep its length.
    #[test]
    fn the_stamp_covers_the_adblock_decision_not_just_file_bytes() {
        let mut on = String::new();
        let mut off = String::new();
        on.push_str(&format!(
            "adblock:{}:{}\n",
            adblock_enabled(&json!({ "enabled": true })),
            false
        ));
        off.push_str(&format!(
            "adblock:{}:{}\n",
            adblock_enabled(&json!({ "enabled": false })),
            false
        ));
        assert_ne!(fnv1a(on.as_bytes()), fnv1a(off.as_bytes()));
    }

    #[test]
    fn a_userscript_name_cannot_escape_its_directory() {
        assert!(set_userscript_enabled("../../etc/passwd", true).is_err());
        assert!(set_userscript_enabled("", true).is_err());
    }

    #[test]
    fn the_policy_json_names_the_fields_yggterm_deserializes() {
        let policy = Policy {
            adblock_rules: Some("[]".to_string()),
            userscripts: vec!["x".to_string()],
        };
        let value = policy.to_json();
        assert_eq!(value["adblock_rules"], "[]");
        assert_eq!(value["userscripts"][0], "x");
    }
}
