//! ychrome's half of the docs SSOT law (`yggterm/docs/docs-ssot.md`).
//!
//! **Why this exists.** yggterm enforces the law on its own queue with
//! `scripts/check-docs-ssot.sh`, and ychrome — which the same law names as
//! owning its own queue — had nothing. It had already drifted: on 2026-08-08
//! five of twelve entries carried no status at all, so "what is open" could not
//! be answered from the file that claims to own the question.
//!
//! That is the precise failure the law was written after. On 2026-08-02 an
//! agent reported five ychrome bugs the user had already fixed, argued from a
//! stale file, and burned a session — because three files claimed to answer one
//! question and no two agreed. An unenforced rule is a rule that has already
//! been broken; nobody just hasn't looked.

use std::path::PathBuf;

fn queue() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/pending-bugs.md");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn entries(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            out.push((head.to_string(), String::new()));
        } else if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// Every entry declares exactly one status from the vocabulary. There is no
/// `FIXED`: a fixed entry is deleted in the same commit as its fix, and git
/// remembers it.
#[test]
fn every_entry_declares_one_status_from_the_vocabulary() {
    const ALLOWED: [&str; 3] = [
        "**Status:** OPEN",
        "**Status:** FIXED IN CODE — LIVE PROOF OWED",
        "**Status:** AWAITING A DECISION",
    ];
    let text = queue();
    let entries = entries(&text);
    assert!(
        entries.len() > 3,
        "the queue parser found only {} entries — it stopped matching the file's shape",
        entries.len()
    );
    for (head, body) in &entries {
        let declared: Vec<&str> = body
            .lines()
            .filter(|line| line.trim_start().starts_with("**Status:**"))
            .collect();
        assert_eq!(
            declared.len(),
            1,
            "`{head}` declares {} statuses; every entry needs exactly one",
            declared.len()
        );
        let line = declared[0].trim();
        assert!(
            ALLOWED.iter().any(|allowed| line.starts_with(allowed)),
            "`{head}` declares {line:?}, which is outside the vocabulary {ALLOWED:?}"
        );
    }
}

/// A heading must never announce its own fix. A live entry may honestly say
/// "this half is fixed" in its body — that is reporting, not a dead entry —
/// but a heading that reads CLOSED is an entry that should have been deleted.
#[test]
fn no_entry_announces_its_own_fix_in_its_heading() {
    const CLOSURE: [&str; 6] = [
        "✅",
        "~~",
        "CLOSED",
        "FIXED AND VERIFIED",
        "FOUND AND FIXED",
        "SHIPPED",
    ];
    for (head, _) in entries(&queue()) {
        for marker in CLOSURE {
            assert!(
                !head.contains(marker),
                "`{head}` announces its own fix ({marker}) — delete it, git remembers"
            );
        }
    }
}

/// No second tracked file may claim to be the list of open bugs. A file that
/// POINTS at the queue is correct and expected; one that reproduces it is how a
/// queue rots unseen.
#[test]
fn no_second_file_advertises_itself_as_the_bug_list() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut rivals: Vec<String> = Vec::new();
    let mut stack = vec![root.join("docs")];
    stack.push(root.clone());
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                // `archive/` is the past by design, and `target/` is not ours.
                if !matches!(name.as_str(), "archive" | "target" | ".git" | "assets") {
                    stack.push(path);
                }
                continue;
            }
            if !name.ends_with(".md") || path.ends_with("docs/pending-bugs.md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let claims = text.lines().any(|line| {
                let lower = line.to_ascii_lowercase();
                lower.starts_with('#')
                    && ["pending bugs", "open bugs", "bug list", "what is left"]
                        .iter()
                        .any(|claim| lower.contains(claim))
            });
            if claims && !text.contains("docs/pending-bugs.md") {
                rivals.push(path.display().to_string());
            }
        }
    }
    assert!(
        rivals.is_empty(),
        "these files also advertise a bug list — point at docs/pending-bugs.md instead: {rivals:?}"
    );
}
