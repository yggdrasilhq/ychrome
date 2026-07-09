# CLAUDE.md

**Read `.claude/skills/ychrome/SKILL.md` in full before touching anything here.**
It is the fast path: repo map, the destructive-verb contract for the vault, the
stale-agent trap, deploy across the 5-host fleet, verification recipes, and what
is still open. Then read `AGENTS.md` for the engineering contract.

## What this repo is

Two things sharing one rule:

- **ychrome** — a web viewport for the Yggdrasil ecosystem, and the **pilot app
  for libyggterm**: a program launched in a yggterm terminal takes over yggterm's
  GUI surfaces (viewport, sidebar panel, cwd-tree document, chooser).
- **`ychrome-vault`** — a native Bitwarden/Vaultwarden client (crypto + agent +
  CLI). It **replaced `rbw`**, purged fleet-wide 2026-07-09.

**The rule:** an app owns its content, its crate, and its state. yggterm provides
only a generic surface interface and persists none of an app's data. State is
**host-resident** — it lives on the host the app RUNS on, which over ssh is not
the GUI's host. Full contract:
`~/gh/yggterm/.agents/skills/libyggterm-surfaces/SKILL.md`.

If you are about to add ychrome-specific chrome to yggterm, stop. That belongs
here, contributed through the surface protocol.

## The three that will bite you

1. **`DELETE /api/ciphers/{id}` is a PERMANENT delete.** The trash route is
   `PUT /api/ciphers/{id}/delete`. A project note once had these backwards.
   `rm` trashes by default; `--permanent` destroys. Never run a vault write
   against the real vault without saying what you are about to do first.
2. **The vault agent outlives the binary.** After every rebuild,
   `ychrome-vault stop-agent` — which re-locks the vault, so the user must
   re-enter the master password. Install the binary BEFORE asking them to unlock.
3. **An edit PUTs the whole cipher.** Patch `RawCipher::raw`; never rebuild a
   body from parsed fields, or you destroy notes, custom fields, favorite and
   password history.

## Working rules

- **Verify live, not just in code.** A vault claim is proven by exercising it
  against `vault.example.com` (or `ychrome-vault check`), a UI claim by a faithful
  screenshot. Compiled binaries and green tests are necessary, not sufficient.
- **Never claim shipped without live proof.** A running agent, a stale binary, or
  an un-restarted GUI can keep the old behavior. Say "code is on disk, not live"
  rather than "shipped".
- **The master password is the user's to type.** stdin only. You cannot unlock.
- **Keep the docs and this skill current in the same change** — `docs/vault.md`
  states what is proven vs merely tested; do not let it drift.
- **Single source of truth.** One owner per concept. The host-matching rules live
  in `matching.rs`, not in a caller.
- **No em-dashes in prose the user reads.**
