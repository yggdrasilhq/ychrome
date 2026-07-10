# ychrome's vault: a native Bitwarden/Vaultwarden client

ychrome owns its password vault. Not a wrapper around `rbw`, not a feature of
the terminal that hosts it — a libyggterm app owns its capabilities, its crate,
and its state. Everything here lives on the host ychrome **runs** on, which over
ssh is not the host the yggterm GUI is on.

Crate: `crates/ychrome-vault` (lib + `ychrome-vault` binary).
State: `~/.yggterm/vault/` — `config.json` (secret-free) and `agent.sock`.

## The pieces

| Module | What it owns |
| --- | --- |
| `crypto` | KDF → master key → stretched key → user key; EncString type-2 (AES-256-CBC + HMAC-SHA256), MAC checked in constant time before decrypt; type-3/4 (RSA-OAEP) for organization keys |
| `api` | `prelogin`, the identity token endpoint, `sync`. Responses navigated case-insensitively (Vaultwarden drifts PascalCase↔camelCase) |
| `model` | The unlocked `Vault`: user key + still-encrypted ciphers. Metadata is secret-free; passwords and TOTP secrets decrypt on demand |
| `totp` | RFC 6238, `otpauth://` URIs |
| `matching` | Page-host → item rules (below) |
| `generator` | Local password generation (no server, no `rbw generate` subprocess) |
| `watchtower` | Reused + weak password analysis. Groups by SHA-256 digest, so no plaintext password ever sits in a collection; only entry labels leave the module |
| `session` | `VaultManager`: config, unlock/lock, `add_login`, and the bearer token held for `resync` |
| `agent` | The unlock cache: a unix-socket daemon holding the decrypted vault |

## The agent

A vault that re-derives PBKDF2/600000 and re-syncs ~1100 ciphers on every `get`
is unusable for automation. That — not the crypto — is what `rbw-agent` actually
bought us. So one long-lived process holds the unlocked vault in memory: `unlock`
once, and `list` / `get` / `totp` are keyless and instant until an idle timeout
drops it (`lock_timeout_secs` in `config.json`, default 3600, `0` = never).

**The socket is the auth.** `~/.yggterm/vault/` is `0700` and `agent.sock` is
`0600`, so reaching it already requires being this uid. There is no TCP port for
another local user to connect to, and no token to leak into an argv or an
environment variable — a same-uid attacker could read any token we invented, so
a token would buy exactly nothing. The filesystem does the work.

Requests and responses are one JSON object per line:

```text
{"op":"get","name":"github.com","user":null}
{"ok":true,"entry":{"name":"github.com","username":"octocat","password":"…"}}
```

Ops: `ping`, `status`, `unlock`, `lock`, `stop`, `sync`, `list`, `get`, `totp`,
`match`, `suggest`, `add`, `generate`. The agent auto-starts on `unlock` (and on
`ping`) and detaches into its own process group, so the shell that first needed
it can go away. A socket left behind by a SIGKILLed agent is detected (nobody
answers) and reclaimed.

Read ops deliberately do **not** auto-start an agent: a fresh one holds no keys,
so `get` would fail anyway, and it would leave a pointless daemon behind. They
say "no agent, run `ychrome-vault unlock`" instead.

### The agent outlives the binary

Rebuild `ychrome-vault` and the *old* process keeps answering. `get` still
works; a newly added op comes back `unknown op`; the confusion is total. This is
the same stale-daemon trap yggterm keeps falling into, so the agent is built to
make it visible:

- `status` reports `version` and `exe_stamp` (path + mtime), and the client sets
  `agent_stale: true` when they differ from its own.
- Any `unknown op` error is rewritten to name the cause and the remedy.
- `ychrome-vault stop-agent` retires it. Because `stop` is *itself* an op that a
  sufficiently old agent does not know, `stop()` falls back to signalling
  `agent.pid` (SIGTERM, then SIGKILL — an agent holding decrypted keys must
  never survive a `stop`). An agent older than the pid file says so plainly
  rather than pretending to have worked.

**After rebuilding, run `ychrome-vault stop-agent`.**

## Organization ciphers

A cipher that belongs to an organization has its fields sealed under **that
org's** symmetric key, not the user key. The org key arrives from `sync` as
`profile.organizations[].key` — a **type-4** EncString, RSA-OAEP-SHA1, sealed to
the user's public key. Unwrapping it needs the user's RSA private key, which
arrives as `profile.privateKey` (a type-2 EncString under the user key,
containing PKCS#8 DER).

So: unlock → user key → private key → org keys → org ciphers.

This was missed at first, and the failure was **silent**: `Vault::items()` skips
any cipher it cannot decrypt, so 59 of a 1107-item vault simply were not there,
while `status` cheerfully reported 1107. `ychrome-vault diagnose` now accounts
for every cipher, and `item_count` counts only what we can actually read. An
account in no organizations never touches RSA at all.

Failing to unwrap ONE org is not fatal — that org's ciphers stay unreadable and
`diagnose` counts them, which beats refusing to open the whole vault.

## Host matching: two deliberately asymmetric rules

Both consider the item **name** and its stored **URIs**. (`rbw list` had no URI
field, so the sidebar's old rules could only read names — which is why an entry
called "Amazon" never matched `amazon.com`.)

- **Loose — `suggest`.** Exact host, its `www.` twin, or a base-domain suffix.
  Used to float rows to the top of the sidebar; a human then clicks one. An
  entry for `gour.top` is offered on `chat.example.com`.
- **Strict — `match`.** Exact host or its `www.` twin only. Used by the auto
  paths (password fill, TOTP), which commit a secret to a page with nobody
  confirming the choice. A base-domain entry must **never** auto-fill a
  subdomain.

Ties (several accounts on one site) break by sorting on `(name, username)` and
taking the first — deterministic.

## CLI

```sh
ychrome-vault configure --server https://vault.example.com --email you@example.com
read -rs PW; echo "$PW" | ychrome-vault unlock   # once
ychrome-vault get github.com                     # password on stdout
ychrome-vault totp github.com                    # 6-digit code
ychrome-vault list                               # name<TAB>user<TAB>folder
ychrome-vault match chat.example.com                  # what an auto-fill may use
ychrome-vault generate 24                        # local dice, no vault touched
ychrome-vault add example.com alice --generate --uri https://example.com
ychrome-vault edit example.com alice --generate   # rotate the password
ychrome-vault rm example.com alice                # to the trash, restorable
ychrome-vault lock
ychrome-vault stop-agent                          # after every rebuild
```

The master password is read from **stdin only** — never a flag, never an
environment variable — and is dropped the moment the keys are derived. A
terminal on stdin is refused rather than echoed into the user's scrollback.

`rbw` parity, so existing scripts keep working:

| rbw | ychrome-vault |
| --- | --- |
| `rbw list --fields name,user,folder` | `ychrome-vault list` (same TSV) |
| `rbw get NAME [USER]` | `ychrome-vault get NAME [USER]` |
| `rbw code NAME [USER]` | `ychrome-vault totp NAME [USER]` |
| `rbw unlock` | `read -rs PW; echo "$PW" \| ychrome-vault unlock` |
| `rbw lock` | `ychrome-vault lock` |
| `rbw add NAME [USER]` | `ychrome-vault add NAME [USER]` |
| `rbw generate` | `ychrome-vault generate` |
| _(none — rbw has no watchtower)_ | `ychrome-vault watchtower` (reused + weak, labels only) |
| `rbw remove NAME [USER]` | `ychrome-vault rm NAME [USER]` (trash, not destroy) |
| — (rbw has none) | `ychrome-vault edit NAME [USER]` |

## Writes

`add` encrypts every field under the user key locally and `POST`s the
EncStrings to `/api/ciphers`; the server never sees plaintext. `--generate`
rolls the password here, so it never crosses a shell's argv.

### `edit` patches the raw record, it does not rebuild one

A Bitwarden `PUT /api/ciphers/{id}` replaces the **whole** cipher. The server
assigns unconditionally — `cipher.notes = data.notes` — so a field missing from
the request is not left alone, it is destroyed. `sync` only parses the fields
this client models, so a body rebuilt from `RawCipher` would silently drop every
item's notes, custom fields, favorite flag and password history.

So `RawCipher` keeps the untouched `raw` JSON from `sync`, and `Vault::edit_body`
patches *that*:

- Server-managed keys (`id`, `revisionDate`, `collectionIds`, …) are stripped by
  a **denylist**, not an allowlist — a field Bitwarden adds in a future version
  rides back untouched instead of being dropped by a client written before it.
- Patched fields are encrypted under the **cipher's** key (its own item key, or
  its organization's), never blindly under the user key. Getting this wrong is
  invisible: the MAC check fails and `items()` silently skips the item.
- The raw `revisionDate` is echoed as `lastKnownRevisionDate`, so a server whose
  copy moved on since our last sync **refuses** the write ("The client copy of
  this cipher is out of date") instead of clobbering another client's edit.
- Replacing a password prepends the OLD ciphertext to `passwordHistory`, reusing
  it verbatim rather than re-encrypting.
- Clearing a field is **not** expressible: `--notes ""` is rejected rather than
  quietly encrypting an empty string. That needs its own verb.

### `rm` trashes by default

The two delete routes are different operations, and the difference is
unrecoverable. Verified against the deployed vaultwarden commit (`f21a3ada`,
2025.12.0) rather than from memory — an earlier note in this campaign had them
backwards, which would have destroyed items while reporting them recoverable:

| call | route | effect |
| --- | --- | --- |
| `ychrome-vault rm` | `PUT /api/ciphers/{id}/delete` | `SoftSingle` → trash, restorable from any client |
| `ychrome-vault rm --permanent` | `DELETE /api/ciphers/{id}` | `HardSingle` → gone, no trash copy, no undo |

Soft is the default at every layer, and the CLI reports which one happened
(`"trashed": true` vs `"permanent": true`). `rbw remove` hard-deletes; this is
deliberately safer than parity. `rm` is **not** wired into the sidebar — a
destructive verb needs its contract confirmed before it gets a button.

## What is proven, and what is not

- **Read path** — proven end to end against the real vault at `vault.example.com`
  (1107 ciphers, 35 with TOTP, 936 with URIs), and in `cargo test` against a
  synthetic vault sealed with the real primitives, so
  `list`/`get`/`totp`/`match`/`suggest` are covered with no network and no
  master password.
- **Organization keys** — the RSA unwrap is cross-checked in `cargo test`
  against openssl-produced fixtures (`testdata/`), and the cipher-key selection
  is tested both with and without the org key. Reading the real vault's 59 org
  ciphers is verified separately (see the campaign memory).
- **Encrypt** — pinned known-answer vector, cross-checked against an
  independently written sealer, plus IV-coverage and wrong-key rejection. A
  round-trip test alone would pass even if encrypt and decrypt drifted together.
- **`add` against a real server** — proven on `vault.example.com`: an item was created,
  `cipher_count` went 1107 → 1108, `get` round-tripped the exact generated
  password, and `match` resolved it by its stored URI.
- **`edit` against a real server** — proven on `vault.example.com`. Notes were written to
  an item on one host; a **password-only** edit was then issued from a *different*
  host's client; the notes read back intact, alongside name, username and URI.
  That is exactly the silent data loss raw-retention exists to prevent. Custom
  fields, favorite and password history are covered by `cargo test` only.
- **The `lastKnownRevisionDate` guard** — fired for real, unplanned: the second
  host's agent had cached the cipher before the first host's edit, and the server
  refused the write ("The client copy of this cipher is out of date"). Two
  long-lived agents WILL go stale against each other — `sync` before a write.
- **`rm` against a real server** — proven: the item was trashed (`"trashed": true`)
  and left the item list (1108 → 1107). Note that `sync` filters `deletedDate`
  items, so this client cannot *display* the trash: "restorable" rests on the
  route, verified in the deployed server's source, not on an observed restore.
  A `restore` verb would close that loop and give `rm` an undo. Not built.
- **Passkeys** (`fido2Credentials`) — not started. Needs a
  `navigator.credentials` shim, because WebKitGTK has no WebAuthn.
