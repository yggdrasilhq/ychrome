//! A self-reliant Bitwarden/Vaultwarden client for ychrome's vault sidebar.
//!
//! This crate replaces shelling out to the `rbw` CLI. It talks to a Vaultwarden
//! (or Bitwarden) server directly — `prelogin` for the KDF parameters, the
//! identity token endpoint to log in, `sync` to pull the vault — and does the
//! EncString crypto to decrypt items and generate TOTP codes.
//!
//! It is used ONLY by the GUI (`yggterm-shell`) that renders the vault sidebar.
//! The terminal daemon never depends on it, and no key material is ever written
//! to disk: the master password unlocks an in-memory user key for the life of
//! the process, and only a device identifier + refresh token are persisted.

pub mod api;
pub mod crypto;
pub mod model;
pub mod session;
pub mod totp;

pub use crypto::{CryptoError, EncString, Kdf, MasterKey, SymmetricKey};
pub use model::{RawCipher, Vault, VaultItem};
pub use session::{VaultConfig, VaultError, VaultManager, VaultStatus};
pub use totp::{Totp, TotpError};
