//! A self-reliant Bitwarden/Vaultwarden client for ychrome's vault sidebar.
//!
//! This crate replaces shelling out to the `rbw` CLI. It talks to a Vaultwarden
//! (or Bitwarden) server directly — `prelogin` for the KDF parameters, the
//! identity token endpoint to log in, `sync` to pull the vault — and does the
//! EncString crypto to decrypt items and generate TOTP codes.
//!
//! It is used by ychrome — the `ychrome-vault` CLI, the unlock-caching
//! [`agent`], and the sidebar ychrome contributes to the yggterm GUI. The
//! yggterm terminal daemon never depends on it, and no key material is ever
//! written to disk: the master password unlocks an in-memory user key held by
//! the agent, and only secret-free configuration is persisted.

pub mod agent;
pub mod api;
pub mod crypto;
pub mod generator;
pub mod matching;
pub mod model;
pub mod session;
pub mod totp;
pub mod watchtower;

pub use crypto::{AsymEncString, CryptoError, EncString, Kdf, MasterKey, PrivateKey, SymmetricKey};
pub use generator::{DEFAULT_LENGTH, MIN_LENGTH, generate_password};
pub use matching::{auto_match_for_host, find_by_name, item_applies_to_host, item_auto_matches_host};
pub use model::{NewLogin, RawCipher, Vault, VaultDiagnostic, VaultItem};
pub use session::{
    DEFAULT_LOCK_TIMEOUT_SECS, VaultConfig, VaultError, VaultManager, VaultStatus,
};
pub use totp::{Totp, TotpError};
pub use watchtower::Report as WatchtowerReport;
