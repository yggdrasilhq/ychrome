//! ychrome's LAUNCHER MANIFEST: how the yggterm menus learn ychrome exists.
//!
//! yggterm's titlebar `+` menu, cwd-tree context menu and start page used to
//! hardcode one arm per launchable thing. They now read a registry of manifests
//! that installed libyggterm apps write to their OWN host:
//!
//! ```text
//! ~/.yggterm/apps/ychrome.json
//! ```
//!
//! We write it on **every run**, which is what repairs the `binary` path after an
//! upgrade moves the executable. The host's yggterm daemon scans the directory,
//! checks the binary still resolves, and deletes the manifests of apps that are
//! gone — so uninstalling ychrome removes it from every menu, and neither the
//! GUI nor we have to remember anything.
//!
//! Deliberately hand-rolled JSON-via-serde_json with no yggterm dependency: an
//! app declares itself with a FILE, not by linking the platform.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Reserved ephemeral profile, mirrored from `main`. The Incognito verb is just
/// the temp profile with a name a human recognises.
const TEMP_PROFILE: &str = "temp";

/// The manifest yggterm's `AppManifest` deserializes, for a given executable.
///
/// Pure: takes the binary rather than asking the process where it lives, so a
/// test can assert the schema without a real `ychrome` on disk — and, more to
/// the point, without writing to the developer's own `~/.yggterm/apps`.
fn manifest_value(binary: &Path) -> Value {
    json!({
        "name": "ychrome",
        "label": "Ychrome",
        // No icon: yggterm's own menu entries ("New Terminal", "New Codex
        // Session") are text, and DESIGN.md is the shell's to own, not ours.
        "icon": "",
        "binary": binary.to_string_lossy(),
        "verbs": [
            // No URL ⇒ ychrome serves its profile picker, which is the right
            // landing for "New Ychrome": choosing an identity comes first.
            { "id": "new", "label": "New Ychrome", "args": [] },
            {
                "id": "incognito",
                "label": "New Ychrome (Incognito)",
                "args": ["--profile", TEMP_PROFILE],
            },
        ],
    })
}

/// Write `<apps_dir>/ychrome.json` describing `binary`.
fn write_to(apps_dir: &Path, binary: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(apps_dir)?;
    let path = apps_dir.join("ychrome.json");
    let manifest = serde_json::to_string_pretty(&manifest_value(binary))?;
    std::fs::write(&path, manifest)?;
    Ok(path)
}

/// Write (or refresh) `~/.yggterm/apps/ychrome.json` on this host.
///
/// Best-effort by contract: a browser must never fail to start because a menu
/// entry could not be registered. The caller logs and carries on.
///
/// Only `main` may call this. A test that calls it would register the *test
/// harness* (`target/debug/deps/ychrome-<hash>`) as this host's browser, and
/// `cargo clean` would then have the daemon prune ychrome out of every menu.
/// Test [`write_to`] against a temp dir instead.
pub fn write() -> Result<PathBuf> {
    let binary = std::env::current_exe().context("resolving ychrome's own path")?;
    // The GUI types this into a fresh PTY, whose PATH is not a login shell's.
    // Canonicalize so a symlinked ~/.local/bin/ychrome records its real target
    // only if that is what we are; either way it must be absolute.
    let binary = binary.canonicalize().unwrap_or(binary);

    let dir = dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("apps");
    write_to(&dir, &binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private temp dir for one test, removed on the way in so a rerun starts
    /// clean. Keyed by test name — no randomness, no shared state.
    fn scratch_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ychrome-manifest-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    // The manifest must name the keys yggterm's `AppManifest` deserializes, and
    // its `name` must equal the file stem — yggterm ignores a manifest that
    // claims a name its filename does not back.
    #[test]
    fn the_manifest_matches_the_schema_yggterm_reads() {
        let dir = scratch_dir("schema");
        let path = write_to(&dir, Path::new("/opt/ychrome/bin/ychrome")).expect("write manifest");
        assert_eq!(path.file_name().unwrap(), "ychrome.json");

        let raw = std::fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["name"], "ychrome");
        assert_eq!(value["binary"], "/opt/ychrome/bin/ychrome");
        assert!(
            value["binary"].as_str().unwrap().starts_with('/'),
            "binary must be absolute: a fresh PTY has no login PATH"
        );
        let verbs = value["verbs"].as_array().unwrap();
        assert_eq!(verbs.len(), 2);
        assert_eq!(verbs[0]["id"], "new");
        assert_eq!(verbs[0]["args"].as_array().unwrap().len(), 0);
        assert_eq!(verbs[1]["args"][1], TEMP_PROFILE);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Writing on every run is what repairs the recorded path after an upgrade,
    // so the write must be idempotent AND must overwrite a stale binary path.
    #[test]
    fn a_rewrite_repairs_a_stale_binary_path() {
        let dir = scratch_dir("repair");
        write_to(&dir, Path::new("/old/ychrome")).expect("first write");
        let path = write_to(&dir, Path::new("/new/ychrome")).expect("second write");

        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["binary"], "/new/ychrome");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "a rewrite must replace ychrome.json, not accumulate manifests"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Regression: this test suite once called `write()` directly, which resolves
    // `current_exe()` and `$HOME`. Running `cargo test` therefore registered the
    // test harness as this host's browser — the real `~/.yggterm/apps/ychrome.json`
    // on the dev box pointed at `target/debug/deps/ychrome-<hash>`, so yggterm's
    // `+` menu would have launched a test binary, and `cargo clean` would have had
    // the daemon prune ychrome from every menu. Nothing under test may touch $HOME.
    #[test]
    fn the_test_suite_never_writes_to_the_real_registry() {
        let home = dirs::home_dir().expect("home dir");
        let real = home.join(".yggterm").join("apps").join("ychrome.json");
        let before = std::fs::read_to_string(&real).ok();

        let dir = scratch_dir("isolation");
        write_to(&dir, Path::new("/opt/ychrome/bin/ychrome")).expect("write manifest");

        assert_eq!(
            std::fs::read_to_string(&real).ok(),
            before,
            "write_to must never touch the host's real launcher registry"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
