//! The decrypted vault held in memory after unlock.
//!
//! The metadata list ([`VaultItem`]) never carries a password or TOTP secret;
//! those are decrypted on demand per item, so a screenshot or a leaked UI state
//! cannot spill them. Item-level keys are resolved exactly as Bitwarden does: a
//! cipher may carry its own `key` (encrypted under the user key), and its fields
//! are then encrypted under that item key rather than the user key directly.

use std::collections::HashMap;

use crate::crypto::{CryptoError, EncString, SymmetricKey};
use crate::totp::Totp;

/// A cipher as it arrives from `sync`, with its fields still encrypted.
#[derive(Debug, Clone, Default)]
pub struct RawCipher {
    pub id: String,
    pub folder_id: Option<String>,
    pub item_type: u8,
    pub key: Option<EncString>,
    pub name: Option<EncString>,
    pub username: Option<EncString>,
    pub password: Option<EncString>,
    pub totp: Option<EncString>,
    pub uris: Vec<EncString>,
}

/// Decrypted, secret-free metadata for one vault item.
#[derive(Debug, Clone)]
pub struct VaultItem {
    pub id: String,
    pub name: String,
    pub username: Option<String>,
    pub folder: Option<String>,
    pub uris: Vec<String>,
    pub has_password: bool,
    pub has_totp: bool,
}

/// The unlocked vault: the user key plus the still-encrypted ciphers. Secrets
/// are decrypted only when asked for.
pub struct Vault {
    user_key: SymmetricKey,
    ciphers: Vec<RawCipher>,
    folder_names: HashMap<String, EncString>,
}

impl Vault {
    pub fn new(
        user_key: SymmetricKey,
        ciphers: Vec<RawCipher>,
        folders: HashMap<String, EncString>,
    ) -> Self {
        Vault {
            user_key,
            ciphers,
            folder_names: folders,
        }
    }

    /// The key that decrypts a cipher's fields: its own item key if present,
    /// else the user key.
    fn cipher_key(&self, cipher: &RawCipher) -> Result<SymmetricKey, CryptoError> {
        match &cipher.key {
            Some(item_key) => {
                let raw = self.user_key.decrypt(item_key)?;
                SymmetricKey::from_bytes(&raw)
            }
            None => Ok(self.user_key.clone()),
        }
    }

    fn folder_name(&self, cipher: &RawCipher) -> Option<String> {
        let id = cipher.folder_id.as_ref()?;
        let enc = self.folder_names.get(id)?;
        self.user_key.decrypt_to_string(enc).ok()
    }

    /// The secret-free item list. A cipher that fails to decrypt (corrupt, or a
    /// type we do not model) is skipped rather than aborting the whole vault.
    pub fn items(&self) -> Vec<VaultItem> {
        self.ciphers
            .iter()
            .filter_map(|cipher| {
                let key = self.cipher_key(cipher).ok()?;
                let name = cipher
                    .name
                    .as_ref()
                    .and_then(|enc| key.decrypt_to_string(enc).ok())?;
                let username = cipher
                    .username
                    .as_ref()
                    .and_then(|enc| key.decrypt_to_string(enc).ok());
                let uris = cipher
                    .uris
                    .iter()
                    .filter_map(|enc| key.decrypt_to_string(enc).ok())
                    .collect();
                Some(VaultItem {
                    id: cipher.id.clone(),
                    name,
                    username,
                    folder: self.folder_name(cipher),
                    uris,
                    has_password: cipher.password.is_some(),
                    has_totp: cipher.totp.is_some(),
                })
            })
            .collect()
    }

    fn find(&self, id: &str) -> Option<&RawCipher> {
        self.ciphers.iter().find(|cipher| cipher.id == id)
    }

    /// Decrypt a specific item's password. `None` if the item is unknown or has
    /// no password.
    pub fn password(&self, id: &str) -> Option<String> {
        let cipher = self.find(id)?;
        let enc = cipher.password.as_ref()?;
        let key = self.cipher_key(cipher).ok()?;
        key.decrypt_to_string(enc).ok()
    }

    /// The current TOTP code for a specific item, with the seconds until it
    /// rolls. `None` if the item is unknown or carries no authenticator secret.
    pub fn totp_code(&self, id: &str) -> Option<(String, u64)> {
        let cipher = self.find(id)?;
        let enc = cipher.totp.as_ref()?;
        let key = self.cipher_key(cipher).ok()?;
        let secret = key.decrypt_to_string(enc).ok()?;
        Totp::parse(&secret).ok().map(|totp| totp.now())
    }

    pub fn len(&self) -> usize {
        self.ciphers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ciphers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::Aes256;
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    // Encrypt a plaintext into a type-2 EncString under a raw 64-byte key,
    // exactly as a Bitwarden client would, so the model can be tested with no
    // network and no real vault.
    fn seal(user_key_bytes: &[u8; 64], plaintext: &str) -> EncString {
        type Enc = cbc::Encryptor<Aes256>;
        let enc_key: [u8; 32] = user_key_bytes[..32].try_into().unwrap();
        let mac_key = &user_key_bytes[32..];
        let iv = [0x24u8; 16];
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(plaintext.as_bytes());
        let ct = Enc::new(&enc_key.into(), &iv.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap()
            .to_vec();
        let mut m = <Hmac<Sha256>>::new_from_slice(mac_key).unwrap();
        m.update(&iv);
        m.update(&ct);
        let mac = m.finalize().into_bytes().to_vec();
        EncString::parse(&format!(
            "2.{}|{}|{}",
            B64.encode(iv),
            B64.encode(&ct),
            B64.encode(&mac)
        ))
        .unwrap()
    }

    #[test]
    fn decrypts_items_and_secrets_on_demand() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();

        let mut folders = HashMap::new();
        folders.insert("f1".to_string(), seal(&key_bytes, "Work"));

        let cipher = RawCipher {
            id: "c1".to_string(),
            folder_id: Some("f1".to_string()),
            item_type: 1,
            key: None,
            name: Some(seal(&key_bytes, "GitHub")),
            username: Some(seal(&key_bytes, "octocat")),
            password: Some(seal(&key_bytes, "s3cret!")),
            totp: Some(seal(&key_bytes, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")),
            uris: vec![seal(&key_bytes, "https://github.com")],
        };
        let vault = Vault::new(user_key, vec![cipher], folders);

        let items = vault.items();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.name, "GitHub");
        assert_eq!(item.username.as_deref(), Some("octocat"));
        assert_eq!(item.folder.as_deref(), Some("Work"));
        assert_eq!(item.uris, vec!["https://github.com"]);
        assert!(item.has_password && item.has_totp);

        // Secrets are NOT in the metadata; they decrypt on demand.
        assert_eq!(vault.password("c1").as_deref(), Some("s3cret!"));
        let (code, remaining) = vault.totp_code("c1").unwrap();
        assert_eq!(code.len(), 6);
        assert!(remaining >= 1 && remaining <= 30);
        assert!(vault.password("nope").is_none());
    }

    // An item with its OWN key: fields are encrypted under the item key, which
    // is itself encrypted under the user key.
    #[test]
    fn resolves_item_level_key() {
        let user_bytes = [0x11u8; 64];
        let item_bytes = [0x77u8; 64];
        let user_key = SymmetricKey::from_bytes(&user_bytes).unwrap();

        // The item key is 64 raw bytes, sealed under the user key.
        let sealed_item_key = {
            // Reuse `seal` by treating the raw key bytes as a latin-1 string is
            // wrong (non-UTF8); instead encrypt the raw bytes directly.
            type Enc = cbc::Encryptor<Aes256>;
            let enc_key: [u8; 32] = user_bytes[..32].try_into().unwrap();
            let iv = [0x31u8; 16];
            let mut buf = vec![0u8; item_bytes.len() + 16];
            buf[..item_bytes.len()].copy_from_slice(&item_bytes);
            let ct = Enc::new(&enc_key.into(), &iv.into())
                .encrypt_padded_mut::<Pkcs7>(&mut buf, item_bytes.len())
                .unwrap()
                .to_vec();
            let mut m = <Hmac<Sha256>>::new_from_slice(&user_bytes[32..]).unwrap();
            m.update(&iv);
            m.update(&ct);
            EncString::parse(&format!(
                "2.{}|{}|{}",
                B64.encode(iv),
                B64.encode(&ct),
                B64.encode(m.finalize().into_bytes())
            ))
            .unwrap()
        };

        let cipher = RawCipher {
            id: "c1".to_string(),
            item_type: 1,
            key: Some(sealed_item_key),
            name: Some(seal(&item_bytes, "Sealed Item")),
            password: Some(seal(&item_bytes, "under-item-key")),
            ..Default::default()
        };
        let vault = Vault::new(user_key, vec![cipher], HashMap::new());
        assert_eq!(vault.items()[0].name, "Sealed Item");
        assert_eq!(vault.password("c1").as_deref(), Some("under-item-key"));
    }
}
