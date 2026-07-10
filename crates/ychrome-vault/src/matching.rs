//! Matching a page host to vault items.
//!
//! Two deliberately asymmetric rules, carried over from the sidebar that used
//! to live in `yggterm-shell`:
//!
//! * [`item_applies_to_host`] — LOOSE. It only *suggests* rows; a human then
//!   picks one. Base-domain suffixes count (an entry for `gour.top` is offered
//!   on `chat.example.com`).
//! * [`item_auto_matches_host`] — STRICT. The auto paths (password fill, TOTP)
//!   commit a secret to a page with nobody confirming the choice, so a
//!   base-domain entry must never auto-fill a subdomain.
//!
//! Both consider the item NAME and its stored URIs. `rbw list` had no URI
//! field, so the old rules could only read names; the native client syncs the
//! real `login.uris`, which is what Bitwarden itself matches on.

use crate::model::VaultItem;

/// The host of a vault URI, lowercased, without userinfo or port. `None` for
/// a scheme we do not match on (`android://`, `iosapp://`) or an empty host.
/// A bare `example.com` with no scheme is a host.
pub fn uri_host(uri: &str) -> Option<String> {
    let rest = match uri.split_once("://") {
        Some((scheme, rest)) => {
            if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
                return None;
            }
            rest
        }
        None => uri,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    // `user:pass@host` — the host is what follows the LAST '@'.
    let authority = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let host = authority.split(':').next()?.trim().to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn normalize_host(host: &str) -> String {
    host.trim().to_ascii_lowercase()
}

/// Loose: exact host, its `www.`-stripped twin, that twin re-prefixed with
/// `www.`, or a base-domain suffix (`gour.top` labels `chat.example.com`).
///
/// The suffix test strips `www.` from BOTH sides, so a stored URI of
/// `https://www.amazon.com/…` still suggests itself on `smile.amazon.com`. The
/// name-only rule this replaces never saw a `www.` label, so it never had to.
fn label_applies(label: &str, host: &str) -> bool {
    let label = label.trim().to_ascii_lowercase();
    if label.is_empty() {
        return false;
    }
    let bare_host = host.strip_prefix("www.").unwrap_or(host);
    let bare_label = label.strip_prefix("www.").unwrap_or(&label);
    label == host
        || label == bare_host
        || label == format!("www.{bare_host}")
        || bare_host.ends_with(&format!(".{bare_label}"))
}

/// Strict: exact host or its `www.` twin, in either direction. No base-domain
/// suffix — an auto path fills without anyone confirming.
fn label_auto_matches(label: &str, host: &str) -> bool {
    let label = label.trim().to_ascii_lowercase();
    if label.is_empty() {
        return false;
    }
    let bare_host = host.strip_prefix("www.").unwrap_or(host);
    label == host || label == bare_host || label == format!("www.{bare_host}")
}

/// Should the sidebar float this item to the top for `host`? Loose rule.
pub fn item_applies_to_host(item: &VaultItem, host: &str) -> bool {
    let host = normalize_host(host);
    label_applies(&item.name, &host)
        || item
            .uris
            .iter()
            .filter_map(|uri| uri_host(uri))
            .any(|uri_host| label_applies(&uri_host, &host))
}

/// May an auto path (fill / TOTP, nobody confirming) use this item for `host`?
/// Strict rule.
pub fn item_auto_matches_host(item: &VaultItem, host: &str) -> bool {
    let host = normalize_host(host);
    label_auto_matches(&item.name, &host)
        || item
            .uris
            .iter()
            .filter_map(|uri| uri_host(uri))
            .any(|uri_host| label_auto_matches(&uri_host, &host))
}

/// The one item an auto path may use for `host`, or `None` when the host
/// matches nothing. Ties (several accounts on one site) are broken by sorting
/// on `(name, username)` and taking the first — deterministic, and the same
/// rule the old `vault_auto_match_for_host` used.
pub fn auto_match_for_host<'a>(items: &'a [VaultItem], host: &str) -> Option<&'a VaultItem> {
    let host = normalize_host(host);
    if host.is_empty() {
        return None;
    }
    let mut candidates: Vec<&VaultItem> = items
        .iter()
        .filter(|item| item.has_password && item_auto_matches_host(item, &host))
        .collect();
    candidates.sort_by(|a, b| {
        (a.name.as_str(), a.username.as_deref().unwrap_or(""))
            .cmp(&(b.name.as_str(), b.username.as_deref().unwrap_or("")))
    });
    candidates.into_iter().next()
}

/// Resolve a user-named entry to exactly one item. Exact (case-insensitive)
/// name match first; if nothing matches exactly, a unique case-insensitive
/// substring match. `user` disambiguates same-name entries. Returns the
/// candidate list when the choice is ambiguous so the caller can report it.
pub fn find_by_name<'a>(
    items: &'a [VaultItem],
    name: &str,
    user: Option<&str>,
) -> Result<&'a VaultItem, Vec<&'a VaultItem>> {
    let wanted = name.trim().to_ascii_lowercase();
    let mut matches: Vec<&VaultItem> = items
        .iter()
        .filter(|item| item.name.to_ascii_lowercase() == wanted)
        .collect();
    if matches.is_empty() {
        matches = items
            .iter()
            .filter(|item| item.name.to_ascii_lowercase().contains(&wanted))
            .collect();
    }
    if let Some(user) = user {
        let user = user.trim().to_ascii_lowercase();
        matches.retain(|item| {
            item.username
                .as_deref()
                .map(|candidate| candidate.to_ascii_lowercase() == user)
                .unwrap_or(false)
        });
    }
    matches.sort_by(|a, b| {
        (a.name.as_str(), a.username.as_deref().unwrap_or(""))
            .cmp(&(b.name.as_str(), b.username.as_deref().unwrap_or("")))
    });
    match matches.len() {
        1 => Ok(matches[0]),
        _ => Err(matches),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, uris: &[&str]) -> VaultItem {
        VaultItem {
            id: name.to_string(),
            name: name.to_string(),
            username: Some("u".to_string()),
            folder: None,
            uris: uris.iter().map(|u| u.to_string()).collect(),
            has_password: true,
            has_totp: false,
            has_passkey: false,
        }
    }

    #[test]
    fn uri_host_extracts_only_web_hosts() {
        assert_eq!(uri_host("https://Example.com/login?a=1").as_deref(), Some("example.com"));
        assert_eq!(uri_host("http://user:pw@example.com:8443/x").as_deref(), Some("example.com"));
        assert_eq!(uri_host("example.com").as_deref(), Some("example.com"));
        assert_eq!(uri_host("android://com.example"), None);
        assert_eq!(uri_host("https://"), None);
    }

    // The two matchers are deliberately asymmetric. The sidebar SUGGESTS rows
    // with the loose rule (a human then clicks one); the auto paths (fill,
    // TOTP) commit a secret to the page with nobody confirming, so they take
    // the strict rule. A base-domain entry must never auto-fill a subdomain.
    #[test]
    fn auto_match_is_stricter_than_the_sidebar_suggestion_rule() {
        for (name, host) in [
            ("example.com", "example.com"),
            ("example.com", "www.example.com"),
            ("www.example.com", "example.com"),
            ("EXAMPLE.com", "example.com"),
        ] {
            let entry = item(name, &[]);
            assert!(item_auto_matches_host(&entry, host), "auto: {name} / {host}");
            assert!(item_applies_to_host(&entry, host), "applies: {name} / {host}");
        }
        let base = item("gour.top", &[]);
        assert!(item_applies_to_host(&base, "chat.example.com"));
        assert!(!item_auto_matches_host(&base, "chat.example.com"));

        for name in ["", "  ", "other.com", "notexample.com"] {
            let entry = item(name, &[]);
            assert!(!item_auto_matches_host(&entry, "example.com"));
            assert!(!item_applies_to_host(&entry, "example.com"));
        }
    }

    // The URI list carries the same two rules — this is what `rbw list` could
    // never do, and it is how an entry named "Amazon" reaches amazon.com.
    #[test]
    fn uris_match_under_both_rules() {
        let amazon = item("Amazon", &["https://www.amazon.com/ap/signin"]);
        assert!(item_applies_to_host(&amazon, "amazon.com"));
        assert!(item_auto_matches_host(&amazon, "amazon.com"));
        assert!(!item_auto_matches_host(&amazon, "smile.amazon.com"));
        assert!(item_applies_to_host(&amazon, "smile.amazon.com"));
        assert!(!item_applies_to_host(&amazon, "amazon.co.uk"));
    }

    #[test]
    fn auto_match_breaks_ties_deterministically_and_needs_a_password() {
        let mut zed = item("example.com", &[]);
        zed.username = Some("zed".to_string());
        let mut abe = item("example.com", &[]);
        abe.username = Some("abe".to_string());
        let items = vec![zed, abe];
        assert_eq!(
            auto_match_for_host(&items, "example.com")
                .and_then(|i| i.username.as_deref()),
            Some("abe")
        );

        let mut passwordless = item("example.com", &[]);
        passwordless.has_password = false;
        assert!(auto_match_for_host(std::slice::from_ref(&passwordless), "example.com").is_none());
        assert!(auto_match_for_host(&items, "").is_none());
    }

    #[test]
    fn find_by_name_prefers_exact_then_unique_substring() {
        let items = vec![item("GitHub", &[]), item("GitHub Enterprise", &[])];
        assert_eq!(find_by_name(&items, "github", None).unwrap().name, "GitHub");
        assert_eq!(
            find_by_name(&items, "enterprise", None).unwrap().name,
            "GitHub Enterprise"
        );
        // "git" is a substring of both — ambiguous, and the candidates come back.
        assert_eq!(find_by_name(&items, "git", None).unwrap_err().len(), 2);
        assert!(find_by_name(&items, "nothing", None).unwrap_err().is_empty());
    }

    #[test]
    fn find_by_name_disambiguates_on_user() {
        let mut one = item("example.com", &[]);
        one.username = Some("alice".to_string());
        let mut two = item("example.com", &[]);
        two.username = Some("bob".to_string());
        let items = vec![one, two];
        assert!(find_by_name(&items, "example.com", None).is_err());
        assert_eq!(
            find_by_name(&items, "example.com", Some("BOB"))
                .unwrap()
                .username
                .as_deref(),
            Some("bob")
        );
    }
}
