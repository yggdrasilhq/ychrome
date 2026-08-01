//! Configuration and the unlock lifecycle.
//!
//! Persisted to disk: the server URL, email, KDF parameters, and a random
//! device identifier — never the master password, the master key, or the user
//! key. Unlocking derives the keys, logs in, syncs, and holds the decrypted
//! [`Vault`] in memory for the life of the process. Locking drops it.

use std::path::PathBuf;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::api::{ApiError, Client};
use crate::crypto::{Kdf, MasterKey};
use crate::model::Vault;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("the vault is not configured yet")]
    NotConfigured,
    #[error("the vault is locked")]
    Locked,
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
    #[error(transparent)]
    Edit(#[from] crate::model::EditError),
    /// The server took the write and a re-read could not find it. Names the
    /// FIELDS, never their values.
    #[error(
        "the server accepted the write but a re-read does not show it: {0}. \
         The item may have been changed by another client — run `ychrome-vault sync` \
         and check before retrying."
    )]
    EditNotVerified(String),
    #[error("config storage: {0}")]
    Io(String),
}

/// How long an idle unlocked vault stays unlocked in the agent, when the
/// config does not say otherwise. Zero means "never auto-lock".
///
/// The default is **never**, by the owner's explicit call (2026-07-24): these are
/// single-owner machines, the unlock costs a master password typed by hand, and an
/// hourly re-lock silently broke long unattended runs — an agent mid-task would
/// find the vault locked with no one around to type it again. An unlock now lasts
/// until `ychrome-vault lock`, a reboot, or `stop-agent`. Set a non-zero
/// `lock-timeout` on any host where that trade is wrong.
pub const DEFAULT_LOCK_TIMEOUT_SECS: u64 = 0;

fn default_lock_timeout_secs() -> u64 {
    DEFAULT_LOCK_TIMEOUT_SECS
}

/// Persisted, secret-free configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    pub server_url: String,
    pub email: String,
    pub kdf_type: u32,
    pub kdf_iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kdf_memory: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kdf_parallelism: Option<u32>,
    pub device_id: String,
    /// Idle seconds before the agent drops the unlocked vault. 0 = never.
    #[serde(default = "default_lock_timeout_secs")]
    pub lock_timeout_secs: u64,
}

impl VaultConfig {
    fn kdf(&self) -> Result<Kdf, crate::crypto::CryptoError> {
        Kdf::from_prelogin(
            self.kdf_type,
            self.kdf_iterations,
            self.kdf_memory,
            self.kdf_parallelism,
        )
    }
}

/// What the sidebar shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultStatus {
    NotConfigured,
    Locked {
        email: String,
        server_url: String,
    },
    Unlocked {
        email: String,
        /// Items we can actually decrypt and show. NOT the cipher count — the
        /// two differ whenever the vault holds ciphers sealed under a key we
        /// do not have, and reporting the larger number was a lie.
        item_count: usize,
        cipher_count: usize,
    },
}

/// The unlocked session, reduced to what survives an `execve`: the user key and
/// the two bearer tokens. Carried by `agent::handover` from one generation of
/// the agent to the next so a rebuilt binary does not cost a re-typed master
/// password.
///
/// Deliberately not `Serialize`, not `Debug` and not `Clone`. It exists for one
/// hop between two generations of the SAME pid, and every field zeroizes.
pub(crate) struct SessionMaterial {
    pub(crate) user_key: Zeroizing<[u8; 64]>,
    pub(crate) access_token: Zeroizing<String>,
    pub(crate) refresh_token: Option<Zeroizing<String>>,
}

/// Owns the vault config and the unlocked session. One per agent process.
pub struct VaultManager {
    dir: PathBuf,
    config: Option<VaultConfig>,
    vault: Option<Vault>,
    /// Bearer token from the last successful unlock, held so `resync` (and
    /// cipher writes) never need the master password a second time. Dropped
    /// by `lock` together with the vault.
    access_token: Option<Zeroizing<String>>,
    /// The long-lived refresh token from the same unlock. The access token
    /// above expires on the server's schedule; this is what buys a new one
    /// WITHOUT the master password, which is what makes "unlocked until
    /// locked" true for writes as well as reads.
    refresh_token: Option<Zeroizing<String>>,
    /// When the in-memory ciphers were last pulled from the server.
    ///
    /// ⛔ THE FAILURE THIS EXISTS TO END. A vault that has not synced since the
    /// morning serves the value it holds with exactly the confidence of a fresh
    /// one, and nothing says otherwise: on 2026-08-01 a password corrected in
    /// the official Bitwarden extension was filled from OUR stale copy into a
    /// sign-in form, and the surface that filled it had no way to say "this is
    /// from N hours ago". Age is not a diagnostic here, it is the single fact
    /// that separates a credential from a wrong one.
    ///
    /// Set in ONE place — [`VaultManager::mark_synced`] — reached by every path
    /// that replaces the cipher set (`unlock`, `resync`, and therefore every
    /// write, which resyncs). Cleared by `lock`, because a locked vault holds no
    /// ciphers and "synced 3 minutes ago" about nothing is a lie.
    last_sync: Option<std::time::SystemTime>,
}

impl VaultManager {
    /// Load `<dir>/config.json` if present. Never fails on a missing/corrupt
    /// config — that just means "not configured".
    pub fn load(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let config = std::fs::read(dir.join("config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<VaultConfig>(&bytes).ok());
        VaultManager {
            dir,
            config,
            vault: None,
            access_token: None,
            refresh_token: None,
            last_sync: None,
        }
    }

    pub fn status(&self) -> VaultStatus {
        match (&self.config, &self.vault) {
            (Some(config), Some(vault)) => VaultStatus::Unlocked {
                email: config.email.clone(),
                item_count: vault.items().len(),
                cipher_count: vault.len(),
            },
            (Some(config), None) => VaultStatus::Locked {
                email: config.email.clone(),
                server_url: config.server_url.clone(),
            },
            (None, _) => VaultStatus::NotConfigured,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.config.is_some()
    }

    pub fn is_unlocked(&self) -> bool {
        self.vault.is_some()
    }

    pub fn vault(&self) -> Option<&Vault> {
        self.vault.as_ref()
    }

    pub fn config(&self) -> Option<&VaultConfig> {
        self.config.as_ref()
    }

    /// Idle-lock timeout from the config (0 = never auto-lock).
    pub fn lock_timeout_secs(&self) -> u64 {
        self.config
            .as_ref()
            .map(|config| config.lock_timeout_secs)
            .unwrap_or(DEFAULT_LOCK_TIMEOUT_SECS)
    }

    /// The bearer token of the current session, for cipher writes.
    pub fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref().map(String::as_str)
    }

    /// Contact the server for the account's KDF parameters and persist the
    /// configuration. Reuses the existing device id, or mints one. Does NOT
    /// unlock — the master password is a separate, unstored step.
    pub fn configure(&mut self, server_url: &str, email: &str) -> Result<(), VaultError> {
        let server_url = server_url.trim().trim_end_matches('/').to_string();
        let email = email.trim().to_string();
        let client = Client::new(&server_url)?;
        let prelogin = client.prelogin(&email)?;
        let device_id = self
            .config
            .as_ref()
            .map(|config| config.device_id.clone())
            .unwrap_or_else(new_device_id);
        let config = VaultConfig {
            server_url,
            email,
            kdf_type: match prelogin.kdf {
                Kdf::Pbkdf2 { .. } => 0,
                Kdf::Argon2id { .. } => 1,
            },
            kdf_iterations: match prelogin.kdf {
                Kdf::Pbkdf2 { iterations } => iterations,
                Kdf::Argon2id { iterations, .. } => iterations,
            },
            kdf_memory: match prelogin.kdf {
                Kdf::Argon2id { memory_mib, .. } => Some(memory_mib),
                _ => None,
            },
            kdf_parallelism: match prelogin.kdf {
                Kdf::Argon2id { parallelism, .. } => Some(parallelism),
                _ => None,
            },
            device_id,
            lock_timeout_secs: self
                .config
                .as_ref()
                .map(|config| config.lock_timeout_secs)
                .unwrap_or(DEFAULT_LOCK_TIMEOUT_SECS),
        };
        self.persist(&config)?;
        self.config = Some(config);
        self.lock();
        Ok(())
    }

    /// Persist a new idle-lock timeout (0 = never).
    pub fn set_lock_timeout(&mut self, seconds: u64) -> Result<(), VaultError> {
        let mut config = self.config.clone().ok_or(VaultError::NotConfigured)?;
        config.lock_timeout_secs = seconds;
        self.persist(&config)?;
        self.config = Some(config);
        Ok(())
    }

    /// Derive the keys from the master password, log in, sync, and hold the
    /// decrypted vault. Returns the item count. The password is used here and
    /// dropped; it is never stored.
    pub fn unlock(&mut self, master_password: &str) -> Result<usize, VaultError> {
        let config = self.config.clone().ok_or(VaultError::NotConfigured)?;
        let kdf = config.kdf()?;
        let master_key = MasterKey::derive(master_password, &config.email, kdf)?;
        let password_hash = master_key.password_hash_b64(master_password);

        let client = Client::new(&config.server_url)?;
        let token = client.token(&config.email, &password_hash, &config.device_id)?;

        // Decrypt the protected user key with the stretched master key.
        let stretched = master_key.stretch();
        let user_key_bytes = stretched.decrypt(&token.protected_user_key)?;
        let user_key = crate::crypto::SymmetricKey::from_bytes(&user_key_bytes)?;

        let sync = client.sync(&token.access_token)?;
        let organization_keys = unwrap_organization_keys(&user_key, &sync)?;
        let vault = Vault::new(
            user_key,
            organization_keys,
            sync.ciphers,
            sync.trashed,
            sync.folders,
        );
        let count = vault.items().len();
        self.vault = Some(vault);
        self.access_token = Some(Zeroizing::new(token.access_token));
        self.refresh_token = token.refresh_token.map(Zeroizing::new);
        self.mark_synced();
        Ok(count)
    }

    /// The ONE writer of [`VaultManager::last_sync`]. Every path that replaces
    /// the in-memory ciphers ends here, so no caller can refresh the vault
    /// without refreshing the fact that says how fresh it is.
    fn mark_synced(&mut self) {
        self.last_sync = Some(std::time::SystemTime::now());
    }

    /// When the ciphers were last pulled, as seconds since the unix epoch.
    /// `None` when the vault is locked or has never synced.
    pub fn last_sync_unix(&self) -> Option<u64> {
        self.last_sync
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs())
    }

    /// Test-only: install an already-decrypted vault, so the agent's op layer
    /// can be exercised without a server or a master password.
    #[cfg(test)]
    pub(crate) fn install_vault_for_test(&mut self, vault: Vault) {
        self.vault = Some(vault);
        // A test vault is "as fresh as it will ever be": the agent tests read
        // `status`, and a `None` here would make them assert against a state no
        // unlocked vault can actually be in.
        self.mark_synced();
    }

    /// Everything a successor process needs to keep THIS unlock alive: the user
    /// key and the two bearer tokens. `None` when the vault is locked — there is
    /// nothing to hand over then, and `stop-agent` is the right verb.
    ///
    /// Deliberately tiny, and that is the whole argument for the handover: the
    /// ciphers, the org keys and the folder names are all re-derived by
    /// [`VaultManager::resync`] from the user key and the bearer alone, so the
    /// payload is a few hundred bytes rather than the ~7 MB of encrypted
    /// ciphers this agent is holding.
    pub(crate) fn export_session(&self) -> Option<SessionMaterial> {
        Some(SessionMaterial {
            user_key: self.vault.as_ref()?.user_key().to_bytes(),
            access_token: self.access_token.clone()?,
            refresh_token: self.refresh_token.clone(),
        })
    }

    /// Restore a session exported by the process this one replaced, then re-pull
    /// the ciphers. Returns the item count, exactly like `unlock`.
    ///
    /// The ciphers are NOT carried across: `resync` fetches them with the bearer
    /// token, which is what keeps the handover payload small enough to fit in a
    /// pipe. That does mean a successor needs the network for one call — the
    /// outgoing agent proves the token still works before it execs, so this
    /// failing means the network went away between the two.
    ///
    /// The keys are installed BEFORE the resync on purpose. A failed resync then
    /// leaves the vault unlocked and empty, recoverable with `ychrome-vault
    /// sync`, rather than throwing away an unlock the user would have to retype.
    pub(crate) fn adopt_session(&mut self, material: SessionMaterial) -> Result<usize, VaultError> {
        let user_key = crate::crypto::SymmetricKey::from_bytes(&material.user_key[..])?;
        self.vault = Some(Vault::new(
            user_key,
            Default::default(),
            Vec::new(),
            Vec::new(),
            Default::default(),
        ));
        self.access_token = Some(material.access_token);
        self.refresh_token = material.refresh_token;
        self.resync()
    }

    /// Drop the in-memory vault and its bearer token (keys zeroize). Config
    /// is kept.
    pub fn lock(&mut self) {
        self.vault = None;
        self.access_token = None;
        self.refresh_token = None;
        // A locked vault holds no ciphers, so it has no freshness to report.
        // Leaving the old stamp would let the pane say "synced 3 minutes ago"
        // about nothing.
        self.last_sync = None;
    }

    /// Run an authenticated server call, renewing the bearer ONCE if the server
    /// says it has expired.
    ///
    /// Every write used to fail permanently when the access token aged out,
    /// because reads are served from the decrypted in-memory vault and never
    /// touch the server — so nothing noticed until a write 401'd, and the only
    /// recovery was re-typing the master password. Reported 2026-07-25, with a
    /// credential parked in a plaintext file as the workaround.
    ///
    /// Retry is exactly once: a second 401 means the fresh token is not the
    /// problem, and looping would turn an auth failure into a hang.
    fn authenticated<T>(
        &mut self,
        call: impl Fn(&Client, &str) -> Result<T, ApiError>,
    ) -> Result<T, VaultError> {
        let config = self.config.clone().ok_or(VaultError::NotConfigured)?;
        let client = Client::new(&config.server_url)?;
        let token = self.access_token.clone().ok_or(VaultError::Locked)?;
        match call(&client, &token) {
            Err(ApiError::Http { status: 401, .. }) => {
                self.renew_access_token(&client)?;
                let token = self.access_token.clone().ok_or(VaultError::Locked)?;
                call(&client, &token).map_err(VaultError::from)
            }
            other => other.map_err(VaultError::from),
        }
    }

    /// Trade the stored refresh token for a fresh access token, in place.
    ///
    /// No refresh token (an unlock against a server that issued none) is
    /// reported as `Locked`: from the caller's point of view the session can no
    /// longer act, and the fix is the same — unlock again.
    fn renew_access_token(&mut self, client: &Client) -> Result<(), VaultError> {
        let refresh = self.refresh_token.clone().ok_or(VaultError::Locked)?;
        let renewed = client.refresh(&refresh)?;
        self.access_token = Some(Zeroizing::new(renewed.access_token));
        // The server ROTATES the refresh token; keeping the old one would make
        // the next renewal fail. Absent means "keep using the one we have".
        if let Some(rotated) = renewed.refresh_token {
            self.refresh_token = Some(Zeroizing::new(rotated));
        }
        Ok(())
    }

    /// Create a login in the vault and re-sync so the new item is immediately
    /// visible. Encryption happens locally under the user key; the server only
    /// ever sees EncStrings. Returns the new cipher's id.
    pub fn add_login(&mut self, login: &crate::model::NewLogin) -> Result<String, VaultError> {
        let body = {
            let vault = self.vault.as_ref().ok_or(VaultError::Locked)?;
            vault.new_login_body(login)?
        };
        let id = self.authenticated(|client, token| client.create_cipher(token, &body))?;
        self.resync()?;
        Ok(id)
    }

    /// Store a freshly minted passkey as a new login and re-sync. The private
    /// key is sealed under the user key by [`Vault::new_passkey_login_body`]; the
    /// server only ever sees ciphertext. Returns the new item id.
    ///
    /// [`Vault::new_passkey_login_body`]: crate::model::Vault::new_passkey_login_body
    pub fn add_passkey_login(
        &mut self,
        passkey: &crate::model::NewPasskey,
    ) -> Result<String, VaultError> {
        let body = {
            let vault = self.vault.as_ref().ok_or(VaultError::Locked)?;
            vault.new_passkey_login_body(passkey)?
        };
        let id = self.authenticated(|client, token| client.create_cipher(token, &body))?;
        self.resync()?;
        Ok(id)
    }

    /// Patch an existing item and re-sync. Only the fields named in `edit`
    /// change; everything else on the cipher — including what this client does
    /// not model — is carried back verbatim by [`Vault::edit_body`].
    ///
    /// If the server's copy has moved on since our last sync, the write is
    /// REFUSED (`lastKnownRevisionDate`) rather than clobbering the other
    /// client's change. Run `sync` and retry.
    ///
    /// [`Vault::edit_body`]: crate::model::Vault::edit_body
    /// Returns what the RE-READ confirmed, and refuses an edit it cannot see.
    ///
    /// ⛔ A 200 FROM `PUT` IS NOT PROOF THE FIELD LANDED. It says the server
    /// accepted a body. Whether the item now holds what the user asked for is a
    /// different question, and the only honest way to answer it is to look:
    /// `resync` re-pulls the cipher and [`Vault::verify_edit`] decrypts it back.
    /// Reporting success without that is the lie-of-success shape, and it is the
    /// one this crate treats as worse than an outright failure.
    pub fn edit_item(
        &mut self,
        id: &str,
        edit: &crate::model::CipherEdit,
    ) -> Result<crate::model::EditVerification, VaultError> {
        let body = {
            let vault = self.vault.as_ref().ok_or(VaultError::Locked)?;
            vault.edit_body(id, edit)?
        };
        self.authenticated(|client, token| client.update_cipher(token, id, &body))?;
        self.resync()?;
        let verification = self
            .vault
            .as_ref()
            .ok_or(VaultError::Locked)?
            .verify_edit(id, edit);
        if !verification.is_complete() {
            return Err(VaultError::EditNotVerified(verification.missing.join(", ")));
        }
        Ok(verification)
    }

    /// Delete an item and re-sync.
    ///
    /// `permanent == false` (the default everywhere above this) moves it to the
    /// vault's trash, where any Bitwarden client can restore it. `permanent ==
    /// true` destroys it outright, with no trash copy and no undo.
    pub fn remove_item(&mut self, id: &str, permanent: bool) -> Result<(), VaultError> {
        if self.vault.is_none() {
            return Err(VaultError::Locked);
        }
        self.authenticated(|client, token| client.delete_cipher(token, id, permanent))?;
        self.resync()?;
        Ok(())
    }

    /// Restore a soft-deleted item from the trash and re-sync, so it reappears
    /// in the live item list. The item must still be in the trash — a
    /// hard-deleted one is gone and the server refuses. This is the exact
    /// inverse of a soft [`remove_item`].
    ///
    /// [`remove_item`]: VaultManager::remove_item
    pub fn restore_item(&mut self, id: &str) -> Result<(), VaultError> {
        if self.vault.is_none() {
            return Err(VaultError::Locked);
        }
        self.authenticated(|client, token| client.restore_cipher(token, id))?;
        self.resync()?;
        Ok(())
    }

    /// Re-pull the ciphers with the session's bearer token, keeping the same
    /// user key. The master password is NOT needed — that is the whole point
    /// of holding the token: an agent can refresh a long-lived unlock.
    pub fn resync(&mut self) -> Result<usize, VaultError> {
        if self.vault.is_none() {
            return Err(VaultError::Locked);
        }
        let sync = self.authenticated(|client, token| client.sync(token))?;
        // Org membership can change between syncs, so the org keys are
        // re-unwrapped rather than carried over.
        let user_key = self.vault.as_ref().expect("checked").user_key().clone();
        let organization_keys = unwrap_organization_keys(&user_key, &sync)?;
        let vault = self.vault.as_mut().expect("checked");
        vault.replace_contents(organization_keys, sync.ciphers, sync.trashed, sync.folders);
        let count = vault.items().len();
        self.mark_synced();
        Ok(count)
    }

    fn persist(&self, config: &VaultConfig) -> Result<(), VaultError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| VaultError::Io(e.to_string()))?;
        let path = self.dir.join("config.json");
        let tmp = self.dir.join("config.json.tmp");
        let json = serde_json::to_vec_pretty(config).map_err(|e| VaultError::Io(e.to_string()))?;
        std::fs::write(&tmp, &json).map_err(|e| VaultError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| VaultError::Io(e.to_string()))?;
        Ok(())
    }
}

/// Decrypt the user's RSA private key with the user key, then unwrap each
/// organization's symmetric key with it.
///
/// A failure to unwrap ONE org is not fatal: that org's ciphers stay
/// undecryptable and `Vault::diagnose` counts them, which is strictly better
/// than refusing to open the whole vault. An account in no orgs never touches
/// RSA at all.
fn unwrap_organization_keys(
    user_key: &crate::crypto::SymmetricKey,
    sync: &crate::api::SyncResponse,
) -> Result<std::collections::HashMap<String, crate::crypto::SymmetricKey>, VaultError> {
    if sync.organization_keys.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let Some(encrypted_private_key) = &sync.private_key else {
        return Ok(std::collections::HashMap::new());
    };
    let der = user_key.decrypt(encrypted_private_key)?;
    let private_key = crate::crypto::PrivateKey::from_pkcs8_der(&der)?;

    let mut keys = std::collections::HashMap::new();
    for (id, sealed) in &sync.organization_keys {
        if let Ok(raw) = private_key.decrypt(sealed)
            && let Ok(key) = crate::crypto::SymmetricKey::from_bytes(&raw)
        {
            keys.insert(id.clone(), key);
        }
    }
    Ok(keys)
}

/// A random RFC-4122 v4 device identifier (Bitwarden wants a stable per-device
/// UUID). Generated once and persisted in the config.
fn new_device_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1
    let h = |slice: &[u8]| slice.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(&bytes[0..4]),
        h(&bytes[4..6]),
        h(&bytes[6..8]),
        h(&bytes[8..10]),
        h(&bytes[10..16]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-shot loopback stand-in for the identity endpoint. Returns the
    /// captured request body so the test can assert the GRANT we actually send,
    /// not just that something was sent.
    fn spawn_refresh_server(
        response: &'static str,
        status: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buffer[..read]).into_owned());
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();
                return;
            }
        });
        (url, rx)
    }

    fn manager_with_tokens(
        server_url: String,
        access: &str,
        refresh: Option<&str>,
    ) -> VaultManager {
        let dir = std::env::temp_dir().join(format!("yggvault-auth-{}", new_device_id()));
        let mut manager = VaultManager::load(&dir);
        manager.config = Some(VaultConfig {
            server_url,
            email: "someone@example.com".into(),
            kdf_type: 0,
            kdf_iterations: 600_000,
            kdf_memory: None,
            kdf_parallelism: None,
            device_id: new_device_id(),
            lock_timeout_secs: 0,
        });
        manager.access_token = Some(Zeroizing::new(access.to_string()));
        manager.refresh_token = refresh.map(|token| Zeroizing::new(token.to_string()));
        manager
    }

    /// THE BUG THIS FIXES: writes died permanently when the access token aged
    /// out, because reads never touch the server and so nothing noticed. The
    /// only recovery was re-typing the master password — which is why a
    /// credential ended up in a plaintext file on 2026-07-25.
    #[test]
    fn an_expired_access_token_is_renewed_and_the_call_retried_once() {
        let (url, requests) = spawn_refresh_server(
            r#"{"access_token":"fresh-access","refresh_token":"rotated-refresh"}"#,
            "200 OK",
        );
        let mut manager = manager_with_tokens(url, "stale-access", Some("old-refresh"));

        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let result: Result<&str, VaultError> = manager.authenticated(|_client, token| {
            calls.borrow_mut().push(token.to_string());
            if token == "stale-access" {
                return Err(ApiError::Http {
                    status: 401,
                    body: "expired".into(),
                });
            }
            Ok("wrote")
        });

        assert_eq!(result.unwrap(), "wrote");
        assert_eq!(
            calls.into_inner(),
            vec!["stale-access".to_string(), "fresh-access".to_string()],
            "the call must be retried with the RENEWED token, not the stale one"
        );
        // The server rotates the refresh token; keeping the old one would make
        // the NEXT renewal fail.
        assert_eq!(
            manager.refresh_token.as_deref().map(String::as_str),
            Some("rotated-refresh")
        );

        let body = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(body.contains("grant_type=refresh_token"), "sent: {body}");
        assert!(
            body.contains("old-refresh"),
            "must spend the STORED refresh token: {body}"
        );
    }

    /// One retry, not a loop: a second 401 means the fresh token is not the
    /// problem, and retrying forever would turn an auth failure into a hang.
    #[test]
    fn a_call_that_401s_again_after_renewal_gives_up() {
        let (url, _requests) = spawn_refresh_server(
            r#"{"access_token":"fresh-access","refresh_token":"rotated"}"#,
            "200 OK",
        );
        let mut manager = manager_with_tokens(url, "stale", Some("refresh"));
        let attempts = std::cell::Cell::new(0usize);
        let result: Result<(), VaultError> = manager.authenticated(|_client, _token| {
            attempts.set(attempts.get() + 1);
            Err(ApiError::Http {
                status: 401,
                body: "still expired".into(),
            })
        });
        assert!(result.is_err());
        assert_eq!(attempts.get(), 2, "exactly one retry");
    }

    /// A revoked refresh token is a genuine "sign in again", and must say so
    /// rather than surfacing as a bare HTTP error.
    #[test]
    fn a_rejected_refresh_token_reports_that_re_authentication_is_needed() {
        let (url, _requests) =
            spawn_refresh_server(r#"{"error":"invalid_grant"}"#, "400 Bad Request");
        let mut manager = manager_with_tokens(url, "stale", Some("revoked"));
        let result: Result<(), VaultError> = manager.authenticated(|_client, _token| {
            Err(ApiError::Http {
                status: 401,
                body: "expired".into(),
            })
        });
        match result {
            Err(VaultError::Api(ApiError::RefreshRejected)) => {}
            other => panic!("expected RefreshRejected, got {other:?}"),
        }
    }

    /// An unlock against a server that issued no refresh token cannot renew.
    /// Reported as `Locked` because the remedy is the same: unlock again.
    #[test]
    fn without_a_refresh_token_the_session_reports_locked() {
        let (url, _requests) = spawn_refresh_server("{}", "200 OK");
        let mut manager = manager_with_tokens(url, "stale", None);
        let result: Result<(), VaultError> = manager.authenticated(|_client, _token| {
            Err(ApiError::Http {
                status: 401,
                body: "expired".into(),
            })
        });
        assert!(matches!(result, Err(VaultError::Locked)));
    }

    /// Locking must drop BOTH bearers. A refresh token outliving `lock` would
    /// let a later call silently re-open the session.
    #[test]
    fn lock_drops_the_refresh_token_too() {
        let mut manager = manager_with_tokens("http://127.0.0.1:1".into(), "a", Some("r"));
        manager.lock();
        assert!(manager.access_token.is_none());
        assert!(
            manager.refresh_token.is_none(),
            "a surviving refresh token re-opens the vault"
        );
    }

    #[test]
    fn device_id_is_uuid_v4_shaped() {
        let id = new_device_id();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert_eq!(&parts[2][0..1], "4", "version nibble");
        assert_ne!(new_device_id(), new_device_id(), "ids are random");
    }

    #[test]
    fn config_round_trips_and_status_reflects_state() {
        let dir = std::env::temp_dir().join(format!("yggvault-test-{}", new_device_id()));
        let mgr = VaultManager::load(&dir);
        assert_eq!(mgr.status(), VaultStatus::NotConfigured);
        assert!(!mgr.is_configured());

        // Persist a config directly (no network) and reload.
        let config = VaultConfig {
            server_url: "https://vault.example.com".into(),
            email: "a@example.com".into(),
            kdf_type: 0,
            kdf_iterations: 600_000,
            kdf_memory: None,
            kdf_parallelism: None,
            device_id: new_device_id(),
            lock_timeout_secs: DEFAULT_LOCK_TIMEOUT_SECS,
        };
        mgr.persist(&config).unwrap();
        let reloaded = VaultManager::load(&dir);
        assert!(reloaded.is_configured());
        assert_eq!(
            reloaded.status(),
            VaultStatus::Locked {
                email: "a@example.com".into(),
                server_url: "https://vault.example.com".into()
            }
        );
        assert!(!reloaded.is_unlocked());
        std::fs::remove_dir_all(&dir).ok();
    }
}
