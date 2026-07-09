# AGENTS.md — the engineering contract for ychrome

Read `.claude/skills/ychrome/SKILL.md` first. This file is the contract; the
skill is the map.

## Ownership

ychrome is a **libyggterm app**. An app owns its content, its crate, and its
state. yggterm provides only a generic surface interface and persists none of an
app's data.

- **Host-resident state.** Config, profiles, credentials and the unlocked-vault
  session live on the host ychrome RUNS on — over ssh, the remote one, not the
  GUI's host. `~/.yggterm/web-profiles/<name>/`, `~/.yggterm/vault/`.
- **No app chrome in yggterm.** If a change wants `RightPanelMode::Vault` or an
  ychrome icon inside `yggterm-shell`, it is in the wrong repo. Contribute the
  surface instead.
- **Extraction, not construction.** Build the minimum this app needs now; extract
  the shared abstraction when a *second* app needs the same thing.

## Single source of truth

Every concept has exactly one owner. Before adding code, name the owner of the
thing you are changing. The two asymmetric host-matching rules live in
`matching.rs` — they were deleted from yggterm's `shell.rs` when this crate took
ownership, and must never be re-implemented in a caller. The same goes for the
password generator, TOTP parsing, and the delete-route selection.

Never add a second encoding, copy, derived field, or fallback layer that can
silently diverge.

## Secrets

- The master password comes from **stdin only** — never a flag (visible in `ps`),
  never an environment variable. A terminal on stdin is refused rather than
  echoed into scrollback.
- No secret in a sidebar schema, an OSC payload, or a log line. The app computes
  the value; the GUI injects it via surface-eval.
- Never print a real password into a transcript. Redact, or assert on length.
- The agent's authority is the unix socket (dir `0700`, socket `0600`). Adding a
  token buys nothing against a same-uid attacker; do not add one.

## Destructive verbs

A verb that can lose user data must:

1. Default to the recoverable form (`rm` trashes; `--permanent` destroys).
2. Report which operation actually happened (`"trashed": true` vs
   `"permanent": true`).
3. Refuse on the lock **before** resolving a target, so it never gets as far as
   naming an item on a locked vault.
4. Not get a button until its contract is confirmed. `rm` is deliberately absent
   from the sidebar.

Verify a route against the **deployed** server's source before relying on it.
`curl https://vault.example.com/api/config` gives the `gitHash`; read that commit. A
project note once claimed `DELETE /api/ciphers/{id}` soft-deletes. It does not.

## Writes preserve what they do not touch

`PUT /api/ciphers/{id}` replaces the whole cipher, and the server assigns
unconditionally (`cipher.notes = data.notes`). Patch the untouched `raw` record
from `sync`; strip server-managed keys with a **denylist** so an unknown future
field survives; encrypt under the **cipher's** key; echo `revisionDate` as
`lastKnownRevisionDate` so a stale client is refused rather than clobbering.

## Proof

- **Verify live, not just in code.** Green tests and a compiled binary are
  necessary and not sufficient. A running agent serves old code; a stale GUI
  paints old pixels.
- **A visual claim needs a faithful pixel.** Telemetry that says "fine" while the
  user sees breakage means the instrument is wrong.
- **Never claim shipped without live proof.** If you cannot exercise it, say so:
  "code is on disk; the running agent predates it" beats "shipped".
- State honestly what is proven vs merely unit-tested. `docs/vault.md` has a
  "What is proven, and what is not" section — keep it true in the same change.

## Tests

- Unit-test against a genuinely sealed vault (`model::seal`) — no network, no
  server, no master password.
- A round-trip test alone is worthless for crypto: it passes even if encrypt and
  decrypt drift together. Pin a known-answer vector and cross-check against an
  independently written implementation.
- Cover the failure the code exists to prevent, not just the happy path: field
  preservation including an **unknown future key**, cipher-key vs user-key
  encryption, org ciphers, PascalCase drift, the revision guard.

## Housekeeping

- Keep `.claude/skills/ychrome/SKILL.md` and `docs/` current **in the same
  change**. A skill that lies is worse than no skill.
- `cargo fmt --check` is not clean on `ychrome-vault` (it predates the settings).
  Do not reformat the crate. `cargo clippy` has 3 pre-existing warnings — add none.
- After every rebuild of the vault binary: `ychrome-vault stop-agent`. It
  re-locks the vault, so install the binary **before** asking the user to unlock.
- No em-dashes in prose the user reads.
