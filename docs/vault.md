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

Ops: `ping`, `status`, `unlock`, `lock`, `stop`, `handover`, `sync`, `list` (`trashed:true`
for the trash), `get`, `notes`, `fields`, `card`, `card-secret`, `totp`,
`totp-secret`, `passkeys`, `match`, `suggest`, `add`, `edit`,
`rm`, `restore`, `generate`. The agent auto-starts on `unlock` (and on
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

**After rebuilding, hand the agent over — or, failing that, stop it.**

### `handover`: a rebuild that does not cost an unlock

`stop-agent` drops the keys, so every rebuild used to cost a master password
typed by hand on every host. `ychrome-vault handover` is the cheap route: the
running agent `execve`s the newly installed binary **in place**, keeping its pid
and its bound listener fd. Same process, same socket, new code, vault still
unlocked.

The framing that matters, because the obvious one is wrong: **`execve` does not
keep memory.** It keeps the pid and the open file descriptors and throws the
whole address space away. The keys have to cross the boundary one way or
another, and the only question is on what.

| channel | verdict |
| --- | --- |
| argv | refused — world-readable in `/proc/<pid>/cmdline` |
| environment variable | refused — same |
| a file | refused — it outlives the process that wrote it |
| a socket | refused — another process could connect to it |
| **an anonymous pipe** | used — its buffer lives in the kernel, is reachable only through its two fds, and dies with the process if the exec never happens |

What crosses is small, and that is the whole argument: 64 bytes of user key, the
two bearer tokens, and the idle clock. Ciphers, org keys and folder names are all
re-derived by `resync`, which needs the user key and the bearer alone. The
listener fd crosses too, with `FD_CLOEXEC` cleared, so there is **no unbind
window** — a client that connects mid-swap is queued in the socket's backlog and
answered by the successor rather than told "no vault agent".

Both fds are prepared in exactly one place, `agent::arm_handover`, and the test
that crosses an exec goes through it rather than clearing the bits itself. That
is not tidiness: while the test did its own fd preparation, the listener's
CLOEXEC clear — the guard the whole no-unbind-window claim rests on — could be
deleted outright with every test still green.

**On the far side, an inherited fd number is checked before it is owned.**
`UnixListener::from_raw_fd` validates nothing: on a number that is no longer open
the Rust runtime aborts the process (SIGABRT, "IO Safety violation") before any
of our code runs, and on a number the successor's own startup reallocated it
succeeds on the *wrong object*, which then fails every `accept` forever. So the
listener must be a live, listening `AF_UNIX` stream socket and the payload must
be a pipe. Neither refusal is fatal, and neither costs more than it has to: the
socket and the session are separate things, so a bad listener binds a fresh
socket and **keeps the unlock** (what is lost is the seamless window, and the
successor says so), while a bad payload keeps the inherited socket and serves
**locked**, exactly as for a payload that will not decode. The accept loop is
bounded for the same reason: repeated failures back off and then exit with a
reason, because a listener nobody can connect to has already lost the unlock, and
spinning on it just burns a core while saying nothing.

The exec is a **one-way door**: if the successor cannot come up, the unlock is
gone and the user retypes. So every guard runs before the reply:

1. the agent must be serving a socket, and the vault must be **unlocked**
   (locked means there is nothing to carry, and `stop-agent` is free then);
2. the successor is resolved **by the agent** (`installed_vault_exe`), never
   named by the client — an exec target taken from a request would be privilege
   escalation by asking;
3. it must not be the binary already running, or the agent would exec into
   itself forever;
4. it must answer `--version` cleanly, which catches a truncated `scp`, a wrong
   architecture, or a libc that is too new — cheaply, while the unlock is still
   recoverable;
5. the **session** must still work: the agent `resync`s first (renewing the
   bearer if it had expired), because the successor re-pulls the ciphers with
   that token and nothing else.

If the exec still fails, the old process keeps serving and says so on stderr: a
failed handover costs nothing. If the successor comes up but cannot reach the
server, it keeps the keys, serves with an empty vault, and prints the remedy
(`ychrome-vault sync`) rather than throwing an unlock away.

**The honest cost.** This creates the first path by which the user key can leave
`SymmetricKey` at all (`to_bytes`, `pub(crate)`, one caller). The bytes exist
unsealed in a `Zeroizing` buffer for the length of one write to a private fd,
read by a process that is about to be this same pid. That is a real widening of
the surface, and it is not free — it is just far cheaper than the alternative it
replaces, and strictly narrower than a keyring or a second live process, either
of which would create a durable second owner of the key where today there is
none.

**The sequencing trap.** `handover` is itself an op the OLD agent does not know,
so installing it costs one final `stop-agent` on each host. It pays from the
deploy after. That is also why the unknown-op hint points a *`handover`* request
at `stop-agent` instead of at itself.

Proven so far: the payload framing (encoder against an independently written
decoder, and every truncation of it), the fd handling through a real pipe
*including the listener's CLOEXEC clear*, the refusal of an inherited fd that is
not what its flag promises, the accept loop giving up instead of spinning, the
guards above, and — in `tests/handover_adopt.rs` — a genuinely separate
`ychrome-vault agent` process adopting an inherited listener and an inherited
session, coming up **unlocked** with no master password and locking on the
inherited idle clock rather than a fresh one. Each of those has been shown to go
red when the guard it covers is removed; a lock nobody has watched fail is not a
lock. **Not yet proven:** the `execve` itself, whose signature is
same-pid-new-image and which only a live agent can show. See the live recipe in
`.claude/skills/ychrome/SKILL.md`.

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
  entry for `example.com` is offered on `chat.example.com`.
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
ychrome-vault passkeys github.com                # rpId<TAB>user<TAB>credId<TAB>created
ychrome-vault card "HDFC Regalia"                # brand<TAB>holder<TAB>month<TAB>year<TAB>last4
ychrome-vault list                               # name<TAB>user<TAB>folder
ychrome-vault match chat.example.com                  # what an auto-fill may use
ychrome-vault generate 24                        # local dice, no vault touched
ychrome-vault add example.com alice --generate --uri https://example.com
ychrome-vault edit example.com alice --generate   # rotate the password
ychrome-vault rm example.com alice                # to the trash, restorable
ychrome-vault list --trashed                      # show the trash
ychrome-vault restore example.com alice           # bring it back from the trash
ychrome-vault lock
ychrome-vault handover                            # after a rebuild, keeps the unlock
ychrome-vault stop-agent                          # the fallback, which RE-LOCKS
```

The master password is read from **stdin only** — never a flag, never an
environment variable — and is dropped the moment the keys are derived. A
terminal on stdin is refused rather than echoed into the user's scrollback.

### Absent is not empty

`get --field FIELD` fails (non-zero, reason on stderr, nothing on stdout) when
the item does not carry that field, and succeeds when the item stores it as an
empty string. Those are different facts and a script has to be able to tell them
apart: `USER=$(ychrome-vault get ITEM --field username)` used to capture `""` and
exit 0 for both, because the printer was `as_str().unwrap_or_default()`. The same
rule now covers `fields --field-name` on a custom field with no readable value.

There are **two** reasons a custom field has none, and the refusal names the one
the vault actually determined rather than guessing: a **linked** field stores no
value of its own (it points at the item's username or password, and there is
nothing to go looking for), while an **unreadable** one has a value stored that
this vault could not decrypt — the org-cipher case, where one organization key
failing to unwrap is deliberately non-fatal and leaves exactly that shape behind.
`Vault::fields` owns the distinction (`FieldValue::Linked` vs
`FieldValue::Unreadable`) and the agent puts it on the wire as `absent`; nothing
downstream re-derives it from a null. Telling a user with a key problem to go
find a link that does not exist is a stale answer, which is the one thing a
credential reader must never serve.

The accepted `--field` values have one owner, the `GetField` enum: clap derives
the flag's help text, its accepted values and its "invalid value" error from it,
and the match is exhaustive over the same variants. The list had drifted across
four places — the flag's doc named four fields, the match accepted five, and the
error message named a different four, which is how `totp-secret` ended up working
but undocumented.

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
| — (rbw has none) | `ychrome-vault restore NAME [USER]` (undo a soft `rm`) |
| — (rbw has none) | `ychrome-vault list --trashed` (show the trash) |

## Cards: metadata on the CLI, the number only through the injector

A card cipher (`type` 3) has no login block at all, so it carries no password —
and the `get` op resolves the password *first*, which is why every one of them
answered "has no password" whatever `--field` asked for. 130 of this vault's 1113
items are in that state; before `card` they were reachable only through `notes`.

Like notes and custom fields, a card is **read off the raw record**: `sync` never
parses one into `RawCipher`, and adding a parsed `card` field there would be a
second encoding that `edit_body`'s raw patching could silently diverge from. The
record's own `type` is what makes an item a card, so a stray `card` object on a
login is not readable as one — otherwise `VaultItem::item_type` (which the
sidebar draws its fill button from) and this reader could disagree about what an
item is.

The reader is split in two, and the split is enforced by the type system rather
than by discipline:

| | carries | shape |
| --- | --- | --- |
| `Vault::card` → `CardInfo` | brand, cardholder, expiry, **last four** | `Serialize`; a test proves the PAN and CVV cannot appear |
| `Vault::card_secret` → `CardSecret` | the full number, the CVV | **not** `Serialize`, **not** `Debug`, `Zeroizing` |

`ychrome-vault card NAME` prints the metadata row and there is deliberately **no
CLI verb for the number**. The threat being answered is not the socket — any
same-uid process can already pull every password one `get` at a time — it is the
**transcript**: a PAN printed to a terminal persists in scrollback, shell
history, and any agent CLI's JSONL, and unlike a password it cannot be rotated on
demand. So the number reaches a page the way a password does, as an `eval` script
the GUI injects (the sidebar's `card-fill` action → the `card-secret` op), and
that script returns the list of field names it filled, never a value.

`VaultItem` now carries `item_type` for the same reason the passkey badge exists:
it is secret-free, and it is the only thing that can explain to a listing why an
item refuses `get`.

### TODO — an audited agent card path (end-to-end)

Today's boundary: metadata via the CLI, PAN/CVV only through the human-driven
sidebar injector. A live run (2026-07-26) proved the gap the hard way: the
yggterm verb plane advertises `web fill-card --field number|expiry|code|holder`
while this plane refuses the op (`vault_cli_no_card_op`) — one tool promises
what the other forbids, and an agent discovers it only at a real gateway's
card form. Filed in yggterm's pending-bugs.

The settled direction is to make the agent path real rather than widen the
leak surface:

1. An AUDITED injection op: the injector serves the page directly (no value in
   any transcript, exactly as the sidebar path already works), gated per-use
   or per-session by an explicit user grant, every use traced (item, target
   host, requesting agent).
2. A phone-bridge OTP watcher for 3DS: one shared implementation, returns a
   code once, never logs it — and its store must prove itself LIVE with a
   triggered canary before any absence-of-a-message conclusion is drawn.
3. Until built: agents cannot pay by card, the sidebar remains human-only, and
   the yggterm verb should refuse at parse time with the policy reason.

#### ⛔ FIELD CORRECTION, 2026-07-26 18:19 IST — `web fill-card` DOES NOT WORK TODAY

The block above says the earlier agent "misread this very design" and that the
verb plane "already exposes" card filling. Half of that is right and the
operative half is wrong. Tested live on the GUI host against a real gateway card form
(`areionsbi.wibmo.com/cardcapture/`, fields `nameOnCard` / `pan` /
`expiryDateYYYY` / `cvv2`), all four calls were refused identically:

```
yggterm server app web fill-card --item 'IDFC WOW AVIKALPA' --field number --selector '#cr_no'
  -> {"accepted": false, "reason": "vault_cli_no_card_op"}
```

**Why, precisely.** `card-secret` exists as an **agent-socket op**
(`crates/ychrome-vault/src/agent.rs:760`) and the **sidebar** uses it
(`src/sidebar.rs:1073` sends `{"op":"card-secret",...}` over the socket). It is
**not a CLI subcommand** — `ychrome-vault card-fill`, `card-secret` and
`cardfill` are all `unrecognized subcommand`, and `ychrome-vault card` is
metadata-only on purpose. yggterm's `fill-card` reaches the vault **through the
CLI**, so it looks for a card op that the CLI deliberately does not expose and
refuses. Hence the error's own wording: *vault_**cli**_no_card_op*.

So the earlier agent's CONCLUSION — an agent cannot pay by card — was CORRECT
for the deployed stack; only their reasoning was incomplete. Please do not
re-invert this without running the verb: the doctrine block above sent this run
down the card path after netbanking had already been closed off, and both routes
were dead.

**The fix is small and specific, and it is NOT the OTP watcher.** The OTP hop is
the *second* gap; the first is that `fill-card` never reaches the secret.
Either:
- point yggterm's `fill-card` at the **vault agent socket** with
  `{"op":"card-secret"}` — the same door the sidebar already uses, keeping the
  PAN off every transcript exactly as designed; or
- add a CLI op that injects without printing, if the socket must stay
  sidebar-only.

Until one of those ships, `web fill-card --help` advertises
`--field number|expiry|code|holder`, which promises what the credential plane
refuses — a verb that can only fail. Worth a parse-time refusal with a pointer
to this note, so the next agent finds out before staging a filing and burning an
OTP rather than after.

**Also correct the netbanking half:** this run did NOT pay via netbanking. It
reached the IDFC transaction-OTP screen, the OTP never arrived, the bank session
timed out, and a later re-login with a verified-correct credential was refused
("Please enter valid credentials"). Netbanking is therefore **not** the
"OTP-free fallback" this doc describes — login is OTP-free, but the *transaction*
demands a 5-minute OTP. Nothing was paid by any route.

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

### `restore` undoes a soft `rm`

A soft-deleted item is retained, not dropped: `sync` sorts every cipher carrying
a `deletedDate` into a separate **trash** bucket instead of discarding it, so the
client can now both *show* the trash (`list --trashed`) and reverse the delete
(`restore NAME [USER]` → `PUT /api/ciphers/{id}/restore`). The two buckets never
overlap, and `restore` resolves its name **against the trash only** — it can
never bring back or touch a live entry that happens to share a name. A
`--permanent` removal leaves nothing in the trash and cannot be restored.

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
  and left the item list (1108 → 1107).
- **`restore` + `list --trashed`** — built, and covered by `cargo test`: `sync`
  now retains `deletedDate` ciphers in a trash bucket, the live list and the
  trash provably never overlap, and `restore` resolves names against the trash
  only. **Not yet exercised against a real server** — the full
  create → soft-`rm` → `list --trashed` → `restore` → verify loop needs one live
  unlock with this binary (installing a new binary re-locks the agent). That is
  the one owed proof, exactly as `edit`/`rm` owed theirs until a live unlock.
- **Cards** — built and covered by `cargo test` against a sealed synthetic card
  (decryption under the cipher key, PascalCase drift, a plaintext `brand`, an
  undecryptable sub-field dropped rather than surfaced as ciphertext, and the
  no-PAN/no-CVV property of `CardInfo` asserted on the serialized form). **Not
  yet exercised against the real vault**: reading a real card needs a live
  unlock with this binary, and the sidebar's `card-fill` injector has had no
  faithful pixel. What is proven is the crypto and the shape; what is owed is one
  `ychrome-vault card "<a real card>"` and one screenshot of a card row.
- **Passkeys** (`fido2Credentials`) — **read layer + assertion signer built**:
  - *Read* (slice 1): `sync` parses `login.fido2Credentials[]` into
    `RawCipher::fido2`, `list` badges `has_passkey`, `passkeys NAME` returns
    secret-free metadata (`Vault::passkeys`). A `cargo test` proves the private
    key (`keyValue`) never reaches the listing; live-checked against the real
    vault (23 items badge a passkey; `dash.cloudflare.com` decrypts, no leak).
  - *Assertion signer* (`crates/ychrome-vault/src/fido2.rs`,
    `Vault::fido2_assert`): the `get()` ceremony's crypto — builds
    `authenticatorData` (SHA-256(rpId)‖flags‖signCount) and signs
    `authenticatorData‖clientDataHash` with ES256 (P-256) over the decrypted
    `keyValue`, DER-encoded. KAT-proven: the signature verifies against the
    credential's public key. **The agent may never auto-consent** — the signer
    takes a `fido2::UserPresence` **by value**, and its only constructor is
    `granted()`, which the GUI's presence dialog calls; a headless agent has no
    path to a signature. There is deliberately **no agent/CLI op** for it yet —
    exposing one over the socket would be an auto-consent path.
  - **Not yet built** (the browser slice): the `navigator.credentials` userscript
    shim (WebKitGTK has no WebAuthn), the loopback signer bridge, the
    user-presence dialog that mints `UserPresence`, credential *creation*
    (`create()`), and signCount increment. `keyValue` decoding + real-RP
    acceptance are validated there.
