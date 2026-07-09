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

/// Decrypted, secret-free metadata for one vault item. Serializable because
/// the agent hands this list to clients — it carries no password and no TOTP
/// secret, only the booleans saying one exists.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VaultItem {
    pub id: String,
    pub name: String,
    pub username: Option<String>,
    pub folder: Option<String>,
    pub uris: Vec<String>,
    pub has_password: bool,
    pub has_totp: bool,
}

/// A login to create. Plaintext — it is encrypted by [`Vault::new_login_body`]
/// and never leaves this process in the clear.
#[derive(Debug, Clone, Default)]
pub struct NewLogin {
    pub name: String,
    pub username: Option<String>,
    pub password: Option<String>,
    /// An authenticator secret (base32) or a full `otpauth://` URI.
    pub totp: Option<String>,
    pub uri: Option<String>,
    pub notes: Option<String>,
    pub folder_id: Option<String>,
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

    /// Build the `POST /api/ciphers` body for a new login, encrypting every
    /// field under the user key. A newly created cipher carries no item key,
    /// so the user key is the cipher key — exactly what [`cipher_key`] will
    /// resolve when the item comes back on the next sync.
    ///
    /// Only the fields we model are emitted. That is safe for CREATE (there is
    /// nothing to lose) and is why there is no `update` counterpart: a PUT
    /// rebuilt from this struct would silently drop the notes, custom fields,
    /// favorite flag and password history that `sync` does not parse.
    ///
    /// [`cipher_key`]: Vault::cipher_key
    pub fn new_login_body(&self, login: &NewLogin) -> Result<serde_json::Value, CryptoError> {
        let enc = |value: &str| -> Result<String, CryptoError> {
            Ok(self.user_key.encrypt_string(value)?.to_string())
        };
        let enc_opt = |value: &Option<String>| -> Result<serde_json::Value, CryptoError> {
            match value.as_deref().filter(|value| !value.is_empty()) {
                Some(value) => Ok(serde_json::Value::String(enc(value)?)),
                None => Ok(serde_json::Value::Null),
            }
        };
        let uris = match login.uri.as_deref().filter(|uri| !uri.is_empty()) {
            Some(uri) => serde_json::json!([{ "uri": enc(uri)?, "match": serde_json::Value::Null }]),
            None => serde_json::json!([]),
        };
        Ok(serde_json::json!({
            "type": 1,
            "name": enc(&login.name)?,
            "notes": enc_opt(&login.notes)?,
            "favorite": false,
            "folderId": login.folder_id,
            "reprompt": 0,
            "fields": [],
            "login": {
                "username": enc_opt(&login.username)?,
                "password": enc_opt(&login.password)?,
                "totp": enc_opt(&login.totp)?,
                "uris": uris,
            },
        }))
    }

    /// Swap in a freshly synced cipher set, keeping the same user key. Used by
    /// `VaultManager::resync`, which refreshes an unlocked vault with the
    /// session's bearer token rather than the master password.
    pub fn replace_contents(
        &mut self,
        ciphers: Vec<RawCipher>,
        folders: HashMap<String, EncString>,
    ) {
        self.ciphers = ciphers;
        self.folder_names = folders;
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

/// Encrypt bytes into a type-2 EncString under a raw 64-byte key, exactly as a
/// Bitwarden client would. Test-only: it lets the model — and the agent's whole
/// op layer — be exercised against a genuinely sealed vault with no network, no
/// server, and no master password.
#[cfg(test)]
pub(crate) fn seal(user_key_bytes: &[u8; 64], plaintext: &[u8]) -> EncString {
    use aes::Aes256;
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type Enc = cbc::Encryptor<Aes256>;
    let enc_key: [u8; 32] = user_key_bytes[..32].try_into().unwrap();
    let mac_key = &user_key_bytes[32..];
    let iv = [0x24u8; 16];
    let mut buf = vec![0u8; plaintext.len() + 16];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let ct = Enc::new(&enc_key.into(), &iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
        .unwrap()
        .to_vec();
    let mut mac = <Hmac<Sha256>>::new_from_slice(mac_key).unwrap();
    mac.update(&iv);
    mac.update(&ct);
    EncString::parse(&format!(
        "2.{}|{}|{}",
        B64.encode(iv),
        B64.encode(&ct),
        B64.encode(mac.finalize().into_bytes())
    ))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal(user_key_bytes: &[u8; 64], plaintext: &str) -> EncString {
        super::seal(user_key_bytes, plaintext.as_bytes())
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

    // What we WRITE must be what we can READ. Every field of a create body is
    // an EncString under the user key, no plaintext leaks into the JSON, and
    // an absent field is null rather than an EncString of "".
    #[test]
    fn new_login_body_encrypts_every_field_and_reads_back() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let vault = Vault::new(user_key.clone(), vec![], HashMap::new());

        let body = vault
            .new_login_body(&NewLogin {
                name: "example.com".to_string(),
                username: Some("alice".to_string()),
                password: Some("hunter2".to_string()),
                uri: Some("https://example.com".to_string()),
                totp: None,
                notes: None,
                folder_id: None,
            })
            .unwrap();

        let decrypt = |value: &serde_json::Value| {
            let enc = EncString::parse(value.as_str().unwrap()).unwrap();
            user_key.decrypt_to_string(&enc).unwrap()
        };
        assert_eq!(body["type"], 1);
        assert_eq!(decrypt(&body["name"]), "example.com");
        assert_eq!(decrypt(&body["login"]["username"]), "alice");
        assert_eq!(decrypt(&body["login"]["password"]), "hunter2");
        assert_eq!(decrypt(&body["login"]["uris"][0]["uri"]), "https://example.com");
        // Fields the user left out are null, not an encrypted empty string.
        assert!(body["login"]["totp"].is_null());
        assert!(body["notes"].is_null());

        // No plaintext anywhere in the serialized request.
        let wire = body.to_string();
        for secret in ["hunter2", "alice", "example.com"] {
            assert!(!wire.contains(secret), "{secret} leaked into the request body");
        }
    }

    // An empty uri must not produce a uris entry at all.
    #[test]
    fn new_login_body_omits_an_empty_uri() {
        let user_key = SymmetricKey::from_bytes(&[0x11u8; 64]).unwrap();
        let vault = Vault::new(user_key, vec![], HashMap::new());
        let body = vault
            .new_login_body(&NewLogin {
                name: "bare".to_string(),
                uri: Some(String::new()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(body["login"]["uris"].as_array().unwrap().len(), 0);
    }

    // An item with its OWN key: fields are encrypted under the item key, which
    // is itself encrypted under the user key.
    #[test]
    fn resolves_item_level_key() {
        let user_bytes = [0x11u8; 64];
        let item_bytes = [0x77u8; 64];
        let user_key = SymmetricKey::from_bytes(&user_bytes).unwrap();

        // The item key is 64 raw (non-UTF8) bytes, sealed under the user key.
        let sealed_item_key = super::seal(&user_bytes, &item_bytes);

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
