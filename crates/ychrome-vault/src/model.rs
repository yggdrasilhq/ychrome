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
    /// The untouched JSON record from `sync`. An update PUT replaces the whole
    /// cipher, so this — not the parsed fields below — is what an edit patches.
    /// Without it, notes, custom fields, favorite and password history would be
    /// silently destroyed by every edit.
    pub raw: serde_json::Value,
    pub id: String,
    pub folder_id: Option<String>,
    /// Set when the cipher belongs to an organization. Its fields are then
    /// encrypted under that ORG's key, not the user key — see [`Vault::diagnose`].
    pub organization_id: Option<String>,
    pub item_type: u8,
    pub key: Option<EncString>,
    pub name: Option<EncString>,
    pub username: Option<EncString>,
    pub password: Option<EncString>,
    pub totp: Option<EncString>,
    pub uris: Vec<EncString>,
    /// The item's stored passkeys (`login.fido2Credentials[]`), fields still
    /// encrypted. Empty for the overwhelming majority of logins. The private
    /// key (`key_value`) is only ever touched by a WebAuthn ceremony, never by
    /// the metadata listing.
    pub fido2: Vec<RawFido2Credential>,
}

/// One stored passkey as it arrives from `sync`, every string field still an
/// EncString (except `creation_date`, which Bitwarden stores in the clear).
/// The shape matches Bitwarden's encrypted `Fido2Credential`; unknown/absent
/// fields are simply `None`.
#[derive(Debug, Clone, Default)]
pub struct RawFido2Credential {
    pub credential_id: Option<EncString>,
    pub rp_id: Option<EncString>,
    pub rp_name: Option<EncString>,
    pub user_name: Option<EncString>,
    pub user_display_name: Option<EncString>,
    /// The account handle — needed for a `get` ceremony, not shown in a listing.
    pub user_handle: Option<EncString>,
    pub counter: Option<EncString>,
    pub discoverable: Option<EncString>,
    pub key_type: Option<EncString>,
    pub key_algorithm: Option<EncString>,
    pub key_curve: Option<EncString>,
    /// The PKCS#8 private key, encrypted. Decrypted ONLY to sign a ceremony
    /// challenge, and NEVER surfaced in [`PasskeyInfo`] or any list.
    pub key_value: Option<EncString>,
    /// Plaintext ISO-8601 in the sync record — Bitwarden does not encrypt it.
    pub creation_date: Option<String>,
}

/// The `type` value of a login cipher. The only type this client can edit's
/// login fields on.
const CIPHER_TYPE_LOGIN: u8 = 1;

/// Keys the SERVER owns. They are read-only projections in a `sync` record and
/// must not be echoed back in an update: `id` is in the URL, and the rest are
/// either derived (`revisionDate`, `edit`, `viewPassword`), not part of the
/// update model (`collectionIds`), or a legacy duplicate that could contradict
/// the fields we patch (`data`).
///
/// Everything NOT listed here rides back to the server verbatim — including
/// fields this client has never heard of. That is the point: the strip list is
/// a denylist, not an allowlist, so a future Bitwarden field survives an edit
/// written before it existed.
const SERVER_MANAGED_KEYS: &[&str] = &[
    "id",
    "object",
    "revisionDate",
    "creationDate",
    "deletedDate",
    "edit",
    "viewPassword",
    "organizationUseTotp",
    "permissions",
    "collectionIds",
    "attachments",
    "data",
];

/// How many past passwords Bitwarden's clients keep on an item.
const PASSWORD_HISTORY_LIMIT: usize = 5;

/// A change to an existing cipher. Only the `Some` fields are touched; every
/// other field of the item survives verbatim.
///
/// There is deliberately no way to CLEAR a field: `Some("")` is rejected rather
/// than quietly encrypting an empty string. Clearing needs its own verb with
/// its own confirmation, and guessing would be the kind of silent data loss
/// this whole struct exists to prevent.
#[derive(Debug, Clone, Default)]
pub struct CipherEdit {
    pub name: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub totp: Option<String>,
    /// Replaces the item's ENTIRE uri list with this single uri.
    pub uri: Option<String>,
    pub notes: Option<String>,
    pub folder_id: Option<String>,
}

impl CipherEdit {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.username.is_none()
            && self.password.is_none()
            && self.totp.is_none()
            && self.uri.is_none()
            && self.notes.is_none()
            && self.folder_id.is_none()
    }

    /// Whether the edit touches a field that only exists on a login cipher.
    fn touches_login(&self) -> bool {
        self.username.is_some()
            || self.password.is_some()
            || self.totp.is_some()
            || self.uri.is_some()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("no vault item with id {0}")]
    UnknownItem(String),
    #[error("item {0} has no raw record from sync — run `ychrome-vault sync` and retry")]
    NoRawRecord(String),
    #[error("{0} is not a login item, so it has no username, password, totp or uri")]
    NotALogin(String),
    #[error("refusing to set a field to the empty string; clearing a field is not supported")]
    EmptyValue,
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

/// Why a passkey assertion could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum Fido2AssertError {
    #[error("no vault item with that id")]
    UnknownItem,
    #[error("the item has no passkey matching that credential id")]
    NoSuchPasskey,
    #[error("the stored passkey key did not base64-decode")]
    BadPrivateKey,
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Fido2(#[from] crate::fido2::Fido2Error),
}

/// Decode a decrypted `keyValue` (base64 text) to the raw PKCS#8 DER bytes.
/// Standard base64 first, then URL-safe, since clients have differed.
fn decode_key_value(b64: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let b64 = b64.trim();
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64))
        .ok()
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
    /// The item stores at least one passkey. Like `has_totp`, this is a boolean
    /// so a listing can badge it without decrypting anything secret.
    pub has_passkey: bool,
}

/// Secret-free metadata for one stored passkey. Carries what a picker or a
/// listing shows — never the private key (`key_value`) and never the raw
/// account handle. Serializable because the agent hands it to clients.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PasskeyInfo {
    pub credential_id: Option<String>,
    pub rp_id: Option<String>,
    pub rp_name: Option<String>,
    pub user_name: Option<String>,
    pub user_display_name: Option<String>,
    pub discoverable: bool,
    pub creation_date: Option<String>,
}

/// A stored passkey that can answer a `get()` ceremony, resolved by RP. Carries
/// the account fields the presence dialog and the assertion response need —
/// `user_handle` is the WebAuthn `userHandle` an RP maps back to an account —
/// but never the private key. Serializable because the agent hands it to the
/// browser signer, which shows the account and echoes `item_id`/`credential_id`
/// back to sign.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PasskeyMatch {
    pub item_id: String,
    pub credential_id: String,
    pub rp_id: String,
    pub rp_name: Option<String>,
    pub user_name: Option<String>,
    pub user_display_name: Option<String>,
    pub user_handle: Option<String>,
    /// The vault item's name — the label the presence dialog shows when the
    /// passkey has no `userName`.
    pub item_name: Option<String>,
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

/// A passkey to store as a new vault login — the `create()` result. The private
/// key is PKCS#8 (as [`crate::fido2::generate_credential`] produced it) and is
/// base64url-sealed by [`Vault::new_passkey_login_body`]; it never leaves the
/// process in the clear. `creation_date` is the plaintext ISO-8601 the sync
/// record echoes back.
#[derive(Debug, Clone, Default)]
pub struct NewPasskey {
    /// The vault item's name — usually the RP name, so the item reads sensibly
    /// in a listing next to password logins.
    pub item_name: String,
    pub rp_id: String,
    pub rp_name: String,
    pub user_name: String,
    pub user_display_name: String,
    /// The WebAuthn `user.id` handle bytes (the RP's account id).
    pub user_id: Vec<u8>,
    /// The generated credential id bytes (the RP's handle for this passkey).
    pub credential_id: Vec<u8>,
    /// The generated P-256 private key, PKCS#8 DER.
    pub pkcs8_der: Vec<u8>,
    /// The login's `username`, for the item listing — often the same as
    /// `user_name`. Optional: a usernameless passkey has none.
    pub account_username: Option<String>,
    pub creation_date: String,
}

/// The gap between "ciphers the server sent" and "items we can show".
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct VaultDiagnostic {
    /// Everything `sync` returned (minus trashed items).
    pub ciphers: usize,
    /// Ciphers whose name decrypts — exactly what `items()` yields.
    pub decrypted: usize,
    /// The cipher belongs to an organization whose key we never unwrapped.
    /// Before org support this showed up as the two buckets below instead.
    pub skipped_missing_organization_key: usize,
    /// The cipher carries its own key and that key will not decrypt.
    pub skipped_item_key_undecryptable: usize,
    /// The name is present but will not decrypt under the resolved key.
    pub skipped_name_undecryptable: usize,
    /// No name field at all.
    pub skipped_no_name: usize,
    /// How many ciphers belong to an organization, decryptable or not.
    pub organization_ciphers: usize,
}

/// The unlocked vault: the user key plus the still-encrypted ciphers. Secrets
/// are decrypted only when asked for.
pub struct Vault {
    user_key: SymmetricKey,
    /// `organization_id -> that org's symmetric key`, already unwrapped with
    /// the user's RSA private key. Empty when the account is in no orgs.
    organization_keys: HashMap<String, SymmetricKey>,
    ciphers: Vec<RawCipher>,
    /// Soft-deleted ciphers (each carries a `deletedDate`). Kept OUT of
    /// [`items`] so the live list never shows them, but retained so `restore`
    /// can look a trashed item up by name and `list --trashed` can show what is
    /// recoverable. A hard delete leaves nothing here — the server drops it.
    ///
    /// [`items`]: Vault::items
    trashed: Vec<RawCipher>,
    folder_names: HashMap<String, EncString>,
}

impl Vault {
    pub fn new(
        user_key: SymmetricKey,
        organization_keys: HashMap<String, SymmetricKey>,
        ciphers: Vec<RawCipher>,
        trashed: Vec<RawCipher>,
        folders: HashMap<String, EncString>,
    ) -> Self {
        Vault {
            user_key,
            organization_keys,
            ciphers,
            trashed,
            folder_names: folders,
        }
    }

    /// The key a cipher's fields (or its item key) are sealed under: its
    /// organization's key when it belongs to one, else the user key.
    ///
    /// Getting this wrong is invisible — the MAC check fails, `items()` skips
    /// the cipher, and the item simply is not there.
    fn base_key(&self, cipher: &RawCipher) -> Result<&SymmetricKey, CryptoError> {
        match &cipher.organization_id {
            Some(id) => self
                .organization_keys
                .get(id)
                .ok_or_else(|| CryptoError::MissingOrganizationKey(id.clone())),
            None => Ok(&self.user_key),
        }
    }

    /// The key that decrypts a cipher's fields: its own item key if present
    /// (itself sealed under the base key), else the base key.
    fn cipher_key(&self, cipher: &RawCipher) -> Result<SymmetricKey, CryptoError> {
        let base = self.base_key(cipher)?;
        match &cipher.key {
            Some(item_key) => {
                let raw = base.decrypt(item_key)?;
                SymmetricKey::from_bytes(&raw)
            }
            None => Ok(base.clone()),
        }
    }

    /// The id of the folder with this name (case-insensitive). Folders are
    /// always sealed under the user key, never an organization key.
    pub fn folder_id(&self, name: &str) -> Option<String> {
        let wanted = name.trim().to_ascii_lowercase();
        self.folder_names.iter().find_map(|(id, enc)| {
            let decrypted = self.user_key.decrypt_to_string(enc).ok()?;
            (decrypted.trim().to_ascii_lowercase() == wanted).then(|| id.clone())
        })
    }

    fn folder_name(&self, cipher: &RawCipher) -> Option<String> {
        let id = cipher.folder_id.as_ref()?;
        let enc = self.folder_names.get(id)?;
        self.user_key.decrypt_to_string(enc).ok()
    }

    /// The secret-free item list. A cipher that fails to decrypt (corrupt, or a
    /// type we do not model) is skipped rather than aborting the whole vault.
    pub fn items(&self) -> Vec<VaultItem> {
        self.items_from(&self.ciphers)
    }

    /// The soft-deleted items, same secret-free shape as [`items`]. These are
    /// what `restore` can bring back and what `list --trashed` shows; the two
    /// buckets never overlap (a cipher is either live or trashed).
    ///
    /// [`items`]: Vault::items
    pub fn trashed_items(&self) -> Vec<VaultItem> {
        self.items_from(&self.trashed)
    }

    fn items_from(&self, ciphers: &[RawCipher]) -> Vec<VaultItem> {
        ciphers
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
                    has_passkey: !cipher.fido2.is_empty(),
                })
            })
            .collect()
    }

    /// The secret-free metadata of an item's stored passkeys, decrypted on
    /// demand. Empty if the item is unknown or holds no passkey. The private
    /// key is never decrypted here — a listing must not be able to spill it.
    pub fn passkeys(&self, id: &str) -> Vec<PasskeyInfo> {
        let Some(cipher) = self.find(id) else {
            return Vec::new();
        };
        let Ok(key) = self.cipher_key(cipher) else {
            return Vec::new();
        };
        let decrypt = |enc: &Option<EncString>| {
            enc.as_ref()
                .and_then(|enc| key.decrypt_to_string(enc).ok())
        };
        cipher
            .fido2
            .iter()
            .map(|credential| PasskeyInfo {
                credential_id: decrypt(&credential.credential_id),
                rp_id: decrypt(&credential.rp_id),
                rp_name: decrypt(&credential.rp_name),
                user_name: decrypt(&credential.user_name),
                user_display_name: decrypt(&credential.user_display_name),
                // A malformed or absent flag reads as not-discoverable rather
                // than failing the whole listing.
                discoverable: decrypt(&credential.discoverable).as_deref() == Some("true"),
                creation_date: credential.creation_date.clone(),
            })
            .collect()
    }

    /// Sign a WebAuthn assertion for one of an item's stored passkeys — the
    /// `navigator.credentials.get()` ceremony, answered from the vault.
    ///
    /// `credential_id` selects which passkey by its decrypted credentialId;
    /// `None` uses the item's first (the common single-passkey case). The
    /// private key (`keyValue`) is decrypted here, used once, and zeroized — it
    /// never leaves the process and is never returned. A [`UserPresence`] is
    /// REQUIRED by value, so there is no path to a signature without consent.
    ///
    /// [`UserPresence`]: crate::fido2::UserPresence
    pub fn fido2_assert(
        &self,
        id: &str,
        credential_id: Option<&str>,
        rp_id: &str,
        client_data_hash: &[u8],
        consent: crate::fido2::UserPresence,
    ) -> Result<crate::fido2::Fido2Assertion, Fido2AssertError> {
        let cipher = self.find(id).ok_or(Fido2AssertError::UnknownItem)?;
        let key = self.cipher_key(cipher)?;
        let decrypt = |enc: &Option<EncString>| {
            enc.as_ref().and_then(|enc| key.decrypt_to_string(enc).ok())
        };

        let credential = match credential_id {
            Some(wanted) => cipher.fido2.iter().find(|c| {
                decrypt(&c.credential_id).as_deref() == Some(wanted)
            }),
            None => cipher.fido2.first(),
        }
        .ok_or(Fido2AssertError::NoSuchPasskey)?;

        // keyValue is base64 text (the fido2 fields are strings) of a P-256
        // PKCS#8 key. Held zeroized: neither the base64 nor the DER lingers.
        let key_value = credential
            .key_value
            .as_ref()
            .ok_or(Fido2AssertError::NoSuchPasskey)?;
        let b64 = zeroize::Zeroizing::new(key.decrypt_to_string(key_value)?);
        let pkcs8 =
            zeroize::Zeroizing::new(decode_key_value(&b64).ok_or(Fido2AssertError::BadPrivateKey)?);

        // WebAuthn signCount. Bitwarden stores it as a stringified int; a
        // missing/garbled one signs with 0 (many authenticators never increment).
        let sign_count = decrypt(&credential.counter)
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);

        Ok(crate::fido2::sign_assertion(
            &pkcs8,
            rp_id,
            client_data_hash,
            sign_count,
            consent,
        )?)
    }

    /// Resolve a `navigator.credentials.get()` request to the stored passkeys
    /// that can answer it. The page names an `rp_id` and, for a non-discoverable
    /// login, an `allow_credential_ids` allow-list (base64url credentialIds from
    /// `allowCredentials`); an empty allow-list means "any resident credential
    /// for this RP" (discoverable / usernameless).
    ///
    /// Returns one [`PasskeyMatch`] per candidate, secret-free — the private key
    /// is not touched. The caller picks (usually the only one), shows the user
    /// the account, and passes `item_id` + `credential_id` back to
    /// [`fido2_assert`]. Multiple matches are the account-picker case, exactly as
    /// `suggest` is for passwords.
    ///
    /// [`fido2_assert`]: Vault::fido2_assert
    pub fn passkeys_for_assertion(
        &self,
        rp_id: &str,
        allow_credential_ids: &[String],
    ) -> Vec<PasskeyMatch> {
        let mut matches = Vec::new();
        for cipher in &self.ciphers {
            if cipher.fido2.is_empty() {
                continue;
            }
            let Ok(key) = self.cipher_key(cipher) else {
                continue;
            };
            let decrypt = |enc: &Option<EncString>| {
                enc.as_ref().and_then(|enc| key.decrypt_to_string(enc).ok())
            };
            let item_name = cipher
                .name
                .as_ref()
                .and_then(|enc| key.decrypt_to_string(enc).ok());
            for credential in &cipher.fido2 {
                if decrypt(&credential.rp_id).as_deref() != Some(rp_id) {
                    continue;
                }
                let credential_id = match decrypt(&credential.credential_id) {
                    Some(id) => id,
                    // A passkey we cannot name a credentialId for cannot be put
                    // in a clientDataJSON, so it cannot answer a ceremony.
                    None => continue,
                };
                if !allow_credential_ids.is_empty()
                    && !allow_credential_ids.iter().any(|id| id == &credential_id)
                {
                    continue;
                }
                matches.push(PasskeyMatch {
                    item_id: cipher.id.clone(),
                    credential_id,
                    rp_id: rp_id.to_string(),
                    rp_name: decrypt(&credential.rp_name),
                    user_name: decrypt(&credential.user_name),
                    user_display_name: decrypt(&credential.user_display_name),
                    user_handle: decrypt(&credential.user_handle),
                    item_name: item_name.clone(),
                });
            }
        }
        matches
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

    /// Build the `POST /api/ciphers` body for a NEW login that carries a passkey
    /// — a `navigator.credentials.create()` result stored in the vault, in the
    /// same encrypted `Fido2Credential` shape `sync` reads back.
    ///
    /// Every field is sealed under the user key (a new cipher has no item key, so
    /// the user key IS the cipher key — exactly what [`cipher_key`] resolves on
    /// the next sync, and what [`fido2_assert`] then decrypts). The private key
    /// arrives here already zeroized by the caller and is base64url-encoded into
    /// `keyValue`, matching what [`fido2_assert`]'s `decode_key_value` accepts.
    ///
    /// [`cipher_key`]: Vault::cipher_key
    /// [`fido2_assert`]: Vault::fido2_assert
    pub fn new_passkey_login_body(
        &self,
        passkey: &NewPasskey,
    ) -> Result<serde_json::Value, CryptoError> {
        use base64::Engine;
        let enc = |value: &str| -> Result<String, CryptoError> {
            Ok(self.user_key.encrypt_string(value)?.to_string())
        };
        let enc_opt = |value: &Option<String>| -> Result<serde_json::Value, CryptoError> {
            match value.as_deref().filter(|value| !value.is_empty()) {
                Some(value) => Ok(serde_json::Value::String(enc(value)?)),
                None => Ok(serde_json::Value::Null),
            }
        };
        let b64url = |bytes: &[u8]| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        };
        // The RP references the credential by this handle; the userHandle is the
        // account id the RP maps back on a usernameless login — both base64url.
        let credential_id = b64url(&passkey.credential_id);
        let user_handle = b64url(&passkey.user_id);
        let key_value = b64url(&passkey.pkcs8_der);

        let fido2 = serde_json::json!({
            "credentialId": enc(&credential_id)?,
            "keyType": enc("public-key")?,
            "keyAlgorithm": enc("ECDSA")?,
            "keyCurve": enc("P-256")?,
            "keyValue": enc(&key_value)?,
            "rpId": enc(&passkey.rp_id)?,
            "rpName": enc(&passkey.rp_name)?,
            "userName": enc(&passkey.user_name)?,
            "userDisplayName": enc(&passkey.user_display_name)?,
            "userHandle": enc(&user_handle)?,
            "counter": enc("0")?,
            "discoverable": enc("true")?,
            // Bitwarden stores this in the clear; the server keeps it verbatim.
            "creationDate": passkey.creation_date,
        });

        Ok(serde_json::json!({
            "type": 1,
            "name": enc(&passkey.item_name)?,
            "notes": serde_json::Value::Null,
            "favorite": false,
            "folderId": serde_json::Value::Null,
            "reprompt": 0,
            "fields": [],
            "login": {
                "username": enc_opt(&passkey.account_username)?,
                "password": serde_json::Value::Null,
                "totp": serde_json::Value::Null,
                "uris": serde_json::json!([{ "uri": enc(&format!("https://{}", passkey.rp_id))?, "match": serde_json::Value::Null }]),
                "fido2Credentials": [fido2],
            },
        }))
    }

    /// Build the `PUT /api/ciphers/{id}` body for an edit, by PATCHING the raw
    /// record `sync` returned rather than rebuilding one from the fields this
    /// client models.
    ///
    /// That distinction is the whole reason `edit` took so long to exist. The
    /// server does `cipher.notes = data.notes` — an absent field is destroyed,
    /// not preserved — so a body rebuilt from [`RawCipher`]'s parsed fields
    /// would wipe notes, custom fields, favorite and password history on every
    /// edit. Here, unknown keys ride along untouched and only what the caller
    /// named is replaced.
    ///
    /// Fields are encrypted under the CIPHER's key, not the user key: an item
    /// with its own item key (or one owned by an organization) seals its fields
    /// under that key, and encrypting under the user key would write a value
    /// that `items()` then silently skips as undecryptable.
    ///
    /// The raw `revisionDate` is echoed as `lastKnownRevisionDate`, so a server
    /// whose copy moved on since our last sync rejects the write instead of
    /// clobbering a concurrent edit.
    pub fn edit_body(&self, id: &str, edit: &CipherEdit) -> Result<serde_json::Value, EditError> {
        use serde_json::{Value, json};

        let cipher = self
            .find(id)
            .ok_or_else(|| EditError::UnknownItem(id.to_string()))?;
        if edit.touches_login() && cipher.item_type != CIPHER_TYPE_LOGIN {
            return Err(EditError::NotALogin(id.to_string()));
        }
        for value in [
            &edit.name,
            &edit.username,
            &edit.password,
            &edit.totp,
            &edit.uri,
            &edit.notes,
        ] {
            if value.as_deref().is_some_and(str::is_empty) {
                return Err(EditError::EmptyValue);
            }
        }
        let raw = cipher
            .raw
            .as_object()
            .ok_or_else(|| EditError::NoRawRecord(id.to_string()))?;

        let key = self.cipher_key(cipher)?;
        let encrypt =
            |value: &str| -> Result<Value, CryptoError> { Ok(json!(key.encrypt_string(value)?.to_string())) };

        let mut body = raw.clone();
        let revision = get_ci(&body, "revisionDate").cloned();
        // Password history is appended BEFORE the password is overwritten,
        // because it needs the OLD ciphertext — which is reused verbatim, never
        // re-encrypted.
        let history = edit
            .password
            .is_some()
            .then(|| password_history_with_current(&body, cipher))
            .flatten();
        for key in SERVER_MANAGED_KEYS {
            remove_ci(&mut body, key);
        }

        let mut login = remove_ci(&mut body, "login")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();

        if let Some(name) = &edit.name {
            set_ci(&mut body, "name", encrypt(name)?);
        }
        if let Some(notes) = &edit.notes {
            set_ci(&mut body, "notes", encrypt(notes)?);
        }
        if let Some(folder_id) = &edit.folder_id {
            set_ci(&mut body, "folderId", json!(folder_id));
        }
        if let Some(username) = &edit.username {
            set_ci(&mut login, "username", encrypt(username)?);
        }
        if let Some(password) = &edit.password {
            set_ci(&mut login, "password", encrypt(password)?);
        }
        if let Some(totp) = &edit.totp {
            set_ci(&mut login, "totp", encrypt(totp)?);
        }
        if let Some(uri) = &edit.uri {
            set_ci(
                &mut login,
                "uris",
                json!([{ "uri": encrypt(uri)?, "match": Value::Null }]),
            );
        }
        if let Some(history) = history {
            set_ci(&mut body, "passwordHistory", history);
        }
        if !login.is_empty() {
            set_ci(&mut body, "login", Value::Object(login));
        }
        if let Some(revision) = revision {
            set_ci(&mut body, "lastKnownRevisionDate", revision);
        }
        Ok(Value::Object(body))
    }

    /// The user key, so a resync can re-unwrap organization keys without the
    /// master password.
    pub(crate) fn user_key(&self) -> &SymmetricKey {
        &self.user_key
    }

    /// Swap in a freshly synced cipher set, keeping the same user key. Used by
    /// `VaultManager::resync`, which refreshes an unlocked vault with the
    /// session's bearer token rather than the master password.
    pub fn replace_contents(
        &mut self,
        organization_keys: HashMap<String, SymmetricKey>,
        ciphers: Vec<RawCipher>,
        trashed: Vec<RawCipher>,
        folders: HashMap<String, EncString>,
    ) {
        self.organization_keys = organization_keys;
        self.ciphers = ciphers;
        self.trashed = trashed;
        self.folder_names = folders;
    }

    /// Why every cipher `sync` returned is, or is not, in [`items`].
    ///
    /// `items()` silently skips a cipher it cannot decrypt, which is right for
    /// robustness and wrong for honesty: the vault reported 1107 items and
    /// listed 1050. This attributes the gap.
    ///
    /// [`items`]: Vault::items
    pub fn diagnose(&self) -> VaultDiagnostic {
        let mut diagnostic = VaultDiagnostic {
            ciphers: self.ciphers.len(),
            ..Default::default()
        };
        for cipher in &self.ciphers {
            if cipher.organization_id.is_some() {
                diagnostic.organization_ciphers += 1;
            }
            if self.base_key(cipher).is_err() {
                diagnostic.skipped_missing_organization_key += 1;
                continue;
            }
            let Ok(key) = self.cipher_key(cipher) else {
                diagnostic.skipped_item_key_undecryptable += 1;
                continue;
            };
            match &cipher.name {
                None => diagnostic.skipped_no_name += 1,
                Some(name) if key.decrypt_to_string(name).is_err() => {
                    diagnostic.skipped_name_undecryptable += 1
                }
                Some(_) => diagnostic.decrypted += 1,
            }
        }
        diagnostic
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

    /// A specific item's notes, decrypted on demand.
    ///
    /// Read straight off the RAW record, because `sync` does not parse notes
    /// into [`RawCipher`] at all — which is exactly why [`Vault::edit_body`]
    /// must patch the raw JSON instead of rebuilding a cipher from the parsed
    /// fields. `None` if the item is unknown, has no notes, or predates the
    /// raw-retention change.
    pub fn notes(&self, id: &str) -> Option<String> {
        let cipher = self.find(id)?;
        let encrypted = get_ci(cipher.raw.as_object()?, "notes")?.as_str()?;
        let key = self.cipher_key(cipher).ok()?;
        key.decrypt_to_string(&EncString::parse(encrypted).ok()?).ok()
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

type JsonMap = serde_json::Map<String, serde_json::Value>;

// Vaultwarden has drifted between PascalCase and camelCase across versions, so
// a raw record's keys cannot be matched exactly. Reads, removals and writes all
// go through these: match any casing, write camelCase, and never leave a
// case-variant twin behind that the server might read instead of our patch.

fn get_ci<'a>(object: &'a JsonMap, key: &str) -> Option<&'a serde_json::Value> {
    object
        .iter()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

/// Remove every case-variant of `key`, returning the first value found.
fn remove_ci(object: &mut JsonMap, key: &str) -> Option<serde_json::Value> {
    let variants: Vec<String> = object
        .keys()
        .filter(|existing| existing.eq_ignore_ascii_case(key))
        .cloned()
        .collect();
    let mut taken = None;
    for variant in variants {
        let value = object.remove(&variant);
        if taken.is_none() {
            taken = value;
        }
    }
    taken
}

fn set_ci(object: &mut JsonMap, key: &str, value: serde_json::Value) {
    remove_ci(object, key);
    object.insert(key.to_string(), value);
}

/// The item's `passwordHistory` with its CURRENT password prepended, as a
/// Bitwarden client does when a password is replaced. Returns `None` when the
/// item has no password to remember.
///
/// The old ciphertext is reused exactly as the server sent it — re-encrypting
/// it would need the plaintext, and history is not worth decrypting a secret.
fn password_history_with_current(
    raw: &JsonMap,
    cipher: &RawCipher,
) -> Option<serde_json::Value> {
    let current = cipher.password.as_ref()?;
    let mut history: Vec<serde_json::Value> = get_ci(raw, "passwordHistory")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    history.insert(
        0,
        serde_json::json!({
            "password": current.to_string(),
            "lastUsedDate": rfc3339_millis_utc(std::time::SystemTime::now()),
        }),
    );
    history.truncate(PASSWORD_HISTORY_LIMIT);
    Some(serde_json::Value::Array(history))
}

/// `2026-07-09T15:52:49.000Z` — the timestamp shape Bitwarden clients write.
/// Hand-rolled because this crate carries no date dependency, and a malformed
/// date here would corrupt what other clients read out of password history.
fn rfc3339_millis_utc(time: std::time::SystemTime) -> String {
    let elapsed = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = elapsed.as_secs() as i64;
    let (days, second_of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        second_of_day / 3_600,
        (second_of_day % 3_600) / 60,
        second_of_day % 60,
    );
    let millis = elapsed.subsec_millis();
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days since the Unix epoch → (year, month, day). Howard Hinnant's
/// `civil_from_days`, valid across the whole proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
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
            organization_id: None,
            raw: serde_json::Value::Null,
            fido2: vec![],
        };
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], vec![], folders);

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

    // `items()` skips what it cannot decrypt, so the cipher count and the item
    // count diverge whenever the vault holds ciphers sealed under a key we do
    // not have — an organization's. `diagnose` must attribute every one.
    #[test]
    fn diagnose_attributes_every_undecryptable_cipher() {
        let user_bytes = [0x5au8; 64];
        let org_bytes = [0x99u8; 64]; // sealed to the user's public key, in reality
        let item_bytes = [0x77u8; 64]; // an org cipher's own item key
        let user_key = SymmetricKey::from_bytes(&user_bytes).unwrap();

        let ciphers = vec![
            // Readable: sealed under the user key.
            RawCipher {
                id: "ok".into(),
                name: Some(seal(&user_bytes, "GitHub")),
                ..Default::default()
            },
            // Org cipher, no item key: the NAME will not decrypt.
            RawCipher {
                id: "org-name".into(),
                organization_id: Some("org1".into()),
                name: Some(seal(&org_bytes, "Shared Login")),
                ..Default::default()
            },
            // Org cipher WITH its own item key: the ITEM key is sealed under
            // the ORG key, and the fields under the item key. Two hops, and
            // both need the org key to start.
            RawCipher {
                id: "org-key".into(),
                organization_id: Some("org1".into()),
                key: Some(super::seal(&org_bytes, &item_bytes)),
                name: Some(seal(&item_bytes, "Shared Note")),
                ..Default::default()
            },
            // Nameless.
            RawCipher {
                id: "nameless".into(),
                ..Default::default()
            },
        ];
        // WITHOUT the org key: the two org ciphers are unreadable, and the
        // diagnostic says exactly why. This is the 59-cipher gap in miniature.
        let blind = Vault::new(user_key.clone(), HashMap::new(), ciphers.clone(), vec![], HashMap::new());
        assert_eq!(blind.items().len(), 1, "only the user-key cipher is readable");
        assert_eq!(
            blind.diagnose(),
            VaultDiagnostic {
                ciphers: 4,
                decrypted: 1,
                skipped_missing_organization_key: 2,
                skipped_item_key_undecryptable: 0,
                skipped_name_undecryptable: 0,
                skipped_no_name: 1,
                organization_ciphers: 2,
            }
        );

        // WITH the org key: both org ciphers decrypt, including the one whose
        // item key is sealed under the org key rather than the user key.
        let mut org_keys = HashMap::new();
        org_keys.insert("org1".to_string(), SymmetricKey::from_bytes(&org_bytes).unwrap());
        let seeing = Vault::new(user_key, org_keys, ciphers, vec![], HashMap::new());
        let names: Vec<String> = seeing.items().into_iter().map(|item| item.name).collect();
        assert_eq!(names, ["GitHub", "Shared Login", "Shared Note"]);
        let diagnostic = seeing.diagnose();
        assert_eq!(diagnostic.decrypted, 3);
        assert_eq!(diagnostic.skipped_missing_organization_key, 0);

        // Every cipher is accounted for — no silent category.
        for d in [blind.diagnose(), diagnostic] {
            assert_eq!(
                d.decrypted
                    + d.skipped_missing_organization_key
                    + d.skipped_item_key_undecryptable
                    + d.skipped_name_undecryptable
                    + d.skipped_no_name,
                d.ciphers
            );
        }
    }

    // What we WRITE must be what we can READ. Every field of a create body is
    // an EncString under the user key, no plaintext leaks into the JSON, and
    // an absent field is null rather than an EncString of "".
    #[test]
    fn new_login_body_encrypts_every_field_and_reads_back() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let vault = Vault::new(user_key.clone(), HashMap::new(), vec![], vec![], HashMap::new());

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
        let vault = Vault::new(user_key, HashMap::new(), vec![], vec![], HashMap::new());
        let body = vault
            .new_login_body(&NewLogin {
                name: "bare".to_string(),
                uri: Some(String::new()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(body["login"]["uris"].as_array().unwrap().len(), 0);
    }

    /// A cipher as the server really sends it: the fields we model, plus the
    /// ones we do not (notes, custom fields, favorite, password history) and
    /// one we have never heard of.
    fn raw_login_record() -> serde_json::Value {
        serde_json::json!({
            "object": "cipherDetails",
            "id": "c1",
            "type": 1,
            "name": "2.enc-name",
            "notes": "2.enc-notes",
            "favorite": true,
            "reprompt": 1,
            "folderId": "f1",
            "organizationId": null,
            "fields": [{"name": "2.enc-field", "value": "2.enc-value", "type": 0}],
            "passwordHistory": [{"password": "2.older", "lastUsedDate": "2020-01-01T00:00:00.000Z"}],
            "login": {
                "username": "2.enc-user",
                "password": "2.enc-pass",
                "totp": null,
                "uris": [{"uri": "2.enc-uri", "match": null}],
                "fido2Credentials": [{"credentialId": "abc"}],
            },
            "revisionDate": "2026-07-09T15:52:49.123Z",
            "creationDate": "2020-01-01T00:00:00.000Z",
            "deletedDate": null,
            "collectionIds": [],
            "edit": true,
            "viewPassword": true,
            "somethingBitwardenAddsIn2027": {"keep": "me"},
        })
    }

    fn login_vault(key_bytes: &[u8; 64]) -> Vault {
        let user_key = SymmetricKey::from_bytes(key_bytes).unwrap();
        let cipher = RawCipher {
            raw: raw_login_record(),
            id: "c1".into(),
            item_type: 1,
            name: Some(seal(key_bytes, "GitHub")),
            username: Some(seal(key_bytes, "octocat")),
            password: Some(seal(key_bytes, "old-password")),
            uris: vec![seal(key_bytes, "https://github.com")],
            ..Default::default()
        };
        Vault::new(user_key, HashMap::new(), vec![cipher], vec![], HashMap::new())
    }

    #[test]
    fn trashed_items_stay_out_of_the_live_list_and_vice_versa() {
        let key_bytes = [0x42u8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let named = |id: &str, name: &str| RawCipher {
            id: id.into(),
            item_type: 1,
            name: Some(seal(&key_bytes, name)),
            ..Default::default()
        };
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![named("live", "Live Entry")],
            vec![named("trashed", "Trashed Entry")],
            HashMap::new(),
        );

        let live: Vec<String> = vault.items().into_iter().map(|i| i.name).collect();
        let trashed: Vec<String> = vault.trashed_items().into_iter().map(|i| i.name).collect();
        assert_eq!(live, ["Live Entry"]);
        assert_eq!(trashed, ["Trashed Entry"]);
        // The whole point: a trashed name never leaks into the live list, so an
        // auto-fill or the sidebar cannot surface a deleted credential.
        assert!(!live.iter().any(|name| name == "Trashed Entry"));
    }

    #[test]
    fn passkeys_decrypt_metadata_and_never_expose_the_private_key() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let s = |text: &str| Some(seal(&key_bytes, text));
        let cipher = RawCipher {
            id: "pk".into(),
            item_type: 1,
            name: Some(seal(&key_bytes, "GitHub")),
            fido2: vec![RawFido2Credential {
                credential_id: s("cred-123"),
                rp_id: s("github.com"),
                rp_name: s("GitHub"),
                user_name: s("octocat"),
                user_display_name: s("Octo Cat"),
                user_handle: s("dXNlci1oYW5kbGU"),
                counter: s("0"),
                discoverable: s("true"),
                key_type: s("public-key"),
                key_algorithm: s("ECDSA"),
                key_curve: s("P-256"),
                key_value: s("SUPER-SECRET-PKCS8-PRIVATE-KEY"),
                creation_date: Some("2026-07-10T00:00:00Z".into()),
            }],
            ..Default::default()
        };
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], vec![], HashMap::new());

        // The badge is set without decrypting anything secret.
        assert!(vault.items()[0].has_passkey);

        let passkeys = vault.passkeys("pk");
        assert_eq!(passkeys.len(), 1);
        let pk = &passkeys[0];
        assert_eq!(pk.rp_id.as_deref(), Some("github.com"));
        assert_eq!(pk.user_name.as_deref(), Some("octocat"));
        assert_eq!(pk.credential_id.as_deref(), Some("cred-123"));
        assert!(pk.discoverable);
        assert_eq!(pk.creation_date.as_deref(), Some("2026-07-10T00:00:00Z"));

        // THE security property: the secret-free view has no field that could
        // carry the private key. Serialize it and prove the plaintext key and
        // its field name are both absent — a listing must never spill it.
        let json = serde_json::to_string(pk).unwrap();
        assert!(
            !json.contains("SUPER-SECRET-PKCS8-PRIVATE-KEY"),
            "private key leaked into the listing: {json}"
        );
        assert!(!json.contains("key_value") && !json.contains("user_handle"), "{json}");

        // An unknown item yields no passkeys rather than panicking.
        assert!(vault.passkeys("nope").is_empty());
    }

    #[test]
    fn passkeys_for_assertion_resolves_by_rp_and_honors_allow_credentials() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let s = |text: &str| Some(seal(&key_bytes, text));
        // A passkey with a given rpId + credentialId on a named item.
        let passkey = |item: &str, rp: &str, cred: &str, user: &str| RawCipher {
            id: item.into(),
            item_type: 1,
            name: Some(seal(&key_bytes, item)),
            fido2: vec![RawFido2Credential {
                credential_id: s(cred),
                rp_id: s(rp),
                user_name: s(user),
                key_value: s("secret"),
                counter: s("0"),
                ..Default::default()
            }],
            ..Default::default()
        };
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![
                passkey("gh-a", "github.com", "cred-a", "octocat"),
                passkey("gh-b", "github.com", "cred-b", "hubot"),
                passkey("other", "example.com", "cred-x", "someone"),
            ],
            vec![],
            HashMap::new(),
        );

        // Discoverable (empty allow-list): every passkey for the RP, and NOT
        // another RP's — that is the account-picker case.
        let mut any = vault.passkeys_for_assertion("github.com", &[]);
        any.sort_by(|a, b| a.credential_id.cmp(&b.credential_id));
        assert_eq!(any.len(), 2);
        assert_eq!(any[0].credential_id, "cred-a");
        assert_eq!(any[0].user_name.as_deref(), Some("octocat"));
        assert_eq!(any[1].credential_id, "cred-b");
        assert!(any.iter().all(|m| m.rp_id == "github.com"));

        // allowCredentials narrows to exactly the named credential.
        let allowed = vault.passkeys_for_assertion("github.com", &["cred-b".into()]);
        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0].item_id, "gh-b");

        // An allow-list naming no stored credential resolves to nothing (the
        // page offered credentials we do not hold), and a secret never rides in
        // the match.
        let none = vault.passkeys_for_assertion("github.com", &["cred-unknown".into()]);
        assert!(none.is_empty());
        assert!(!serde_json::to_string(&any).unwrap().contains("secret"));
    }

    // create() then get(): a passkey minted and stored by `new_passkey_login_body`
    // must be signable when it comes back. This round-trips the whole vault path
    // minus the network — generate, encrypt into a POST body, decrypt the sealed
    // key back out, and sign an assertion that verifies. If the field encoding
    // were wrong, the stored passkey would be unusable; this catches it.
    #[test]
    fn a_created_passkey_stores_a_key_that_signs_a_verifiable_assertion() {
        use base64::Engine;
        use p256::ecdsa::signature::Verifier;
        use p256::ecdsa::{Signature, VerifyingKey};

        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let vault = Vault::new(
            SymmetricKey::from_bytes(&key_bytes).unwrap(),
            HashMap::new(),
            vec![],
            vec![],
            HashMap::new(),
        );

        let credential = crate::fido2::generate_credential(&mut rand::rngs::OsRng);
        let cose = credential.cose_public_key.clone();
        let passkey = NewPasskey {
            item_name: "Cloudflare".into(),
            rp_id: "dash.cloudflare.com".into(),
            rp_name: "Cloudflare".into(),
            user_name: "avikalpa".into(),
            user_display_name: "Avikalpa".into(),
            user_id: b"user-handle-bytes".to_vec(),
            credential_id: credential.credential_id.clone(),
            pkcs8_der: credential.pkcs8_der.to_vec(),
            account_username: Some("avikalpa".into()),
            creation_date: "2026-07-10T00:00:00.000Z".into(),
        };
        let body = vault.new_passkey_login_body(&passkey).unwrap();

        // The wire shape Vaultwarden expects: a login cipher with one passkey.
        assert_eq!(body["type"], 1);
        let fido2 = &body["login"]["fido2Credentials"][0];
        assert_eq!(fido2["creationDate"], "2026-07-10T00:00:00.000Z");

        // Every secret field is an EncString, not plaintext — decrypt them back.
        let dec = |field: &str| {
            let enc = EncString::parse(fido2[field].as_str().unwrap()).unwrap();
            user_key.decrypt_to_string(&enc).unwrap()
        };
        assert_eq!(dec("keyType"), "public-key");
        assert_eq!(dec("keyAlgorithm"), "ECDSA");
        assert_eq!(dec("keyCurve"), "P-256");
        assert_eq!(dec("counter"), "0");
        assert_eq!(dec("rpId"), "dash.cloudflare.com");
        // The private key never appears in the clear anywhere in the body.
        let key_value_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&credential.pkcs8_der);
        assert!(!body.to_string().contains(&key_value_b64));

        // THE round-trip: the sealed keyValue decrypts to the same PKCS#8 key,
        // and an assertion signed with it verifies under the COSE public key we
        // would have handed the RP at create time.
        let stored_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(dec("keyValue"))
            .unwrap();
        let client_data_hash = [0x07u8; 32];
        let assertion = crate::fido2::sign_assertion(
            &stored_key,
            "dash.cloudflare.com",
            &client_data_hash,
            0,
            crate::fido2::UserPresence::granted(true),
        )
        .unwrap();

        let mut sec1 = vec![0x04];
        sec1.extend_from_slice(&cose[10..42]); // x
        sec1.extend_from_slice(&cose[45..77]); // y
        let verifying = VerifyingKey::from_sec1_bytes(&sec1).unwrap();
        let sig = Signature::from_der(&assertion.signature).unwrap();
        let mut message = assertion.authenticator_data.clone();
        message.extend_from_slice(&client_data_hash);
        verifying.verify(&message, &sig).expect("stored key must sign a verifiable assertion");
    }

    // THE contract this whole struct exists for. `PUT /api/ciphers/{id}`
    // replaces the cipher wholesale (`cipher.notes = data.notes`), so anything
    // missing from the body is destroyed. An edit that touches only the
    // password must carry everything else back untouched.
    #[test]
    fn edit_body_preserves_every_field_it_was_not_asked_to_change() {
        let key_bytes = [0x5au8; 64];
        let vault = login_vault(&key_bytes);
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    password: Some("new-password".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        // Untouched fields ride back verbatim — including one we do not model.
        assert_eq!(body["notes"], "2.enc-notes");
        assert_eq!(body["favorite"], true);
        assert_eq!(body["reprompt"], 1);
        assert_eq!(body["fields"][0]["value"], "2.enc-value");
        assert_eq!(body["somethingBitwardenAddsIn2027"]["keep"], "me");
        assert_eq!(body["name"], "2.enc-name", "name was not asked to change");
        // Untouched login subfields survive too.
        assert_eq!(body["login"]["username"], "2.enc-user");
        assert_eq!(body["login"]["uris"][0]["uri"], "2.enc-uri");
        assert_eq!(body["login"]["fido2Credentials"][0]["credentialId"], "abc");

        // The password IS changed, and decrypts to the new value.
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let written = EncString::parse(body["login"]["password"].as_str().unwrap()).unwrap();
        assert_eq!(user_key.decrypt_to_string(&written).unwrap(), "new-password");

        // Server-managed keys never go back.
        for key in ["id", "object", "revisionDate", "creationDate", "deletedDate",
                    "collectionIds", "edit", "viewPassword"] {
            assert!(body.get(key).is_none(), "{key} must be stripped from the update body");
        }
        // ...except as the concurrency guard, which is how a stale client is
        // refused instead of clobbering a concurrent edit.
        assert_eq!(body["lastKnownRevisionDate"], "2026-07-09T15:52:49.123Z");
        // No plaintext leaked into the request.
        assert!(!body.to_string().contains("new-password"));
    }

    // Replacing a password pushes the OLD ciphertext onto password history,
    // reusing it verbatim rather than re-encrypting, and keeps the existing
    // entries below it.
    #[test]
    fn edit_body_prepends_the_old_password_to_history() {
        let key_bytes = [0x5au8; 64];
        let vault = login_vault(&key_bytes);
        let body = vault
            .edit_body("c1", &CipherEdit { password: Some("new".into()), ..Default::default() })
            .unwrap();

        let history = body["passwordHistory"].as_array().unwrap();
        assert_eq!(history.len(), 2, "old password prepended, prior entry kept");
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let remembered = EncString::parse(history[0]["password"].as_str().unwrap()).unwrap();
        assert_eq!(user_key.decrypt_to_string(&remembered).unwrap(), "old-password");
        assert!(history[0]["lastUsedDate"].as_str().unwrap().ends_with('Z'));
        assert_eq!(history[1]["password"], "2.older", "prior history survives");

        // An edit that does NOT touch the password leaves history exactly as-is.
        let renamed = vault
            .edit_body("c1", &CipherEdit { name: Some("New Name".into()), ..Default::default() })
            .unwrap();
        assert_eq!(renamed["passwordHistory"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn edit_body_caps_password_history() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let mut raw = raw_login_record();
        raw["passwordHistory"] = serde_json::json!(
            (0..PASSWORD_HISTORY_LIMIT).map(|i| serde_json::json!({"password": format!("2.old{i}")}))
                .collect::<Vec<_>>()
        );
        let cipher = RawCipher {
            raw,
            id: "c1".into(),
            item_type: 1,
            password: Some(seal(&key_bytes, "old-password")),
            ..Default::default()
        };
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], vec![], HashMap::new());
        let body = vault
            .edit_body("c1", &CipherEdit { password: Some("new".into()), ..Default::default() })
            .unwrap();
        assert_eq!(body["passwordHistory"].as_array().unwrap().len(), PASSWORD_HISTORY_LIMIT);
    }

    // An edited field must be sealed under the key that `items()` will use to
    // read it back. Encrypting under the user key when the cipher has its own
    // item key writes a value that then silently vanishes from the item list.
    #[test]
    fn edit_body_encrypts_under_the_cipher_key_not_the_user_key() {
        let user_bytes = [0x11u8; 64];
        let item_bytes = [0x77u8; 64];
        let user_key = SymmetricKey::from_bytes(&user_bytes).unwrap();
        let item_key = SymmetricKey::from_bytes(&item_bytes).unwrap();

        let cipher = RawCipher {
            raw: raw_login_record(),
            id: "c1".into(),
            item_type: 1,
            key: Some(super::seal(&user_bytes, &item_bytes)),
            name: Some(seal(&item_bytes, "Sealed Item")),
            password: Some(seal(&item_bytes, "under-item-key")),
            ..Default::default()
        };
        let vault = Vault::new(user_key.clone(), HashMap::new(), vec![cipher], vec![], HashMap::new());
        let body = vault
            .edit_body("c1", &CipherEdit { password: Some("rotated".into()), ..Default::default() })
            .unwrap();

        let written = EncString::parse(body["login"]["password"].as_str().unwrap()).unwrap();
        assert_eq!(item_key.decrypt_to_string(&written).unwrap(), "rotated");
        assert!(user_key.decrypt_to_string(&written).is_err(), "must NOT be under the user key");
        // The sealed item key rides back so the server keeps it.
        assert!(body["key"].is_string() || body.get("key").is_none());
    }

    // An organization cipher's fields are sealed under the ORG key.
    #[test]
    fn edit_body_encrypts_an_org_cipher_under_the_org_key() {
        let user_bytes = [0x11u8; 64];
        let org_bytes = [0x99u8; 64];
        let user_key = SymmetricKey::from_bytes(&user_bytes).unwrap();
        let org_key = SymmetricKey::from_bytes(&org_bytes).unwrap();
        let mut raw = raw_login_record();
        raw["organizationId"] = serde_json::json!("org1");

        let cipher = RawCipher {
            raw,
            id: "c1".into(),
            item_type: 1,
            organization_id: Some("org1".into()),
            name: Some(seal(&org_bytes, "Shared")),
            ..Default::default()
        };
        let mut org_keys = HashMap::new();
        org_keys.insert("org1".to_string(), org_key.clone());
        let vault = Vault::new(user_key.clone(), org_keys, vec![cipher], vec![], HashMap::new());

        let body = vault
            .edit_body("c1", &CipherEdit { name: Some("Renamed".into()), ..Default::default() })
            .unwrap();
        let written = EncString::parse(body["name"].as_str().unwrap()).unwrap();
        assert_eq!(org_key.decrypt_to_string(&written).unwrap(), "Renamed");
        assert!(user_key.decrypt_to_string(&written).is_err());
        assert_eq!(body["organizationId"], "org1", "org ownership must survive the edit");
    }

    // Vaultwarden has drifted between PascalCase and camelCase. A patch must
    // not leave the old-cased twin behind for the server to read instead.
    #[test]
    fn edit_body_replaces_a_pascal_case_twin() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let cipher = RawCipher {
            raw: serde_json::json!({
                "Id": "c1", "Type": 1, "Name": "2.old-name", "Notes": "2.keep",
                "RevisionDate": "2026-01-01T00:00:00.000Z",
                "Login": {"Username": "2.old-user"},
            }),
            id: "c1".into(),
            item_type: 1,
            ..Default::default()
        };
        let vault = Vault::new(user_key.clone(), HashMap::new(), vec![cipher], vec![], HashMap::new());
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    name: Some("Renamed".into()),
                    username: Some("newuser".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let object = body.as_object().unwrap();
        // Exactly one key for each concept, and it is the camelCase one.
        assert_eq!(object.keys().filter(|k| k.eq_ignore_ascii_case("name")).count(), 1);
        assert!(object.contains_key("name") && !object.contains_key("Name"));
        assert_eq!(object.keys().filter(|k| k.eq_ignore_ascii_case("login")).count(), 1);
        assert!(object.contains_key("lastKnownRevisionDate"));
        assert!(!object.contains_key("Id") && !object.contains_key("RevisionDate"));
        // The un-patched PascalCase field is preserved as it came.
        assert_eq!(body["Notes"], "2.keep");
        let written = EncString::parse(body["name"].as_str().unwrap()).unwrap();
        assert_eq!(user_key.decrypt_to_string(&written).unwrap(), "Renamed");
        let login = body["login"].as_object().unwrap();
        assert_eq!(login.keys().filter(|k| k.eq_ignore_ascii_case("username")).count(), 1);
    }

    #[test]
    fn edit_body_refuses_empty_values_unknown_items_and_non_logins() {
        let key_bytes = [0x5au8; 64];
        let vault = login_vault(&key_bytes);

        // Clearing a field is not expressible — it must not silently encrypt "".
        let empty = vault.edit_body("c1", &CipherEdit { notes: Some(String::new()), ..Default::default() });
        assert!(matches!(empty, Err(EditError::EmptyValue)));

        let unknown = vault.edit_body("nope", &CipherEdit { name: Some("x".into()), ..Default::default() });
        assert!(matches!(unknown, Err(EditError::UnknownItem(_))));

        // A secure note (type 2) has no login fields.
        let note = RawCipher {
            raw: serde_json::json!({"id": "n1", "type": 2}),
            id: "n1".into(),
            item_type: 2,
            ..Default::default()
        };
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let notes_vault = Vault::new(user_key, HashMap::new(), vec![note], vec![], HashMap::new());
        let bad = notes_vault.edit_body("n1", &CipherEdit { password: Some("x".into()), ..Default::default() });
        assert!(matches!(bad, Err(EditError::NotALogin(_))));
        // But its NOTES are editable.
        assert!(notes_vault
            .edit_body("n1", &CipherEdit { notes: Some("hello".into()), ..Default::default() })
            .is_ok());
    }

    // A cipher that never came from `sync` (no raw record) must fail loudly
    // rather than PUT a body that would erase the item's real contents.
    #[test]
    fn edit_body_refuses_a_cipher_with_no_raw_record() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let cipher = RawCipher { id: "c1".into(), item_type: 1, ..Default::default() };
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], vec![], HashMap::new());
        let result = vault.edit_body("c1", &CipherEdit { name: Some("x".into()), ..Default::default() });
        assert!(matches!(result, Err(EditError::NoRawRecord(_))));
    }

    // Notes are decrypted off the RAW record under the cipher key, and survive
    // a round trip through `edit_body` — the property the whole raw-retention
    // design exists to guarantee.
    #[test]
    fn notes_read_off_the_raw_record_and_survive_an_edit() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let mut raw = raw_login_record();
        raw["notes"] = serde_json::json!(seal(&key_bytes, "remember me").to_string());
        let cipher = RawCipher {
            raw,
            id: "c1".into(),
            item_type: 1,
            name: Some(seal(&key_bytes, "GitHub")),
            password: Some(seal(&key_bytes, "old-password")),
            ..Default::default()
        };
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], vec![], HashMap::new());
        assert_eq!(vault.notes("c1").as_deref(), Some("remember me"));
        assert!(vault.notes("nope").is_none());

        // A password-only edit carries the SAME encrypted notes back.
        let body = vault
            .edit_body("c1", &CipherEdit { password: Some("new".into()), ..Default::default() })
            .unwrap();
        let carried = EncString::parse(body["notes"].as_str().unwrap()).unwrap();
        let key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        assert_eq!(key.decrypt_to_string(&carried).unwrap(), "remember me");
    }

    #[test]
    fn edit_body_replaces_the_whole_uri_list() {
        let key_bytes = [0x5au8; 64];
        let vault = login_vault(&key_bytes);
        let body = vault
            .edit_body("c1", &CipherEdit { uri: Some("https://example.com".into()), ..Default::default() })
            .unwrap();
        let uris = body["login"]["uris"].as_array().unwrap();
        assert_eq!(uris.len(), 1);
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let written = EncString::parse(uris[0]["uri"].as_str().unwrap()).unwrap();
        assert_eq!(user_key.decrypt_to_string(&written).unwrap(), "https://example.com");
    }

    // The timestamp goes into other clients' password history, so its shape is
    // a compatibility surface, not a cosmetic detail.
    #[test]
    fn rfc3339_matches_known_instants() {
        use std::time::{Duration, UNIX_EPOCH};
        let at = |secs: u64, millis: u32| {
            rfc3339_millis_utc(UNIX_EPOCH + Duration::new(secs, millis * 1_000_000))
        };
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(at(1_783_612_369, 123), "2026-07-09T15:52:49.123Z");
        // A leap day, and the last second of a leap year.
        assert_eq!(at(1_709_164_800, 0), "2024-02-29T00:00:00.000Z");
        assert_eq!(at(1_735_689_599, 999), "2024-12-31T23:59:59.999Z");
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
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], vec![], HashMap::new());
        assert_eq!(vault.items()[0].name, "Sealed Item");
        assert_eq!(vault.password("c1").as_deref(), Some("under-item-key"));
    }
}
