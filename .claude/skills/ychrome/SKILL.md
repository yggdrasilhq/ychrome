---
name: ychrome
description: Read BEFORE touching anything in the ychrome repo — the browser (libyggterm pilot app) or the ychrome-vault crate (native Bitwarden/Vaultwarden client, the rbw replacement). Covers the repo map, the destructive-verb contract for the vault (soft vs hard delete, the revision guard, raw-record patching), the stale-agent trap, build/deploy across the 5-host fleet, verification recipes, and what is still open. Triggers on: ychrome, ychrome-vault, vault, Bitwarden, Vaultwarden, rbw, passkeys, web surface, OSC 7717, profile picker, adblock, userscripts.
---

# ychrome

Two things live in this repo, and they share one rule.

1. **ychrome, the browser** — a web viewport for the Yggdrasil ecosystem and the
   **pilot app for libyggterm**: a program launched in a yggterm terminal takes
   over yggterm's GUI surfaces. `src/main.rs` (~770 lines).
2. **`ychrome-vault`** — a native Bitwarden/Vaultwarden client: crypto, an
   unlock-caching agent, and a CLI. It **replaced `rbw`**, which was purged
   fleet-wide on 2026-07-09. `crates/ychrome-vault/` (lib + bin).

**The rule both obey:** an app OWNS its content, its crate, and its state;
yggterm provides only a generic surface interface. Never add ychrome-specific
chrome to yggterm. Full contract: `yggterm/.agents/skills/libyggterm-surfaces/SKILL.md`.
State is **host-resident** — it lives on the host the app RUNS on (over ssh, the
remote one), never on the GUI host.

## Repo map

```
src/main.rs                     the browser: OSC 7717 thin client, profile picker,
                                loopback control server, ssh -L tunnel, standalone window
crates/ychrome-vault/src/
  crypto.rs    KDF -> master key -> stretched key -> user key; EncString (type 2)
               decrypt AND encrypt; AsymEncString (type 3/4, RSA-OAEP); PrivateKey
  api.rs       prelogin / token / sync / create_cipher / update_cipher / delete_cipher
  model.rs     RawCipher (incl. `raw` JSON), Vault, CipherEdit, edit_body, diagnose
  session.rs   VaultConfig, VaultManager: unlock/lock/resync/add_login/edit_item/remove_item
  agent.rs     unix-socket daemon + the op dispatch table
  matching.rs  the two asymmetric host rules (strict `match`, loose `suggest`)
  totp.rs generator.rs
docs/vault.md        the vault's design + what is proven vs not   <- READ for vault work
docs/protocol.md     OSC 7717 from the app's side
docs/architecture.md docs/product.md
```

## ⛔ Destructive-verb contract (read before `rm`, `edit`, or any write)

**The two delete routes are different operations and the difference is
unrecoverable.** Verified against the DEPLOYED vaultwarden, not from memory
(`curl https://vault.example.com/api/config` → `gitHash`, then read that commit's
`src/api/core/ciphers.rs`):

```
PUT    /api/ciphers/{id}/delete   -> CipherDeleteOptions::SoftSingle  (trash, restorable)
DELETE /api/ciphers/{id}          -> CipherDeleteOptions::HardSingle  (GONE, no undo)
PUT    /api/ciphers/{id}/restore  -> restore from trash
```

An earlier project note had these **backwards** and would have permanently
destroyed items while reporting them recoverable. `ychrome-vault rm` trashes by
default; `--permanent` is explicit and says so in its output. **`rm` is
deliberately NOT wired into the sidebar** — a destructive verb needs its contract
confirmed before it gets a button. User standing steer: *"Be very careful before
rm-ing."* Never run a write against the real vault without saying what you are
about to do.

### `edit` patches the raw record; it never rebuilds one

`PUT /api/ciphers/{id}` replaces the **whole** cipher. The server assigns
unconditionally — `cipher.notes = data.notes` — so a field missing from the
request is **destroyed**, not left alone. `sync` parses only the fields this
client models, so a body rebuilt from `RawCipher`'s parsed fields would silently
wipe every item's notes, custom fields, favorite flag and password history.

Therefore `RawCipher` carries `raw: serde_json::Value` (the untouched sync
record) and `Vault::edit_body` patches **that**:

- Server-managed keys are stripped by a **denylist**, not an allowlist — a field
  Bitwarden adds in a future version rides back untouched instead of being
  dropped by a client written before it existed.
- Patched fields are encrypted under the **cipher's** key (its own item key, or
  its organization's), never blindly under the user key. Getting this wrong is
  *invisible*: the MAC check fails and `items()` silently skips the item.
- `revisionDate` is echoed as `lastKnownRevisionDate`, so a stale client is
  **refused** instead of clobbering a concurrent edit.
- Replacing a password prepends the old ciphertext to `passwordHistory`.
- Clearing a field is rejected rather than encrypting `""`.

### Two unlocked agents WILL go stale against each other

Each host's agent caches the vault at its own `unlock`/`sync`. A write from a
long-lived agent whose copy predates another host's edit gets:

> HTTP 400 — The client copy of this cipher is out of date. Resync the client and try again.

That is the system working. **`ychrome-vault sync` before a write.** Do not
"fix" it by dropping `lastKnownRevisionDate`.

## ⛔ The stale-agent trap (this WILL bite you)

The agent is a daemon holding the decrypted vault in memory. **It outlives the
binary.** After any rebuild it keeps serving the OLD code:

```sh
ychrome-vault stop-agent      # after EVERY rebuild
```

`status` reports `agent_stale: true` by comparing `exe_stamp` to the on-disk
binary — trust it. An unknown op answers with its own remedy
(`unknown op "notes" — the running agent predates this binary; run 'ychrome-vault stop-agent'`).

**`stop-agent` drops the keys, so the vault RE-LOCKS and the user must type the
master password again.** Consequence for planning: **install the new binary
BEFORE asking the user to unlock**, or you will ask twice.

## Unlock, and what agents may not do

The master password is read from **stdin only** — never a flag (visible in `ps`),
never an env var — and is dropped the moment the keys are derived. A terminal on
stdin is refused rather than echoed into scrollback. **You cannot unlock for the
user; ask them to run it themselves:**

```sh
read -rs PW; echo "$PW" | ychrome-vault unlock
```

Homes are not shared, so **every host needs its own unlock** — exactly as rbw
did. Idle auto-lock defaults to 3600s (`lock_timeout_secs`, 0 = never).

Security model: the agent's authority is the unix socket — dir `0700`, socket
`0600`. There is no token, because a token buys nothing against a same-uid
attacker. Never print a real password into a transcript.

## CLI (rbw parity, plus what rbw could not do)

```sh
ychrome-vault configure --server https://vault.example.com --email you@example.com
read -rs PW; echo "$PW" | ychrome-vault unlock
ychrome-vault list                     # name<TAB>user<TAB>folder   (--json for exact bytes)
ychrome-vault get NAME [USER]          # password; --field username|totp|notes
ychrome-vault totp NAME [USER]         # 6-digit code
ychrome-vault match HOST               # strict: the ONE entry an auto-fill may use
ychrome-vault suggest HOST             # loose: rows the sidebar floats up (secret-free)
ychrome-vault add NAME [USER] --generate --uri https://...
ychrome-vault edit NAME [USER] --generate            # rotate; everything else preserved
ychrome-vault rm NAME [USER]           # -> TRASH.  --permanent destroys it.
ychrome-vault generate 24              # local dice, no vault touched
ychrome-vault sync | lock | stop-agent | ping | status | diagnose | check
```

- `list` emits one record per line: control chars in names become spaces (two of
  this user's items really do contain newlines, which once made `list | wc -l`
  read 1050 for 1048 items). Use `--json` when exact bytes matter.
- `diagnose` accounts for **every** cipher the server sent — `items()` skips what
  it cannot decrypt, which is robust and dishonest. `item_count` = decryptable,
  `cipher_count` = what the server sent, `undecryptable` = the gap.
- **Organization ciphers** are sealed under an org key, unwrapped from the user's
  RSA private key (`profile.privateKey`, type-2) via a **type-4** asymmetric
  EncString. Without this, 59 of 1107 items vanished silently. `Vault::base_key`
  selects by `organizationId`.
- `--field notes` reads notes off the **raw** record, because `sync` never parses
  them into `RawCipher`. It is also the read that proves an edit preserved them.

## Fleet, build, deploy

Five hosts: **dev(=pi), jojo, oc, practice, jyas-webapp** — all x86_64 Debian.
**`pi` and `dev` are the SAME MACHINE** (machine-id `03d282108f6f`; `ssh dev`
loops back). jojo is the live desktop (yggterm GUI + daemon).

```sh
cargo test -p ychrome-vault && cargo build --release -p ychrome-vault
sudo install -m 0755 target/release/ychrome-vault /usr/local/bin/ychrome-vault
ychrome-vault stop-agent            # remember: this re-locks the vault
```

Deploy to a remote host: `scp` to `/tmp`, then `sudo install` there. The GUI
resolves the binary via `which_binary("ychrome-vault")` → `/usr/local/bin`.
`cargo fmt --check` is **not** clean on this crate (it predates rustfmt
settings); do not reformat the whole crate to satisfy it. `cargo clippy` has 3
pre-existing warnings — add none.

## Verification recipes

```sh
# Crypto end-to-end, in-process, leaving any running agent alone:
read -rs PW; echo "$PW" | ychrome-vault check

# Prove an edit preserved an UNMODELLED field (the whole point of raw retention):
ychrome-vault edit ITEM --notes "stamp"
ychrome-vault edit ITEM --generate          # a PASSWORD-ONLY edit
ychrome-vault get ITEM --field notes        # must still print "stamp"
```

**Opening a contributed pane in the live GUI.** `server app right-panel
pane:<id>` opens it and fetches its schema — idempotent, unlike clicking the
titlebar button. ychrome declares two ids: `vault` and `settings`.

```sh
Y=~/.local/bin/yggterm
S=$($Y server app terminal new | jq -r .data.session_path)
printf '~/.local/bin/ychrome https://example.com\n' | $Y server app terminal send $S --stdin
# ychrome is NOT on the non-interactive ssh PATH — use the absolute path.
$Y server app right-panel pane:vault      # or pane:settings
$Y server app screenshot /tmp/pane.png --crop 1400,0,520,700 --scale 2
# cleanup: Ctrl+C the surface, `app session remove <that exact id>`, `app open <your session>`
```

The vault pane renders `MAX_ROWS = 80` of the item list ("Showing 80 of 1107"),
so count ⏱ buttons against the first 80 rows, not all of them.

## The sidebar contribution (`src/sidebar.rs`) — SHIPPED, live-proven

ychrome DECLARES two panes over `OSC 7717 ; sidebar ; declare` — `vault` and
`settings` — and serves both from a loopback control endpoint. yggterm renders
generic widgets and knows nothing about vaults or ad blocking. See
`docs/protocol.md` and the `libyggterm-surfaces` SKILL.

- The schema never leaves the app's host over the PTY — the GUI `GET`s it.
- **No secret in a schema.** A credential reaches the page only as the `eval`
  script an action returns, which the GUI injects into the surface. A `secret`
  field is one-way: it carries what the user TYPED up to us, and we declare it
  back empty. An empty password on the Add tab means `add --generate`, so a
  generated password is never echoed down into the GUI at all.
- **We own every field's value.** yggterm's copy is only the user's edits since
  the last schema, and applying a schema replaces it — so the Add-tab draft lives
  in our `PaneState` and every schema echoes it back. A value we stop declaring is
  dropped by the GUI (that is what keeps a typed password out of later POSTs).
- Row ids are `name \x1f username`, not the cipher id: the agent resolves by that
  pair, so no new agent op (and no forced re-unlock) was needed.
- The pane shells out to the `ychrome-vault` CLI. The browser deliberately does
  **not** link the vault crate — the workspace keeps the browser build lean.
- Open one headlessly: `yggterm server app right-panel pane:vault`.

## The web-content policy (`src/webpolicy.rs`) — the settings pane

Ad blocking and userscripts are OURS, and they live on the host ychrome runs on
(`~/.yggterm/web-adblock/*`, `~/.yggterm/web-userscripts/*`). They act on the
GUI's webview, so we serve the *effective* policy and yggterm applies it:

- `declare` carries `policy_version` — a **stat-only** stamp (paths, lengths,
  mtimes, plus the enabled/disabled decision). yggterm refetches
  `GET /policy` only when it moves, so a 10 KB `rules.json` never rides the ~4s
  heartbeat. Never hash the file contents here.
- `/policy` answers `{adblock_rules, userscripts}` with every decision made.
  `adblock_rules: null` = no ad blocking; yggterm never asks why.
- **`emit_declare` runs BEFORE `emit_web_surface_osc("open", ...)`**, in
  `run_thin_client` and in the post-suspend re-emit. Userscripts inject at
  document-start, so yggterm holds the surface's creation until the policy
  lands. Open first and the surface is built unblocked — no userscripts, no
  adblock, silently, forever.
- An adblock RULESET change needs a yggterm restart (WebKit compiles the filter
  once per GUI process). Toggling it off, and every userscript change, take
  effect on the next surface (re)create — the pane's "Reload surface now" button
  returns `{"reload_surface": true}`, NOT `eval: location.reload()`: a content
  filter and its userscripts bind to the WEBVIEW at creation, so an in-page reload
  leaves them attached. Only destroy-and-recreate applies a new policy.

## Per-site zoom (`src/webzoom.rs`) — the settings pane's "This site" row

yggterm has one global zoom; a per-site number is OURS, host-resident in
`~/.yggterm/web-zoom.json` (`{sites:{host:percent}}`, host-global across
profiles — zoom is readability, not identity).

- `declare` carries `app_name` ("Ychrome" — labels the main zoom control "Ychrome
  Global Zoom") and `zoom_version` (a change-detector stamp over the map, the same
  trick as `policy_version`, a SEPARATE stamp so a zoom edit never drags the
  ruleset over the wire).
- `GET /zoom` → `{sites:{host:percent}}`. yggterm does the host match itself
  (longest-suffix, so `youtube.com` covers `music.youtube.com`; a bare TLD never
  matches). `webzoom::zoom_for_host` is the CLI/test twin of yggterm's matcher —
  keep them in step.
- The pane's "This site" row (`−`/`+`/`Reset`) steps the override from the live
  `values.zoom` the GUI injects, and the action reply sets `refetch_zoom: true` so
  the change reaches the live page at once (the zoom analogue of
  `reload_surface`). `Reset` clears the override so the site falls back to the
  global — it never persists "same as global".

## Still open
- **`restore`** (`PUT /api/ciphers/{id}/restore`) — `rm` has no undo, and because
  `sync` filters `deletedDate` items this client cannot even *show* the trash.
  A `list --trashed` plus `restore` would close the loop and make the
  soft-vs-hard delete distinction empirically observable, not just read off the
  server's source.
- **Passkeys** (`fido2Credentials`) — needs a `navigator.credentials` userscript
  shim (WebKitGTK has no WebAuthn) plus a user-presence dialog. **The agent may
  never auto-consent.**
- **`auto_match_for_host` silently picks the alphabetically-first candidate.**
  Deterministic, but a headless `app web fill` on a host with 4 accounts fills one
  without asking. Latent footgun.
- Chrome extensions are impossible on WebKitGTK — content filters + userscripts
  instead.

## Anti-patterns

- Rebuilding a cipher from parsed fields for a PUT. → patch `RawCipher::raw`.
- Encrypting an edited field under the user key. → use the **cipher's** key.
- `DELETE /api/ciphers/{id}` when you meant "trash". → that is the HARD delete.
- Trusting a running agent after a rebuild. → `stop-agent`.
- A secret in a sidebar schema, an OSC payload, a flag, or an env var.
- Two implementations of one vault rule (yggterm had the host matchers; they were
  deleted when `matching.rs` took ownership). One owner per concept.
- Reformatting the crate to satisfy `cargo fmt --check`.
