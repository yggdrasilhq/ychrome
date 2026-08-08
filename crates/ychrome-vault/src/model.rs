//! The decrypted vault held in memory after unlock.
//!
//! The metadata list ([`VaultItem`]) never carries a password or TOTP secret;
//! those are decrypted on demand per item, so a screenshot or a leaked UI state
//! cannot spill them. Item-level keys are resolved exactly as Bitwarden does: a
//! cipher may carry its own `key` (encrypted under the user key), and its fields
//! are then encrypted under that item key rather than the user key directly.

use std::collections::HashMap;

use zeroize::Zeroizing;

use crate::crypto::{CryptoError, EncString, SymmetricKey};
use crate::totp::Totp;
use ychrome_vault_proto::{CIPHER_TYPE_CARD, CIPHER_TYPE_LOGIN, CIPHER_TYPE_NOTE};

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
    /// `archivedDate` — plaintext, server-owned, `None` for an ordinary item.
    /// Bitwarden's archive is a third bucket beside live and trashed, and the
    /// server keeps archived ciphers in the LIVE list with this set.
    pub archived_date: Option<String>,
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

// The cipher `type` discriminants are imported at the top of this file from the
// wire crate: they cross the agent socket in `VaultItem::item_type`, and the
// browser's sidebar decides which fill button a row gets from the SAME numbers.
// One owner, or both ends would spell "3 means card" and could drift.

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

/// A field an edit REMOVES rather than replaces.
///
/// Setting and clearing are genuinely different operations and the "no empty
/// value" rule cannot express the second: `--notes ""` would encrypt an empty
/// string, which is a stored value, not an absent one. So clearing is asked for
/// by name — and by ONE name, this enum, rather than a `clear_x` flag per field
/// that the CLI, the wire and the patcher could each spell differently.
///
/// There is deliberately no `Password`. Removing a password destroys a secret
/// with nothing to recover it from: `passwordHistory` records a REPLACEMENT, and
/// an item with no password at all is what `rm` is for (which trashes, and is
/// restorable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClearField {
    Notes,
    Totp,
    Username,
    /// The whole uri list.
    Uri,
    /// Move the item out of every folder (back to "no folder").
    Folder,
}

impl ClearField {
    /// The name the user typed and the error messages quote. One owner, so the
    /// CLI's accepted values, the wire and the refusals cannot drift.
    pub const fn as_str(self) -> &'static str {
        match self {
            ClearField::Notes => "notes",
            ClearField::Totp => "totp",
            ClearField::Username => "username",
            ClearField::Uri => "uri",
            ClearField::Folder => "folder",
        }
    }

    /// Parse the same spelling back off the wire. `None` for anything else —
    /// a client naming a field this build does not know must be refused, never
    /// silently ignored.
    pub fn parse(name: &str) -> Option<Self> {
        [
            ClearField::Notes,
            ClearField::Totp,
            ClearField::Username,
            ClearField::Uri,
            ClearField::Folder,
        ]
        .into_iter()
        .find(|candidate| candidate.as_str() == name)
    }

    /// Only meaningful on a login cipher.
    fn is_login_only(self) -> bool {
        matches!(
            self,
            ClearField::Totp | ClearField::Username | ClearField::Uri
        )
    }
}

/// Which custom-field type a [`FieldEdit::Set`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Create a plain-text field. On a field that ALREADY exists the stored type
    /// wins, so setting a value on a hidden field cannot silently downgrade a
    /// secret into one every client renders in the clear.
    Text,
    /// Create the field hidden, or convert an existing one to hidden.
    Hidden,
}

/// One change to an item's custom fields. Names match case-insensitively, the
/// same way `fields --field-name` reads them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldEdit {
    /// Replace the named field's value, appending the field if it is absent.
    Set {
        name: String,
        value: String,
        kind: FieldKind,
    },
    /// Drop the named field entirely. An absent name is an ERROR, not a no-op:
    /// "there was nothing to remove" and "removed it" are different facts, and
    /// reporting the second for the first is the lie-of-success this crate
    /// treats as worse than a failure.
    Remove { name: String },
}

impl FieldEdit {
    pub fn name(&self) -> &str {
        match self {
            FieldEdit::Set { name, .. } | FieldEdit::Remove { name } => name,
        }
    }
}

/// A change to an existing cipher. Only the named parts are touched; every
/// other field of the item survives verbatim.
///
/// Setting a field to `Some("")` is rejected rather than quietly encrypting an
/// empty string — that is what [`ClearField`] is for, and guessing between the
/// two would be the kind of silent data loss this whole struct exists to
/// prevent.
#[derive(Debug, Clone, Default)]
pub struct CipherEdit {
    pub name: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub totp: Option<String>,
    /// Replaces the item's ENTIRE uri list with these, in order. Empty means
    /// "leave the uris alone"; use [`ClearField::Uri`] to remove them all.
    pub uris: Vec<String>,
    pub notes: Option<String>,
    pub folder_id: Option<String>,
    /// Custom-field changes, applied in order.
    pub fields: Vec<FieldEdit>,
    /// Fields to REMOVE. Ordered so an edit's shape is deterministic.
    pub clear: std::collections::BTreeSet<ClearField>,
    /// A card's own content, which none of the fields above can reach.
    ///
    /// ⛔ WITHOUT THESE, A CARD WAS UNEDITABLE BY THIS CLIENT AT ALL. `edit`
    /// modelled rename / user / uri / totp / notes / custom-field / folder —
    /// and a card has none of those as its real content, so the only reachable
    /// edits on one were its title, its notes and its custom fields. Every card
    /// expires; updating one meant opening the Bitwarden web vault, which is
    /// the single thing this client exists to avoid.
    pub card_brand: Option<String>,
    pub card_holder: Option<String>,
    pub card_exp_month: Option<String>,
    pub card_exp_year: Option<String>,
    /// The PAN and the CVV. They are ordinary `Option<String>` here because
    /// [`edit_body`] must encrypt them like any other field — the boundary that
    /// matters is at the EDGES: no CLI flag carries them in argv (they are read
    /// from stdin like `add`'s password), and [`verify_edit`] reports them as
    /// labels, so neither ever reaches a `--json` reply, a `ps` listing or a
    /// shell history.
    ///
    /// [`edit_body`]: Vault::edit_body
    /// [`verify_edit`]: Vault::verify_edit
    pub card_number: Option<String>,
    pub card_code: Option<String>,
}

impl CipherEdit {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.username.is_none()
            && self.password.is_none()
            && self.totp.is_none()
            && self.uris.is_empty()
            && self.notes.is_none()
            && self.folder_id.is_none()
            && self.fields.is_empty()
            && self.clear.is_empty()
            && !self.touches_card()
    }

    /// Whether the edit touches a field that only exists on a login cipher.
    fn touches_login(&self) -> bool {
        self.username.is_some()
            || self.password.is_some()
            || self.totp.is_some()
            || !self.uris.is_empty()
            || self.clear.iter().any(|field| field.is_login_only())
    }

    /// Whether the edit touches a field that only exists on a card cipher.
    ///
    /// The exact twin of [`touches_login`], and it must stay one: a card field
    /// aimed at a login is the same mistake in the other direction, and it
    /// would write a `card` object onto an item whose type says it has none —
    /// invisible to every reader afterwards, because `card_object` refuses to
    /// look at a non-card.
    ///
    /// [`touches_login`]: CipherEdit::touches_login
    pub(crate) fn touches_card(&self) -> bool {
        self.card_brand.is_some()
            || self.card_holder.is_some()
            || self.card_exp_month.is_some()
            || self.card_exp_year.is_some()
            || self.card_number.is_some()
            || self.card_code.is_some()
    }

    /// The set-side twin of a clear, so "set and clear in one edit" can be
    /// refused for every field from one place instead of one `if` per field.
    fn also_set(&self, field: ClearField) -> bool {
        match field {
            ClearField::Notes => self.notes.is_some(),
            ClearField::Totp => self.totp.is_some(),
            ClearField::Username => self.username.is_some(),
            ClearField::Uri => !self.uris.is_empty(),
            ClearField::Folder => self.folder_id.is_some(),
        }
    }
}

/// What a re-read found after an edit was written. Field LABELS only — a
/// verification that echoed values would put a password in every caller's
/// `--json` output, which is the one thing this crate never does.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct EditVerification {
    /// Changes confirmed present on the freshly synced item.
    pub landed: Vec<String>,
    /// Changes the re-read could NOT confirm. Non-empty means the write must be
    /// reported as a failure however cleanly the `PUT` returned.
    pub missing: Vec<String>,
}

impl EditVerification {
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
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
    #[error("{0} is not a card item, so it has no brand, cardholder, expiry, number or code")]
    NotACard(String),
    #[error(
        "{0:?} is not a usable card {1}; an expiry that syncs cleanly and is wrong is \
         refused at the gateway months later, with nothing to point at"
    )]
    BadCardExpiry(String, &'static str),
    #[error(
        "refusing to set a field to the empty string; to remove a value use \
         `--clear <field>` or `--remove-field <name>`"
    )]
    EmptyValue,
    #[error("cannot set and clear {0} in one edit; ask for one or the other")]
    ClearAndSet(&'static str),
    #[error("this edit names the custom field {0:?} more than once")]
    RepeatedField(String),
    #[error(
        "the item carries {1} custom fields named {0:?}; this vault will not guess \
         which one to change"
    )]
    AmbiguousField(String, usize),
    #[error("the item has no custom field named {0:?}")]
    NoSuchField(String),
    #[error(
        "custom field {0:?} is a LINKED field: it points at the item's own username \
         or password and stores no value of its own"
    )]
    LinkedField(String),
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

/// The RAW BYTES a WebAuthn credential id denotes, whatever spelling the vault
/// stored it in. This exists because the two sides spell the same id differently:
/// Bitwarden stores `credentialId` as a **hyphenated UUID string**, while a page
/// hands `allowCredentials[].id` to the shim as **raw bytes**, which the shim
/// base64url-encodes. Comparing those as strings can never match — that bug made
/// every stored passkey invisible to every ceremony (measured 2026-08-07 against
/// a Google sign-in: `no passkey in this vault answers that request`, for an
/// rpId the vault demonstrably held).
///
/// Order matters: try UUID first, because a 36-char hyphenated UUID is not valid
/// base64url and would otherwise fall through to its own UTF-8 bytes and still
/// never match.
pub fn credential_id_bytes(stored: &str) -> Vec<u8> {
    use base64::Engine;
    let text = stored.trim();
    if let Some(bytes) = uuid_to_bytes(text) {
        return bytes;
    }
    if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text) {
        return bytes;
    }
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(text) {
        return bytes;
    }
    text.as_bytes().to_vec()
}

/// A hyphenated UUID to its 16 bytes, or `None` if it is not one.
fn uuid_to_bytes(text: &str) -> Option<Vec<u8>> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || text.len() != 36 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..16)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// The spelling an RP must receive: base64url(no pad) of the credential id BYTES.
pub fn credential_id_b64url(stored: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(credential_id_bytes(stored))
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
    /// Bitwarden's cipher type: 1 login, 2 secure note, 3 card, 4 identity.
    /// Secret-free, and the answer to a question the list could not previously
    /// be asked: 130 of this user's 1113 items are not logins, so `get` refuses
    /// them ("has no password") with nothing in the listing to explain why.
    pub item_type: u8,
    /// The item is ARCHIVED: put away without being destroyed.
    ///
    /// Bitwarden's own third bucket, distinct from the trash — the server keeps
    /// archived ciphers in the LIVE list with an `archivedDate` set, so unlike
    /// [`Vault::trashed_items`] this cannot be a separate collection without
    /// inventing a split the wire does not have. It is a flag on the item and
    /// the callers filter.
    pub archived: bool,
}

/// Everything a DETAIL VIEW needs and nothing it must not have.
///
/// ⛔ SECRET-FREE BY CONSTRUCTION, exactly like [`VaultItem`]. It carries the
/// booleans saying a password/notes/authenticator EXIST and the plaintext dates
/// the server stores in the clear; it never carries a value. The one reader is
/// the sidebar's View pane, which is re-fetched on every render — so anything
/// placed here is broadcast for as long as the pane is open, which is precisely
/// why the eye and the copy stay one-render reads through their own ops.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ItemDetail {
    /// The listing's own record, so a detail page and the row that opened it can
    /// never disagree about a name, a username or a type.
    #[serde(flatten)]
    pub item: VaultItem,
    pub has_notes: bool,
    /// Custom-field NAMES in stored order. A name identifies; it does not
    /// reveal — the same line the card audit and the edit receipt draw.
    pub field_names: Vec<String>,
    pub passkeys: Vec<PasskeyInfo>,
    /// `creationDate` — plaintext in the sync record.
    pub created: Option<String>,
    /// `revisionDate` — when the server last saw a change.
    pub revised: Option<String>,
    /// `login.passwordRevisionDate` — when the PASSWORD last changed, which is
    /// a different question from when the item did.
    pub password_revised: Option<String>,
    pub archived_date: Option<String>,
    /// One entry per REPLACED password, newest first — dates only. The values
    /// are read one at a time through [`Vault::past_password`].
    pub password_history: Vec<PastPassword>,
    /// A card item's secret-free metadata, from the SAME reader the `card` op
    /// and the CLI use ([`Vault::card`]). `None` for everything that is not a
    /// card.
    ///
    /// ⛔ IT BELONGS ON THE DETAIL BECAUSE THE DETAIL IS WHAT A PANE DRAWS.
    /// Before this, `ychrome-vault card` printed brand, cardholder, expiry and
    /// last4 while the sidebar's View pane showed a card as an entry with no
    /// content at all — the CLI and the pane disagreeing about the same five
    /// fields, with the CLI right. The operator's loss was concrete: he keeps
    /// two cards from the same issuer with the same product name, and **last4
    /// is the only thing that tells them apart at a glance**, so the pane was
    /// withholding the one field that stops the wrong person being charged.
    ///
    /// No PAN and no CVV are reachable from here, by construction:
    /// [`CardInfo`] cannot carry them. `card_secret` remains the only path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<CardInfo>,
}

/// A past password's metadata. The value is deliberately absent: a history list
/// is a LISTING, and a listing never carries a secret.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PastPassword {
    pub last_used_date: Option<String>,
}

/// Secret-free metadata for a card cipher: what a human needs to tell two cards
/// apart. The full number and the CVV are NOT here and cannot be — see
/// [`CardSecret`]. Same posture as [`PasskeyInfo`], whose no-leak property is a
/// test, not a convention.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CardInfo {
    pub brand: Option<String>,
    pub cardholder: Option<String>,
    pub exp_month: Option<String>,
    pub exp_year: Option<String>,
    /// The last four digits of the stored number, derived once inside
    /// [`Vault::card`] from a decrypted PAN that is dropped in the same
    /// expression. Four digits identify a card to its owner and are not the
    /// credential.
    pub last4: Option<String>,
}

/// A card's actual secrets: the full number and the CVV.
///
/// Deliberately NOT `Serialize` and deliberately NOT `Debug` — a PAN reaching a
/// log line, a `{:?}`, or a schema is exactly the failure this split prevents.
/// The only reader is the agent's `card-secret` op, which hands them to the
/// sidebar's fill injector, mirroring how the passkey private key is reachable
/// from `fido2_assert` alone and never from `passkeys()`.
pub struct CardSecret {
    pub number: Option<Zeroizing<String>>,
    pub code: Option<Zeroizing<String>>,
}

/// What one custom field holds — or, when it holds nothing readable, WHY.
///
/// The two reasons are not interchangeable to whoever asked. A linked field
/// stores no value by design and there is nothing to go looking for; an
/// unreadable one is a key this vault does not hold, which is a real problem
/// with a real fix. Collapsing both into a bare `None` (and then naming one of
/// them in the error) sent the user hunting for a link that does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// Decrypted.
    Value(String),
    /// A LINKED field: it points at the item's own username or password and
    /// carries no value of its own (the server sends `null`, or omits the key).
    Linked,
    /// A value IS stored and this vault could not read it: not an `EncString`,
    /// not a string at all, or sealed under a key we do not have.
    Unreadable,
}

/// Classify one raw custom-field entry's value under the cipher's key.
///
/// The single owner of the linked-vs-unreadable distinction; every surface that
/// reports it reads this and does not re-derive it from a null.
fn field_value(field: &serde_json::Value, key: &SymmetricKey) -> FieldValue {
    let Some(object) = field.as_object() else {
        // Not even an object: a record we cannot read, not a link.
        return FieldValue::Unreadable;
    };
    match get_ci(object, "value") {
        None | Some(serde_json::Value::Null) => FieldValue::Linked,
        Some(stored) => stored
            .as_str()
            .and_then(|text| EncString::parse(text).ok())
            .and_then(|encrypted| key.decrypt_to_string(&encrypted).ok())
            .map_or(FieldValue::Unreadable, FieldValue::Value),
    }
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
    /// The vault's own spelling (Bitwarden UUID) — what `fido2-assert` looks up.
    pub credential_id: String,
    /// The spelling the RP must see: base64url of the credential id BYTES.
    pub credential_id_b64url: String,
    pub rp_id: String,
    pub rp_name: Option<String>,
    pub user_name: Option<String>,
    pub user_display_name: Option<String>,
    pub user_handle: Option<String>,
    /// The vault item's name — the label the presence dialog shows when the
    /// passkey has no `userName`.
    pub item_name: Option<String>,
}

/// An item to create, of any type this client can also READ BACK. Plaintext —
/// it is encrypted by [`Vault::new_item_body`] and never leaves this process in
/// the clear.
///
/// The three fields every cipher type carries live here; everything else hangs
/// off [`NewItemBody`], which is what makes "a note has no password" and "a
/// card has no uri" structural instead of a promise. `item_type` is DERIVED
/// from the body ([`NewItemBody::cipher_type`]) rather than stored beside it,
/// so a card can never be filed as a login.
#[derive(Debug, Clone, Default)]
pub struct NewItem {
    pub name: String,
    pub notes: Option<String>,
    pub folder_id: Option<String>,
    pub body: NewItemBody,
}

/// The type-specific half of a new item — one variant per Bitwarden cipher
/// type this client models end to end.
///
/// ⛔ A variant belongs here only once the item it creates can be READ BACK by
/// this client. A create for a type nothing can decrypt or display writes an
/// item the user can see the name of and nothing else, which is worse than not
/// offering it: they would believe the data is stored and reachable. That is
/// why `Identity` (Bitwarden type 4) is absent — see ychrome
/// `docs/pending-bugs.md`.
#[derive(Debug, Clone)]
pub enum NewItemBody {
    Login(NewLoginFields),
    /// A secure note. The content IS [`NewItem::notes`] — Bitwarden's `type 2`
    /// carries no fields of its own beyond a sub-type discriminant, which is
    /// why this variant is empty rather than holding a `text`.
    Note,
    Card(NewCardFields),
}

impl NewItemBody {
    /// The Bitwarden cipher `type` this body creates. THE one place the mapping
    /// lives, read by the create body and by anything reporting what was made.
    pub fn cipher_type(&self) -> u8 {
        match self {
            Self::Login(_) => CIPHER_TYPE_LOGIN,
            Self::Note => CIPHER_TYPE_NOTE,
            Self::Card(_) => CIPHER_TYPE_CARD,
        }
    }
}

/// A login with nothing filled in — the default an untyped caller gets, and the
/// shape `..Default::default()` fills around in the tests.
impl Default for NewItemBody {
    fn default() -> Self {
        Self::Login(NewLoginFields::default())
    }
}

/// A login's own fields.
#[derive(Debug, Clone, Default)]
pub struct NewLoginFields {
    pub username: Option<String>,
    pub password: Option<String>,
    /// An authenticator secret (base32) or a full `otpauth://` URI.
    pub totp: Option<String>,
    pub uri: Option<String>,
}

/// A payment card's own fields, named exactly as [`Vault::card`] and
/// [`Vault::card_secret`] read them back, so a create and the read that proves
/// it landed cannot drift.
#[derive(Debug, Clone, Default)]
pub struct NewCardFields {
    pub cardholder: Option<String>,
    pub brand: Option<String>,
    pub number: Option<String>,
    pub exp_month: Option<String>,
    pub exp_year: Option<String>,
    pub code: Option<String>,
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
                    item_type: cipher.item_type,
                    archived: cipher.archived_date.is_some(),
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
        let decrypt =
            |enc: &Option<EncString>| enc.as_ref().and_then(|enc| key.decrypt_to_string(enc).ok());
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
        let decrypt =
            |enc: &Option<EncString>| enc.as_ref().and_then(|enc| key.decrypt_to_string(enc).ok());

        let credential = match credential_id {
            Some(wanted) => cipher
                .fido2
                .iter()
                .find(|c| decrypt(&c.credential_id).as_deref() == Some(wanted)),
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

    /// Every distinct rpId this vault holds a passkey for — sorted, deduplicated.
    ///
    /// ⛔ METADATA ONLY, AND DELIBERATELY THE LEAST THAT ANSWERS THE QUESTION.
    /// No credentialIds, no user names, no keys, no item names — an rpId is a
    /// public hostname the site itself announces to any visitor. The caller is
    /// the browser deciding WHERE to install the WebAuthn shim, and "does this
    /// vault hold a passkey for this host" is the whole of what that needs.
    ///
    /// Why the browser asks at all: the shim used to be installed on EVERY page,
    /// which told every site that a platform authenticator exists — on an engine
    /// (WebKitGTK) that has no WebAuthn whatsoever. That mismatch is readable by
    /// any bot check. Scoping the shim to the rpIds a passkey actually exists
    /// for means a site you have no passkey for sees a pristine `navigator`.
    pub fn passkey_rp_ids(&self) -> Vec<String> {
        let mut hosts: Vec<String> = Vec::new();
        for cipher in &self.ciphers {
            if cipher.fido2.is_empty() {
                continue;
            }
            let Ok(key) = self.cipher_key(cipher) else {
                continue;
            };
            for credential in &cipher.fido2 {
                let Some(rp_id) = credential
                    .rp_id
                    .as_ref()
                    .and_then(|enc| key.decrypt_to_string(enc).ok())
                else {
                    continue;
                };
                let rp_id = rp_id.trim().to_ascii_lowercase();
                if !rp_id.is_empty() && !hosts.contains(&rp_id) {
                    hosts.push(rp_id);
                }
            }
        }
        hosts.sort();
        hosts
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
                // Compare by BYTES, never by spelling: the stored id is a UUID
                // string and the requested one is base64url of the raw bytes.
                if !allow_credential_ids.is_empty() {
                    let want = credential_id_bytes(&credential_id);
                    if !allow_credential_ids
                        .iter()
                        .any(|id| credential_id_bytes(id) == want)
                    {
                        continue;
                    }
                }
                matches.push(PasskeyMatch {
                    item_id: cipher.id.clone(),
                    credential_id_b64url: credential_id_b64url(&credential_id),
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

    /// Build the `POST /api/ciphers` body for a new item of ANY type this
    /// client models, encrypting every field under the user key. A newly
    /// created cipher carries no item key, so the user key is the cipher key —
    /// exactly what [`cipher_key`] will resolve when the item comes back on the
    /// next sync.
    ///
    /// Only the fields we model are emitted. That is safe for CREATE (there is
    /// nothing to lose) and is why there is no `update` counterpart: a PUT
    /// rebuilt from this struct would silently drop the notes, custom fields,
    /// favorite flag and password history that `sync` does not parse.
    ///
    /// ⛔ The type-specific sub-object is emitted for the item's OWN type and
    /// for no other. A `card` object on a type-1 cipher is invisible to every
    /// reader afterwards — [`card_object`] refuses to look at a non-card — so
    /// writing one would be data the user can never see again. The `match` on
    /// [`NewItemBody`] is what makes that impossible rather than merely
    /// avoided.
    ///
    /// [`cipher_key`]: Vault::cipher_key
    /// [`card_object`]: Vault::card_object
    pub fn new_item_body(&self, item: &NewItem) -> Result<serde_json::Value, CryptoError> {
        let enc = |value: &str| -> Result<String, CryptoError> {
            Ok(self.user_key.encrypt_string(value)?.to_string())
        };
        let enc_opt = |value: &Option<String>| -> Result<serde_json::Value, CryptoError> {
            match value.as_deref().filter(|value| !value.is_empty()) {
                Some(value) => Ok(serde_json::Value::String(enc(value)?)),
                None => Ok(serde_json::Value::Null),
            }
        };
        let mut body = serde_json::json!({
            "type": item.body.cipher_type(),
            "name": enc(&item.name)?,
            "notes": enc_opt(&item.notes)?,
            "favorite": false,
            "folderId": item.folder_id,
            "reprompt": 0,
            "fields": [],
        });
        let map = body
            .as_object_mut()
            .expect("the body above is a JSON object");
        match &item.body {
            NewItemBody::Login(login) => {
                let uris = match login.uri.as_deref().filter(|uri| !uri.is_empty()) {
                    Some(uri) => {
                        serde_json::json!([{ "uri": enc(uri)?, "match": serde_json::Value::Null }])
                    }
                    None => serde_json::json!([]),
                };
                map.insert(
                    "login".to_string(),
                    serde_json::json!({
                        "username": enc_opt(&login.username)?,
                        "password": enc_opt(&login.password)?,
                        "totp": enc_opt(&login.totp)?,
                        "uris": uris,
                    }),
                );
            }
            // A secure note's whole content is `notes`, already emitted above.
            // The sub-object exists only to carry Bitwarden's note sub-type,
            // and 0 ("generic") is the only one it defines. It is PLAINTEXT: a
            // discriminant, not user data, exactly like `type` itself.
            NewItemBody::Note => {
                map.insert("secureNote".to_string(), serde_json::json!({ "type": 0 }));
            }
            NewItemBody::Card(card) => {
                map.insert(
                    "card".to_string(),
                    serde_json::json!({
                        "cardholderName": enc_opt(&card.cardholder)?,
                        "brand": enc_opt(&card.brand)?,
                        "number": enc_opt(&card.number)?,
                        "expMonth": enc_opt(&card.exp_month)?,
                        "expYear": enc_opt(&card.exp_year)?,
                        "code": enc_opt(&card.code)?,
                    }),
                );
            }
        }
        Ok(body)
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
        let b64url = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
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
        if edit.touches_card() && cipher.item_type != CIPHER_TYPE_CARD {
            return Err(EditError::NotACard(id.to_string()));
        }
        for cleared in &edit.clear {
            if edit.also_set(*cleared) {
                return Err(EditError::ClearAndSet(cleared.as_str()));
            }
        }
        for value in [
            &edit.name,
            &edit.username,
            &edit.password,
            &edit.totp,
            &edit.notes,
            &edit.card_brand,
            &edit.card_holder,
            &edit.card_exp_month,
            &edit.card_exp_year,
            &edit.card_number,
            &edit.card_code,
        ] {
            if value.as_deref().is_some_and(str::is_empty) {
                return Err(EditError::EmptyValue);
            }
        }
        if edit.uris.iter().any(String::is_empty) {
            return Err(EditError::EmptyValue);
        }
        // ⛔ THE EXPIRY IS THE ONE CARD FIELD WITH A SHAPE, AND A WRONG ONE IS
        // SILENT. A month of "13" or a year of "29" encrypts, syncs and reads
        // back perfectly; the first thing that objects is a payment gateway,
        // months later, with nothing on screen to point at. Refuse here rather
        // than in the CLI, because the pane and the agent op reach `edit_body`
        // without passing through it.
        if let Some(month) = &edit.card_exp_month
            && !matches!(month.parse::<u8>(), Ok(1..=12))
        {
            return Err(EditError::BadCardExpiry(month.clone(), "month (want 1-12)"));
        }
        if let Some(year) = &edit.card_exp_year
            && !(year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()))
        {
            return Err(EditError::BadCardExpiry(
                year.clone(),
                "year (want four digits, e.g. 2031 — a two-digit year is stored verbatim \
                 and read back as the year 29)",
            ));
        }
        // A custom field named twice in one edit has no defined outcome — the
        // second write would silently win, or the remove would, depending on
        // order. Refuse instead of picking.
        for (index, change) in edit.fields.iter().enumerate() {
            if change.name().is_empty() {
                return Err(EditError::EmptyValue);
            }
            if let FieldEdit::Set { value, .. } = change
                && value.is_empty()
            {
                return Err(EditError::EmptyValue);
            }
            if edit.fields[..index]
                .iter()
                .any(|earlier| earlier.name().eq_ignore_ascii_case(change.name()))
            {
                return Err(EditError::RepeatedField(change.name().to_string()));
            }
        }
        let raw = cipher
            .raw
            .as_object()
            .ok_or_else(|| EditError::NoRawRecord(id.to_string()))?;

        let key = self.cipher_key(cipher)?;
        let encrypt = |value: &str| -> Result<Value, CryptoError> {
            Ok(json!(key.encrypt_string(value)?.to_string()))
        };

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
        if !edit.uris.is_empty() {
            // ⛔ REUSE THE STORED ENTRY FOR A URI THAT IS NOT CHANGING. A uri is
            // an OBJECT, not a string: beside `uri` it carries `match` (the per-
            // uri match type the user chose in another client) and, on newer
            // servers, `uriChecksum` — plus whatever Bitwarden adds next. Minting
            // a fresh `{uri, match: null}` for a uri the item already has would
            // silently discard all of that, which is the same data loss that
            // raw-patching exists to prevent, one level down.
            let stored = get_ci(&login, "uris")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let existing = |wanted: &str| -> Option<Value> {
                stored
                    .iter()
                    .find(|entry| {
                        entry
                            .as_object()
                            .and_then(|entry| get_ci(entry, "uri"))
                            .and_then(Value::as_str)
                            .and_then(|text| EncString::parse(text).ok())
                            .and_then(|enc| key.decrypt_to_string(&enc).ok())
                            .is_some_and(|text| text == wanted)
                    })
                    .cloned()
            };
            let mut uris = Vec::with_capacity(edit.uris.len());
            for uri in &edit.uris {
                match existing(uri) {
                    Some(entry) => uris.push(entry),
                    None => uris.push(json!({ "uri": encrypt(uri)?, "match": Value::Null })),
                }
            }
            set_ci(&mut login, "uris", Value::Array(uris));
        }
        // THE CARD OBJECT, patched exactly the way `login` is above: lifted out
        // whole, only the named keys replaced, put back. Bitwarden's spellings
        // (`cardholderName`, `expMonth`, `expYear`) are what `Vault::card`
        // reads, so the writer and the reader cannot drift apart.
        //
        // ⛔ EVERY KEY IS ENCRYPTED, INCLUDING THE EXPIRY AND THE BRAND. They
        // look like harmless strings and the server stores them as ciphertext
        // like everything else; writing one in the clear produces an item that
        // syncs cleanly and then reads back as garbage in every other client —
        // the silent corruption raw-patching exists to prevent.
        if edit.touches_card() {
            let mut card = remove_ci(&mut body, "card")
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            for (key, value) in [
                ("brand", &edit.card_brand),
                ("cardholderName", &edit.card_holder),
                ("expMonth", &edit.card_exp_month),
                ("expYear", &edit.card_exp_year),
                ("number", &edit.card_number),
                ("code", &edit.card_code),
            ] {
                if let Some(value) = value {
                    set_ci(&mut card, key, encrypt(value)?);
                }
            }
            set_ci(&mut body, "card", Value::Object(card));
        }
        for change in &edit.fields {
            apply_field_edit(&mut body, change, &key)?;
        }
        // Clears run LAST so that, whatever order the caller built the edit in,
        // a set and a clear of the same field can never both apply — the refusal
        // above is the only outcome, and this cannot quietly become a
        // last-writer-wins.
        for cleared in &edit.clear {
            // The server assigns unconditionally (`cipher.notes = data.notes`),
            // so an explicit null WIPES the field; omitting the key would leave
            // whatever we copied out of the raw record in place. Null, never
            // remove.
            match cleared {
                ClearField::Notes => set_ci(&mut body, "notes", Value::Null),
                ClearField::Folder => set_ci(&mut body, "folderId", Value::Null),
                ClearField::Totp => set_ci(&mut login, "totp", Value::Null),
                ClearField::Username => set_ci(&mut login, "username", Value::Null),
                ClearField::Uri => set_ci(&mut login, "uris", Value::Null),
            }
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

    /// Which of an edit's named changes are actually on the item NOW.
    ///
    /// ⛔ A WRITE THAT REPORTS SUCCESS WITHOUT LOOKING IS THE LIE-OF-SUCCESS
    /// SHAPE THIS CRATE TREATS AS WORSE THAN A FAILURE. `PUT` returning 200 and
    /// a resync completing say the server accepted a body; neither says the
    /// field the user asked for is what they asked it to be. Run this AFTER the
    /// resync and refuse to report an edit that did not land.
    ///
    /// Labels only, never values: a custom field's NAME travels (the card audit
    /// line already sets that precedent), its value never does.
    pub fn verify_edit(&self, id: &str, edit: &CipherEdit) -> EditVerification {
        let mut verification = EditVerification::default();
        let Some(cipher) = self.find(id) else {
            verification.missing.push("item".into());
            return verification;
        };
        let decrypted = |enc: Option<&EncString>| -> Option<String> {
            let key = self.cipher_key(cipher).ok()?;
            key.decrypt_to_string(enc?).ok()
        };
        let mut check = |label: &str, ok: bool| {
            if ok {
                verification.landed.push(label.to_string());
            } else {
                verification.missing.push(label.to_string());
            }
        };

        if let Some(name) = &edit.name {
            check(
                "name",
                decrypted(cipher.name.as_ref()).as_ref() == Some(name),
            );
        }
        if let Some(username) = &edit.username {
            check(
                "username",
                decrypted(cipher.username.as_ref()).as_ref() == Some(username),
            );
        }
        if let Some(password) = &edit.password {
            check(
                "password",
                decrypted(cipher.password.as_ref()).as_ref() == Some(password),
            );
        }
        if let Some(totp) = &edit.totp {
            check("totp", self.totp_secret(id).as_ref() == Some(totp));
        }
        if !edit.uris.is_empty() {
            check("uri", self.uris_of(cipher) == edit.uris);
        }
        if let Some(notes) = &edit.notes {
            check("notes", self.notes(id).as_ref() == Some(notes));
        }
        if let Some(folder_id) = &edit.folder_id {
            check("folder", cipher.folder_id.as_ref() == Some(folder_id));
        }
        // THE CARD, re-read through the same two readers everything else uses —
        // `card` for the metadata, `card_secret` for the PAN and the CVV. The
        // comparison happens here, inside the crate, and only a LABEL leaves:
        // `verify_edit`'s whole contract is that its output can be printed.
        if edit.touches_card() {
            let card = self.card(id);
            let mut card_check = |label: &str, stored: Option<&String>, wanted: &Option<String>| {
                if let Some(wanted) = wanted {
                    check(label, stored == Some(wanted));
                }
            };
            let info = card.as_ref();
            card_check(
                "card-brand",
                info.and_then(|card| card.brand.as_ref()),
                &edit.card_brand,
            );
            card_check(
                "card-holder",
                info.and_then(|card| card.cardholder.as_ref()),
                &edit.card_holder,
            );
            card_check(
                "card-exp-month",
                info.and_then(|card| card.exp_month.as_ref()),
                &edit.card_exp_month,
            );
            card_check(
                "card-exp-year",
                info.and_then(|card| card.exp_year.as_ref()),
                &edit.card_exp_year,
            );
            // The secret half is read only when it was asked for, so an edit
            // that never touched the number does not decrypt one to check it.
            if edit.card_number.is_some() || edit.card_code.is_some() {
                let secret = self.card_secret(id);
                if let Some(number) = &edit.card_number {
                    check(
                        "card-number",
                        secret
                            .as_ref()
                            .and_then(|secret| secret.number.as_deref())
                            .is_some_and(|stored| stored == number),
                    );
                }
                if let Some(code) = &edit.card_code {
                    check(
                        "card-code",
                        secret
                            .as_ref()
                            .and_then(|secret| secret.code.as_deref())
                            .is_some_and(|stored| stored == code),
                    );
                }
            }
        }
        for change in &edit.fields {
            let stored = self
                .fields(id)
                .into_iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(change.name()));
            let label = format!("field:{}", change.name());
            let ok = match change {
                FieldEdit::Set { value, .. } => {
                    stored.map(|(_, held)| held) == Some(FieldValue::Value(value.clone()))
                }
                FieldEdit::Remove { .. } => stored.is_none(),
            };
            check(&label, ok);
        }
        for cleared in &edit.clear {
            let gone = match cleared {
                ClearField::Notes => self.notes(id).is_none(),
                ClearField::Totp => cipher.totp.is_none(),
                ClearField::Username => cipher.username.is_none(),
                ClearField::Uri => self.uris_of(cipher).is_empty(),
                ClearField::Folder => cipher.folder_id.is_none(),
            };
            check(&format!("clear:{}", cleared.as_str()), gone);
        }
        verification
    }

    /// One cipher's decrypted uris, in stored order. Undecryptable entries are
    /// dropped, exactly as `items()` does.
    fn uris_of(&self, cipher: &RawCipher) -> Vec<String> {
        let Ok(key) = self.cipher_key(cipher) else {
            return Vec::new();
        };
        cipher
            .uris
            .iter()
            .filter_map(|enc| key.decrypt_to_string(enc).ok())
            .collect()
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

    /// Everything a detail view needs about one item, and nothing secret.
    ///
    /// ONE read for a whole page. The alternative — the pane asking `list`,
    /// `fields`, `passkeys` and three date questions separately — is four
    /// round-trips that can each describe a different moment, which is how a
    /// page ends up showing one item's dates beside another's fields.
    ///
    /// `None` only when the id is unknown; an item whose raw record is missing
    /// a date simply reports `None` for it, because "the server did not say" and
    /// "there is no such item" are different answers and the caller acts on them
    /// differently.
    pub fn detail(&self, id: &str) -> Option<ItemDetail> {
        let cipher = self.find(id)?;
        let item = self.items_from(std::slice::from_ref(cipher)).pop()?;
        let raw = cipher.raw.as_object();
        let plain = |key: &str| {
            raw.and_then(|raw| get_ci(raw, key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        // `passwordRevisionDate` lives INSIDE the login object, unlike the two
        // dates the server keeps at the top level.
        let password_revised = raw
            .and_then(|raw| get_ci(raw, "login"))
            .and_then(serde_json::Value::as_object)
            .and_then(|login| get_ci(login, "passwordRevisionDate"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Some(ItemDetail {
            has_notes: self.notes(id).is_some(),
            field_names: self
                .fields(id)
                .into_iter()
                .map(|(name, _)| name)
                .filter(|name| !name.is_empty())
                .collect(),
            passkeys: self.passkeys(id),
            created: plain("creationDate"),
            revised: plain("revisionDate"),
            password_revised,
            archived_date: cipher.archived_date.clone(),
            password_history: self
                .password_history_entries(cipher)
                .iter()
                .map(|entry| PastPassword {
                    last_used_date: entry
                        .as_object()
                        .and_then(|entry| get_ci(entry, "lastUsedDate"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                })
                .collect(),
            // `card` already answers `None` for a non-card, off the record's own
            // `type` — so this asks once rather than testing the type here and
            // letting two places decide what a card is.
            card: self.card(id),
            item,
        })
    }

    /// The raw `passwordHistory` array, newest first, as the server stored it.
    /// Each entry's `password` is still ciphertext here — see
    /// [`Vault::past_password`] for the one read that decrypts one.
    fn password_history_entries(&self, cipher: &RawCipher) -> Vec<serde_json::Value> {
        cipher
            .raw
            .as_object()
            .and_then(|raw| get_ci(raw, "passwordHistory"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// ONE past password, by its index in [`ItemDetail::password_history`],
    /// decrypted on demand.
    ///
    /// Deliberately indexed rather than returned with the history: a list of
    /// every password an item ever had is the single most dangerous payload this
    /// vault can produce, and a detail page re-fetches its schema constantly. So
    /// the page gets DATES, and a value arrives only for the entry whose eye was
    /// pressed, for exactly one render — the same contract the current
    /// password's reveal has had since 2026-08-02.
    pub fn past_password(&self, id: &str, index: usize) -> Option<String> {
        let cipher = self.find(id)?;
        let entry = self.password_history_entries(cipher).into_iter().nth(index)?;
        let encrypted = get_ci(entry.as_object()?, "password")?.as_str()?.to_string();
        let key = self.cipher_key(cipher).ok()?;
        key.decrypt_to_string(&EncString::parse(&encrypted).ok()?)
            .ok()
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
        key.decrypt_to_string(&EncString::parse(encrypted).ok()?)
            .ok()
    }

    /// The cipher's `card` object and the key its sub-fields are sealed under.
    ///
    /// Read off the RAW record for the same reason as [`Vault::notes`]: `sync`
    /// never parses a card into [`RawCipher`], and adding a parsed `card` field
    /// there would be a second encoding of the same data that `edit_body`'s raw
    /// patching could silently diverge from. `None` when the item is unknown,
    /// undecryptable, or is not a card.
    fn card_object(&self, id: &str) -> Option<(SymmetricKey, &JsonMap)> {
        let cipher = self.find(id)?;
        // The record's own `type` is what makes an item a card. Reading a stray
        // `card` object off a login would let the two disagree about what an
        // item IS, and `VaultItem::item_type` (which the sidebar draws its fill
        // button from) reports that same field.
        if cipher.item_type != CIPHER_TYPE_CARD {
            return None;
        }
        let card = get_ci(cipher.raw.as_object()?, "card")?.as_object()?;
        let key = self.cipher_key(cipher).ok()?;
        Some((key, card))
    }

    /// A card item's SECRET-FREE metadata: brand, cardholder, expiry, last four.
    ///
    /// The 130 items in this user's vault that carry no password are mostly
    /// these — `get` refuses them all, so before this read they were reachable
    /// only through `notes`. The full number and the CVV are not here by
    /// construction; [`Vault::card_secret`] is the only path to those.
    pub fn card(&self, id: &str) -> Option<CardInfo> {
        let (key, card) = self.card_object(id)?;
        let read = |name: &str| get_ci(card, name).and_then(|value| decrypt_or_plain(&key, value));
        // The PAN is decrypted here and lives exactly as long as this binding:
        // last4 is taken from it and the rest is zeroized on drop.
        let number = read("number").map(Zeroizing::new);
        Some(CardInfo {
            brand: read("brand"),
            cardholder: read("cardholderName"),
            exp_month: read("expMonth"),
            exp_year: read("expYear"),
            last4: number.as_ref().and_then(|number| last_four(number)),
        })
    }

    /// A card's full number and CVV, decrypted on demand.
    ///
    /// The one reader is the agent's `card-secret` op, which exists for the
    /// sidebar's fill injector. There is deliberately no CLI verb: a PAN printed
    /// to a terminal is durable — scrollback, shell history, an agent CLI's
    /// JSONL transcript — and unlike a password it cannot be rotated on demand.
    pub fn card_secret(&self, id: &str) -> Option<CardSecret> {
        let (key, card) = self.card_object(id)?;
        let read = |name: &str| {
            get_ci(card, name)
                .and_then(|value| decrypt_or_plain(&key, value))
                .map(Zeroizing::new)
        };
        Some(CardSecret {
            number: read("number"),
            code: read("code"),
        })
    }

    /// A specific item's custom fields, each decrypted on demand, returned as
    /// `(name, value)` in stored order. A hidden field's value decrypts like any
    /// other. Read straight off the RAW record for the same reason as
    /// [`Vault::notes`] — `sync` never parses custom fields into [`RawCipher`],
    /// which is exactly why they must be preserved by patching the raw JSON in
    /// [`Vault::edit_body`]. Empty vec if the item is unknown, undecryptable, or
    /// carries no fields. A field whose name will not decrypt is dropped rather
    /// than failing the whole read.
    pub fn fields(&self, id: &str) -> Vec<(String, FieldValue)> {
        let Some(cipher) = self.find(id) else {
            return Vec::new();
        };
        let Ok(key) = self.cipher_key(cipher) else {
            return Vec::new();
        };
        let Some(raw) = cipher.raw.as_object() else {
            return Vec::new();
        };
        let Some(fields) = get_ci(raw, "fields").and_then(|value| value.as_array()) else {
            return Vec::new();
        };
        let name_of = |field: &serde_json::Value| -> Option<String> {
            let encrypted = get_ci(field.as_object()?, "name")?.as_str()?;
            key.decrypt_to_string(&EncString::parse(encrypted).ok()?)
                .ok()
        };
        // A field carries an optional name and an optional value; a hidden field
        // the user never named still has a value, so a missing/undecryptable
        // NAME must not drop the whole entry (that would hide exactly the secret
        // we came for). Name defaults to the empty string in that case.
        fields
            .iter()
            .map(|field| (name_of(field).unwrap_or_default(), field_value(field, &key)))
            .collect()
    }

    /// Does this item match a search term in the places a LISTING cannot see?
    ///
    /// `needle` must already be lowercased. Covers the item's **notes** and its
    /// **custom-field names**; the caller checks name, username and uris first,
    /// because those are already decrypted in [`VaultItem`] and cost nothing.
    ///
    /// ⛔ CUSTOM-FIELD VALUES ARE DELIBERATELY NOT SEARCHED. A hidden field's
    /// value is a secret, and a search that matched on it would turn the search
    /// box into an oracle: type a guess, and the result list tells you whether
    /// the guess was right. Field NAMES travel here for the same reason they
    /// travel in the card audit line and the edit receipt — they identify, they
    /// do not reveal. Notes ARE searched, because notes are where people put the
    /// context they later search for, and every other client indexes them.
    ///
    /// Nothing about the match leaves this function but a boolean; the caller
    /// returns the same secret-free metadata it always did.
    pub fn deep_search_match(&self, id: &str, needle: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        if self
            .notes(id)
            .is_some_and(|notes| notes.to_lowercase().contains(needle))
        {
            return true;
        }
        self.fields(id)
            .iter()
            .any(|(name, _)| name.to_lowercase().contains(needle))
    }

    /// How many custom-field entries the RAW cipher carries, decrypted or not.
    /// A diagnostic companion to [`Vault::fields`]: when `fields` comes back
    /// empty, this says whether the item truly has no custom fields (`Some(0)`
    /// or `None`) or has some that would not decrypt (`Some(n)` with `n` > the
    /// decrypted count).
    pub fn raw_field_count(&self, id: &str) -> Option<usize> {
        let cipher = self.find(id)?;
        let raw = cipher.raw.as_object()?;
        Some(get_ci(raw, "fields")?.as_array()?.len())
    }

    /// The current TOTP code for a specific item, with the seconds until it
    /// rolls.
    ///
    /// Three answers, kept apart on purpose: `None` — the item is unknown or
    /// carries no authenticator secret; `Some(Err(..))` — it HAS one and this
    /// host's clock is not fit to mint from it (see [`crate::clock`]);
    /// `Some(Ok(..))` — the code. Folding the refusal into `None` would tell a
    /// caller "no authenticator here", which is a different and wrong story.
    pub fn totp_code(
        &self,
        id: &str,
    ) -> Option<Result<(String, u64), crate::clock::ClockUntrusted>> {
        Some(self.totp_for(id)?.now())
    }

    /// The same code, minted with the clock check WAIVED. Only reachable from a
    /// caller that passed `--ignore-clock` after being told what is wrong.
    pub fn totp_code_ignoring_clock(&self, id: &str) -> Option<(String, u64)> {
        Some(self.totp_for(id)?.now_unchecked())
    }

    /// The parsed authenticator for an item, if it has one that parses. The ONE
    /// decrypt-and-parse path, so the checked and waived mints can never differ
    /// about which secret they used.
    fn totp_for(&self, id: &str) -> Option<Totp> {
        let cipher = self.find(id)?;
        let enc = cipher.totp.as_ref()?;
        let key = self.cipher_key(cipher).ok()?;
        let secret = key.decrypt_to_string(enc).ok()?;
        Totp::parse(&secret).ok()
    }

    /// The RAW authenticator-secret string stored in the item's TOTP slot,
    /// decrypted but NOT parsed as an `otpauth`/base32 secret. `totp_code`
    /// returns `None` whenever the stored text is not a valid authenticator
    /// (e.g. a user pasted a 64-hex key into the TOTP field by mistake); this
    /// surfaces that text verbatim so it can be recovered. `None` if the item is
    /// unknown or its TOTP slot is empty.
    pub fn totp_secret(&self, id: &str) -> Option<String> {
        let cipher = self.find(id)?;
        let enc = cipher.totp.as_ref()?;
        let key = self.cipher_key(cipher).ok()?;
        key.decrypt_to_string(enc).ok()
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

/// Decrypt a raw-record string under `key`, tolerating one stored in the clear.
///
/// Bitwarden encrypts every card sub-field, but a record written by an older or
/// third-party client can carry a plaintext `brand` ("Visa"), exactly as a
/// passkey's `creationDate` is plaintext in the sync record. The rule is
/// deliberate and one-way: a value that PARSES as an EncString must decrypt or
/// it is dropped (`None`), so ciphertext can never be surfaced as if it were the
/// plaintext; only a value that is not an EncString at all is taken verbatim.
fn decrypt_or_plain(key: &SymmetricKey, value: &serde_json::Value) -> Option<String> {
    let text = value.as_str()?;
    match EncString::parse(text) {
        Ok(enc) => key.decrypt_to_string(&enc).ok(),
        Err(_) => Some(text.to_string()),
    }
}

/// The last four DIGITS of a stored card number. People type separators, so
/// non-digits are ignored rather than counted; fewer than four digits is not a
/// card number and yields `None` rather than a partial one.
fn last_four(number: &str) -> Option<String> {
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    (digits.len() >= 4).then(|| digits[digits.len() - 4..].to_string())
}

/// Bitwarden's custom-field `type` discriminants. A LINKED field is the one
/// that matters here: it stores no value of its own, so writing one would be
/// meaningless rather than merely wrong.
const FIELD_TYPE_TEXT: u64 = 0;
const FIELD_TYPE_HIDDEN: u64 = 1;
const FIELD_TYPE_LINKED: u64 = 3;

/// Apply one custom-field change to the cipher body, in place.
///
/// Custom fields live on the RAW record and `sync` never parses them into
/// [`RawCipher`] — the same reason [`Vault::notes`] reads raw. Editing them
/// therefore means patching this array, and every property of the field the
/// caller did NOT name (its type, its `linkedId`, anything Bitwarden adds
/// later) has to survive: the entry object is mutated, never rebuilt.
fn apply_field_edit(
    body: &mut JsonMap,
    change: &FieldEdit,
    key: &SymmetricKey,
) -> Result<(), EditError> {
    let mut fields: Vec<serde_json::Value> = get_ci(body, "fields")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Match on the DECRYPTED name, case-insensitively — the same rule
    // `fields --field-name` reads by, so what the user can see is what they can
    // change. An undecryptable name simply does not match; it is not this
    // edit's business and it stays put.
    let matched: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| {
            field
                .as_object()
                .and_then(|field| get_ci(field, "name"))
                .and_then(serde_json::Value::as_str)
                .and_then(|text| EncString::parse(text).ok())
                .and_then(|enc| key.decrypt_to_string(&enc).ok())
                .is_some_and(|name| name.eq_ignore_ascii_case(change.name()))
        })
        .map(|(index, _)| index)
        .collect();
    // Bitwarden permits duplicate field names, so this is a real shape and not a
    // hypothetical. Guessing which one the user meant could overwrite the wrong
    // secret, and there is no undo for that.
    if matched.len() > 1 {
        return Err(EditError::AmbiguousField(
            change.name().to_string(),
            matched.len(),
        ));
    }
    let at = matched.first().copied();

    match change {
        FieldEdit::Remove { name } => {
            let Some(at) = at else {
                return Err(EditError::NoSuchField(name.clone()));
            };
            fields.remove(at);
        }
        FieldEdit::Set { name, value, kind } => {
            let encrypted = serde_json::json!(key.encrypt_string(value)?.to_string());
            match at {
                Some(at) => {
                    let field = fields[at]
                        .as_object_mut()
                        .ok_or_else(|| EditError::NoSuchField(name.clone()))?;
                    let stored_type = get_ci(field, "type")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(FIELD_TYPE_TEXT);
                    if stored_type == FIELD_TYPE_LINKED {
                        return Err(EditError::LinkedField(name.clone()));
                    }
                    // ⛔ `Text` means "do not change the visibility", not "make
                    // it text". Setting a new value on a HIDDEN field must not
                    // downgrade it to one every Bitwarden client renders in the
                    // clear — that would expose a secret as a side effect of
                    // updating it. Only an explicit `Hidden` changes the type.
                    if *kind == FieldKind::Hidden {
                        set_ci(field, "type", serde_json::json!(FIELD_TYPE_HIDDEN));
                    }
                    set_ci(field, "value", encrypted);
                }
                None => fields.push(serde_json::json!({
                    "name": key.encrypt_string(name)?.to_string(),
                    "value": encrypted,
                    "type": match kind {
                        FieldKind::Text => FIELD_TYPE_TEXT,
                        FieldKind::Hidden => FIELD_TYPE_HIDDEN,
                    },
                    "linkedId": serde_json::Value::Null,
                })),
            }
        }
    }
    set_ci(body, "fields", serde_json::Value::Array(fields));
    Ok(())
}

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
fn password_history_with_current(raw: &JsonMap, cipher: &RawCipher) -> Option<serde_json::Value> {
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
            archived_date: None,
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
        // On a host whose clock the kernel calls disciplined this mints; on one
        // it does not, it REFUSES rather than emitting a confident wrong code.
        // Both are correct answers, and the test says which it observed instead
        // of quietly accepting either.
        match vault.totp_code("c1").expect("c1 carries an authenticator") {
            Ok((code, remaining)) => {
                assert_eq!(code.len(), 6);
                assert!(remaining >= 1 && remaining <= 30);
            }
            Err(untrusted) => assert!(
                untrusted.to_string().starts_with("clock_unsynchronized"),
                "the only reason to withhold a code is a measured bad clock: {untrusted}"
            ),
        }
        // The waiver reaches the same secret; it only skips the clock question.
        let (code, remaining) = vault
            .totp_code_ignoring_clock("c1")
            .expect("c1 carries an authenticator");
        assert_eq!(code.len(), 6);
        assert!(remaining >= 1 && remaining <= 30);

        // An item with no authenticator is `None` — never a clock refusal, and
        // never the other way round.
        assert!(vault.totp_code("nope").is_none());
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
        let blind = Vault::new(
            user_key.clone(),
            HashMap::new(),
            ciphers.clone(),
            vec![],
            HashMap::new(),
        );
        assert_eq!(
            blind.items().len(),
            1,
            "only the user-key cipher is readable"
        );
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
        org_keys.insert(
            "org1".to_string(),
            SymmetricKey::from_bytes(&org_bytes).unwrap(),
        );
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
    fn new_item_body_encrypts_every_field_and_reads_back() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let vault = Vault::new(
            user_key.clone(),
            HashMap::new(),
            vec![],
            vec![],
            HashMap::new(),
        );

        let body = vault
            .new_item_body(&NewItem {
                name: "example.com".to_string(),
                notes: None,
                folder_id: None,
                body: NewItemBody::Login(NewLoginFields {
                    username: Some("alice".to_string()),
                    password: Some("hunter2".to_string()),
                    uri: Some("https://example.com".to_string()),
                    totp: None,
                }),
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
        assert_eq!(
            decrypt(&body["login"]["uris"][0]["uri"]),
            "https://example.com"
        );
        // Fields the user left out are null, not an encrypted empty string.
        assert!(body["login"]["totp"].is_null());
        assert!(body["notes"].is_null());

        // No plaintext anywhere in the serialized request.
        let wire = body.to_string();
        for secret in ["hunter2", "alice", "example.com"] {
            assert!(
                !wire.contains(secret),
                "{secret} leaked into the request body"
            );
        }
    }

    // An empty uri must not produce a uris entry at all.
    #[test]
    fn new_item_body_omits_an_empty_uri() {
        let user_key = SymmetricKey::from_bytes(&[0x11u8; 64]).unwrap();
        let vault = Vault::new(user_key, HashMap::new(), vec![], vec![], HashMap::new());
        let body = vault
            .new_item_body(&NewItem {
                name: "bare".to_string(),
                body: NewItemBody::Login(NewLoginFields {
                    uri: Some(String::new()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(body["login"]["uris"].as_array().unwrap().len(), 0);
    }

    /// A SECURE NOTE is a real cipher type, not a login with the boxes left
    /// blank. Filed as type 1 it would show a Fill button that fills nothing,
    /// and `card_object`'s twin refusal is what makes the type load-bearing.
    ///
    /// User-reported 2026-08-08: the vault pane could only ever create a
    /// login, discovered while trying to save a note.
    #[test]
    fn a_secure_note_is_created_as_its_own_type_with_its_text_encrypted() {
        let key_bytes = [0x77u8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let vault = Vault::new(
            user_key.clone(),
            HashMap::new(),
            vec![],
            vec![],
            HashMap::new(),
        );
        let body = vault
            .new_item_body(&NewItem {
                name: "Boiler service code".to_string(),
                notes: Some("engineer said 4417".to_string()),
                folder_id: None,
                body: NewItemBody::Note,
            })
            .unwrap();

        assert_eq!(body["type"], 2, "a note filed as a login is the bug");
        // The sub-object carries Bitwarden's note sub-type and nothing else —
        // and it is the ONLY plaintext in the body besides the discriminants.
        assert_eq!(body["secureNote"]["type"], 0);
        // ⛔ No login object at all: a `login` key on a type-2 cipher is a
        // password box every reader would then offer to fill from nothing.
        assert!(
            body.get("login").is_none(),
            "a note must not carry a login object: {body}"
        );
        assert!(body.get("card").is_none());

        let enc = EncString::parse(body["notes"].as_str().unwrap()).unwrap();
        assert_eq!(
            user_key.decrypt_to_string(&enc).unwrap(),
            "engineer said 4417"
        );
        let wire = body.to_string();
        for secret in ["engineer said 4417", "Boiler service code"] {
            assert!(!wire.contains(secret), "{secret} leaked into the body");
        }
    }

    /// A CARD, written with the field names [`Vault::card`] and
    /// [`Vault::card_secret`] read back — the round trip is the point, so the
    /// assertion is the READ, not the spelling of the JSON.
    #[test]
    fn a_card_is_created_with_the_field_names_the_card_reader_looks_for() {
        let key_bytes = [0x3cu8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let vault = Vault::new(
            user_key.clone(),
            HashMap::new(),
            vec![],
            vec![],
            HashMap::new(),
        );
        let body = vault
            .new_item_body(&NewItem {
                name: "Bank of Invention".to_string(),
                notes: None,
                folder_id: None,
                body: NewItemBody::Card(NewCardFields {
                    cardholder: Some("A Reader".to_string()),
                    brand: Some("Visa".to_string()),
                    number: Some("4111111111111111".to_string()),
                    exp_month: Some("11".to_string()),
                    exp_year: Some("2031".to_string()),
                    code: Some("737".to_string()),
                }),
            })
            .unwrap();

        assert_eq!(body["type"], 3);
        assert!(body.get("login").is_none(), "a card has no password");

        // THE ROUND TRIP: feed the created body back through the reader as a
        // synced cipher and ask the vault what it holds. A create whose field
        // names drift from `card_object`'s produces an item that decrypts to
        // nothing — the failure that has no symptom until the user looks.
        let mut raw = body.clone();
        raw["id"] = serde_json::json!("new-card");
        raw["object"] = serde_json::json!("cipherDetails");
        let cipher = RawCipher {
            raw,
            id: "new-card".into(),
            item_type: CIPHER_TYPE_CARD,
            ..Default::default()
        };
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );
        let card = vault.card("new-card").expect("the created card reads back");
        assert_eq!(card.cardholder.as_deref(), Some("A Reader"));
        assert_eq!(card.brand.as_deref(), Some("Visa"));
        assert_eq!(card.exp_month.as_deref(), Some("11"));
        assert_eq!(card.exp_year.as_deref(), Some("2031"));
        assert_eq!(card.last4.as_deref(), Some("1111"));
        let secret = vault.card_secret("new-card").expect("the PAN reads back");
        assert_eq!(
            secret.number.as_deref().map(String::as_str),
            Some("4111111111111111")
        );
        assert_eq!(secret.code.as_deref().map(String::as_str), Some("737"));
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
        Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        )
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
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );

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
        assert!(
            !json.contains("key_value") && !json.contains("user_handle"),
            "{json}"
        );

        // An unknown item yields no passkeys rather than panicking.
        assert!(vault.passkeys("nope").is_empty());
    }

    #[test]
    fn a_uuid_stored_passkey_answers_an_allow_list_spelled_in_base64url() {
        // THE REGRESSION. Bitwarden stores `credentialId` as a hyphenated UUID;
        // a page hands `allowCredentials[].id` as raw bytes, which the shim
        // base64url-encodes. Comparing the two spellings as strings matched
        // NOTHING, so every stored passkey was invisible to every ceremony.
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let s = |text: &str| Some(seal(&key_bytes, text));
        let uuid = "77b7f3f3-a3b6-4754-bcaf-b0e3e928f83e";
        let cipher = RawCipher {
            id: "item-1".into(),
            item_type: 1,
            name: Some(seal(&key_bytes, "a-google-item")),
            fido2: vec![RawFido2Credential {
                credential_id: s(uuid),
                rp_id: s("google.com"),
                user_name: s("an-account"),
                key_value: s("secret"),
                counter: s("0"),
                ..Default::default()
            }],
            ..Default::default()
        };
        let vault = Vault::new(user_key, HashMap::new(), vec![cipher], vec![], HashMap::new());

        // What the shim actually sends: base64url of the UUID's 16 bytes.
        let requested = credential_id_b64url(uuid);
        assert_ne!(requested, uuid, "the two spellings must genuinely differ");

        let matches = vault.passkeys_for_assertion("google.com", &[requested.clone()]);
        assert_eq!(matches.len(), 1, "the stored passkey must answer its own id");
        assert_eq!(matches[0].credential_id, uuid, "the vault keeps its spelling");
        assert_eq!(
            matches[0].credential_id_b64url, requested,
            "the RP must be handed the byte spelling"
        );

        // And an allow-list naming a DIFFERENT credential still excludes it.
        let other = credential_id_b64url("00000000-0000-4000-8000-000000000000");
        assert!(vault
            .passkeys_for_assertion("google.com", &[other])
            .is_empty());
    }

    #[test]
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
        verifying
            .verify(&message, &sig)
            .expect("stored key must sign a verifiable assertion");
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
        assert_eq!(
            user_key.decrypt_to_string(&written).unwrap(),
            "new-password"
        );

        // Server-managed keys never go back.
        for key in [
            "id",
            "object",
            "revisionDate",
            "creationDate",
            "deletedDate",
            "collectionIds",
            "edit",
            "viewPassword",
        ] {
            assert!(
                body.get(key).is_none(),
                "{key} must be stripped from the update body"
            );
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
            .edit_body(
                "c1",
                &CipherEdit {
                    password: Some("new".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let history = body["passwordHistory"].as_array().unwrap();
        assert_eq!(history.len(), 2, "old password prepended, prior entry kept");
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let remembered = EncString::parse(history[0]["password"].as_str().unwrap()).unwrap();
        assert_eq!(
            user_key.decrypt_to_string(&remembered).unwrap(),
            "old-password"
        );
        assert!(history[0]["lastUsedDate"].as_str().unwrap().ends_with('Z'));
        assert_eq!(history[1]["password"], "2.older", "prior history survives");

        // An edit that does NOT touch the password leaves history exactly as-is.
        let renamed = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    name: Some("New Name".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(renamed["passwordHistory"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn edit_body_clears_the_totp() {
        let key_bytes = [0x5au8; 64];
        let vault = login_vault(&key_bytes);
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    clear: [ClearField::Totp].into_iter().collect(),
                    ..Default::default()
                },
            )
            .unwrap();
        // The authenticator slot is nulled, not omitted (the server would keep
        // an omitted field), and the rest of the login rides along untouched.
        assert!(body["login"]["totp"].is_null());
        assert_eq!(body["login"]["username"], "2.enc-user");
        assert_eq!(body["login"]["password"], "2.enc-pass");
    }

    #[test]
    fn edit_body_rejects_setting_and_clearing_the_totp_together() {
        let vault = login_vault(&[0x5au8; 64]);
        let error = vault.edit_body(
            "c1",
            &CipherEdit {
                totp: Some("JBSWY3DPEHPK3PXP".into()),
                clear: [ClearField::Totp].into_iter().collect(),
                ..Default::default()
            },
        );
        assert!(matches!(error, Err(EditError::ClearAndSet("totp"))));
    }

    // The same refusal for EVERY clearable field, from the one owner. A per-
    // field `if` is how `--notes X --clear notes` would have quietly become
    // last-writer-wins on the four fields nobody wrote an `if` for.
    #[test]
    fn setting_and_clearing_is_refused_for_every_field() {
        let vault = login_vault(&[0x5au8; 64]);
        let cases: [(ClearField, CipherEdit); 5] = [
            (
                ClearField::Notes,
                CipherEdit {
                    notes: Some("n".into()),
                    ..Default::default()
                },
            ),
            (
                ClearField::Totp,
                CipherEdit {
                    totp: Some("JBSWY3DPEHPK3PXP".into()),
                    ..Default::default()
                },
            ),
            (
                ClearField::Username,
                CipherEdit {
                    username: Some("u".into()),
                    ..Default::default()
                },
            ),
            (
                ClearField::Uri,
                CipherEdit {
                    uris: vec!["https://example.com".into()],
                    ..Default::default()
                },
            ),
            (
                ClearField::Folder,
                CipherEdit {
                    folder_id: Some("f".into()),
                    ..Default::default()
                },
            ),
        ];
        for (field, base) in cases {
            let edit = CipherEdit {
                clear: [field].into_iter().collect(),
                ..base
            };
            let error = vault.edit_body("c1", &edit);
            assert!(
                matches!(&error, Err(EditError::ClearAndSet(named)) if *named == field.as_str()),
                "setting and clearing {} in one edit must be refused, by name",
                field.as_str()
            );
        }
    }

    #[test]
    fn a_clear_alone_is_not_an_empty_edit() {
        let edit = CipherEdit {
            clear: [ClearField::Totp].into_iter().collect(),
            ..Default::default()
        };
        assert!(!edit.is_empty());
    }

    #[test]
    fn edit_body_caps_password_history() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let mut raw = raw_login_record();
        raw["passwordHistory"] = serde_json::json!(
            (0..PASSWORD_HISTORY_LIMIT)
                .map(|i| serde_json::json!({"password": format!("2.old{i}")}))
                .collect::<Vec<_>>()
        );
        let cipher = RawCipher {
            raw,
            id: "c1".into(),
            item_type: 1,
            password: Some(seal(&key_bytes, "old-password")),
            ..Default::default()
        };
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    password: Some("new".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            body["passwordHistory"].as_array().unwrap().len(),
            PASSWORD_HISTORY_LIMIT
        );
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
        let vault = Vault::new(
            user_key.clone(),
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    password: Some("rotated".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let written = EncString::parse(body["login"]["password"].as_str().unwrap()).unwrap();
        assert_eq!(item_key.decrypt_to_string(&written).unwrap(), "rotated");
        assert!(
            user_key.decrypt_to_string(&written).is_err(),
            "must NOT be under the user key"
        );
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
        let vault = Vault::new(
            user_key.clone(),
            org_keys,
            vec![cipher],
            vec![],
            HashMap::new(),
        );

        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    name: Some("Renamed".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let written = EncString::parse(body["name"].as_str().unwrap()).unwrap();
        assert_eq!(org_key.decrypt_to_string(&written).unwrap(), "Renamed");
        assert!(user_key.decrypt_to_string(&written).is_err());
        assert_eq!(
            body["organizationId"], "org1",
            "org ownership must survive the edit"
        );
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
        let vault = Vault::new(
            user_key.clone(),
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );
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
        assert_eq!(
            object
                .keys()
                .filter(|k| k.eq_ignore_ascii_case("name"))
                .count(),
            1
        );
        assert!(object.contains_key("name") && !object.contains_key("Name"));
        assert_eq!(
            object
                .keys()
                .filter(|k| k.eq_ignore_ascii_case("login"))
                .count(),
            1
        );
        assert!(object.contains_key("lastKnownRevisionDate"));
        assert!(!object.contains_key("Id") && !object.contains_key("RevisionDate"));
        // The un-patched PascalCase field is preserved as it came.
        assert_eq!(body["Notes"], "2.keep");
        let written = EncString::parse(body["name"].as_str().unwrap()).unwrap();
        assert_eq!(user_key.decrypt_to_string(&written).unwrap(), "Renamed");
        let login = body["login"].as_object().unwrap();
        assert_eq!(
            login
                .keys()
                .filter(|k| k.eq_ignore_ascii_case("username"))
                .count(),
            1
        );
    }

    #[test]
    fn edit_body_refuses_empty_values_unknown_items_and_non_logins() {
        let key_bytes = [0x5au8; 64];
        let vault = login_vault(&key_bytes);

        // Clearing a field is not expressible — it must not silently encrypt "".
        let empty = vault.edit_body(
            "c1",
            &CipherEdit {
                notes: Some(String::new()),
                ..Default::default()
            },
        );
        assert!(matches!(empty, Err(EditError::EmptyValue)));

        let unknown = vault.edit_body(
            "nope",
            &CipherEdit {
                name: Some("x".into()),
                ..Default::default()
            },
        );
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
        let bad = notes_vault.edit_body(
            "n1",
            &CipherEdit {
                password: Some("x".into()),
                ..Default::default()
            },
        );
        assert!(matches!(bad, Err(EditError::NotALogin(_))));
        // But its NOTES are editable.
        assert!(
            notes_vault
                .edit_body(
                    "n1",
                    &CipherEdit {
                        notes: Some("hello".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
    }

    // A cipher that never came from `sync` (no raw record) must fail loudly
    // rather than PUT a body that would erase the item's real contents.
    #[test]
    fn edit_body_refuses_a_cipher_with_no_raw_record() {
        let key_bytes = [0x5au8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let cipher = RawCipher {
            id: "c1".into(),
            item_type: 1,
            ..Default::default()
        };
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );
        let result = vault.edit_body(
            "c1",
            &CipherEdit {
                name: Some("x".into()),
                ..Default::default()
            },
        );
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
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );
        assert_eq!(vault.notes("c1").as_deref(), Some("remember me"));
        assert!(vault.notes("nope").is_none());

        // A password-only edit carries the SAME encrypted notes back.
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    password: Some("new".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let carried = EncString::parse(body["notes"].as_str().unwrap()).unwrap();
        let key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        assert_eq!(key.decrypt_to_string(&carried).unwrap(), "remember me");
    }

    /// A card cipher as `sync` really sends one: type 3, every card sub-field an
    /// EncString, and no login block at all — which is why `get` refuses it.
    fn card_vault(key_bytes: &[u8; 64], keys: &[(&str, &str)]) -> Vault {
        let user_key = SymmetricKey::from_bytes(key_bytes).unwrap();
        let card: serde_json::Map<String, serde_json::Value> = keys
            .iter()
            .map(|(name, value)| (name.to_string(), json_str(seal(key_bytes, value))))
            .collect();
        let cipher = RawCipher {
            raw: serde_json::json!({
                "object": "cipherDetails",
                "id": "card1",
                "type": 3,
                "name": "2.enc-name",
                "card": card,
            }),
            id: "card1".into(),
            item_type: 3,
            name: Some(seal(key_bytes, "HDFC Regalia")),
            ..Default::default()
        };
        Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        )
    }

    fn json_str(enc: EncString) -> serde_json::Value {
        serde_json::json!(enc.to_string())
    }

    // A card is read off the RAW record like notes are, and the split is the
    // whole point: the metadata view carries the last four and CANNOT carry the
    // number or the CVV, which come out of a separate, unserializable reader.
    #[test]
    fn a_card_reads_its_metadata_without_carrying_the_number() {
        let key_bytes = [0x5au8; 64];
        let vault = card_vault(
            &key_bytes,
            &[
                ("cardholderName", "RIVKA HOLLANDER"),
                ("brand", "Visa"),
                ("number", "4111 1111 1111 4242"),
                ("expMonth", "11"),
                ("expYear", "2029"),
                ("code", "737"),
            ],
        );

        let card = vault.card("card1").expect("a type-3 cipher is a card");
        assert_eq!(card.brand.as_deref(), Some("Visa"));
        assert_eq!(card.cardholder.as_deref(), Some("RIVKA HOLLANDER"));
        assert_eq!(card.exp_month.as_deref(), Some("11"));
        assert_eq!(card.exp_year.as_deref(), Some("2029"));
        // Separators are not digits: the last four are 4242, not "4242" with a
        // space, and not the last four CHARACTERS.
        assert_eq!(card.last4.as_deref(), Some("4242"));

        // THE security property, the same one locked for the passkey private
        // key: serialize the metadata view and prove the PAN and the CVV are
        // absent, and that it has no field that could ever carry them.
        let wire = serde_json::to_string(&card).unwrap();
        assert!(
            !wire.contains("4111"),
            "PAN leaked into the metadata: {wire}"
        );
        assert!(
            !wire.contains("737"),
            "CVV leaked into the metadata: {wire}"
        );
        assert!(!wire.contains("number") && !wire.contains("code"), "{wire}");

        // The secrets have exactly one reader, and it is not serializable.
        let secret = vault.card_secret("card1").unwrap();
        assert_eq!(
            secret.number.as_deref().map(String::as_str),
            Some("4111 1111 1111 4242")
        );
        assert_eq!(secret.code.as_deref().map(String::as_str), Some("737"));

        // A cipher that is not a card is not readable as one, and neither is an
        // item that does not exist. The record's own `type` decides that, not
        // the presence of a `card` object: a LOGIN carrying one (a stray key, a
        // future Bitwarden field, a hand-edited record) must stay a login, or
        // the sidebar's fill button and this reader would disagree about what
        // the item IS.
        let key_bytes = [0x42u8; 64];
        let mut raw = raw_login_record();
        raw["card"] = serde_json::json!({"number": json_str(seal(&key_bytes, "4111111111114242"))});
        let login = RawCipher {
            raw,
            id: "c1".into(),
            item_type: 1,
            name: Some(seal(&key_bytes, "GitHub")),
            ..Default::default()
        };
        let logins = Vault::new(
            SymmetricKey::from_bytes(&key_bytes).unwrap(),
            HashMap::new(),
            vec![login],
            vec![],
            HashMap::new(),
        );
        assert!(logins.card("c1").is_none(), "a login is not a card");
        assert!(logins.card_secret("c1").is_none());
        assert!(vault.card("nope").is_none());
    }

    /// ⛔ A CARD WAS UNEDITABLE BY THIS CLIENT AT ALL, and every card expires.
    ///
    /// `edit` modelled rename / user / uri / totp / notes / custom-field /
    /// folder — none of which is a card's real content — so the only reachable
    /// edits on one were its title, its notes and its custom fields. Updating an
    /// expiry meant opening the Bitwarden web vault, the single thing this
    /// client exists to avoid.
    ///
    /// This proves the write lands, that it is CIPHERTEXT (a brand or an expiry
    /// written in the clear syncs cleanly and then reads back as garbage in
    /// every other client), and that untouched keys survive.
    #[test]
    fn edit_body_writes_a_card_and_leaves_the_keys_it_was_not_given() {
        let key_bytes = [0x71u8; 64];
        let vault = card_vault(
            &key_bytes,
            &[
                ("brand", "Visa"),
                ("cardholderName", "RIVKA HOLLANDER"),
                ("expMonth", "11"),
                ("expYear", "2029"),
                ("number", "4111 1111 1111 4242"),
                ("code", "737"),
            ],
        );
        let body = vault
            .edit_body(
                "card1",
                &CipherEdit {
                    card_exp_month: Some("3".into()),
                    card_exp_year: Some("2031".into()),
                    ..Default::default()
                },
            )
            .expect("a card takes a card edit");
        let card = body["card"].as_object().expect("the card object survives");

        // What was asked for is there, and it is SEALED.
        for (key, plain) in [("expMonth", "3"), ("expYear", "2031")] {
            let stored = card[key].as_str().expect("a string");
            assert_ne!(stored, plain, "{key} was written in the clear: {stored}");
            let enc = EncString::parse(stored).expect("an EncString");
            let key_for = vault.cipher_key(vault.find("card1").unwrap()).unwrap();
            assert_eq!(key_for.decrypt_to_string(&enc).unwrap(), plain);
        }
        // And what was NOT asked for is untouched, byte for byte — the same
        // raw-patching promise `login` keeps. A rebuilt card object would have
        // silently dropped the PAN, which no other client could restore.
        let original = vault.find("card1").unwrap().raw["card"]
            .as_object()
            .unwrap()
            .clone();
        for key in ["brand", "cardholderName", "number", "code"] {
            assert_eq!(card[key], original[key], "{key} was rewritten for nothing");
        }
    }

    /// A card field aimed at a login is the same mistake as a password aimed at
    /// a card, and it is the worse direction: it would write a `card` object
    /// onto an item whose type says it has none, where `card_object` refuses to
    /// look at it — invisible to every reader afterwards.
    ///
    /// And the expiry is refused on SHAPE, because a wrong one is silent. "13"
    /// and "29" encrypt, sync and read back perfectly; the first thing that
    /// objects is a payment gateway, months later.
    #[test]
    fn edit_body_refuses_a_card_edit_on_a_login_and_an_expiry_that_cannot_be_one() {
        let key_bytes = [0x72u8; 64];
        let logins = login_vault(&key_bytes);
        let wrong_type = logins.edit_body(
            "c1",
            &CipherEdit {
                card_brand: Some("Visa".into()),
                ..Default::default()
            },
        );
        assert!(matches!(wrong_type, Err(EditError::NotACard(_))));

        let cards = card_vault(&key_bytes, &[("brand", "Visa")]);
        for bad in ["13", "0", "March", ""] {
            let refused = cards.edit_body(
                "card1",
                &CipherEdit {
                    card_exp_month: Some(bad.into()),
                    ..Default::default()
                },
            );
            assert!(
                matches!(
                    refused,
                    Err(EditError::BadCardExpiry(..) | EditError::EmptyValue)
                ),
                "month {bad:?} was accepted"
            );
        }
        // ⚠ A TWO-DIGIT YEAR IS THE DANGEROUS ONE: it is what is printed on the
        // card, it looks right in every box, and it is stored verbatim — so the
        // card then reads back as expiring in the year 29.
        let short_year = cards.edit_body(
            "card1",
            &CipherEdit {
                card_exp_year: Some("29".into()),
                ..Default::default()
            },
        );
        assert!(matches!(short_year, Err(EditError::BadCardExpiry(..))));
        // And the shapes that ARE right are taken, including a padded month.
        for (month, year) in [("3", "2031"), ("03", "2031"), ("12", "2099")] {
            assert!(
                cards
                    .edit_body(
                        "card1",
                        &CipherEdit {
                            card_exp_month: Some(month.into()),
                            card_exp_year: Some(year.into()),
                            ..Default::default()
                        }
                    )
                    .is_ok(),
                "{month}/{year} was refused"
            );
        }
    }

    /// The receipt must be a READBACK, and for a card that means both readers:
    /// `card` for the metadata and `card_secret` for the PAN and the CVV. A
    /// `PUT` returning 200 says the server accepted a body, never that the
    /// field is what the user asked it to be.
    ///
    /// ⛔ And only LABELS leave. `verify_edit`'s whole contract is that its
    /// output can be printed, so a card verification that echoed values would
    /// put a PAN in every `--json` reply.
    #[test]
    fn verify_edit_reads_a_card_back_and_reports_labels_only() {
        let key_bytes = [0x73u8; 64];
        let stored = card_vault(
            &key_bytes,
            &[
                ("brand", "Visa"),
                ("expMonth", "11"),
                ("expYear", "2029"),
                ("number", "4111 1111 1111 4242"),
                ("code", "737"),
            ],
        );
        // What the vault already holds verifies; what it does not, does not.
        let verification = stored.verify_edit(
            "card1",
            &CipherEdit {
                card_brand: Some("Visa".into()),
                card_exp_year: Some("2031".into()),
                card_number: Some("4111 1111 1111 4242".into()),
                card_code: Some("999".into()),
                ..Default::default()
            },
        );
        assert_eq!(verification.landed, ["card-brand", "card-number"]);
        assert_eq!(verification.missing, ["card-exp-year", "card-code"]);

        let wire = serde_json::to_string(&verification).unwrap();
        for secret in ["4111", "4242", "737", "999"] {
            assert!(!wire.contains(secret), "a card value reached the receipt: {wire}");
        }
    }

    // A custom field with nothing to show says WHICH of the two reasons it is.
    // Both used to come back as a bare `None`, and the CLI then named one of
    // them for both: a value sealed under a key we do not have was reported as
    // "a linked field", sending the user to look for a link that is not there.
    #[test]
    fn a_custom_field_reports_linked_and_unreadable_as_different_facts() {
        let key_bytes = [0x63u8; 64];
        let other_key = [0x64u8; 64];
        let cipher = RawCipher {
            raw: serde_json::json!({
                "id": "c1",
                "type": 1,
                "fields": [
                    // Readable: a hidden field's value decrypts like any other.
                    {"name": json_str(seal(&key_bytes, "PAN")),
                     "value": json_str(seal(&key_bytes, "4242")), "type": 1},
                    // LINKED: the server sends an explicit null...
                    {"name": json_str(seal(&key_bytes, "User")), "value": null, "type": 3},
                    // ...or omits the key entirely. Same fact.
                    {"name": json_str(seal(&key_bytes, "Pass")), "type": 3},
                    // Stored, and sealed under a key this vault does not hold —
                    // the org-key case, which is deliberately non-fatal and so
                    // leaves exactly this shape behind.
                    {"name": json_str(seal(&key_bytes, "Org")),
                     "value": json_str(seal(&other_key, "secret")), "type": 1},
                    // Stored, and not an EncString at all.
                    {"name": json_str(seal(&key_bytes, "Junk")), "value": "not-an-encstring"},
                    // Stored, and not even a string.
                    {"name": json_str(seal(&key_bytes, "Number")), "value": 42},
                ],
            }),
            id: "c1".into(),
            item_type: 1,
            name: Some(seal(&key_bytes, "GitHub")),
            ..Default::default()
        };
        let vault = Vault::new(
            SymmetricKey::from_bytes(&key_bytes).unwrap(),
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );

        assert_eq!(
            vault.fields("c1"),
            vec![
                ("PAN".to_string(), FieldValue::Value("4242".to_string())),
                ("User".to_string(), FieldValue::Linked),
                ("Pass".to_string(), FieldValue::Linked),
                ("Org".to_string(), FieldValue::Unreadable),
                ("Junk".to_string(), FieldValue::Unreadable),
                ("Number".to_string(), FieldValue::Unreadable),
            ]
        );
    }

    // Vaultwarden has drifted PascalCase↔camelCase across versions, which is why
    // every raw read goes through `get_ci`. A card written by a drifted server
    // must read identically — without this the whole object would be invisible.
    #[test]
    fn a_card_survives_pascal_case_drift_and_a_plaintext_brand() {
        let key_bytes = [0x11u8; 64];
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let cipher = RawCipher {
            raw: serde_json::json!({
                "id": "card1",
                "Type": 3,
                "Card": {
                    "Number": json_str(seal(&key_bytes, "5500 0000 0000 0004")),
                    "CardholderName": json_str(seal(&key_bytes, "A KUNDU")),
                    // Not an EncString at all: an older client stored it in the
                    // clear. Taken verbatim rather than dropped.
                    "Brand": "Mastercard",
                    // An EncString sealed under a DIFFERENT key: it parses but
                    // will not decrypt, so it must be dropped, never surfaced
                    // as ciphertext.
                    "Code": json_str(seal(&[0x99u8; 64], "999")),
                },
            }),
            id: "card1".into(),
            item_type: 3,
            name: Some(seal(&key_bytes, "Mastercard")),
            ..Default::default()
        };
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );

        let card = vault.card("card1").expect("PascalCase is still a card");
        assert_eq!(card.brand.as_deref(), Some("Mastercard"));
        assert_eq!(card.cardholder.as_deref(), Some("A KUNDU"));
        assert_eq!(card.last4.as_deref(), Some("0004"));
        let secret = vault.card_secret("card1").unwrap();
        assert_eq!(
            secret.number.as_deref().map(String::as_str),
            Some("5500 0000 0000 0004")
        );
        assert!(
            secret.code.is_none(),
            "an undecryptable field must be dropped, never surfaced as ciphertext"
        );
    }

    // `last4` is derived, so its rules are worth pinning: digits only, and a
    // value too short to be a card number yields nothing rather than a fragment.
    #[test]
    fn last_four_counts_digits_not_characters() {
        assert_eq!(last_four("4111-1111-1111-4242").as_deref(), Some("4242"));
        assert_eq!(last_four("4242").as_deref(), Some("4242"));
        assert_eq!(last_four("42 42").as_deref(), Some("4242"));
        assert_eq!(last_four("424"), None);
        assert_eq!(last_four(""), None);
        assert_eq!(last_four("no digits here"), None);
    }

    // The list can finally say WHY an item refuses `get`: it is not a login.
    #[test]
    fn the_item_list_reports_the_cipher_type() {
        let key_bytes = [0x5au8; 64];
        let card = card_vault(&key_bytes, &[("number", "4111111111114242")]);
        assert_eq!(card.items()[0].item_type, CIPHER_TYPE_CARD);
        assert!(!card.items()[0].has_password);
        assert_eq!(
            login_vault(&[0x42u8; 64]).items()[0].item_type,
            CIPHER_TYPE_LOGIN
        );
    }

    /// A login whose custom fields are REAL: names and values sealed with the
    /// vault key, one text, one hidden, one linked (no value at all), plus a
    /// key inside a field entry that this client has never heard of.
    ///
    /// The fixture in `raw_login_record` cannot serve here — its field name is
    /// the literal `"2.enc-field"`, which does not decrypt, so nothing would
    /// ever match by name.
    fn field_vault(key_bytes: &[u8; 64]) -> Vault {
        let seal_str = |text: &str| seal(key_bytes, text).to_string();
        let mut raw = raw_login_record();
        raw["fields"] = serde_json::json!([
            {
                "name": seal_str("API Key"),
                "value": seal_str("old-token"),
                "type": FIELD_TYPE_TEXT,
                "linkedId": serde_json::Value::Null,
                "somethingBitwardenAddsIn2027": {"per": "field"},
            },
            {
                "name": seal_str("Recovery Code"),
                "value": seal_str("old-recovery"),
                "type": FIELD_TYPE_HIDDEN,
                "linkedId": serde_json::Value::Null,
            },
            {
                "name": seal_str("Linked User"),
                "value": serde_json::Value::Null,
                "type": FIELD_TYPE_LINKED,
                "linkedId": 100,
            },
        ]);
        let user_key = SymmetricKey::from_bytes(key_bytes).unwrap();
        Vault::new(
            user_key,
            HashMap::new(),
            vec![RawCipher {
                raw,
                id: "c1".into(),
                item_type: 1,
                name: Some(seal(key_bytes, "GitHub")),
                username: Some(seal(key_bytes, "octocat")),
                password: Some(seal(key_bytes, "old-password")),
                uris: vec![seal(key_bytes, "https://github.com")],
                ..Default::default()
            }],
            vec![],
            HashMap::new(),
        )
    }

    /// Decrypt one custom field's value straight out of a built body.
    fn field_in(body: &serde_json::Value, key_bytes: &[u8; 64], wanted: &str) -> Option<String> {
        let key = SymmetricKey::from_bytes(key_bytes).unwrap();
        let read = |entry: &serde_json::Value, name: &str| -> Option<String> {
            let text = entry.get(name)?.as_str()?;
            key.decrypt_to_string(&EncString::parse(text).ok()?).ok()
        };
        body["fields"].as_array()?.iter().find_map(|entry| {
            (read(entry, "name")?.eq_ignore_ascii_case(wanted)).then(|| read(entry, "value"))?
        })
    }

    // Custom fields were readable and NOT writable, which is the gap the user
    // reported as "our vault cannot edit entries".
    #[test]
    fn edit_body_sets_an_existing_custom_field_and_appends_a_new_one() {
        let key_bytes = [0x5au8; 64];
        let vault = field_vault(&key_bytes);
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    fields: vec![
                        FieldEdit::Set {
                            name: "api key".into(), // case-insensitive, as `fields` reads
                            value: "new-token".into(),
                            kind: FieldKind::Text,
                        },
                        FieldEdit::Set {
                            name: "Deploy Key".into(),
                            value: "fresh".into(),
                            kind: FieldKind::Hidden,
                        },
                    ],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            field_in(&body, &key_bytes, "API Key").as_deref(),
            Some("new-token")
        );
        assert_eq!(
            field_in(&body, &key_bytes, "Deploy Key").as_deref(),
            Some("fresh")
        );
        // The other two ride along untouched, and the appended one is hidden.
        let fields = body["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 4, "one field appended, none dropped");
        assert_eq!(fields[3]["type"], FIELD_TYPE_HIDDEN);
        assert_eq!(
            field_in(&body, &key_bytes, "Recovery Code").as_deref(),
            Some("old-recovery")
        );
    }

    // ⛔ THE REGRESSION THIS EXISTS TO CATCH. A field entry carries more than
    // name/value/type, and rebuilding the entry from what this client models
    // would drop the rest — the same data loss as rebuilding a cipher, one
    // level down. Setting a value MUTATES the entry.
    #[test]
    fn setting_a_custom_field_preserves_the_rest_of_that_entry() {
        let key_bytes = [0x5au8; 64];
        let body = field_vault(&key_bytes)
            .edit_body(
                "c1",
                &CipherEdit {
                    fields: vec![FieldEdit::Set {
                        name: "API Key".into(),
                        value: "new-token".into(),
                        kind: FieldKind::Text,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        let entry = &body["fields"][0];
        assert_eq!(
            entry["somethingBitwardenAddsIn2027"],
            serde_json::json!({"per": "field"}),
            "an unknown key inside a field entry must survive the write"
        );
        assert!(entry.get("linkedId").is_some(), "linkedId must survive");
    }

    // ⛔ UPDATING A SECRET MUST NOT EXPOSE IT. `FieldKind::Text` means "do not
    // change the visibility"; a hidden field that silently became a text field
    // on its next edit would be rendered in the clear by every Bitwarden client.
    #[test]
    fn setting_a_hidden_field_by_value_does_not_downgrade_it_to_text() {
        let key_bytes = [0x5au8; 64];
        let body = field_vault(&key_bytes)
            .edit_body(
                "c1",
                &CipherEdit {
                    fields: vec![FieldEdit::Set {
                        name: "Recovery Code".into(),
                        value: "new-recovery".into(),
                        kind: FieldKind::Text,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(body["fields"][1]["type"], FIELD_TYPE_HIDDEN);
        assert_eq!(
            field_in(&body, &key_bytes, "Recovery Code").as_deref(),
            Some("new-recovery")
        );
    }

    // …and the opposite direction IS allowed, because it is the user asking to
    // hide something, not a side effect.
    #[test]
    fn set_hidden_converts_a_text_field() {
        let key_bytes = [0x5au8; 64];
        let body = field_vault(&key_bytes)
            .edit_body(
                "c1",
                &CipherEdit {
                    fields: vec![FieldEdit::Set {
                        name: "API Key".into(),
                        value: "now-secret".into(),
                        kind: FieldKind::Hidden,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(body["fields"][0]["type"], FIELD_TYPE_HIDDEN);
    }

    #[test]
    fn removing_a_custom_field_drops_only_that_one() {
        let key_bytes = [0x5au8; 64];
        let body = field_vault(&key_bytes)
            .edit_body(
                "c1",
                &CipherEdit {
                    fields: vec![FieldEdit::Remove {
                        name: "API Key".into(),
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(body["fields"].as_array().unwrap().len(), 2);
        assert!(field_in(&body, &key_bytes, "API Key").is_none());
        assert_eq!(
            field_in(&body, &key_bytes, "Recovery Code").as_deref(),
            Some("old-recovery")
        );
    }

    // Absent is not empty, here too: "there was nothing to remove" and "removed
    // it" are different facts and the second must not be reported for the first.
    #[test]
    fn removing_an_absent_custom_field_is_an_error_not_a_no_op() {
        let vault = field_vault(&[0x5au8; 64]);
        let error = vault.edit_body(
            "c1",
            &CipherEdit {
                fields: vec![FieldEdit::Remove {
                    name: "Nope".into(),
                }],
                ..Default::default()
            },
        );
        assert!(matches!(error, Err(EditError::NoSuchField(name)) if name == "Nope"));
    }

    // A LINKED field points at the item's own username or password and stores
    // no value. Writing one would be meaningless, and quietly turning it into a
    // value field would break what it links to.
    #[test]
    fn setting_a_linked_custom_field_is_refused() {
        let vault = field_vault(&[0x5au8; 64]);
        let error = vault.edit_body(
            "c1",
            &CipherEdit {
                fields: vec![FieldEdit::Set {
                    name: "Linked User".into(),
                    value: "x".into(),
                    kind: FieldKind::Text,
                }],
                ..Default::default()
            },
        );
        assert!(matches!(error, Err(EditError::LinkedField(name)) if name == "Linked User"));
    }

    // Bitwarden permits duplicate field names. Picking one could overwrite the
    // wrong secret, and there is no undo for that.
    #[test]
    fn a_duplicated_custom_field_name_is_refused_rather_than_guessed() {
        let key_bytes = [0x5au8; 64];
        let mut raw = raw_login_record();
        let seal_str = |text: &str| seal(&key_bytes, text).to_string();
        raw["fields"] = serde_json::json!([
            {"name": seal_str("Token"), "value": seal_str("a"), "type": FIELD_TYPE_TEXT},
            {"name": seal_str("token"), "value": seal_str("b"), "type": FIELD_TYPE_TEXT},
        ]);
        let vault = Vault::new(
            SymmetricKey::from_bytes(&key_bytes).unwrap(),
            HashMap::new(),
            vec![RawCipher {
                raw,
                id: "c1".into(),
                item_type: 1,
                name: Some(seal(&key_bytes, "GitHub")),
                ..Default::default()
            }],
            vec![],
            HashMap::new(),
        );
        let error = vault.edit_body(
            "c1",
            &CipherEdit {
                fields: vec![FieldEdit::Set {
                    name: "Token".into(),
                    value: "c".into(),
                    kind: FieldKind::Text,
                }],
                ..Default::default()
            },
        );
        assert!(matches!(error, Err(EditError::AmbiguousField(name, 2)) if name == "Token"));
    }

    // Naming one field twice has no defined outcome — order would decide it.
    #[test]
    fn naming_one_custom_field_twice_in_an_edit_is_refused() {
        let vault = field_vault(&[0x5au8; 64]);
        let error = vault.edit_body(
            "c1",
            &CipherEdit {
                fields: vec![
                    FieldEdit::Set {
                        name: "API Key".into(),
                        value: "a".into(),
                        kind: FieldKind::Text,
                    },
                    FieldEdit::Remove {
                        name: "api key".into(),
                    },
                ],
                ..Default::default()
            },
        );
        assert!(matches!(error, Err(EditError::RepeatedField(name)) if name == "api key"));
    }

    // Every clearable field nulls, rather than omits: the server assigns
    // unconditionally, so an omitted key would leave the OLD value in place and
    // the clear would silently do nothing.
    #[test]
    fn every_clear_nulls_its_field_rather_than_omitting_it() {
        let vault = login_vault(&[0x5au8; 64]);
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    clear: [
                        ClearField::Notes,
                        ClearField::Totp,
                        ClearField::Username,
                        ClearField::Uri,
                        ClearField::Folder,
                    ]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(body["notes"].is_null(), "notes");
        assert!(body["folderId"].is_null(), "folderId");
        assert!(body["login"]["totp"].is_null(), "totp");
        assert!(body["login"]["username"].is_null(), "username");
        assert!(body["login"]["uris"].is_null(), "uris");
        // The password is deliberately NOT clearable, and it must ride through
        // a clear-everything edit untouched.
        assert_eq!(body["login"]["password"], "2.enc-pass");
        // And the unknown future key still survives all of that.
        assert_eq!(
            body["somethingBitwardenAddsIn2027"],
            serde_json::json!({"keep": "me"})
        );
    }

    // ⛔ A URI IS AN OBJECT, NOT A STRING. Re-minting an entry for a uri the
    // item already stores would discard its `match` type (and `uriChecksum`, and
    // whatever comes next) — the same loss raw-patching exists to prevent.
    #[test]
    fn an_unchanged_uri_keeps_its_match_type_and_unknown_keys() {
        let key_bytes = [0x5au8; 64];
        let mut raw = raw_login_record();
        raw["login"]["uris"] = serde_json::json!([{
            "uri": seal(&key_bytes, "https://github.com").to_string(),
            "match": 3,
            "uriChecksum": "chk",
        }]);
        let vault = Vault::new(
            SymmetricKey::from_bytes(&key_bytes).unwrap(),
            HashMap::new(),
            vec![RawCipher {
                raw,
                id: "c1".into(),
                item_type: 1,
                name: Some(seal(&key_bytes, "GitHub")),
                uris: vec![seal(&key_bytes, "https://github.com")],
                ..Default::default()
            }],
            vec![],
            HashMap::new(),
        );
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    uris: vec![
                        "https://github.com".into(),
                        "https://gist.github.com".into(),
                    ],
                    ..Default::default()
                },
            )
            .unwrap();
        let uris = body["login"]["uris"].as_array().unwrap();
        assert_eq!(uris.len(), 2, "the list is replaced, in the order given");
        assert_eq!(uris[0]["match"], 3, "an unchanged uri keeps its match type");
        assert_eq!(uris[0]["uriChecksum"], "chk", "and its unknown keys");
        // The new one is freshly sealed, with no match type invented for it.
        assert!(uris[1]["match"].is_null());
        let key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let written = EncString::parse(uris[1]["uri"].as_str().unwrap()).unwrap();
        assert_eq!(
            key.decrypt_to_string(&written).unwrap(),
            "https://gist.github.com"
        );
    }

    // An empty value is refused everywhere a value can be set, so "" can never
    // reach the server as an encrypted empty string that looks like a value.
    #[test]
    fn an_empty_value_is_refused_for_a_uri_and_a_custom_field() {
        let vault = field_vault(&[0x5au8; 64]);
        for edit in [
            CipherEdit {
                uris: vec![String::new()],
                ..Default::default()
            },
            CipherEdit {
                fields: vec![FieldEdit::Set {
                    name: "API Key".into(),
                    value: String::new(),
                    kind: FieldKind::Text,
                }],
                ..Default::default()
            },
            CipherEdit {
                fields: vec![FieldEdit::Set {
                    name: String::new(),
                    value: "x".into(),
                    kind: FieldKind::Text,
                }],
                ..Default::default()
            },
        ] {
            assert!(matches!(
                vault.edit_body("c1", &edit),
                Err(EditError::EmptyValue)
            ));
        }
    }

    // ⛔ THE WRITE-VERIFICATION IS THE POINT OF THE WHOLE EDIT PATH. A 200 from
    // PUT says the server took a body; only a re-read says the field is what
    // the user asked for. This runs the built body back through a vault, as the
    // resync does, and checks both directions.
    #[test]
    fn verify_edit_finds_what_landed_and_names_what_did_not() {
        let key_bytes = [0x5au8; 64];
        let edit = CipherEdit {
            name: Some("GitHub (work)".into()),
            username: Some("octocat-work".into()),
            uris: vec!["https://github.com".into()],
            fields: vec![FieldEdit::Set {
                name: "API Key".into(),
                value: "new-token".into(),
                kind: FieldKind::Text,
            }],
            clear: [ClearField::Notes].into_iter().collect(),
            ..Default::default()
        };
        let body = field_vault(&key_bytes).edit_body("c1", &edit).unwrap();

        // What the server would send back on the next sync, parsed as `sync`
        // parses it. This is the re-read `edit_item` performs.
        let mut raw = body.clone();
        raw["id"] = serde_json::json!("c1");
        let written = |path: &str| EncString::parse(body[path].as_str().unwrap()).unwrap();
        let after = Vault::new(
            SymmetricKey::from_bytes(&key_bytes).unwrap(),
            HashMap::new(),
            vec![RawCipher {
                raw,
                id: "c1".into(),
                item_type: 1,
                name: Some(written("name")),
                username: Some(
                    EncString::parse(body["login"]["username"].as_str().unwrap()).unwrap(),
                ),
                uris: vec![
                    EncString::parse(body["login"]["uris"][0]["uri"].as_str().unwrap()).unwrap(),
                ],
                ..Default::default()
            }],
            vec![],
            HashMap::new(),
        );
        let verification = after.verify_edit("c1", &edit);
        assert!(
            verification.is_complete(),
            "every change should be visible on a re-read, missing: {:?}",
            verification.missing
        );
        for label in ["name", "username", "uri", "field:API Key", "clear:notes"] {
            assert!(
                verification.landed.iter().any(|got| got == label),
                "{label} should be reported as landed: {:?}",
                verification.landed
            );
        }

        // …and a change the item does NOT carry is named, not glossed over.
        let unlanded = after.verify_edit(
            "c1",
            &CipherEdit {
                password: Some("never-written".into()),
                ..Default::default()
            },
        );
        assert_eq!(unlanded.missing, vec!["password".to_string()]);
        assert!(!unlanded.is_complete());
    }

    // ⛔ A VERIFICATION MUST NOT BECOME A LEAK. It travels to every caller's
    // --json output, so it may name a field and must never carry its value.
    #[test]
    fn a_verification_carries_names_and_never_values() {
        let key_bytes = [0x5au8; 64];
        let edit = CipherEdit {
            password: Some("hunter2-in-the-clear".into()),
            fields: vec![FieldEdit::Set {
                name: "API Key".into(),
                value: "sk-live-topsecret".into(),
                kind: FieldKind::Hidden,
            }],
            ..Default::default()
        };
        let verification = field_vault(&key_bytes).verify_edit("c1", &edit);
        let wire = serde_json::to_string(&verification).unwrap();
        for secret in ["hunter2-in-the-clear", "sk-live-topsecret"] {
            assert!(
                !wire.contains(secret),
                "{secret} leaked into the verification"
            );
        }
        assert!(
            wire.contains("field:API Key"),
            "the field NAME is the receipt"
        );
    }

    #[test]
    fn edit_body_replaces_the_whole_uri_list() {
        let key_bytes = [0x5au8; 64];
        let vault = login_vault(&key_bytes);
        let body = vault
            .edit_body(
                "c1",
                &CipherEdit {
                    uris: vec!["https://example.com".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        let uris = body["login"]["uris"].as_array().unwrap();
        assert_eq!(uris.len(), 1);
        let user_key = SymmetricKey::from_bytes(&key_bytes).unwrap();
        let written = EncString::parse(uris[0]["uri"].as_str().unwrap()).unwrap();
        assert_eq!(
            user_key.decrypt_to_string(&written).unwrap(),
            "https://example.com"
        );
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
        let vault = Vault::new(
            user_key,
            HashMap::new(),
            vec![cipher],
            vec![],
            HashMap::new(),
        );
        assert_eq!(vault.items()[0].name, "Sealed Item");
        assert_eq!(vault.password("c1").as_deref(), Some("under-item-key"));
    }
}
