//! RFC 6238 TOTP, generated from a decrypted vault secret.
//!
//! A cipher's `login.totp` field decrypts to either a bare base32 secret or an
//! `otpauth://totp/...` URI. Both are handled; URI parameters (`algorithm`,
//! `digits`, `period`) override the defaults (SHA-1, 6 digits, 30 s).

use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};

#[derive(Debug, thiserror::Error)]
pub enum TotpError {
    #[error("empty TOTP secret")]
    Empty,
    #[error("invalid base32 secret")]
    BadSecret,
    #[error("unsupported TOTP algorithm {0}")]
    BadAlgorithm(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Algorithm {
    Sha1,
    Sha256,
    Sha512,
}

/// A parsed authenticator configuration.
#[derive(Debug, Clone)]
pub struct Totp {
    secret: Vec<u8>,
    algorithm: Algorithm,
    digits: u32,
    period: u64,
}

impl Totp {
    /// Parse a decrypted `login.totp` value: an `otpauth://` URI or a bare
    /// base32 secret (with optional `steam://` prefix treated as base32).
    pub fn parse(value: &str) -> Result<Self, TotpError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(TotpError::Empty);
        }
        if let Some(rest) = value.strip_prefix("otpauth://") {
            return Self::parse_uri(rest);
        }
        let secret = value.strip_prefix("steam://").unwrap_or(value);
        Ok(Totp {
            secret: decode_base32(secret)?,
            algorithm: Algorithm::Sha1,
            digits: 6,
            period: 30,
        })
    }

    fn parse_uri(rest: &str) -> Result<Self, TotpError> {
        let query = rest.split_once('?').map(|(_, q)| q).unwrap_or("");
        let mut secret = None;
        let mut algorithm = Algorithm::Sha1;
        let mut digits = 6u32;
        let mut period = 30u64;
        for pair in query.split('&') {
            let Some((key, val)) = pair.split_once('=') else {
                continue;
            };
            match key.to_ascii_lowercase().as_str() {
                "secret" => secret = Some(decode_base32(val)?),
                "algorithm" => {
                    algorithm = match val.to_ascii_uppercase().as_str() {
                        "SHA1" => Algorithm::Sha1,
                        "SHA256" => Algorithm::Sha256,
                        "SHA512" => Algorithm::Sha512,
                        other => return Err(TotpError::BadAlgorithm(other.to_string())),
                    }
                }
                "digits" => digits = val.parse().unwrap_or(6).clamp(6, 10),
                "period" => period = val.parse().unwrap_or(30).max(1),
                _ => {}
            }
        }
        Ok(Totp {
            secret: secret.ok_or(TotpError::BadSecret)?,
            algorithm,
            digits,
            period,
        })
    }

    /// The code for a given Unix time (seconds). Split out from [`Self::now`]
    /// so it can be tested against RFC 6238's fixed timestamps.
    pub fn code_at(&self, unix_seconds: u64) -> String {
        let counter = unix_seconds / self.period;
        let msg = counter.to_be_bytes();
        let digest = match self.algorithm {
            Algorithm::Sha1 => {
                let mut m = <Hmac<Sha1>>::new_from_slice(&self.secret).expect("any key length");
                m.update(&msg);
                m.finalize().into_bytes().to_vec()
            }
            Algorithm::Sha256 => {
                let mut m = <Hmac<Sha256>>::new_from_slice(&self.secret).expect("any key length");
                m.update(&msg);
                m.finalize().into_bytes().to_vec()
            }
            Algorithm::Sha512 => {
                let mut m = <Hmac<Sha512>>::new_from_slice(&self.secret).expect("any key length");
                m.update(&msg);
                m.finalize().into_bytes().to_vec()
            }
        };
        // Dynamic truncation (RFC 4226 §5.3).
        let offset = (digest[digest.len() - 1] & 0x0f) as usize;
        let bin = ((u32::from(digest[offset]) & 0x7f) << 24)
            | (u32::from(digest[offset + 1]) << 16)
            | (u32::from(digest[offset + 2]) << 8)
            | u32::from(digest[offset + 3]);
        let modulo = 10u32.pow(self.digits);
        format!("{:0width$}", bin % modulo, width = self.digits as usize)
    }

    /// The code for the current wall-clock time, and the seconds until it rolls.
    pub fn now(&self) -> (String, u64) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let remaining = self.period - (secs % self.period);
        (self.code_at(secs), remaining)
    }
}

/// Decode RFC 4648 base32 (upper/lowercase, spaces and `=` padding ignored).
fn decode_base32(input: &str) -> Result<Vec<u8>, TotpError> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut buffer = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for ch in input.bytes() {
        let ch = ch.to_ascii_uppercase();
        if ch == b'=' || ch == b' ' || ch == b'-' {
            continue;
        }
        let Some(value) = ALPHABET.iter().position(|&c| c == ch) else {
            return Err(TotpError::BadSecret);
        };
        buffer = (buffer << 5) | value as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    if out.is_empty() {
        return Err(TotpError::BadSecret);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 Appendix B test vectors. The SHA-1 seed is the ASCII string
    // "12345678901234567890" = base32 GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ.
    #[test]
    fn rfc6238_sha1_vectors() {
        let totp = Totp {
            secret: b"12345678901234567890".to_vec(),
            algorithm: Algorithm::Sha1,
            digits: 8,
            period: 30,
        };
        assert_eq!(totp.code_at(59), "94287082");
        assert_eq!(totp.code_at(1111111109), "07081804");
        assert_eq!(totp.code_at(1111111111), "14050471");
        assert_eq!(totp.code_at(1234567890), "89005924");
        assert_eq!(totp.code_at(2000000000), "69279037");
    }

    #[test]
    fn base32_and_uri_parsing() {
        let bare = Totp::parse("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap();
        assert_eq!(bare.digits, 6);
        assert_eq!(bare.period, 30);
        // 6-digit truncation of the RFC vector at t=59.
        assert_eq!(bare.code_at(59), "287082");

        let uri = Totp::parse(
            "otpauth://totp/ACME:alice@example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&algorithm=SHA1&digits=8&period=30",
        )
        .unwrap();
        assert_eq!(uri.digits, 8);
        assert_eq!(uri.code_at(59), "94287082");

        assert!(matches!(Totp::parse(""), Err(TotpError::Empty)));
        assert!(Totp::parse("not base32 @@@").is_err());
    }
}
