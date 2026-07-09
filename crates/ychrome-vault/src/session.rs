//! Configuration and the unlock lifecycle.
//!
//! Persisted to disk: the server URL, email, KDF parameters, and a random
//! device identifier — never the master password, the master key, or the user
//! key. Unlocking derives the keys, logs in, syncs, and holds the decrypted
//! [`Vault`] in memory for the life of the process. Locking drops it.

use std::path::PathBuf;

use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, Client};
use crate::crypto::{Kdf, MasterKey};
use crate::model::Vault;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("the vault is not configured yet")]
    NotConfigured,
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
    #[error("config storage: {0}")]
    Io(String),
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
    Locked { email: String, server_url: String },
    Unlocked { email: String, item_count: usize },
}

/// Owns the vault config and the unlocked session. One per GUI process.
pub struct VaultManager {
    dir: PathBuf,
    config: Option<VaultConfig>,
    vault: Option<Vault>,
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
        }
    }

    pub fn status(&self) -> VaultStatus {
        match (&self.config, &self.vault) {
            (Some(config), Some(vault)) => VaultStatus::Unlocked {
                email: config.email.clone(),
                item_count: vault.len(),
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
        };
        self.persist(&config)?;
        self.config = Some(config);
        self.vault = None;
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

        let (ciphers, folders) = client.sync(&token.access_token)?;
        let vault = Vault::new(user_key, ciphers, folders);
        let count = vault.len();
        self.vault = Some(vault);
        Ok(count)
    }

    /// Drop the in-memory vault (keys zeroize). Config is kept.
    pub fn lock(&mut self) {
        self.vault = None;
    }

    /// Re-sync an already-unlocked vault by re-deriving from the master
    /// password. (Refresh-token reuse is a later optimization.)
    pub fn resync(&mut self, master_password: &str) -> Result<usize, VaultError> {
        self.unlock(master_password)
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

    #[test]
    fn device_id_is_uuid_v4_shaped() {
        let id = new_device_id();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);
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
