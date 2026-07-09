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
    folder_names: HashMap<String, EncString>,
}

impl Vault {
    pub fn new(
        user_key: SymmetricKey,
        organization_keys: HashMap<String, SymmetricKey>,
        ciphers: Vec<RawCipher>,
        folders: HashMap<String, EncString>,
    ) -> Self {
        Vault {
            user_key,
            organization_keys,
            ciphers,
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
        folders: HashMap<String, EncString>,
    ) {
        self.organization_keys = organization_keys;
        self.ciphers = ciphers;
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
        };
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], folders);

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
        let blind = Vault::new(user_key.clone(), HashMap::new(), ciphers.clone(), HashMap::new());
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
        let seeing = Vault::new(user_key, org_keys, ciphers, HashMap::new());
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
        let vault = Vault::new(user_key.clone(), HashMap::new(), vec![], HashMap::new());

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
        let vault = Vault::new(user_key, HashMap::new(), vec![], HashMap::new());
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
        Vault::new(user_key, HashMap::new(), vec![cipher], HashMap::new())
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
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], HashMap::new());
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
        let vault = Vault::new(user_key.clone(), HashMap::new(), vec![cipher], HashMap::new());
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
        let vault = Vault::new(user_key.clone(), org_keys, vec![cipher], HashMap::new());

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
        let vault = Vault::new(user_key.clone(), HashMap::new(), vec![cipher], HashMap::new());
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
        let notes_vault = Vault::new(user_key, HashMap::new(), vec![note], HashMap::new());
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
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], HashMap::new());
        let result = vault.edit_body("c1", &CipherEdit { name: Some("x".into()), ..Default::default() });
        assert!(matches!(result, Err(EditError::NoRawRecord(_))));
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
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], HashMap::new());
        assert_eq!(vault.items()[0].name, "Sealed Item");
        assert_eq!(vault.password("c1").as_deref(), Some("under-item-key"));
    }
}
