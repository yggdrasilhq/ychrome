//! `ychrome-vault` — host-resident vault access for agents and for validating
//! the native Bitwarden client against a real server. The vault sidebar (in the
//! yggterm GUI) will drive this same crate; this binary is the headless face.
//!
//! `status` — report configuration/lock state.
//! `configure --server <url> --email <e>` — run prelogin, persist secret-free config.
//! `check` (alias `unlock`) — read the master password from STDIN (use `read -rs`,
//! never a flag or env var), unlock, sync, print an item summary, exit.
//!
//! Config lives on THIS host at `~/.yggterm/vault/config.json` (host-resident
//! state — a libyggterm app owns its state where it runs).

use anyhow::{Context, Result};
use ychrome_vault::{VaultManager, VaultStatus};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let vault_dir = dirs::home_dir()
        .context("no home directory")?
        .join(".yggterm")
        .join("vault");
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let action = args.first().map(String::as_str).unwrap_or("status");
    let mut manager = VaultManager::load(&vault_dir);

    match action {
        "status" => println!("{}", serde_json::to_string_pretty(&status_json(&manager))?),
        "configure" => {
            let server = flag("--server")
                .context("usage: ychrome-vault configure --server <url> --email <email>")?;
            let email = flag("--email")
                .context("usage: ychrome-vault configure --server <url> --email <email>")?;
            manager
                .configure(&server, &email)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            println!("{}", serde_json::to_string_pretty(&status_json(&manager))?);
        }
        "check" | "unlock" => {
            if !manager.is_configured() {
                anyhow::bail!(
                    "not configured; run `ychrome-vault configure --server <url> --email <email>` first"
                );
            }
            let mut password = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut password)
                .context("reading master password from stdin")?;
            let password = password.trim_end_matches(['\n', '\r']);
            if password.is_empty() {
                anyhow::bail!(
                    "no master password on stdin (pipe it: `read -rs PW; echo \"$PW\" | ychrome-vault check`)"
                );
            }
            let count = manager
                .unlock(password)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let vault = manager.vault().expect("unlocked");
            let items = vault.items();
            let with_totp = items.iter().filter(|i| i.has_totp).count();
            let sample: Vec<&str> = items.iter().take(8).map(|i| i.name.as_str()).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "unlocked": true,
                    "item_count": count,
                    "items_with_totp": with_totp,
                    "sample_names": sample,
                }))?
            );
        }
        other => anyhow::bail!("unknown action {other:?} (status | configure | check)"),
    }
    Ok(())
}

fn status_json(manager: &VaultManager) -> serde_json::Value {
    match manager.status() {
        VaultStatus::NotConfigured => serde_json::json!({ "state": "not_configured" }),
        VaultStatus::Locked { email, server_url } => {
            serde_json::json!({ "state": "locked", "email": email, "server_url": server_url })
        }
        VaultStatus::Unlocked { email, item_count } => {
            serde_json::json!({ "state": "unlocked", "email": email, "item_count": item_count })
        }
    }
}
