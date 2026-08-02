//! THE per-site host rule: given a map keyed by host and a page's host, which
//! entry applies.
//!
//! Two settings are now per-site — the zoom override ([`crate::webzoom`]) and
//! the browser-identity override ([`crate::useragent`]) — and "which entry
//! covers `music.youtube.com`" must mean exactly one thing for both. It was
//! written once for zoom; a second copy for identity is how the two would drift
//! until a user's override applied to one setting and not the other on the same
//! page. AGENTS.md's rule is to EXTRACT when the second consumer arrives, which
//! is what this module is.
//!
//! The rule, unchanged from the zoom implementation this was lifted from:
//!
//! - hosts are lowercased and port-stripped before anything looks at them;
//! - the walk is longest-suffix — an exact `music.youtube.com` entry beats a
//!   `youtube.com` entry, which beats none;
//! - `www` is NOT special-cased away: it is its own host, and the walk already
//!   lets a `youtube.com` entry reach it;
//! - a bare TLD is never consulted. An entry keyed `com` would otherwise
//!   swallow the whole web.
//!
//! yggterm does the same walk on the live page for zoom. Keep them in step: the
//! matcher here is the CLI/test twin of the GUI's, and the tests below are the
//! contract both sides implement.

use std::collections::BTreeMap;

/// Lowercase, port-stripped host. `normalize("WWW.YouTube.com:443")` ->
/// `"www.youtube.com"`.
pub fn normalize(host: &str) -> String {
    host.split(':')
        .next()
        .unwrap_or(host)
        .trim()
        .to_ascii_lowercase()
}

/// The entry that applies to `host`, most specific first. `None` when no entry
/// covers it.
pub fn lookup<'a, T>(sites: &'a BTreeMap<String, T>, host: &str) -> Option<&'a T> {
    let host = normalize(host);
    if host.is_empty() {
        return None;
    }
    let mut candidate = host.as_str();
    loop {
        if let Some(value) = sites.get(candidate) {
            return Some(value);
        }
        match candidate.split_once('.') {
            // Strip the leftmost label and try the parent domain, but only while
            // at least two labels remain — never fall through to a bare TLD.
            Some((_, rest)) if rest.contains('.') => candidate = rest,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(hosts: &[&str]) -> BTreeMap<String, &'static str> {
        hosts
            .iter()
            .map(|host| (host.to_string(), "x"))
            .collect::<BTreeMap<_, _>>()
    }

    #[test]
    fn the_exact_host_wins_over_its_parent() {
        let mut sites = BTreeMap::new();
        sites.insert("youtube.com".to_string(), 1);
        sites.insert("music.youtube.com".to_string(), 2);
        assert_eq!(lookup(&sites, "music.youtube.com"), Some(&2));
        assert_eq!(lookup(&sites, "www.youtube.com"), Some(&1));
        assert_eq!(lookup(&sites, "youtube.com"), Some(&1));
    }

    #[test]
    fn a_bare_tld_entry_never_matches_across_the_web() {
        assert_eq!(lookup(&map(&["com"]), "example.com"), None);
        assert_eq!(lookup(&map(&["example.com"]), "example.com"), Some(&"x"));
    }

    #[test]
    fn the_host_is_normalized_before_matching() {
        assert_eq!(
            lookup(&map(&["youtube.com"]), "WWW.YouTube.com:443"),
            Some(&"x")
        );
        assert_eq!(normalize(" EXAMPLE.com:8443 "), "example.com");
    }

    #[test]
    fn nothing_matches_an_empty_host() {
        assert_eq!(lookup(&map(&["example.com"]), ""), None);
        assert_eq!(lookup(&map(&["example.com"]), "  "), None);
    }
}
