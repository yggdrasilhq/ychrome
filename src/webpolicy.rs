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
//! yggterm --GET <control>/policy--> ychrome
//!     {adblock_rules, userscripts, userscripts_v2, user_agent, user_agent_sites}
//! ```
//!
//! `userscripts` and `userscripts_v2` are the SAME scripts in two shapes, both
//! derived from one list (see [`Policy::to_json`]). The flat array of bodies is
//! what a GUI older than the scriptlet plane reads and must never change; the
//! rich array adds the `@match`/`@world`/`@all-frames` placement a newer GUI
//! applies. A GUI that understands the rich array uses it OUTRIGHT — the two are
//! never merged, or every script would inject twice.
//!
//! The enabled/disabled decision lives HERE, not in yggterm. `adblock_rules` is
//! `None` when the master switch is off, the profile opted out, or no ruleset is
//! installed — three reasons, one answer, and the GUI never re-derives it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::userscript::Userscript;

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
    ///
    /// THE one owner of "what scripts this surface runs and where". The legacy
    /// `userscripts` strings on the wire are DERIVED from this list in
    /// [`Policy::to_json`], never maintained beside it.
    pub userscripts: Vec<Userscript>,
    /// The UA the surface identifies as at creation; `None` leaves WebKitGTK's
    /// own, which is now the DEFAULT because it is the coherent one (see
    /// [`crate::useragent`]). Owned by [`crate::useragent`] and carried here
    /// because /policy is the channel the GUI reads before it builds a webview.
    pub user_agent: Option<String>,
    /// Per-site identity overrides, host -> the UA STRING to send, `null` for
    /// "the engine's own". Applied by the GUI on navigation, the same division
    /// of labour as `/zoom`: ychrome owns the map, yggterm matches the live
    /// page's host against it.
    ///
    /// Resolved to strings HERE so the GUI never learns what a preset is, and
    /// carried as its own field so a GUI older than per-site identity keeps
    /// working off [`Policy::user_agent`] alone.
    pub user_agent_sites: Value,
}

impl Policy {
    /// Put a script ychrome itself supplies at the FRONT of the list — the
    /// passkey shim, which has to patch `navigator.credentials` before any page
    /// script can reach for it.
    ///
    /// It goes through the same list as everything else on purpose. When the
    /// shim was spliced into the JSON after the fact, "what scripts ship" had
    /// two owners, and the second one only knew about the legacy string array:
    /// the moment a richer array existed beside it, the shim would have been
    /// dropped from it silently.
    pub fn prepend(&mut self, script: Userscript) {
        self.userscripts.insert(0, script);
    }

    /// Both wire shapes, from one list.
    ///
    /// `userscripts` stays EXACTLY what it always was — a flat array of script
    /// bodies — because a GUI older than this change deserializes the policy
    /// with that field name and ignores every field it does not know. Drop it
    /// or rename it and those GUIs silently stop running userscripts at all.
    /// `userscripts_v2` carries the placement facts; a GUI that understands it
    /// prefers it outright and never merges the two.
    pub fn to_json(&self) -> Value {
        let legacy: Vec<&str> = self
            .userscripts
            .iter()
            .map(|script| script.body.as_str())
            .collect();
        let rich: Vec<Value> = self
            .userscripts
            .iter()
            .map(|script| script.to_json())
            .collect();
        json!({
            "adblock_rules": self.adblock_rules,
            "userscripts": legacy,
            "userscripts_v2": rich,
            "user_agent": self.user_agent,
            "user_agent_sites": self.user_agent_sites,
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

    // Each body carries its own placement in a metadata block; see
    // `crate::userscript`. Parsed HERE, once, on the host that owns the files —
    // the GUI never re-derives a `@match` and so can never disagree about one.
    // Every script then passes the promotion gate: an untranslatable @include
    // refuses the whole script, loudly, instead of shipping it half-scoped.
    let mut userscripts: Vec<Userscript> = userscript_dirs(profile)
        .iter()
        .flat_map(|dir| enabled_scripts(dir))
        .filter_map(|path| {
            let body = std::fs::read_to_string(&path).ok()?;
            promote_or_refuse(&path, crate::userscript::parse(&body))
        })
        .collect();

    // SponsorBlock's per-category settings, as a tiny synthetic script injected
    // BEFORE the one that reads them. It is not a file: splicing settings into
    // `sponsorblock.js` would make this host's copy diverge from the bundled
    // asset, which is exactly the state `crate::provision` reads as "the user
    // edited this, leave it alone" — and the next ychrome release would then
    // never update the script. `crate::sponsorblock` owns the catalogue; this
    // is only the delivery.
    //
    // Only when the script is actually enabled: a config for a script that is
    // not running is a global on every YouTube page for nothing.
    if userscript_enabled(crate::extensions::SPONSORBLOCK_STEM).unwrap_or(false) {
        userscripts.insert(0, crate::sponsorblock::config_userscript());
    }

    Policy {
        adblock_rules,
        userscripts,
        user_agent: crate::useragent::effective(),
        user_agent_sites: crate::useragent::sites_json(),
    }
}

/// The per-script ALL-OR-NOTHING promotion gate (integrator-settled): a script
/// whose every `@include`/`@exclude` translated promotes whole; a script with
/// ANY untranslatable one is refused whole, loudly, naming each offending
/// line verbatim on stderr. Promoting the translatable subset is forbidden —
/// the script would run on pages other than the ones it declares: fewer for a
/// dropped `@include`, and for a dropped `@exclude` the very pages its author
/// ruled out — the same silent-wrong class as feeding a glob to WebKit
/// verbatim.
///
/// Refused means refused from BOTH wire shapes. The legacy array would run
/// the raw body on every page in the page's world, which is even further from
/// what the script declared than the half-scoped version.
fn promote_or_refuse(
    source: &Path,
    script: crate::userscript::Userscript,
) -> Option<crate::userscript::Userscript> {
    // The SECOND reason, and the one the YouTube 2x report came down to: a
    // body carrying one of ychrome's own stems but declaring no metadata block
    // would be injected with the DEFAULT placement instead of the one it needs,
    // which for `youtube-adblock` means the isolated world, where its network
    // prune is invisible and only the fast-forward fallback runs. Refusing is
    // strictly better than injecting it wrong; `crate::provision` owns the
    // decision and the wording.
    if let Some(refusal) = crate::provision::placement_refusal(source, &script) {
        eprintln!(
            "ychrome: REFUSING userscript {}: {refusal}",
            source.display()
        );
        return None;
    }
    if script.untranslatable_includes.is_empty() {
        return Some(script);
    }
    eprint!(
        "{}",
        crate::userscript::refusal_report(
            &source.display().to_string(),
            &script.untranslatable_includes
        )
    );
    None
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
    // Same reason: the UA is a decision, not a file the GUI could stat.
    manifest.push_str(&crate::useragent::stamp());
    // And SponsorBlock's per-category settings, which travel as an injected
    // preamble rather than a file — a stat over the directory cannot see them,
    // so a category the user just switched to auto-skip would not reach the
    // page until something else in the policy happened to change.
    manifest.push_str(&crate::sponsorblock::stamp());
    manifest.push_str(&passkey_shim_stamp());
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

/// The vault facts that change WHICH sites the passkey shim is installed on,
/// as cheaply as a stat.
///
/// ⛔ THE STAMP WAS BLIND TO THE SHIM, AND THAT MADE THE FAILURE PERMANENT.
/// `/policy` prepends a shim whose scope comes from the vault agent, while this
/// stamp covered only adblock, the UA, SponsorBlock and userscript FILES. Two
/// fetches under one stamp could therefore return different policies, and the
/// GUI refetches only when the stamp MOVES — so a surface kept whatever shim
/// decision happened to be true when it opened, forever.
///
/// Measured on jojo, 2026-08-01, one unchanged `policy_version`
/// (`ebc219f7d40ddc53`): `sidebar_contribution/policy` recorded
/// `userscripts: 6` at 14:53 and `userscripts: 5` from 16:07 on, across the
/// ychrome deploy that landed per-origin scoping. The missing script was the
/// shim, nothing refetched, and the user met it as "your browser does not
/// support WebAuthn" on a 2FA page.
///
/// ⚠ STAT-ONLY, DELIBERATELY. This runs on the ~4 s re-declare, where a unix
/// socket round trip was already measured to wreck the surface tests, so the
/// probe itself may never come here. These two files answer the case that
/// actually bit:
///
/// * `agent.pid` — rewritten by `serve_on` every time an agent starts OR is
///   handed over (an `execve` keeps the pid, and the write still moves the
///   mtime), so "the agent's code changed" is a stat.
/// * the installed `ychrome-vault` — so installing a new one moves the stamp
///   even before the handover.
///
/// ⚠ WHAT THIS STILL DOES NOT SEE: a plain lock → unlock, which touches no
/// file. A surface opened over a locked vault therefore still needs to be
/// reopened; `sidebar::passkey_shim_widgets` says so in words rather than
/// leaving the user to guess. See `docs/pending-bugs.md`.
fn passkey_shim_stamp() -> String {
    let mut manifest = String::new();
    if let Ok(dir) = ychrome_vault_proto::default_dir() {
        stamp(&mut manifest, &ychrome_vault_proto::pid_path(&dir));
    }
    manifest.push_str(&format!(
        "vault_exe:{}\n",
        ychrome_vault_proto::installed_vault_exe_stamp()
    ));
    manifest
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
    /// The SHARED userscripts, one entry per on-disk file. Per-profile scripts
    /// are applied but not toggled here — one owner per control.
    pub userscripts: Vec<UserscriptStatus>,
}

/// One shared userscript as the settings pane sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserscriptStatus {
    /// The file stem: `<stem>.js` / `<stem>.js.disabled` on this host's disk.
    pub stem: String,
    /// `<stem>.js` = on, `<stem>.js.disabled` = off. The FILENAME's answer;
    /// a refused script still reads `true` here because the file is enabled —
    /// which is exactly why `refusal` must travel beside it.
    pub enabled: bool,
    /// `Some(one-line summary)` when the promotion gate refuses this script
    /// whole: it is injected NOWHERE, whatever `enabled` says, and the pane
    /// must not present it as running. Read off the same parse the gate reads
    /// ([`crate::userscript::Userscript::untranslatable_includes`] is the one
    /// owner of the decision). `None` for a script that promotes — and for a
    /// disabled one, where nothing is claiming to run in the first place.
    pub refusal: Option<String>,
    /// `Some(note)` when this host's copy of a BUNDLED script diverges from the
    /// one ychrome ships and the reconciler deliberately left it alone: the
    /// user edited it, or it is newer than the bundle. Read off
    /// [`crate::provision`], the one owner of that judgement, so the pane
    /// cannot disagree with what the launch actually did. Strictly less serious
    /// than `refusal` — a noted script is running, a refused one is not.
    pub note: Option<String>,
}

/// The pane's half of the promotion gate's verdict for one on-disk script:
/// `Some(summary)` when [`promote_or_refuse`] would refuse it whole. The same
/// decision datum — the parse's `untranslatable_includes` — read without the
/// gate's stderr report, because the pane redraws on every click and the
/// report is the policy BUILD's loudness, not the pane's.
fn userscript_refusal(path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    let script = crate::userscript::parse(&body);
    if script.untranslatable_includes.is_empty() {
        None
    } else {
        Some(crate::userscript::refusal_summary(
            &script.untranslatable_includes,
        ))
    }
}

/// The pane's half of the reconciler's verdict for one on-disk script: the
/// note `crate::provision` would show for it, or `None`. Read through the same
/// owner the launch-time reconcile used — the pane never re-derives "is this
/// current".
fn userscript_note(stem: &str, path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    crate::provision::userscript_note(stem, &body)
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

    let mut userscripts: Vec<UserscriptStatus> = Vec::new();
    if let Ok(dir) = shared_userscript_dir()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".js") {
                userscripts.push(UserscriptStatus {
                    stem: stem.to_string(),
                    enabled: true,
                    // An enabled file is CLAIMING to run, so its claim is
                    // checked against the gate's verdict here — the pane must
                    // never present a refused script as merely "Enabled".
                    refusal: userscript_refusal(&entry.path()),
                    note: userscript_note(stem, &entry.path()),
                });
            } else if let Some(stem) = name.strip_suffix(".js.disabled") {
                userscripts.push(UserscriptStatus {
                    stem: stem.to_string(),
                    enabled: false,
                    refusal: None,
                    note: userscript_note(stem, &entry.path()),
                });
            }
        }
    }
    // Same order the old `(stem, enabled)` tuples sorted into: by stem, then
    // disabled before enabled — deterministic across renders.
    userscripts.sort_by(|a, b| (a.stem.as_str(), a.enabled).cmp(&(b.stem.as_str(), b.enabled)));

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

    fn demo_policy() -> Policy {
        Policy {
            adblock_rules: Some("[]".to_string()),
            userscripts: vec![crate::userscript::parse(
                "// ==UserScript==\n// @match https://*.youtube.com/*\n// @world main\n// ==/UserScript==\nx\n",
            )],
            user_agent: Some("UA/1".to_string()),
            user_agent_sites: json!({ "portal.gov.in": "UA/2" }),
        }
    }

    #[test]
    fn the_policy_json_names_the_fields_yggterm_deserializes() {
        let value = demo_policy().to_json();
        assert_eq!(value["adblock_rules"], "[]");
        assert_eq!(value["user_agent"], "UA/1");
        // The per-site map rides beside the global, never instead of it: a GUI
        // that has never heard of per-site identity still reads `user_agent`.
        assert_eq!(value["user_agent_sites"]["portal.gov.in"], "UA/2");
    }

    // MIXED-VERSION FLEET. A GUI older than the scriptlet plane deserializes
    // `userscripts` as a flat array of bodies and has never heard of
    // `userscripts_v2`. Both halves have to hold: the legacy array must still be
    // there, and it must still be STRINGS — turning its elements into objects
    // would fail that GUI's deserialize and take the whole policy down with it,
    // including the adblock ruleset.
    #[test]
    fn the_legacy_userscripts_array_is_still_a_flat_list_of_bodies() {
        let value = demo_policy().to_json();
        let legacy = value["userscripts"]
            .as_array()
            .expect("`userscripts` must stay an array for pre-scriptlet GUIs");
        assert_eq!(legacy.len(), 1);
        let body = legacy[0]
            .as_str()
            .expect("`userscripts` elements must stay STRINGS, not objects");
        assert!(body.contains("@match"));
    }

    // The rich array carries the placement facts a new GUI applies, derived from
    // the same list — so a script can never appear in one array and not the
    // other.
    #[test]
    fn the_v2_array_carries_the_placement_facts_for_the_same_scripts() {
        let value = demo_policy().to_json();
        let rich = value["userscripts_v2"]
            .as_array()
            .expect("`userscripts_v2` must be present");
        assert_eq!(rich.len(), value["userscripts"].as_array().unwrap().len());
        assert_eq!(rich[0]["world"], "main");
        assert_eq!(rich[0]["matches"][0], "https://*.youtube.com/*");
        assert_eq!(rich[0]["all_frames"], false);
        assert_eq!(rich[0]["body"], value["userscripts"][0]);
    }

    // ALL-OR-NOTHING, the gate half: a script with any untranslatable
    // @include must not promote — not into the rich array, not into the
    // legacy one — while a clean script passes through untouched.
    #[test]
    fn the_promotion_gate_refuses_a_script_with_any_untranslatable_include() {
        let poisoned = crate::userscript::parse(
            "// ==UserScript==\n\
             // @match https://*.youtube.com/*\n\
             // @include /^https?://.*music.*/\n\
             // ==/UserScript==\n\
             x();\n",
        );
        assert!(
            promote_or_refuse(Path::new("poisoned.js"), poisoned).is_none(),
            "one untranslatable @include must refuse the WHOLE script"
        );

        let clean = crate::userscript::parse(
            "// ==UserScript==\n\
             // @include https://m.example.test/*\n\
             // ==/UserScript==\n\
             x();\n",
        );
        let promoted =
            promote_or_refuse(Path::new("clean.js"), clean).expect("a clean script promotes");
        assert_eq!(
            promoted.matches,
            vec!["https://m.example.test/*".to_string()]
        );
    }

    // Source-anchored on the ONE disk→Policy pipeline: every script read off
    // the filesystem must pass through `promote_or_refuse` before it can
    // reach either wire shape. Without this anchor, a refactor could collect
    // `parse` results directly and a refused script would ship half-scoped —
    // exactly the class the gate exists to stop. Bounded to the body of
    // `policy()` so this test module's own mention of the gate cannot
    // satisfy it.
    #[test]
    fn the_policy_loader_routes_every_script_through_the_promotion_gate() {
        let source = include_str!("webpolicy.rs");
        let suffix = source
            .split("pub fn policy(")
            .nth(1)
            .expect("policy() is present");
        // Bound at the next TOP-LEVEL fn of either visibility. The gate
        // itself is a private `fn` right after policy(); a bound that only
        // knew `pub fn` would sweep the gate's own definition into the slice
        // and find its name there even after the call site is gone — which is
        // exactly the could-only-pass shape this lock exists to avoid.
        let end = [suffix.find("\nfn "), suffix.find("\npub fn ")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(suffix.len());
        let body = &suffix[..end];
        assert!(
            body.contains("promote_or_refuse("),
            "policy() no longer routes scripts through the promotion gate"
        );
        assert!(
            !body.contains(".map(|body| crate::userscript::parse"),
            "policy() collects raw parse results, bypassing the promotion gate"
        );
    }

    /// Set on the CHILD half of the end-to-end pipeline lock below: seeing it
    /// means "$HOME is a scratch dir the parent owns — run the real pipeline
    /// and print what it produced". The child-process split is how a test can
    /// own $HOME: the environment is process-global and the suite runs many
    /// tests in parallel.
    const PIPELINE_PROBE_VAR: &str = "YCHROME_POLICY_PIPELINE_PROBE";

    /// How the child's answer is picked out of libtest's own output.
    const PIPELINE_PROBE_PREFIX: &str = "ychrome-policy-pipeline-probe: ";

    // THE BEHAVIOURAL BACKSTOP for the promotion gate — the reason a
    // source-anchor bypass cannot survive a release build. This drives the
    // REAL disk→`policy()`→`to_json()` pipeline (and `state()`, the pane's
    // view of the same files) over a scratch $HOME and asserts on what
    // actually ships. A decoy call that satisfies the source anchor above —
    // `let _ = promote_or_refuse(..)` beside a raw `parse` — ships the
    // poisoned script anyway, and fails HERE in every build profile, because
    // nothing below leans on `debug_assert`.
    #[test]
    fn the_shipped_policy_never_carries_a_refused_script() {
        if std::env::var(PIPELINE_PROBE_VAR).is_ok() {
            // CHILD: $HOME is the parent's scratch dir.
            let policy = policy("default");
            let pane: Vec<Value> = state("default")
                .userscripts
                .iter()
                .map(|script| {
                    json!({
                        "stem": script.stem,
                        "enabled": script.enabled,
                        "refusal": script.refusal,
                    })
                })
                .collect();
            let facts = json!({ "wire": policy.to_json(), "pane": pane });
            println!("{PIPELINE_PROBE_PREFIX}{facts}");
            return;
        }

        // PARENT: a scratch home with one promotable script and one the gate
        // refuses — the review's exact shape, an @exclude whose wildcard-host
        // glob cannot be proven equivalent to a match pattern.
        let home =
            std::env::temp_dir().join(format!("ychrome-policy-pipeline-{}", std::process::id()));
        let scripts = home.join(".yggterm").join("web-userscripts");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&scripts).expect("scratch userscript dir");
        std::fs::write(
            scripts.join("clean.js"),
            "// ==UserScript==\n\
             // @match https://*.youtube.com/*\n\
             // @exclude-match https://*.youtube.com/embed/*\n\
             // @exclude https://m.youtube.com/embed*\n\
             // ==/UserScript==\n\
             clean_body();\n",
        )
        .expect("clean.js");
        std::fs::write(
            scripts.join("poisoned.js"),
            "// ==UserScript==\n\
             // @match https://*.youtube.com/*\n\
             // @exclude https://*.youtube.com/embed/*\n\
             // ==/UserScript==\n\
             poisoned_body();\n",
        )
        .expect("poisoned.js");
        // THE SECOND REFUSAL REASON, as it actually reached a user: a body
        // carrying one of ychrome's OWN stems with its metadata block gone.
        // Injected, it would land in the isolated world where
        // youtube-adblock's network prune is invisible and only its
        // fast-forward fallback runs — every ad still on screen, at 2x. It
        // must not reach either wire shape.
        std::fs::write(
            scripts.join("youtube-adblock.js"),
            "(function () { headerless_bundled_body(); })();\n",
        )
        .expect("youtube-adblock.js");

        let exe = std::env::current_exe().expect("test binary path");
        let output = std::process::Command::new(exe)
            .args([
                "--exact",
                "webpolicy::tests::the_shipped_policy_never_carries_a_refused_script",
                "--nocapture",
            ])
            .env("HOME", &home)
            .env(PIPELINE_PROBE_VAR, "1")
            .output()
            .expect("spawning the pipeline probe");
        let _ = std::fs::remove_dir_all(&home);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "the pipeline probe child failed:\n{stdout}\n{stderr}"
        );
        let line = stdout
            .lines()
            .find_map(|line| line.strip_prefix(PIPELINE_PROBE_PREFIX))
            .unwrap_or_else(|| panic!("no probe line in:\n{stdout}"));
        let facts: Value = serde_json::from_str(line).expect("probe facts parse");

        // THE WIRE: the clean script ships in BOTH shapes, with its block-list
        // intact; the refused script ships in NEITHER.
        let wire = &facts["wire"];
        let legacy: Vec<&str> = wire["userscripts"]
            .as_array()
            .expect("legacy array")
            .iter()
            .map(|body| body.as_str().expect("legacy bodies are strings"))
            .collect();
        assert!(legacy.iter().any(|body| body.contains("clean_body")));
        assert!(
            !legacy.iter().any(|body| body.contains("poisoned_body")),
            "a refused script shipped on the legacy wire, unscoped in the page world"
        );
        let rich = wire["userscripts_v2"].as_array().expect("v2 array");
        let clean = rich
            .iter()
            .find(|entry| {
                entry["body"]
                    .as_str()
                    .is_some_and(|body| body.contains("clean_body"))
            })
            .expect("the clean script promotes");
        assert_eq!(clean["matches"][0], "https://*.youtube.com/*");
        assert_eq!(clean["exclude_matches"][0], "https://*.youtube.com/embed/*");
        assert_eq!(clean["exclude_matches"][1], "https://m.youtube.com/embed*");
        assert!(
            !rich.iter().any(|entry| {
                entry["body"]
                    .as_str()
                    .is_some_and(|body| body.contains("poisoned_body"))
            }),
            "a refused script shipped half-scoped on the v2 wire"
        );

        // The header-less copy of a BUNDLED script is in neither shape either,
        // and the pane says why.
        assert!(
            !legacy
                .iter()
                .any(|body| body.contains("headerless_bundled_body")),
            "a bundled script with no metadata block shipped anyway — it would run in the \
             isolated world, which is the YouTube-ads-at-2x bug"
        );
        assert!(
            !rich.iter().any(|entry| {
                entry["body"]
                    .as_str()
                    .is_some_and(|body| body.contains("headerless_bundled_body"))
            }),
            "a bundled script with no metadata block shipped on the v2 wire"
        );
        assert!(
            stderr.contains("declares no metadata block"),
            "the header-less bundled script was dropped without saying why:\n{stderr}"
        );

        // THE LOUD HALF: the child's stderr carries the refusal, naming the
        // offending @exclude line verbatim.
        assert!(
            stderr.contains("REFUSING userscript"),
            "no refusal report on stderr:\n{stderr}"
        );
        assert!(stderr.contains("@exclude https://*.youtube.com/embed/*"));

        // THE PANE: the refused script must not read as merely "enabled".
        let pane = facts["pane"].as_array().expect("pane rows");
        let poisoned = pane
            .iter()
            .find(|row| row["stem"] == "poisoned")
            .expect("the pane lists the refused script");
        let refusal = poisoned["refusal"]
            .as_str()
            .expect("the pane must carry the refusal, or the user's only clue is stderr");
        assert!(refusal.contains("@exclude https://*.youtube.com/embed/*"));
        let clean_row = pane
            .iter()
            .find(|row| row["stem"] == "clean")
            .expect("the pane lists the clean script");
        assert!(
            clean_row["refusal"].is_null(),
            "a promotable script must carry no refusal"
        );
    }

    const SPONSORBLOCK_PROBE_VAR: &str = "YCHROME_SPONSORBLOCK_POLICY_PROBE";
    const SPONSORBLOCK_PROBE_PREFIX: &str = "ychrome-sponsorblock-policy-probe: ";

    /// SponsorBlock's per-category settings are the ONE thing in the policy that
    /// is not a file on disk — they ride as a synthetic preamble. That makes two
    /// failure modes invisible to every other test here: the preamble not
    /// shipping at all (the pane would offer settings that reach nothing), and
    /// it shipping for a script that is switched off (a global on every YouTube
    /// page for nothing). This drives the real disk→`policy()`→`to_json()`
    /// pipeline over a scratch $HOME and asserts on what actually goes out.
    #[test]
    fn the_sponsorblock_settings_ride_the_wire_only_when_the_script_is_on() {
        if std::env::var(SPONSORBLOCK_PROBE_VAR).is_ok() {
            let facts = json!({
                "wire": policy("default").to_json(),
                "version": policy_version("default"),
            });
            println!("{SPONSORBLOCK_PROBE_PREFIX}{facts}");
            return;
        }

        // ⚠ ONE scratch home per on/off state, and the SCRIPT IS WRITTEN ONCE.
        // A first draft rebuilt the whole directory per probe, and the version
        // assertion below passed against a mutant that had no stamp at all —
        // because rewriting `sponsorblock.js` moved its mtime, which
        // `policy_version` stats. Only the config JSON changes between probes,
        // and a `.json` is invisible to `enabled_scripts`, so the version can
        // only move if the decision itself is stamped.
        let prepare = |enabled: bool| -> std::path::PathBuf {
            let home = std::env::temp_dir().join(format!(
                "ychrome-sponsorblock-policy-{}-{enabled}",
                std::process::id()
            ));
            let scripts = home.join(".yggterm").join("web-userscripts");
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&scripts).expect("scratch userscript dir");
            let name = if enabled {
                "sponsorblock.js"
            } else {
                "sponsorblock.js.disabled"
            };
            std::fs::write(
                scripts.join(name),
                crate::extensions::find(crate::extensions::SPONSORBLOCK_STEM)
                    .expect("sponsorblock in catalog")
                    .body,
            )
            .expect("the bundled body");
            home
        };

        let run = |home: &std::path::Path, intro: &str| -> Value {
            let scripts = home.join(".yggterm").join("web-userscripts");
            std::fs::write(
                scripts.join("sponsorblock.config.json"),
                json!({ "categories": { "intro": intro }, "kept": "by a future ychrome" })
                    .to_string(),
            )
            .expect("the config");
            let exe = std::env::current_exe().expect("test binary path");
            let output = std::process::Command::new(exe)
                .args([
                    "--exact",
                    "webpolicy::tests::the_sponsorblock_settings_ride_the_wire_only_when_the_script_is_on",
                    "--nocapture",
                ])
                .env("HOME", home)
                .env(SPONSORBLOCK_PROBE_VAR, "1")
                .output()
                .expect("spawning the sponsorblock policy probe");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "the sponsorblock policy probe child failed:\n{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let line = stdout
                .lines()
                .find_map(|line| line.strip_prefix(SPONSORBLOCK_PROBE_PREFIX))
                .unwrap_or_else(|| panic!("no probe line in:\n{stdout}"));
            serde_json::from_str(line).expect("probe facts parse")
        };

        let live = prepare(true);
        let on = run(&live, "auto");
        let scripts = on["wire"]["userscripts_v2"]
            .as_array()
            .expect("v2 array")
            .clone();
        let preamble = scripts
            .iter()
            .find(|entry| {
                entry["body"]
                    .as_str()
                    .is_some_and(|body| body.contains("window.__ysbConfig"))
            })
            .expect("the settings preamble must ship when SponsorBlock is on");
        // It must be FIRST: the script reads the global lazily so order is not
        // load-bearing, but a preamble that arrives after its reader is a trap
        // for the next person who makes it eager.
        assert!(
            scripts[0]["body"]
                .as_str()
                .is_some_and(|body| body.contains("window.__ysbConfig")),
            "the preamble did not lead the list"
        );
        assert_eq!(preamble["world"], "isolated");
        assert!(
            preamble["matches"].as_array().is_some_and(|list| list
                .iter()
                .any(|m| m.as_str().is_some_and(|m| m.contains("youtube.com")))),
            "the preamble must be YouTube-scoped: {:?}",
            preamble["matches"]
        );
        // The user's choice, not the default: `intro` defaults to a button.
        assert_eq!(
            crate::sponsorblock::find("intro").expect("intro").default,
            "manual"
        );
        let body = preamble["body"].as_str().expect("preamble body");
        assert!(
            body.contains("\"intro\":{\"behaviour\":\"auto\""),
            "the stored choice did not reach the page: {body}"
        );

        // Off: no preamble anywhere on either wire shape.
        let dark = prepare(false);
        let off = run(&dark, "auto");
        assert!(
            !off["wire"].to_string().contains("__ysbConfig"),
            "the preamble shipped for a script that is switched off"
        );

        // And the policy version MOVES with the choice, or the GUI would keep
        // serving the page the settings it fetched before the click.
        // ⚠ SAME home, same untouched `sponsorblock.js` — only the stored
        // choice differs.
        let other = run(&live, "off");
        assert_ne!(
            on["version"], other["version"],
            "policy_version is blind to a category change, so a settings click \
             would never reach a running surface"
        );
        let _ = std::fs::remove_dir_all(&live);
        let _ = std::fs::remove_dir_all(&dark);
    }

    // The passkey shim patches `navigator.credentials` for the PAGE to call, so
    // it must land in the page's own world and it must land FIRST. Isolated
    // would make the patch invisible to every page that needs it; late would let
    // a page grab the real API before the shim replaced it.
    #[test]
    fn a_prepended_shim_lands_first_and_in_the_main_world() {
        let mut policy = demo_policy();
        policy.prepend(crate::userscript::Userscript::new("SHIM").in_main_world());
        let value = policy.to_json();
        assert_eq!(value["userscripts"][0], "SHIM");
        assert_eq!(value["userscripts_v2"][0]["body"], "SHIM");
        assert_eq!(value["userscripts_v2"][0]["world"], "main");
        assert!(
            value["userscripts_v2"][0]["matches"]
                .as_array()
                .is_some_and(|list| list.is_empty()),
            "the shim must apply to every URL"
        );
    }
}
