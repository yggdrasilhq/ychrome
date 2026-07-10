//! `ychrome-vault` — host-resident vault access for ychrome, its sidebar, and
//! any agent or script on this machine. The native replacement for `rbw`.
//!
//! Unlock once; the agent (a unix-socket daemon, auto-started on first need)
//! caches the decrypted vault so `list`/`get`/`totp` are instant and keyless
//! until an idle timeout drops it:
//!
//! ```text
//! read -rs PW; echo "$PW" | ychrome-vault unlock
//! ychrome-vault get github.com          # password on stdout, rbw-compatible
//! ychrome-vault totp github.com         # 6-digit code
//! ychrome-vault list                    # name<TAB>user<TAB>folder
//! ```
//!
//! Config and socket live on THIS host at `~/.yggterm/vault/` — host-resident
//! state, as a libyggterm app owns its state where it runs. The master password
//! is read from stdin only (never a flag, never an environment variable) and is
//! dropped the moment the keys are derived.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use ychrome_vault::VaultManager;
use ychrome_vault::agent;

#[derive(Parser)]
#[command(name = "ychrome-vault", version, about = "ychrome's native Bitwarden/Vaultwarden client")]
struct Cli {
    /// Vault directory (config + agent socket). Default `~/.yggterm/vault`.
    #[arg(long, global = true)]
    dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Report configuration and lock state.
    Status,
    /// Fetch the account's KDF parameters and persist a secret-free config.
    Configure {
        #[arg(long)]
        server: String,
        #[arg(long)]
        email: String,
        /// Idle seconds before the agent re-locks (0 = never).
        #[arg(long)]
        lock_timeout: Option<u64>,
    },
    /// Unlock the vault in the agent, reading the master password from stdin.
    Unlock,
    /// Drop the agent's decrypted vault.
    Lock,
    /// Re-pull the ciphers into the unlocked agent (no password needed).
    Sync,
    /// Report reused and weak passwords as JSON. The scan runs inside the
    /// agent, where the ciphers are already decrypted; only entry names come
    /// back, never a password.
    Watchtower,
    /// List items as `name<TAB>user<TAB>folder`, optionally filtered.
    List {
        query: Option<String>,
        #[arg(long)]
        json: bool,
        /// List the recoverable soft-deleted items (the trash) instead of the
        /// live ones. Restore one with `restore NAME`.
        #[arg(long)]
        trashed: bool,
    },
    /// Print an item's password (or another field) — `rbw get` parity.
    Get {
        name: String,
        user: Option<String>,
        /// One of: password, username, totp, notes.
        #[arg(long, default_value = "password")]
        field: String,
    },
    /// Print an item's current TOTP code — `rbw code` parity.
    #[command(alias = "code")]
    Totp { name: String, user: Option<String> },
    /// Create a login — `rbw add` parity. The password is read from stdin, or
    /// rolled locally with `--generate` (and echoed once, so you can save it).
    Add {
        name: String,
        user: Option<String>,
        #[arg(long)]
        uri: Option<String>,
        /// Authenticator secret (base32) or a full `otpauth://` URI.
        #[arg(long)]
        totp: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        /// Name of an existing vault folder to file the item under.
        #[arg(long)]
        folder: Option<String>,
        /// Roll the password instead of reading it from stdin.
        #[arg(long)]
        generate: bool,
        #[arg(long, default_value_t = ychrome_vault::DEFAULT_LENGTH)]
        length: usize,
        #[arg(long)]
        no_symbols: bool,
    },
    /// Change fields on an existing item. Fields you do not name are preserved
    /// — including the notes, custom fields, favorite flag and password history
    /// this client does not otherwise model.
    Edit {
        name: String,
        user: Option<String>,
        /// New item name.
        #[arg(long)]
        rename: Option<String>,
        /// New username.
        #[arg(long)]
        set_user: Option<String>,
        /// Replaces the item's entire uri list with this one uri.
        #[arg(long)]
        uri: Option<String>,
        #[arg(long)]
        totp: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        /// Move the item to this existing folder.
        #[arg(long)]
        folder: Option<String>,
        /// Read a new password from stdin. The old one is kept in the item's
        /// password history.
        #[arg(long)]
        password: bool,
        /// Roll a new password instead of reading one (echoed once).
        #[arg(long, conflicts_with = "password")]
        generate: bool,
        #[arg(long, default_value_t = ychrome_vault::DEFAULT_LENGTH)]
        length: usize,
        #[arg(long)]
        no_symbols: bool,
    },
    /// Delete an item — `rbw remove` parity, but recoverable by default.
    ///
    /// The item moves to the vault's trash, where any Bitwarden client can
    /// restore it. `--permanent` destroys it instead: no trash copy, no undo.
    #[command(alias = "remove")]
    Rm {
        name: String,
        user: Option<String>,
        /// Destroy the item outright instead of trashing it. Irreversible.
        #[arg(long)]
        permanent: bool,
    },
    /// Restore a soft-deleted item from the trash — the inverse of a soft `rm`.
    ///
    /// The name is resolved among trashed items only (`list --trashed` shows
    /// them). A `--permanent` removal is gone and cannot be restored.
    Restore {
        name: String,
        user: Option<String>,
    },
    /// Roll a password without touching the vault.
    Generate {
        #[arg(default_value_t = ychrome_vault::DEFAULT_LENGTH)]
        length: usize,
        #[arg(long)]
        no_symbols: bool,
    },
    /// Resolve a page host to the ONE entry an auto-fill may use (strict rule).
    Match { host: String },
    /// Items the sidebar would float to the top for a host (loose rule, secret-free).
    Suggest { host: String },
    /// Account for every cipher the server sent: how many decrypt, and why the
    /// rest do not.
    Diagnose,
    /// Ensure the agent is running (starting it if needed) and report state.
    /// Touches no secrets and no network — the sidebar calls this on open.
    Ping,
    /// Stop the agent (drops its keys and exits). Needed after a rebuild: the
    /// agent outlives the binary, so it keeps serving the old code.
    StopAgent,
    /// Run the agent in the foreground (normally auto-started on demand).
    Agent,
    /// Unlock in-process and print a summary — validates the client end to end.
    Check,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let dir = match cli.dir {
        Some(dir) => dir,
        None => dirs::home_dir()
            .context("no home directory")?
            .join(".yggterm")
            .join("vault"),
    };

    match cli.command.unwrap_or(Command::Status) {
        Command::Agent => agent::serve(&dir),
        Command::Ping => {
            agent::request_autostart(&dir, &json!({"op": "ping"}))?;
            print_json(&agent::request(&dir, &json!({"op": "status"}))?)
        }
        Command::StopAgent => {
            let stopped = agent::stop(&dir)?;
            print_json(&json!({ "stopped": stopped }))
        }
        Command::Status => {
            // The agent is the source of truth when it is running (only it
            // knows whether the vault is unlocked); otherwise read the config.
            let status = if agent::is_running(&dir) {
                let mut response = agent::request(&dir, &json!({"op": "status"}))?;
                response["agent"] = json!(true);
                // The agent may be running a binary older than this one.
                let stale = response["exe_stamp"].as_str() != Some(&agent::exe_stamp());
                response["agent_stale"] = json!(stale);
                response
            } else {
                let mut status = agent::status_json(&VaultManager::load(&dir));
                status["agent"] = json!(false);
                status
            };
            print_json(&status)
        }
        Command::Configure {
            server,
            email,
            lock_timeout,
        } => {
            let mut manager = VaultManager::load(&dir);
            manager
                .configure(&server, &email)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if let Some(seconds) = lock_timeout {
                manager
                    .set_lock_timeout(seconds)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            }
            // A running agent still holds the OLD account's keys.
            if agent::is_running(&dir) {
                agent::request(&dir, &json!({"op": "lock"})).ok();
            }
            print_json(&agent::status_json(&manager))
        }
        Command::Unlock => {
            if !VaultManager::load(&dir).is_configured() {
                bail!(
                    "not configured — run `ychrome-vault configure --server <url> --email <email>` first"
                );
            }
            let password = read_master_password()?;
            let response =
                agent::request_autostart(&dir, &json!({"op": "unlock", "password": password}))?;
            print_json(&json!({
                "unlocked": true,
                "item_count": response["item_count"],
            }))
        }
        Command::Diagnose => print_json(&agent::request(&dir, &json!({"op": "diagnose"}))?),
        Command::Lock => print_json(&agent::request(&dir, &json!({"op": "lock"}))?),
        Command::Sync => print_json(&agent::request(&dir, &json!({"op": "sync"}))?),
        Command::Watchtower => print_json(&agent::request(&dir, &json!({"op": "watchtower"}))?),
        Command::List {
            query,
            json,
            trashed,
        } => {
            let response = agent::request(
                &dir,
                &json!({"op": "list", "query": query, "trashed": trashed}),
            )?;
            let items = response["items"].as_array().cloned().unwrap_or_default();
            if json {
                return print_json(&response["items"]);
            }
            // `name<TAB>user<TAB>folder` — the shape `rbw list --fields
            // name,user,folder` printed, so existing scripts keep parsing.
            //
            // Vault names really do contain newlines and tabs (two of this
            // user's 1048 items do), and an unescaped one turns a single record
            // into two rows: `list | wc -l` read 1050. One record, one line.
            for item in items {
                println!(
                    "{}\t{}\t{}",
                    tsv_field(&item["name"]),
                    tsv_field(&item["username"]),
                    tsv_field(&item["folder"]),
                );
            }
            Ok(())
        }
        Command::Get { name, user, field } => {
            let entry = match field.as_str() {
                "totp" => {
                    let response =
                        agent::request(&dir, &json!({"op": "totp", "name": name, "user": user}))?;
                    println!("{}", string_field(&response, "code"));
                    return Ok(());
                }
                // Notes are not in the parsed cipher at all — the agent reads
                // them off the raw record.
                "notes" => {
                    let response =
                        agent::request(&dir, &json!({"op": "notes", "name": name, "user": user}))?;
                    println!("{}", string_field(&response, "notes"));
                    return Ok(());
                }
                "password" | "username" => {
                    agent::request(&dir, &json!({"op": "get", "name": name, "user": user}))?
                }
                other => bail!("unknown --field {other:?} (password | username | totp | notes)"),
            };
            println!("{}", string_field(&entry["entry"], &field));
            Ok(())
        }
        Command::Totp { name, user } => {
            let response = agent::request(&dir, &json!({"op": "totp", "name": name, "user": user}))?;
            println!("{}", string_field(&response, "code"));
            Ok(())
        }
        Command::Generate { length, no_symbols } => {
            // Local dice — no agent, no unlock, no network.
            println!("{}", *ychrome_vault::generate_password(length, !no_symbols));
            Ok(())
        }
        Command::Add {
            name,
            user,
            uri,
            totp,
            notes,
            folder,
            generate,
            length,
            no_symbols,
        } => {
            let password = if generate {
                None
            } else {
                Some(read_secret("password")?)
            };
            let response = agent::request(
                &dir,
                &json!({
                    "op": "add", "name": name, "user": user, "uri": uri,
                    "totp": totp, "notes": notes, "password": password,
                    "folder": folder,
                    "generate": generate, "length": length, "symbols": !no_symbols,
                }),
            )?;
            print_json(&json!({
                "added": response["name"],
                "id": response["id"],
                "generated_password": response["generated_password"],
            }))
        }
        Command::Edit {
            name,
            user,
            rename,
            set_user,
            uri,
            totp,
            notes,
            folder,
            password,
            generate,
            length,
            no_symbols,
        } => {
            let password = password.then(|| read_secret("new password")).transpose()?;
            let response = agent::request(
                &dir,
                &json!({
                    "op": "edit", "name": name, "user": user,
                    "rename": rename, "set_user": set_user, "uri": uri,
                    "totp": totp, "notes": notes, "folder": folder,
                    "password": password,
                    "generate": generate, "length": length, "symbols": !no_symbols,
                }),
            )?;
            print_json(&json!({
                "edited": response["name"],
                "id": response["id"],
                "generated_password": response["generated_password"],
            }))
        }
        Command::Rm {
            name,
            user,
            permanent,
        } => {
            let response = agent::request(
                &dir,
                &json!({"op": "rm", "name": name, "user": user, "permanent": permanent}),
            )?;
            print_json(&json!({
                "removed": response["name"],
                "id": response["id"],
                // Which of the two operations actually happened. They are not
                // interchangeable: only a trashed item can be restored.
                "trashed": response["trashed"],
                "permanent": response["permanent"],
            }))
        }
        Command::Restore { name, user } => {
            let response =
                agent::request(&dir, &json!({"op": "restore", "name": name, "user": user}))?;
            print_json(&json!({
                "restored": response["name"],
                "id": response["id"],
            }))
        }
        Command::Match { host } => {
            print_json(&agent::request(&dir, &json!({"op": "match", "host": host}))?["entry"])
        }
        Command::Suggest { host } => {
            print_json(&agent::request(&dir, &json!({"op": "suggest", "host": host}))?["items"])
        }
        Command::Check => {
            let mut manager = VaultManager::load(&dir);
            if !manager.is_configured() {
                bail!("not configured; run `ychrome-vault configure --server <url> --email <email>`");
            }
            let password = read_master_password()?;
            manager
                .unlock(&password)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let vault = manager.vault().expect("unlocked");
            let items = vault.items();
            let with_totp = items.iter().filter(|item| item.has_totp).count();
            let sample: Vec<&str> = items.iter().take(8).map(|item| item.name.as_str()).collect();
            // Prove the URI index is live too — this is what `rbw list` never had.
            let with_uris = items.iter().filter(|item| !item.uris.is_empty()).count();
            print_json(&json!({
                "unlocked": true,
                "item_count": items.len(),
                "items_with_totp": with_totp,
                "items_with_uris": with_uris,
                "sample_names": sample,
                // Accounts for every cipher the server sent, including the ones
                // we cannot read. Runs in this process, so a running agent is
                // left alone.
                "diagnostic": vault.diagnose(),
            }))
        }
    }
}

fn read_master_password() -> Result<String> {
    read_secret("master password")
}

/// Secrets come from stdin and nowhere else — never a flag (visible in `ps`),
/// never an environment variable. A terminal on stdin means the user ran the
/// command with no pipe; reading it there would echo the secret into their
/// scrollback, so refuse and show the no-echo incantation instead.
fn read_secret(what: &str) -> Result<String> {
    if std::io::stdin().is_terminal() {
        bail!(
            "pipe the {what} in without echoing it:\n    \
             read -rs PW; echo \"$PW\" | ychrome-vault …"
        );
    }
    let mut secret = String::new();
    std::io::stdin()
        .read_to_string(&mut secret)
        .with_context(|| format!("reading the {what} from stdin"))?;
    let secret = secret.trim_end_matches(['\n', '\r']).to_string();
    if secret.is_empty() {
        bail!("no {what} on stdin");
    }
    Ok(secret)
}

/// One TSV cell: control characters that would break the record boundary are
/// replaced with a space. Use `--json` when the exact bytes matter.
fn tsv_field(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or("")
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn string_field(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
