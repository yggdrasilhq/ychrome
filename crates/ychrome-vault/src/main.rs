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
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use ychrome_vault::VaultManager;
use ychrome_vault::agent;

#[derive(Parser)]
#[command(
    name = "ychrome-vault",
    version,
    about = "ychrome's native Bitwarden/Vaultwarden client"
)]
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
    /// Show or set the idle-lock timeout (0 = never auto-lock, the default).
    /// Takes effect on the running agent immediately and persists to the config;
    /// it does NOT lock the vault.
    LockTimeout {
        /// Seconds of inactivity before the agent drops the vault. Omit to read.
        seconds: Option<u64>,
    },
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
        /// Which field to print.
        #[arg(long, default_value = "password")]
        field: GetField,
    },
    /// Print an item's current TOTP code — `rbw code` parity.
    ///
    /// Refuses on a host whose kernel says its clock is not disciplined: a
    /// 30 s window tolerates one window of skew, and manin was 72 s out while
    /// minting confident, always-wrong codes. `--ignore-clock` waives that
    /// after you have read what is wrong; `ychrome-vault clock` shows it.
    #[command(alias = "code")]
    Totp {
        name: String,
        user: Option<String>,
        /// Mint anyway on an undisciplined clock. The code will very probably
        /// be rejected; this exists for a host you have verified by other means.
        #[arg(long)]
        ignore_clock: bool,
    },
    /// Print what the KERNEL says about this host's clock, as JSON.
    ///
    /// Needs no unlock. ⚠ Do NOT diagnose from `chronyc tracking`'s
    /// `Last offset`/`RMS offset` — they reported perfect tracking on a host
    /// that was 72 s out. `timedatectl`'s `System clock synchronized:` and
    /// `chronyc tracking`'s `System time :` are the lines that tell the truth,
    /// and this verb reads the same kernel state they do.
    Clock,
    /// List an item's stored passkeys as `rpId<TAB>user<TAB>credentialId<TAB>created`.
    ///
    /// Metadata only — the passkey private key is never printed, and a listing
    /// can never trigger a WebAuthn ceremony.
    Passkeys { name: String, user: Option<String> },
    /// Print a card item's metadata as
    /// `brand<TAB>cardholder<TAB>expMonth<TAB>expYear<TAB>last4`.
    ///
    /// Metadata only. The full number and the CVV are deliberately unreachable
    /// from the CLI: a PAN printed to a terminal is durable — scrollback, shell
    /// history, an agent CLI's transcript — and unlike a password it cannot be
    /// rotated on demand. The number reaches a page through the sidebar's fill
    /// injector instead, which never prints it.
    Card { name: String, user: Option<String> },
    /// Print an item's custom fields as `name<TAB>value`, one per line — the read
    /// `get --field` cannot do (it only models password/username/totp/notes).
    /// With `--field-name NAME`, print just that field's value, unadorned.
    Fields {
        name: String,
        user: Option<String>,
        /// Print only the value of the custom field with this exact name
        /// (case-insensitive), with no name column — for scripting.
        #[arg(long)]
        field_name: Option<String>,
    },
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
    ///
    /// Every change is RE-READ after the write and the result names what
    /// actually landed; an edit the server took but a re-read cannot see is
    /// reported as a failure.
    Edit {
        name: String,
        user: Option<String>,
        /// New item name (the entry's title).
        #[arg(long)]
        rename: Option<String>,
        /// New username.
        #[arg(long)]
        set_user: Option<String>,
        /// Replaces the item's ENTIRE uri list. Repeat for several uris, in
        /// order. A uri the item already stores keeps its match type.
        #[arg(long)]
        uri: Vec<String>,
        #[arg(long)]
        totp: Option<String>,
        /// Clear the authenticator secret entirely (removes a value mis-stored
        /// in the TOTP slot). The same thing as `--clear totp`, kept because it
        /// shipped first.
        #[arg(long, conflicts_with = "totp")]
        clear_totp: bool,
        /// Remove a field's value rather than replacing it. Repeatable. This is
        /// the ONLY way to empty a field: setting one to "" is refused, because
        /// a stored empty string and an absent value are different facts.
        #[arg(long, value_name = "FIELD")]
        clear: Vec<ClearFieldArg>,
        #[arg(long)]
        notes: Option<String>,
        /// Move the item to this existing folder.
        #[arg(long)]
        folder: Option<String>,
        /// Set a custom field: `--set-field NAME=VALUE`. Repeatable. Creates the
        /// field if it is absent. A field that is already HIDDEN stays hidden —
        /// updating a secret must never expose it as a side effect.
        #[arg(long, value_name = "NAME=VALUE")]
        set_field: Vec<String>,
        /// Set a HIDDEN custom field, reading its value from stdin like a
        /// password. One per edit, because stdin carries exactly one value.
        #[arg(long, value_name = "NAME", conflicts_with = "password")]
        set_hidden_field: Option<String>,
        /// Delete a custom field. Repeatable. A name the item does not carry is
        /// an error, not a silent no-op.
        #[arg(long, value_name = "NAME")]
        remove_field: Vec<String>,
        /// Set a card's brand (Visa, Mastercard, …).
        #[arg(long, value_name = "BRAND")]
        card_brand: Option<String>,
        /// Set the name printed on a card.
        #[arg(long, value_name = "NAME")]
        card_holder: Option<String>,
        /// Set a card's expiry month, 1-12.
        #[arg(long, value_name = "MM")]
        card_exp_month: Option<String>,
        /// Set a card's expiry year, four digits.
        #[arg(long, value_name = "YYYY")]
        card_exp_year: Option<String>,
        /// Read a new card number from stdin.
        ///
        /// ⛔ THERE IS NO `--card-number VALUE` FORM, AND THERE WILL NOT BE. A
        /// PAN in argv is readable by every process on this host through `ps`
        /// and lands in the shell's history file; unlike a password it cannot
        /// be rotated on demand once it leaks.
        #[arg(long, conflicts_with_all = ["password", "set_hidden_field", "card_code"])]
        card_number: bool,
        /// Read a new card security code (CVV) from stdin. Same reason as
        /// `--card-number`: never argv.
        #[arg(long, conflicts_with_all = ["password", "set_hidden_field"])]
        card_code: bool,
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
    Restore { name: String, user: Option<String> },
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
    /// Hand the running agent's unlocked session to the newly installed binary,
    /// WITHOUT re-locking the vault — the cheap alternative to `stop-agent`.
    ///
    /// The agent execs the installed `ychrome-vault` in place: same pid, same
    /// bound socket, new code, unlock intact. The successor is chosen by the
    /// agent (the installed binary), never named here.
    Handover,
    /// Run the agent in the foreground (normally auto-started on demand).
    Agent {
        /// Internal: serve on an inherited, already-bound listener fd instead of
        /// binding one. Set by the `handover` exec; not for hand use.
        #[arg(long, hide = true, requires = "adopt_payload")]
        adopt_listener: Option<std::os::fd::RawFd>,
        /// Internal: read the inherited session from this fd. Set by the
        /// `handover` exec; not for hand use.
        #[arg(long, hide = true, requires = "adopt_listener")]
        adopt_payload: Option<std::os::fd::RawFd>,
    },
    /// Unlock in-process and print a summary — validates the client end to end.
    Check,
}

/// The fields `get` can print. ONE list owns this: clap derives the accepted
/// values, the `--help` text and the "invalid value" error from these variants,
/// and the match in `Command::Get` is exhaustive over them.
///
/// It was spelled in four places that had already drifted apart — the flag's
/// doc comment promised four fields, the match accepted five, and the error
/// message named a different four. A whitelist that disagrees with its own help
/// is how `totp-secret` came to be undocumented but working.
/// The fields `edit --clear` can remove.
///
/// A thin clap shell over [`ychrome_vault::model::ClearField`], which is the
/// real owner: the spelling, the refusals and the patcher all read from there,
/// and this exists only because clap cannot derive `ValueEnum` on a type in
/// another crate. The `From` below is the ONE place the two meet.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ClearFieldArg {
    Notes,
    Totp,
    Username,
    /// The whole uri list.
    Uri,
    /// Move the item out of every folder.
    Folder,
}

impl From<ClearFieldArg> for ychrome_vault::model::ClearField {
    fn from(arg: ClearFieldArg) -> Self {
        use ychrome_vault::model::ClearField;
        match arg {
            ClearFieldArg::Notes => ClearField::Notes,
            ClearFieldArg::Totp => ClearField::Totp,
            ClearFieldArg::Username => ClearField::Username,
            ClearFieldArg::Uri => ClearField::Uri,
            ClearFieldArg::Folder => ClearField::Folder,
        }
    }
}

/// `NAME=VALUE` for `--set-field`. The name may not be empty; the value may
/// contain `=` (a token or a URL routinely does), so only the FIRST `=` splits.
fn parse_name_value(pair: &str) -> Result<(String, String)> {
    let (name, value) = pair
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("expected NAME=VALUE, got {pair:?}"))?;
    if name.trim().is_empty() {
        bail!("a custom field needs a name: {pair:?}");
    }
    Ok((name.to_string(), value.to_string()))
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum GetField {
    Password,
    Username,
    /// The current authenticator code.
    Totp,
    /// The verbatim text in the TOTP slot, even when it is not a valid
    /// authenticator (a key pasted there by mistake) — `totp` rejects that,
    /// this recovers it.
    TotpSecret,
    Notes,
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
        Command::Agent {
            adopt_listener,
            adopt_payload,
        } => match (adopt_listener, adopt_payload) {
            // clap's `requires` makes the half-given case unreachable from a
            // command line; the match is exhaustive because an unreachable arm
            // that panics is worse than one that explains itself.
            (Some(listener), Some(payload)) => agent::serve_adopted(&dir, listener, payload),
            _ => agent::serve(&dir),
        },
        Command::Handover => {
            // The agent replies BEFORE it execs, so "accepted" is not proof. The
            // proof is a SECOND round trip: ask the agent on that socket who it
            // is now. A connection that lands mid-exec is queued on the
            // inherited listener rather than refused, which is the whole reason
            // the listener fd crosses the boundary.
            let accepted = agent::request(&dir, &json!({"op": "handover"}))?;
            let after = agent::request(&dir, &json!({"op": "status"}))?;
            let now = after["exe_stamp"].as_str().unwrap_or_default();
            let handed_over = !now.is_empty() && Some(now) == accepted["successor_stamp"].as_str();
            print_json(&json!({
                "handed_over": handed_over,
                "successor": accepted["successor"],
                "pid": accepted["pid"],
                "exe_stamp": now,
                "state": after["state"],
                "item_count": after["item_count"],
            }))?;
            if !handed_over {
                bail!(
                    "the agent is still running its old binary — its stderr says why, \
                     and the vault is untouched"
                );
            }
            Ok(())
        }
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
            // Where this vault's socket is, whether or not anything is
            // listening on it. A client outside this workspace (yggterm's
            // `web fill-card`, which speaks the wire but cannot link the proto
            // crate) has to find the socket somehow, and asking the binary that
            // owns the layout beats re-deriving `~/.yggterm/vault` from a home
            // directory that the caller's own `--dir` may have moved.
            let mut status = status;
            status["socket"] = json!(agent::socket_path(&dir).display().to_string());
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
        Command::LockTimeout { seconds } => match seconds {
            Some(seconds) => {
                // Route through the agent when one is running, so a LIVE unlocked
                // vault picks the change up without being dropped. With no agent
                // there is nothing to inform — just persist it. (Never autostart:
                // spawning an agent to change a setting would be absurd.)
                if agent::is_running(&dir) {
                    print_json(&agent::request(
                        &dir,
                        &json!({"op": "set-lock-timeout", "seconds": seconds}),
                    )?)
                } else {
                    let mut manager = VaultManager::load(&dir);
                    manager
                        .set_lock_timeout(seconds)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    print_json(&json!({
                        "lock_timeout_secs": seconds,
                        "auto_lock": seconds != 0,
                        "agent_running": false,
                    }))
                }
            }
            None => {
                let manager = VaultManager::load(&dir);
                let secs = manager.lock_timeout_secs();
                print_json(&json!({
                    "lock_timeout_secs": secs,
                    "auto_lock": secs != 0,
                }))
            }
        },
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
            let named = |op: &str| json!({"op": op, "name": &name, "user": &user});
            // Which agent op answers, and which key of its reply carries the
            // value. Notes and the raw TOTP secret are not in the parsed cipher
            // at all — the agent reads those off the raw record.
            let (reply, key) = match field {
                GetField::Totp => (agent::request(&dir, &named("totp"))?, "code"),
                GetField::Notes => (agent::request(&dir, &named("notes"))?, "notes"),
                GetField::TotpSecret => {
                    (agent::request(&dir, &named("totp-secret"))?, "totp_secret")
                }
                // One round trip answers both: the `get` op returns the whole
                // entry.
                GetField::Password | GetField::Username => {
                    let mut reply = agent::request(&dir, &named("get"))?;
                    let key = if field == GetField::Password {
                        "password"
                    } else {
                        "username"
                    };
                    (reply["entry"].take(), key)
                }
            };
            println!("{}", required_field(&reply, key, &name)?);
            Ok(())
        }
        Command::Totp {
            name,
            user,
            ignore_clock,
        } => {
            let response = agent::request(
                &dir,
                &json!({"op": "totp", "name": name, "user": user,
                        "ignore_clock": ignore_clock}),
            )?;
            println!("{}", required_field(&response, "code", &name)?);
            Ok(())
        }
        Command::Clock => {
            let response = agent::request(&dir, &json!({"op": "clock"}))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
        Command::Passkeys { name, user } => {
            let response =
                agent::request(&dir, &json!({"op": "passkeys", "name": name, "user": user}))?;
            // rpId<TAB>user<TAB>credentialId<TAB>created — one passkey per line,
            // same TSV discipline as `list` (control chars neutralised).
            for pk in response["passkeys"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{}\t{}\t{}\t{}",
                    tsv_field(&pk["rp_id"]),
                    tsv_field(&pk["user_name"]),
                    tsv_field(&pk["credential_id"]),
                    tsv_field(&pk["creation_date"]),
                );
            }
            Ok(())
        }
        Command::Card { name, user } => {
            let response =
                agent::request(&dir, &json!({"op": "card", "name": name, "user": user}))?;
            let card = &response["card"];
            // One column per stored field, no joining — same TSV discipline as
            // `passkeys`. An absent sub-field is an empty column: unlike `get`,
            // a card row is a record, and a card with no cardholder name is
            // ordinary rather than a failure.
            println!(
                "{}\t{}\t{}\t{}\t{}",
                tsv_field(&card["brand"]),
                tsv_field(&card["cardholder"]),
                tsv_field(&card["exp_month"]),
                tsv_field(&card["exp_year"]),
                tsv_field(&card["last4"]),
            );
            Ok(())
        }
        Command::Fields {
            name,
            user,
            field_name,
        } => {
            let response =
                agent::request(&dir, &json!({"op": "fields", "name": name, "user": user}))?;
            let fields = response["fields"].as_array().cloned().unwrap_or_default();
            // `--field-name` prints exactly one value with no name column, so a
            // script can capture it; an unknown name is an error, not silence.
            if let Some(want) = field_name {
                for field in &fields {
                    if field["name"]
                        .as_str()
                        .is_some_and(|got| got.eq_ignore_ascii_case(&want))
                    {
                        // A field can come over with no value for two different
                        // reasons, and they send the reader to two different
                        // places: a LINKED field stores none by design (it points
                        // at the item's username or password) and there is
                        // nothing to chase, while an unreadable one is a key this
                        // vault does not hold. The agent says which; naming one
                        // cause for both is how a key problem got reported as a
                        // link that does not exist. Printing an empty line would
                        // be worse still — the same absent-vs-empty confusion
                        // `required_field` exists to end.
                        let Some(value) = field["value"].as_str() else {
                            match field["absent"].as_str() {
                                Some("linked") => bail!(
                                    "custom field {want:?} is a linked field and has no stored value"
                                ),
                                Some("unreadable") => bail!(
                                    "custom field {want:?} holds a value this vault could not decrypt"
                                ),
                                // An agent too old to say which. Name both and
                                // claim neither.
                                _ => bail!(
                                    "custom field {want:?} has no readable value: it is either a \
                                     linked field, which stores none, or one this vault could not \
                                     decrypt"
                                ),
                            }
                        };
                        println!("{value}");
                        return Ok(());
                    }
                }
                bail!("no custom field named {want:?}");
            }
            // name<TAB>value — same TSV discipline as `list`/`passkeys`.
            for field in &fields {
                println!(
                    "{}\t{}",
                    tsv_field(&field["name"]),
                    tsv_field(&field["value"])
                );
            }
            // When nothing prints, say whether the item truly has no custom
            // fields or has some that would not decrypt — on stderr, so a script
            // capturing stdout is unaffected.
            if fields.is_empty() {
                match response["raw_field_count"].as_u64() {
                    Some(0) | None => eprintln!("(item carries no custom fields)"),
                    Some(n) => eprintln!("({n} custom field(s) present but none decrypted)"),
                }
            }
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
            clear_totp,
            clear,
            notes,
            folder,
            set_field,
            set_hidden_field,
            remove_field,
            card_brand,
            card_holder,
            card_exp_month,
            card_exp_year,
            card_number,
            card_code,
            password,
            generate,
            length,
            no_symbols,
        } => {
            // `--clear-totp` is the same request as `--clear totp`; it folds in
            // here, at the CLI edge, so the enum stays the single owner and the
            // two spellings cannot mean different things.
            let mut clear: Vec<Value> = clear
                .into_iter()
                .map(|field| json!(ychrome_vault::model::ClearField::from(field).as_str()))
                .collect();
            if clear_totp {
                clear.push(json!(ychrome_vault::model::ClearField::Totp.as_str()));
            }

            let mut fields: Vec<Value> = Vec::new();
            for pair in &set_field {
                let (field, value) = parse_name_value(pair)?;
                fields.push(json!({"name": field, "action": "set", "value": value}));
            }
            for field in &remove_field {
                fields.push(json!({"name": field, "action": "remove"}));
            }
            // Exactly one stdin read per invocation, and clap has already ruled
            // out asking for both. A hidden custom field is a secret like a
            // password, so it comes the same way a password does rather than
            // through an argv every `ps` on this host can read.
            let mut hidden_value = None;
            if let Some(field) = &set_hidden_field {
                hidden_value = Some(read_secret(&format!("value for custom field {field:?}"))?);
            }
            // The card's two secrets come the same way, and clap has already
            // ruled out asking for more than one stdin value in a run.
            let card_number_value = card_number
                .then(|| read_secret("new card number"))
                .transpose()?;
            let card_code_value = card_code
                .then(|| read_secret("new card security code"))
                .transpose()?;
            let password = match (password, hidden_value) {
                (true, _) => Some(read_secret("new password")?),
                (false, Some(value)) => {
                    let field = set_hidden_field.clone().expect("read under this flag");
                    fields.push(json!({
                        "name": field, "action": "set-hidden", "value": value,
                    }));
                    None
                }
                (false, None) => None,
            };

            let response = agent::request(
                &dir,
                &json!({
                    "op": "edit", "name": name, "user": user,
                    "rename": rename, "set_user": set_user, "uris": uri,
                    "totp": totp, "clear": clear, "notes": notes, "folder": folder,
                    "fields": fields,
                    "password": password,
                    "card_brand": card_brand, "card_holder": card_holder,
                    "card_exp_month": card_exp_month, "card_exp_year": card_exp_year,
                    "card_number": card_number_value, "card_code": card_code_value,
                    "generate": generate, "length": length, "symbols": !no_symbols,
                }),
            )?;
            // ⛔ NO RECEIPT MEANS NO PROOF, AND AN AGENT OUTLIVES ITS BINARY.
            // An agent older than this CLI silently ignores every argument it
            // does not know — `uris`, `clear`, `fields` — and would otherwise
            // report a cheerful success for an edit that changed nothing. It
            // cannot fake `verified`, so its absence is the tell.
            let Some(verified) = response.get("verified").and_then(Value::as_array) else {
                bail!(
                    "this vault agent is older than this binary: it did not verify the \
                     edit, and it silently ignores the fields it does not know. \
                     Run `ychrome-vault handover` and retry."
                );
            };
            print_json(&json!({
                "edited": response["name"],
                "id": response["id"],
                "generated_password": response["generated_password"],
                // Which changes a re-read actually found. Names, never values.
                "verified": verified,
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
                bail!(
                    "not configured; run `ychrome-vault configure --server <url> --email <email>`"
                );
            }
            let password = read_master_password()?;
            manager
                .unlock(&password)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let vault = manager.vault().expect("unlocked");
            let items = vault.items();
            let with_totp = items.iter().filter(|item| item.has_totp).count();
            let sample: Vec<&str> = items
                .iter()
                .take(8)
                .map(|item| item.name.as_str())
                .collect();
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

/// The ONE owner of "the field is not there, so this command failed".
///
/// This used to be `as_str().unwrap_or_default()`, which printed a bare newline
/// and exited 0 for a value the agent sent as JSON null. `USER=$(ychrome-vault
/// get ITEM --field username)` then captured "" and reported success, so a
/// script could not tell an item with NO username from one whose username is an
/// empty string. An absent field is now a non-zero exit with the reason on
/// stderr, like every other failure here; a stored empty string still prints and
/// still succeeds.
fn required_field<'a>(reply: &'a Value, key: &str, item: &str) -> Result<&'a str> {
    match reply.get(key) {
        Some(Value::String(value)) => Ok(value),
        // The wire is snake_case; the user speaks the CLI's kebab-case.
        _ => bail!("{item:?} has no {}", key.replace('_', "-")),
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
