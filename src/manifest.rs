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

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::json;

/// Reserved ephemeral profile, mirrored from `main`. The Incognito verb is just
/// the temp profile with a name a human recognises.
const TEMP_PROFILE: &str = "temp";

/// Write (or refresh) `~/.yggterm/apps/ychrome.json` on this host.
///
/// Best-effort by contract: a browser must never fail to start because a menu
/// entry could not be registered. The caller logs and carries on.
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
    std::fs::create_dir_all(&dir)?;

    let manifest = json!({
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
    });

    let path = dir.join("ychrome.json");
    std::fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The manifest must name the keys yggterm's `AppManifest` deserializes, and
    // its `name` must equal the file stem — yggterm ignores a manifest that
    // claims a name its filename does not back.
    #[test]
    fn the_manifest_matches_the_schema_yggterm_reads() {
        let path = write().expect("write manifest");
        assert_eq!(path.file_name().unwrap(), "ychrome.json");

        let raw = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["name"], "ychrome");
        assert!(
            value["binary"].as_str().unwrap().starts_with('/'),
            "binary must be absolute: a fresh PTY has no login PATH"
        );
        let verbs = value["verbs"].as_array().unwrap();
        assert_eq!(verbs.len(), 2);
        assert_eq!(verbs[0]["id"], "new");
        assert_eq!(verbs[0]["args"].as_array().unwrap().len(), 0);
        assert_eq!(verbs[1]["args"][1], TEMP_PROFILE);
    }
}
