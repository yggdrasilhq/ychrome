//! ychrome's PER-SITE ZOOM, owned by the host ychrome runs on.
//!
//! yggterm has ONE global "Web View" zoom (`web_surface_zoom_percent`). A daily
//! browser needs more than one number: some sites read better at 130%, some at
//! 80%, and the choice has to persist per site. That is browsing config, so it
//! lives HERE, on the app's host, exactly like ad blocking and userscripts, and
//! never in yggterm.
//!
//! ```text
//! yggterm --GET <control>/zoom--> ychrome   { "sites": { "youtube.com": 130 } }
//! ```
//!
//! yggterm applies the override for the current page's host on every navigation
//! (longest-suffix match, so an entry for `youtube.com` covers `music.youtube.com`
//! too — the sub-domain reach the task calls for, which extends Chrome's
//! host-exact model), and falls back to its own global "Ychrome Global Zoom" when
//! a site has no entry. The GUI does the matching on the live page; ychrome owns
//! only the map and the writes.
//!
//! The file is host-global (all profiles share it): zoom is about readability,
//! not identity, so a site that reads well at 130% reads well at 130% whichever
//! profile is looking at it. If a profile ever needs its own zoom, this is the
//! one place that changes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// The band the GUI's web-surface zoom setting accepts. A stored override is
/// clamped to it, so ychrome can never persist a factor yggterm would reject.
pub const MIN_ZOOM: f64 = 50.0;
pub const MAX_ZOOM: f64 = 250.0;
/// One tap of the pane's +/- control.
pub const ZOOM_STEP: f64 = 10.0;

/// `~/.yggterm` on the host ychrome runs on — the app's host, which over ssh is
/// the remote one, not the GUI's. Same rule as [`crate::webpolicy`].
fn yggterm_home() -> Result<PathBuf> {
    Ok(dirs::home_dir().context("no home dir")?.join(".yggterm"))
}

fn zoom_path() -> Result<PathBuf> {
    Ok(yggterm_home()?.join("web-zoom.json"))
}

/// The per-site overrides on disk, host -> percent. A missing or broken file is
/// simply an empty map: no site has an override, so everything uses the global.
pub fn sites() -> BTreeMap<String, f64> {
    let Ok(path) = zoom_path() else {
        return BTreeMap::new();
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<Value>(&raw)
        .map(|value| parse_sites(&value))
        .unwrap_or_default()
}

fn parse_sites(value: &Value) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    if let Some(map) = value.get("sites").and_then(Value::as_object) {
        for (host, percent) in map {
            let host = normalize_host(host);
            if host.is_empty() {
                continue;
            }
            if let Some(percent) = percent.as_f64() {
                out.insert(host, clamp(percent));
            }
        }
    }
    out
}

/// The JSON the GUI fetches from `/zoom`. Only per-site overrides — the global
/// is yggterm's, and the GUI already holds it.
pub fn to_json() -> Value {
    json!({ "sites": sites() })
}

/// An opaque change-detector over the site map, carried on the ~4s re-declare so
/// the GUI refetches `/zoom` only when an override actually moved. FNV-1a, like
/// the policy stamp: a change detector, not a security primitive.
pub fn zoom_version() -> String {
    let mut manifest = String::new();
    // `sites()` returns a BTreeMap, so the manifest order is deterministic.
    for (host, percent) in sites() {
        manifest.push_str(&format!("{host}:{percent}\n"));
    }
    format!("{:016x}", fnv1a(manifest.as_bytes()))
}

/// Set (or, with `None`, clear) the override for one host. Clearing removes the
/// key so the site falls back to the global — never persists "same as global".
pub fn set(host: &str, percent: Option<f64>) -> Result<()> {
    let host = normalize_host(host);
    if host.is_empty() {
        anyhow::bail!("cannot set zoom for an empty host");
    }
    let mut sites = sites();
    match percent {
        Some(percent) => {
            sites.insert(host, clamp(percent));
        }
        None => {
            sites.remove(&host);
        }
    }
    write(&sites)
}

/// The effective override for a host, most specific first: an exact
/// `music.youtube.com` entry beats a `youtube.com` entry, which beats none. This
/// mirrors the match yggterm does on the live page, so the CLI and the GUI agree.
/// A bare TLD (`com`) is never consulted — an override there would swallow the
/// whole web — so the walk stops at the last two labels.
pub fn zoom_for_host(sites: &BTreeMap<String, f64>, host: &str) -> Option<f64> {
    let host = normalize_host(host);
    if host.is_empty() {
        return None;
    }
    let mut candidate = host.as_str();
    loop {
        if let Some(percent) = sites.get(candidate) {
            return Some(*percent);
        }
        match candidate.split_once('.') {
            // Strip the leftmost label and try the parent domain, but only while
            // at least two labels remain — never fall through to a bare TLD.
            Some((_, rest)) if rest.contains('.') => candidate = rest,
            _ => return None,
        }
    }
}

/// Lowercase, port-stripped host. `normalize_host("WWW.YouTube.com:443")` ->
/// `"www.youtube.com"`. `www` is deliberately kept: it is its own host, and the
/// longest-suffix walk already lets a `youtube.com` override reach it.
fn normalize_host(host: &str) -> String {
    host.split(':')
        .next()
        .unwrap_or(host)
        .trim()
        .to_ascii_lowercase()
}

fn clamp(percent: f64) -> f64 {
    percent.round().clamp(MIN_ZOOM, MAX_ZOOM)
}

fn write(sites: &BTreeMap<String, f64>) -> Result<()> {
    let path = zoom_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let body = json!({ "sites": sites });
    std::fs::write(&path, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs
            .iter()
            .map(|(host, percent)| (host.to_string(), *percent))
            .collect()
    }

    #[test]
    fn exact_host_wins() {
        let sites = map(&[("youtube.com", 130.0), ("music.youtube.com", 90.0)]);
        assert_eq!(zoom_for_host(&sites, "music.youtube.com"), Some(90.0));
    }

    #[test]
    fn parent_domain_covers_subdomains() {
        let sites = map(&[("youtube.com", 130.0)]);
        assert_eq!(zoom_for_host(&sites, "www.youtube.com"), Some(130.0));
        assert_eq!(zoom_for_host(&sites, "m.youtube.com"), Some(130.0));
        assert_eq!(zoom_for_host(&sites, "youtube.com"), Some(130.0));
    }

    #[test]
    fn no_entry_falls_through_to_none() {
        let sites = map(&[("youtube.com", 130.0)]);
        assert_eq!(zoom_for_host(&sites, "example.com"), None);
    }

    #[test]
    fn a_bare_tld_override_never_matches_across_the_web() {
        // An override keyed on "com" must NOT apply to unrelated .com sites.
        let sites = map(&[("com", 130.0)]);
        assert_eq!(zoom_for_host(&sites, "example.com"), None);
        // ...but keyed exactly on the site, it does.
        let sites = map(&[("example.com", 130.0)]);
        assert_eq!(zoom_for_host(&sites, "example.com"), Some(130.0));
    }

    #[test]
    fn host_is_normalized_before_matching() {
        let sites = map(&[("youtube.com", 130.0)]);
        assert_eq!(zoom_for_host(&sites, "WWW.YouTube.com:443"), Some(130.0));
    }

    #[test]
    fn parse_clamps_and_drops_garbage() {
        let value = json!({ "sites": { "a.com": 9999, "b.com": 10, "c.com": "nope" } });
        let sites = parse_sites(&value);
        assert_eq!(sites.get("a.com"), Some(&MAX_ZOOM));
        assert_eq!(sites.get("b.com"), Some(&MIN_ZOOM));
        assert_eq!(sites.get("c.com"), None);
    }

    #[test]
    fn version_moves_only_when_the_map_moves() {
        let a = fnv1a(b"youtube.com:130\n");
        let b = fnv1a(b"youtube.com:130\n");
        let c = fnv1a(b"youtube.com:120\n");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
