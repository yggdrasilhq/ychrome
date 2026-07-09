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
| `crypto` | KDF → master key → stretched key → user key; EncString type-2 (AES-256-CBC + HMAC-SHA256), MAC checked in constant time before decrypt |
| `api` | `prelogin`, the identity token endpoint, `sync`. Responses navigated case-insensitively (Vaultwarden drifts PascalCase↔camelCase) |
| `model` | The unlocked `Vault`: user key + still-encrypted ciphers. Metadata is secret-free; passwords and TOTP secrets decrypt on demand |
| `totp` | RFC 6238, `otpauth://` URIs |
| `matching` | Page-host → item rules (below) |
| `session` | `VaultManager`: config, unlock/lock, and the bearer token held for `resync` |
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

Ops: `ping`, `status`, `unlock`, `lock`, `sync`, `list`, `get`, `totp`, `match`,
`suggest`. The agent auto-starts on `unlock` (and on `ping`) and detaches into
its own process group, so the shell that first needed it can go away. A socket
left behind by a SIGKILLed agent is detected (nobody answers) and reclaimed.

Read ops deliberately do **not** auto-start an agent: a fresh one holds no keys,
so `get` would fail anyway, and it would leave a pointless daemon behind. They
say "no agent, run `ychrome-vault unlock`" instead.

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
ychrome-vault lock
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

## What is proven, and what is not

The read path is proven end to end against the real vault at `vault.example.com`
(1107 items, 34 with TOTP) and, in `cargo test`, against a synthetic vault
sealed with the real primitives — so `list`/`get`/`totp`/`match`/`suggest` are
covered without a network or a master password.

**Writes are not implemented.** `crypto` has EncString decrypt only; adding a
login needs encrypt (AES-256-CBC + HMAC, with known-answer tests) plus
`POST /api/ciphers`. Passkeys (`fido2Credentials`) come after that, and need a
`navigator.credentials` shim because WebKitGTK has no WebAuthn.
