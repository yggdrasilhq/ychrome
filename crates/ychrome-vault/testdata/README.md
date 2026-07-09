Test fixtures for the RSA path (organization keys).

* `rsa_pkcs8_private_key.b64` — a throwaway 2048-bit RSA key in PKCS#8 DER,
  base64'd. The shape `profile.privateKey` decrypts to.
* `rsa_oaep_sha1_org_key.b64` — 64 bytes of `0x99` sealed to that key's public
  half with RSA-OAEP-SHA1, i.e. an organization key exactly as Vaultwarden
  serves it (`"4." + this`).

Generated with openssl, NOT with our own code, so the test is a genuine
cross-implementation check rather than a round-trip against ourselves. The key
protects nothing.
