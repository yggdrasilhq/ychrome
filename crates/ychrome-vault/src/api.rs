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

use crate::crypto::{EncString, Kdf};
use crate::model::RawCipher;

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

    /// `GET /api/sync` → the raw ciphers and folder names (still encrypted).
    /// Returns `(ciphers, folder_id -> encrypted_name)`.
    pub fn sync(
        &self,
        access_token: &str,
    ) -> Result<(Vec<RawCipher>, HashMap<String, EncString>), ApiError> {
        let url = format!("{}/api/sync?excludeDomains=true", self.base);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(access_token)
            .send()
            .map_err(|error| ApiError::Network(error.to_string()))?;
        let value = json_or_err(resp)?;

        let mut folders = HashMap::new();
        for folder in get_array(&value, "folders") {
            if let (Some(id), Some(name)) = (get_str(folder, "id"), get_str(folder, "name")) {
                if let Ok(enc) = EncString::parse(name) {
                    folders.insert(id.to_string(), enc);
                }
            }
        }

        let mut ciphers = Vec::new();
        for cipher in get_array(&value, "ciphers") {
            // Deleted items carry a deletedDate; skip them.
            if get_str(cipher, "deletedDate").is_some() {
                continue;
            }
            let login = get_ci(cipher, "login");
            ciphers.push(RawCipher {
                id: get_str(cipher, "id").unwrap_or_default().to_string(),
                folder_id: get_str(cipher, "folderId").map(str::to_string),
                item_type: get_u64(cipher, "type").unwrap_or(1) as u8,
                key: EncString::parse_opt(get_str(cipher, "key")).ok().flatten(),
                name: EncString::parse_opt(get_str(cipher, "name")).ok().flatten(),
                username: login
                    .and_then(|l| EncString::parse_opt(get_str(l, "username")).ok().flatten()),
                password: login
                    .and_then(|l| EncString::parse_opt(get_str(l, "password")).ok().flatten()),
                totp: login.and_then(|l| EncString::parse_opt(get_str(l, "totp")).ok().flatten()),
                uris: login
                    .map(|l| {
                        get_array(l, "uris")
                            .iter()
                            .filter_map(|u| EncString::parse_opt(get_str(u, "uri")).ok().flatten())
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }
        Ok((ciphers, folders))
    }
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
}
