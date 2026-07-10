//! The Bitwarden/Vaultwarden HTTP surface: `prelogin`, the identity token
//! endpoint, and `sync`. Blocking reqwest — callers already run vault work on a
//! blocking task.
//!
//! Vaultwarden has drifted between PascalCase and camelCase JSON across
//! versions (`Key` vs `key`, `KdfIterations` vs `kdfIterations`), so responses
//! are navigated case-insensitively rather than deserialized into a fixed
//! shape.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine as _;
use serde_json::Value;

use crate::crypto::{AsymEncString, EncString, Kdf};
use crate::model::{RawCipher, RawFido2Credential};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("network: {0}")]
    Network(String),
    #[error("server returned {status}: {body}")]
    Http { status: u16, body: String },
    #[error("invalid email or master password")]
    BadCredentials,
    #[error("this account requires two-factor authentication, which is not supported yet")]
    TwoFactorRequired,
    #[error("unexpected response: {0}")]
    Malformed(String),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
}

/// KDF parameters returned by `prelogin`.
#[derive(Debug, Clone)]
pub struct Prelogin {
    pub kdf: Kdf,
}

/// The successful result of the identity token endpoint.
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// The user's symmetric key, encrypted under the stretched master key.
    pub protected_user_key: EncString,
}

/// Everything `sync` returns, still encrypted.
pub struct SyncResponse {
    pub ciphers: Vec<RawCipher>,
    /// Soft-deleted ciphers (each carries a `deletedDate`). Separated from
    /// `ciphers` here rather than dropped, so the vault can offer `restore` and
    /// `list --trashed`. A hard-deleted item is absent from `sync` entirely.
    pub trashed: Vec<RawCipher>,
    /// `folder_id -> encrypted name` (always under the user key).
    pub folders: HashMap<String, EncString>,
    /// The user's RSA private key, sealed under the user key. Absent on an
    /// account that has never had one.
    pub private_key: Option<EncString>,
    /// `organization_id -> that org's symmetric key`, sealed to the user's
    /// public key.
    pub organization_keys: HashMap<String, AsymEncString>,
}

/// A thin client bound to one server base URL.
pub struct Client {
    base: String,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn new(base_url: &str) -> Result<Self, ApiError> {
        let base = base_url.trim().trim_end_matches('/').to_string();
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("yggterm-vault")
            .build()
            .map_err(|error| ApiError::Network(error.to_string()))?;
        Ok(Client { base, http })
    }

    /// `POST /identity/accounts/prelogin` → KDF parameters for the email.
    pub fn prelogin(&self, email: &str) -> Result<Prelogin, ApiError> {
        let url = format!("{}/identity/accounts/prelogin", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "email": email }))
            .send()
            .map_err(|error| ApiError::Network(error.to_string()))?;
        let value = json_or_err(resp)?;
        let kdf_type = get_u64(&value, "kdf").unwrap_or(0) as u32;
        let iterations = get_u64(&value, "kdfIterations").unwrap_or(600_000) as u32;
        let memory = get_u64(&value, "kdfMemory").map(|v| v as u32);
        let parallelism = get_u64(&value, "kdfParallelism").map(|v| v as u32);
        let kdf = Kdf::from_prelogin(kdf_type, iterations, memory, parallelism)?;
        Ok(Prelogin { kdf })
    }

    /// `POST /identity/connect/token` (password grant). `master_password_hash`
    /// is the base64 login hash — never the master password itself.
    pub fn token(
        &self,
        email: &str,
        master_password_hash: &str,
        device_id: &str,
    ) -> Result<TokenResponse, ApiError> {
        let url = format!("{}/identity/connect/token", self.base);
        let auth_email = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(email.as_bytes());
        let body = form_urlencode(&[
            ("grant_type", "password"),
            ("username", email),
            ("password", master_password_hash),
            ("scope", "api offline_access"),
            ("client_id", "web"),
            ("deviceType", "8"), // LinuxDesktop
            ("deviceIdentifier", device_id),
            ("deviceName", "yggterm"),
        ]);
        let resp = self
            .http
            .post(&url)
            .header("Auth-Email", auth_email)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .map_err(|error| ApiError::Network(error.to_string()))?;

        let status = resp.status();
        let value: Value = resp
            .json()
            .map_err(|error| ApiError::Malformed(error.to_string()))?;
        if !status.is_success() {
            // Two-factor and bad-credential cases both come back as 400.
            if value.get("TwoFactorProviders").is_some()
                || value.get("twoFactorProviders2").is_some()
                || get_str(&value, "error_description")
                    .map(|d| d.to_lowercase().contains("two factor"))
                    .unwrap_or(false)
            {
                return Err(ApiError::TwoFactorRequired);
            }
            if get_str(&value, "error")
                .map(|e| e == "invalid_grant")
                .unwrap_or(false)
            {
                return Err(ApiError::BadCredentials);
            }
            return Err(ApiError::Http {
                status: status.as_u16(),
                body: value.to_string(),
            });
        }

        let access_token = get_str(&value, "access_token")
            .ok_or_else(|| ApiError::Malformed("token response has no access_token".into()))?
            .to_string();
        let refresh_token = get_str(&value, "refresh_token").map(str::to_string);
        let protected = get_str(&value, "Key")
            .ok_or_else(|| ApiError::Malformed("token response has no user Key".into()))?;
        let protected_user_key = EncString::parse(protected)?;
        Ok(TokenResponse {
            access_token,
            refresh_token,
            protected_user_key,
        })
    }

    /// `GET /api/sync` → everything needed to open the vault, still encrypted.
    pub fn sync(&self, access_token: &str) -> Result<SyncResponse, ApiError> {
        let url = format!("{}/api/sync?excludeDomains=true", self.base);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(access_token)
            .send()
            .map_err(|error| ApiError::Network(error.to_string()))?;
        let value = json_or_err(resp)?;
        Ok(parse_sync(&value))
    }

    /// `POST /api/ciphers` → the created cipher's id. `body` must already be
    /// encrypted (every string field an EncString); this call never sees
    /// plaintext.
    pub fn create_cipher(&self, access_token: &str, body: &Value) -> Result<String, ApiError> {
        let url = format!("{}/api/ciphers", self.base);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .map_err(|error| ApiError::Network(error.to_string()))?;
        let value = json_or_err(resp)?;
        Ok(get_str(&value, "id")
            .ok_or_else(|| ApiError::Malformed("create response has no id".into()))?
            .to_string())
    }

    /// `PUT /api/ciphers/{id}` — replaces the WHOLE cipher.
    ///
    /// The server assigns unconditionally (`cipher.notes = data.notes`), so a
    /// field missing from `body` is not "left alone", it is DESTROYED. Build
    /// `body` with [`crate::model::Vault::edit_body`], which patches the raw
    /// record `sync` returned rather than rebuilding one from the fields this
    /// client happens to model.
    pub fn update_cipher(
        &self,
        access_token: &str,
        id: &str,
        body: &Value,
    ) -> Result<(), ApiError> {
        let url = format!("{}/api/ciphers/{id}", self.base);
        let resp = self
            .http
            .put(&url)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .map_err(|error| ApiError::Network(error.to_string()))?;
        ok_or_err(resp)
    }

    /// Delete a cipher. The two routes are NOT the same operation, and the
    /// difference is unrecoverable — verified against the deployed vaultwarden
    /// commit (`f21a3ada`, 2025.12.0), not from memory:
    ///
    /// * `permanent == false` → `PUT /api/ciphers/{id}/delete` → `SoftSingle`:
    ///   the item moves to the trash and can be restored from any client.
    /// * `permanent == true`  → `DELETE /api/ciphers/{id}` → `HardSingle`:
    ///   the item is gone, with no trash copy.
    ///
    /// Soft is the default everywhere above this call.
    pub fn delete_cipher(
        &self,
        access_token: &str,
        id: &str,
        permanent: bool,
    ) -> Result<(), ApiError> {
        let resp = if permanent {
            self.http
                .delete(format!("{}/api/ciphers/{id}", self.base))
                .bearer_auth(access_token)
                .send()
        } else {
            self.http
                .put(format!("{}/api/ciphers/{id}/delete", self.base))
                .bearer_auth(access_token)
                .send()
        }
        .map_err(|error| ApiError::Network(error.to_string()))?;
        ok_or_err(resp)
    }

    /// Restore a soft-deleted cipher: `PUT /api/ciphers/{id}/restore` — the
    /// inverse of a `SoftSingle` delete, verified against the same deployed
    /// vaultwarden commit. Only a trashed item can be restored; a hard-deleted
    /// one is gone, and the server answers with an error that surfaces here.
    pub fn restore_cipher(&self, access_token: &str, id: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .put(format!("{}/api/ciphers/{id}/restore", self.base))
            .bearer_auth(access_token)
            .send()
            .map_err(|error| ApiError::Network(error.to_string()))?;
        ok_or_err(resp)
    }
}

/// Parse a `GET /api/sync` document into a [`SyncResponse`]. Pure (no network),
/// so the cipher/trash split and the org/folder extraction are unit-testable.
///
/// Every sub-parse is lenient: a folder or org key that will not parse is
/// dropped rather than failing the whole sync, matching the vault's
/// decrypt-what-you-can posture (`Vault::diagnose` accounts for the gap).
fn parse_sync(value: &serde_json::Value) -> SyncResponse {
    // The user's RSA private key (sealed under the user key) and, per
    // organization, that org's symmetric key (sealed to the user's public key).
    // Without these, every organization cipher is undecryptable — which is
    // exactly how 59 of them silently vanished from the item list.
    let profile = get_ci(value, "profile");
    let private_key = profile
        .and_then(|profile| EncString::parse_opt(get_str(profile, "privateKey")).ok().flatten());
    let mut organization_keys = HashMap::new();
    if let Some(profile) = profile {
        for organization in get_array(profile, "organizations") {
            if let (Some(id), Some(key)) =
                (get_str(organization, "id"), get_str(organization, "key"))
                && let Ok(key) = AsymEncString::parse(key)
            {
                organization_keys.insert(id.to_string(), key);
            }
        }
    }

    let mut folders = HashMap::new();
    for folder in get_array(value, "folders") {
        if let (Some(id), Some(name)) = (get_str(folder, "id"), get_str(folder, "name")) {
            if let Ok(enc) = EncString::parse(name) {
                folders.insert(id.to_string(), enc);
            }
        }
    }

    // A soft-deleted item carries a `deletedDate`. It is not dropped — it goes
    // to the `trashed` bucket so `restore` can find it by name and
    // `list --trashed` can show it. A hard delete removes it from `sync`.
    let mut ciphers = Vec::new();
    let mut trashed = Vec::new();
    for cipher in get_array(value, "ciphers") {
        let parsed = parse_raw_cipher(cipher);
        if get_str(cipher, "deletedDate").is_some() {
            trashed.push(parsed);
        } else {
            ciphers.push(parsed);
        }
    }
    SyncResponse {
        ciphers,
        trashed,
        folders,
        private_key,
        organization_keys,
    }
}

/// Parse one `sync` cipher record into a [`RawCipher`], keeping the untouched
/// JSON alongside the fields this client models. Live and trashed ciphers are
/// parsed identically — only which bucket they land in differs.
fn parse_raw_cipher(cipher: &serde_json::Value) -> RawCipher {
    let login = get_ci(cipher, "login");
    RawCipher {
        // The whole record, verbatim. An update PUT replaces the entire cipher,
        // so the fields this client does not model (notes, custom fields,
        // favorite, password history, and anything Bitwarden adds later) can
        // only survive an edit by being carried back from here. See
        // `Vault::edit_body`.
        raw: cipher.clone(),
        id: get_str(cipher, "id").unwrap_or_default().to_string(),
        folder_id: get_str(cipher, "folderId").map(str::to_string),
        organization_id: get_str(cipher, "organizationId").map(str::to_string),
        item_type: get_u64(cipher, "type").unwrap_or(1) as u8,
        key: EncString::parse_opt(get_str(cipher, "key")).ok().flatten(),
        name: EncString::parse_opt(get_str(cipher, "name")).ok().flatten(),
        username: login.and_then(|l| EncString::parse_opt(get_str(l, "username")).ok().flatten()),
        password: login.and_then(|l| EncString::parse_opt(get_str(l, "password")).ok().flatten()),
        totp: login.and_then(|l| EncString::parse_opt(get_str(l, "totp")).ok().flatten()),
        uris: login
            .map(|l| {
                get_array(l, "uris")
                    .iter()
                    .filter_map(|u| EncString::parse_opt(get_str(u, "uri")).ok().flatten())
                    .collect()
            })
            .unwrap_or_default(),
        fido2: login.map(parse_fido2).unwrap_or_default(),
    }
}

/// Parse `login.fido2Credentials[]` into the still-encrypted passkey records.
/// Every string field is an EncString except `creationDate` (plaintext), and a
/// field that will not parse is simply dropped to `None` rather than failing.
fn parse_fido2(login: &serde_json::Value) -> Vec<RawFido2Credential> {
    let enc = |c: &serde_json::Value, k: &str| EncString::parse_opt(get_str(c, k)).ok().flatten();
    get_array(login, "fido2Credentials")
        .iter()
        .map(|c| RawFido2Credential {
            credential_id: enc(c, "credentialId"),
            rp_id: enc(c, "rpId"),
            rp_name: enc(c, "rpName"),
            user_name: enc(c, "userName"),
            user_display_name: enc(c, "userDisplayName"),
            user_handle: enc(c, "userHandle"),
            counter: enc(c, "counter"),
            discoverable: enc(c, "discoverable"),
            key_type: enc(c, "keyType"),
            key_algorithm: enc(c, "keyAlgorithm"),
            key_curve: enc(c, "keyCurve"),
            key_value: enc(c, "keyValue"),
            creation_date: get_str(c, "creationDate").map(str::to_string),
        })
        .collect()
}

/// A write endpoint that answers with an empty body on success.
fn ok_or_err(resp: reqwest::blocking::Response) -> Result<(), ApiError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body = resp.text().unwrap_or_default();
    Err(ApiError::Http {
        status: status.as_u16(),
        body: body.chars().take(400).collect(),
    })
}

/// `application/x-www-form-urlencoded` body. Percent-encodes every byte outside
/// the unreserved set, so a base64 password hash (`+`, `/`, `=`) and the space
/// in `scope` survive intact.
fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    fn encode(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(byte as char)
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn json_or_err(resp: reqwest::blocking::Response) -> Result<Value, ApiError> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(ApiError::Http {
            status: status.as_u16(),
            body: body.chars().take(400).collect(),
        });
    }
    resp.json()
        .map_err(|error| ApiError::Malformed(error.to_string()))
}

/// Case-insensitive object-key lookup, for Vaultwarden's casing drift.
fn get_ci<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    let obj = value.as_object()?;
    if let Some(v) = obj.get(key) {
        return Some(v);
    }
    obj.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn get_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    get_ci(value, key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn get_u64(value: &Value, key: &str) -> Option<u64> {
    let v = get_ci(value, key)?;
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn get_array<'a>(value: &'a Value, key: &str) -> Vec<&'a Value> {
    get_ci(value, key)
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_navigation() {
        let v: Value = serde_json::from_str(
            r#"{"Key":"2.a|b|c","KdfIterations":"600000","Ciphers":[{"Id":"x"}]}"#,
        )
        .unwrap();
        assert_eq!(get_str(&v, "key"), Some("2.a|b|c"));
        assert_eq!(get_str(&v, "KEY"), Some("2.a|b|c"));
        assert_eq!(get_u64(&v, "kdfIterations"), Some(600_000));
        assert_eq!(get_array(&v, "ciphers").len(), 1);
        assert_eq!(get_str(get_array(&v, "ciphers")[0], "id"), Some("x"));
    }

    #[test]
    fn get_u64_accepts_number_or_string() {
        let v: Value = serde_json::from_str(r#"{"a":5,"b":"7"}"#).unwrap();
        assert_eq!(get_u64(&v, "a"), Some(5));
        assert_eq!(get_u64(&v, "b"), Some(7));
        assert_eq!(get_u64(&v, "c"), None);
    }

    #[test]
    fn sync_splits_live_from_trashed_by_deleted_date() {
        // A soft-deleted item carries a non-null deletedDate; a live one does
        // not (absent or explicitly null). The split must key on presence, not
        // on the field merely existing as null — vaultwarden emits `null` for
        // live items.
        let doc: Value = serde_json::from_str(
            r#"{
                "ciphers": [
                    {"id": "live-1", "type": 1, "deletedDate": null},
                    {"id": "trashed-1", "type": 1, "deletedDate": "2026-07-10T00:00:00Z"},
                    {"id": "live-2", "type": 1}
                ]
            }"#,
        )
        .unwrap();

        let sync = parse_sync(&doc);
        let live: Vec<&str> = sync.ciphers.iter().map(|c| c.id.as_str()).collect();
        let trashed: Vec<&str> = sync.trashed.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(live, ["live-1", "live-2"]);
        assert_eq!(trashed, ["trashed-1"]);
    }

    #[test]
    fn sync_parses_stored_passkeys_into_the_fido2_bucket() {
        let doc: Value = serde_json::from_str(
            r#"{"ciphers":[
                {"id":"has-pk","type":1,"login":{"fido2Credentials":[
                    {"credentialId":"2.aa|bb|cc","rpId":"2.dd|ee|ff",
                     "creationDate":"2026-07-10T00:00:00Z"}
                ]}},
                {"id":"no-pk","type":1,"login":{"username":"2.gg|hh|ii"}}
            ]}"#,
        )
        .unwrap();

        let sync = parse_sync(&doc);
        let by_id = |id: &str| sync.ciphers.iter().find(|c| c.id == id).unwrap();
        // The credential lands in the fido2 bucket; the plaintext creationDate
        // rides along even though the encrypted fields are placeholders here.
        assert_eq!(by_id("has-pk").fido2.len(), 1);
        assert_eq!(
            by_id("has-pk").fido2[0].creation_date.as_deref(),
            Some("2026-07-10T00:00:00Z")
        );
        assert!(by_id("no-pk").fido2.is_empty());
    }
}
