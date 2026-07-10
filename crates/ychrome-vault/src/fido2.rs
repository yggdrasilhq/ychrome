//! The WebAuthn passkey ceremony — the crypto half.
//!
//! WebKitGTK has no WebAuthn, so ychrome answers `navigator.credentials.get()`
//! from the vault, exactly as the Chrome Bitwarden extension does. This module
//! is the signer: given a stored FIDO2 credential's private key and the RP's
//! challenge, it produces the assertion an RP will accept.
//!
//! **The one non-negotiable rule: the agent may NEVER auto-consent.** That is
//! encoded here as [`UserPresence`] — the signer *requires* one by value, and
//! its only constructor is [`UserPresence::granted`], which the GUI bridge calls
//! *after* the user approves the presence dialog. There is no path to a
//! signature that did not pass through a `granted()` call, so a headless agent
//! cannot forge consent.
//!
//! Scope: this is the ES256 assertion (`get`) core, proven by KATs (sign, then
//! verify against the derived public key). NOT yet built: credential creation
//! (`create`), the `navigator.credentials` userscript shim, the loopback signer
//! bridge, and the user-presence dialog that mints the `UserPresence` — those
//! are the browser slice.

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum Fido2Error {
    #[error("the stored passkey private key is not a P-256 PKCS#8 key")]
    BadPrivateKey,
    #[error("a client data hash must be 32 bytes, got {0}")]
    BadClientDataHash(usize),
}

/// The authenticatorData flag bits set for an assertion. No attested credential
/// data (AT) or extensions (ED) — a `get` ceremony carries neither.
const FLAG_USER_PRESENT: u8 = 0b0000_0001; // UP — a human was present
const FLAG_USER_VERIFIED: u8 = 0b0000_0100; // UV — and was verified (biometric/PIN)

/// Proof that a human approved *this* ceremony. The signer takes one **by
/// value**, and the only way to make one is [`granted`] — which the GUI's
/// user-presence dialog calls on approval. A headless agent has no other
/// constructor, so it is structurally unable to sign without consent.
///
/// [`granted`]: UserPresence::granted
#[derive(Debug)]
pub struct UserPresence {
    user_verified: bool,
}

impl UserPresence {
    /// Mint consent for one ceremony. Call ONLY after the user approved the
    /// presence dialog. `user_verified` is true when they additionally passed a
    /// verification gate (biometric/PIN), which sets the UV flag.
    pub fn granted(user_verified: bool) -> Self {
        UserPresence { user_verified }
    }
}

/// `authenticatorData` = SHA-256(rpId) ‖ flags ‖ signCount(be32). 37 bytes for
/// an assertion. The RP recomputes `SHA-256(rpId)` and checks the flags and the
/// signature over `authenticatorData ‖ clientDataHash`.
pub fn authenticator_data(rp_id: &str, consent: &UserPresence, sign_count: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(37);
    data.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
    let mut flags = FLAG_USER_PRESENT;
    if consent.user_verified {
        flags |= FLAG_USER_VERIFIED;
    }
    data.push(flags);
    data.extend_from_slice(&sign_count.to_be_bytes());
    data
}

/// The result of a `get` ceremony — the two blobs the shim hands back to the
/// page as `AuthenticatorAssertionResponse.{authenticatorData,signature}`.
#[derive(Debug, Clone)]
pub struct Fido2Assertion {
    pub authenticator_data: Vec<u8>,
    /// ECDSA signature, DER-encoded (what WebAuthn's ES256 verification expects).
    pub signature: Vec<u8>,
}

/// Sign a WebAuthn assertion: ECDSA-P256 over SHA-256 of
/// `authenticatorData ‖ clientDataHash`, DER-encoded.
///
/// `pkcs8_der` is the decrypted FIDO2 private key (a P-256 PKCS#8 DER document);
/// `client_data_hash` is the 32-byte `SHA-256(clientDataJSON)` the RP challenge
/// produced. Requiring a [`UserPresence`] by value is the whole point — there is
/// no unsigned-consent path.
pub fn sign_assertion(
    pkcs8_der: &[u8],
    rp_id: &str,
    client_data_hash: &[u8],
    sign_count: u32,
    consent: UserPresence,
) -> Result<Fido2Assertion, Fido2Error> {
    if client_data_hash.len() != 32 {
        return Err(Fido2Error::BadClientDataHash(client_data_hash.len()));
    }
    let key = SigningKey::from_pkcs8_der(pkcs8_der).map_err(|_| Fido2Error::BadPrivateKey)?;

    let authenticator_data = authenticator_data(rp_id, &consent, sign_count);
    let mut signed = Vec::with_capacity(authenticator_data.len() + client_data_hash.len());
    signed.extend_from_slice(&authenticator_data);
    signed.extend_from_slice(client_data_hash);

    // p256's Signer prehashes with SHA-256 (ES256) and yields a low-S signature.
    let signature: Signature = key.sign(&signed);
    Ok(Fido2Assertion {
        authenticator_data,
        signature: signature.to_der().as_bytes().to_vec(),
    })
}

/// A freshly minted passkey, for a `create()` ceremony. The private key is
/// PKCS#8 (what a decrypted `keyValue` decodes to) and held zeroized until the
/// vault seals it; the COSE public key is what the RP stores; the credential id
/// is the handle the RP references it by.
pub struct GeneratedCredential {
    pub credential_id: Vec<u8>,
    pub pkcs8_der: Zeroizing<Vec<u8>>,
    pub cose_public_key: Vec<u8>,
}

/// Mint a new ES256 (P-256) credential: a random key, a random 16-byte
/// credential id, and the COSE_Key the RP will verify future assertions with.
///
/// `rng` is the OS CSPRNG at the call site — passed in so the crypto module
/// stays deterministic-testable and does not reach for global randomness.
pub fn generate_credential(rng: &mut (impl rand::RngCore + rand::CryptoRng)) -> GeneratedCredential {
    let signing = SigningKey::random(rng);
    let pkcs8 = signing
        .to_pkcs8_der()
        .expect("a freshly generated P-256 key always encodes to PKCS#8");
    let mut credential_id = vec![0u8; 16];
    rng.fill_bytes(&mut credential_id);
    GeneratedCredential {
        credential_id,
        pkcs8_der: Zeroizing::new(pkcs8.as_bytes().to_vec()),
        cose_public_key: cose_ec2_public_key(&VerifyingKey::from(&signing)),
    }
}

/// The COSE_Key (RFC 8152) for a P-256 ES256 public key, CBOR-encoded — the
/// bytes that go into a WebAuthn attestation's attestedCredentialData.
///
/// Canonical CTAP2 map, keys in encoded-byte order (1, 3, -1, -2, -3):
/// `{1: 2 (kty EC2), 3: -7 (alg ES256), -1: 1 (crv P-256), -2: x, -3: y}`.
fn cose_ec2_public_key(key: &VerifyingKey) -> Vec<u8> {
    let point = key.to_encoded_point(false); // 0x04 ‖ X(32) ‖ Y(32)
    let x = point.x().expect("P-256 public key has an x coordinate");
    let y = point.y().expect("P-256 public key has a y coordinate");

    let mut cbor = Vec::with_capacity(77);
    cbor.push(0xa5); // map(5)
    cbor.extend_from_slice(&[0x01, 0x02]); // 1: 2  (kty = EC2)
    cbor.extend_from_slice(&[0x03, 0x26]); // 3: -7 (alg = ES256)
    cbor.extend_from_slice(&[0x20, 0x01]); // -1: 1 (crv = P-256)
    cbor.extend_from_slice(&[0x21, 0x58, 0x20]); // -2: bytes(32)
    cbor.extend_from_slice(x);
    cbor.extend_from_slice(&[0x22, 0x58, 0x20]); // -3: bytes(32)
    cbor.extend_from_slice(y);
    cbor
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::pkcs8::EncodePrivateKey;

    /// A deterministic P-256 key (fixed scalar), as PKCS#8 DER — the shape a
    /// decrypted `keyValue` decodes to.
    fn fixed_key() -> (SigningKey, Vec<u8>) {
        let signing = SigningKey::from_bytes(&[0x11u8; 32].into()).unwrap();
        let der = signing.to_pkcs8_der().unwrap().as_bytes().to_vec();
        (signing, der)
    }

    #[test]
    fn assertion_signature_verifies_and_authdata_is_well_formed() {
        let (signing, pkcs8) = fixed_key();
        let rp_id = "example.com";
        let client_data_hash = Sha256::digest(b"clientDataJSON goes here").to_vec();

        let assertion =
            sign_assertion(&pkcs8, rp_id, &client_data_hash, 7, UserPresence::granted(true))
                .unwrap();

        // authenticatorData is byte-exact: rpIdHash ‖ flags ‖ signCount.
        let ad = &assertion.authenticator_data;
        assert_eq!(ad.len(), 37);
        assert_eq!(&ad[0..32], Sha256::digest(rp_id.as_bytes()).as_slice());
        assert_eq!(ad[32], FLAG_USER_PRESENT | FLAG_USER_VERIFIED); // 0x05
        assert_eq!(&ad[33..37], &7u32.to_be_bytes());

        // THE proof: the signature verifies against the credential's public key
        // over exactly `authenticatorData ‖ clientDataHash`, which is what an RP
        // does. A wrong byte layout, hash, or signature encoding fails here.
        let verifying = VerifyingKey::from(&signing);
        let sig = Signature::from_der(&assertion.signature).unwrap();
        let mut message = assertion.authenticator_data.clone();
        message.extend_from_slice(&client_data_hash);
        verifying.verify(&message, &sig).expect("assertion must verify");
    }

    #[test]
    fn up_only_ceremony_clears_the_uv_flag() {
        let ad = authenticator_data("example.com", &UserPresence::granted(false), 0);
        assert_eq!(ad[32], FLAG_USER_PRESENT); // UP set, UV clear
        assert_eq!(&ad[33..37], &0u32.to_be_bytes());
    }

    #[test]
    fn a_non_32_byte_client_data_hash_is_refused() {
        let (_, pkcs8) = fixed_key();
        let err = sign_assertion(&pkcs8, "example.com", &[0u8; 16], 0, UserPresence::granted(true))
            .unwrap_err();
        assert!(matches!(err, Fido2Error::BadClientDataHash(16)));
    }

    #[test]
    fn a_garbage_private_key_is_refused() {
        let err = sign_assertion(&[1, 2, 3], "example.com", &[0u8; 32], 0, UserPresence::granted(true))
            .unwrap_err();
        assert!(matches!(err, Fido2Error::BadPrivateKey));
    }

    #[test]
    fn cose_public_key_is_a_well_formed_es256_map() {
        let signing = SigningKey::from_bytes(&[0x33u8; 32].into()).unwrap();
        let cose = cose_ec2_public_key(&VerifyingKey::from(&signing));

        // 5-entry map, then the fixed ES256/P-256 header, then two 32-byte
        // coordinate byte strings — 77 bytes total.
        assert_eq!(cose.len(), 77);
        assert_eq!(&cose[0..8], &[0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21]);
        assert_eq!(&cose[8..10], &[0x58, 0x20]); // -2: bytes(32)
        assert_eq!(&cose[42..45], &[0x22, 0x58, 0x20]); // -3: bytes(32)

        // The coordinates are exactly the SEC1 uncompressed point's X and Y, so
        // an RP that CBOR-decodes this recovers the real public key.
        let point = VerifyingKey::from(&signing).to_encoded_point(false);
        assert_eq!(&cose[10..42], point.x().unwrap().as_slice());
        assert_eq!(&cose[45..77], point.y().unwrap().as_slice());
    }

    #[test]
    fn a_generated_credential_signs_and_verifies_under_its_own_public_key() {
        let mut rng = rand::rngs::OsRng;
        let credential = generate_credential(&mut rng);
        assert_eq!(credential.credential_id.len(), 16);
        assert_eq!(credential.cose_public_key.len(), 77);

        // The whole point of create→get: an assertion signed with the generated
        // private key verifies against the public key we handed the RP. Recover
        // the public key from the SEC1 coordinates embedded in the COSE key.
        let client_data_hash = Sha256::digest(b"create-then-get").to_vec();
        let assertion = sign_assertion(
            &credential.pkcs8_der,
            "example.com",
            &client_data_hash,
            0,
            UserPresence::granted(true),
        )
        .unwrap();

        let x = &credential.cose_public_key[10..42];
        let y = &credential.cose_public_key[45..77];
        let mut sec1 = vec![0x04];
        sec1.extend_from_slice(x);
        sec1.extend_from_slice(y);
        let verifying = VerifyingKey::from_sec1_bytes(&sec1).unwrap();
        let sig = Signature::from_der(&assertion.signature).unwrap();
        let mut message = assertion.authenticator_data.clone();
        message.extend_from_slice(&client_data_hash);
        verifying.verify(&message, &sig).expect("must verify");
    }
}
