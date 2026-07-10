//! Watchtower: which logins share a password, and which passwords are weak.
//!
//! This lived in `yggterm`'s sidebar, where it could only be computed by asking
//! the agent for all ~1100 passwords over a unix socket, 25 at a time, one
//! process spawn per item. It belongs here: the agent already holds every
//! cipher decrypted, so the whole scan is a pass over memory.
//!
//! **No plaintext password is ever placed in a collection.** Reuse is grouped by
//! a SHA-256 digest of the password, so the map that finds duplicates holds
//! digests, not secrets. Only entry LABELS leave this module — a report says
//! "these four logins share a password", never which password.

use std::collections::HashMap;

use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The shortest password that is not weak on length alone.
const MIN_STRONG_LENGTH: usize = 10;
/// Below this many character classes a password is weak however long it is.
const MIN_STRONG_CLASSES: usize = 2;

/// Labels only — never a password, never a digest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// How many logins carried a password we could read.
    pub scanned: usize,
    /// Groups of labels sharing one password, largest group first.
    pub reused: Vec<Vec<String>>,
    /// Labels whose password is short or single-class.
    pub weak: Vec<String>,
}

/// Weak = shorter than 10 characters, or drawn from fewer than two character
/// classes. Lifted verbatim from `yggterm`'s `vault_password_is_weak`, which is
/// deleted now that the pane is ychrome's: one owner for the rule.
pub fn is_weak(password: &str) -> bool {
    if password.chars().count() < MIN_STRONG_LENGTH {
        return true;
    }
    let classes = [
        password.chars().any(|c| c.is_ascii_lowercase()),
        password.chars().any(|c| c.is_ascii_uppercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_ascii_alphanumeric()),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    classes < MIN_STRONG_CLASSES
}

/// Analyze `(label, password)` pairs. The passwords are consumed: each is
/// classified, digested, and dropped (zeroized) before the next one is read.
pub fn analyze(entries: impl IntoIterator<Item = (String, Zeroizing<String>)>) -> Report {
    let mut by_digest: HashMap<[u8; 32], Vec<String>> = HashMap::new();
    let mut weak: Vec<String> = Vec::new();
    let mut scanned = 0usize;

    for (label, password) in entries {
        scanned += 1;
        if is_weak(&password) {
            weak.push(label.clone());
        }
        let digest: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        by_digest.entry(digest).or_default().push(label);
    }

    let mut reused: Vec<Vec<String>> = by_digest
        .into_values()
        .filter(|labels| labels.len() >= 2)
        .map(|mut labels| {
            labels.sort();
            labels
        })
        .collect();
    // Biggest blast radius first; ties broken by label so the report is
    // deterministic (a HashMap's iteration order is not).
    reused.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    weak.sort();

    Report {
        scanned,
        reused,
        weak,
    }
}

/// How an item is named in a report: `name (user)`, or just `name`.
pub fn label(name: &str, username: Option<&str>) -> String {
    match username.filter(|user| !user.is_empty()) {
        Some(user) => format!("{name} ({user})"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, Zeroizing<String>)> {
        pairs
            .iter()
            .map(|(label, password)| {
                ((*label).to_string(), Zeroizing::new((*password).to_string()))
            })
            .collect()
    }

    #[test]
    fn weak_is_short_or_single_class() {
        assert!(is_weak("short"), "under 10 chars");
        assert!(is_weak("aaaaaaaaaaaaaaa"), "one class, however long");
        assert!(is_weak("abcdefghij"), "lowercase only");
        assert!(!is_weak("abcdefghij1"), "two classes at 11 chars");
        assert!(!is_weak("Tr0ub4dor&3x"), "four classes");
        // Exactly at the boundary: 10 chars, two classes.
        assert!(!is_weak("abcdefghi1"));
        assert!(is_weak("abcdefghi"), "9 chars, two classes");
    }

    #[test]
    fn reuse_groups_by_password_and_never_reports_one() {
        let report = analyze(entries(&[
            ("a", "hunter2hunter2A1"),
            ("b", "hunter2hunter2A1"),
            ("c", "unique-Passw0rd!"),
            ("d", "hunter2hunter2A1"),
            ("e", "shared-Passw0rd!"),
            ("f", "shared-Passw0rd!"),
        ]));
        assert_eq!(report.scanned, 6);
        // Largest group first, labels sorted inside it.
        assert_eq!(report.reused, vec![vec!["a", "b", "d"], vec!["e", "f"]]);
        assert!(report.weak.is_empty());

        // The report is the only thing that leaves — it must not carry secrets.
        let wire = serde_json::to_string(&report).expect("report serializes");
        assert!(!wire.contains("hunter2"), "password leaked into a report");
        assert!(!wire.contains("shared-Passw0rd"), "password leaked into a report");
    }

    #[test]
    fn a_password_used_once_is_not_reuse() {
        let report = analyze(entries(&[("a", "unique-Passw0rd!"), ("b", "another-Passw0rd!")]));
        assert!(report.reused.is_empty());
        assert_eq!(report.scanned, 2);
    }

    #[test]
    fn weak_entries_are_listed_and_sorted() {
        let report = analyze(entries(&[
            ("zebra", "abc"),
            ("apple", "password"),
            ("strong", "Tr0ub4dor&3x"),
        ]));
        assert_eq!(report.weak, vec!["apple", "zebra"]);
        // Both weak passwords differ, so neither is reuse.
        assert!(report.reused.is_empty());
    }

    // Two logins can share a weak password: it is both reuse and weak.
    #[test]
    fn a_shared_weak_password_appears_in_both_lists() {
        let report = analyze(entries(&[("a", "password"), ("b", "password")]));
        assert_eq!(report.reused, vec![vec!["a", "b"]]);
        assert_eq!(report.weak, vec!["a", "b"]);
    }

    #[test]
    fn label_omits_an_absent_or_empty_user() {
        assert_eq!(label("github.com", Some("octocat")), "github.com (octocat)");
        assert_eq!(label("github.com", None), "github.com");
        assert_eq!(label("github.com", Some("")), "github.com");
    }
}
