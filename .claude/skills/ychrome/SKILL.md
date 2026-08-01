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
                                the ROUTING decision, ssh -L tunnel, standalone window
src/daemon.rs                   the HOST DAEMON: one per host, owns every session's
                                control listener + registry + command queue + routing +
                                journal + `status`. `ychrome --daemon` runs it; the view
                                client spawns + supervises it (docs/host-daemon.md)
src/sidebar.rs                  the control routes: vault + settings pane schemas, actions
src/webpolicy.rs                adblock + userscripts (enable/disable/delete/install)
src/abp.rs                      ABP/uBO filter syntax -> WebKit content-blocker JSON,
                                gated on what the engine MEASURABLY accepts
src/adblock.rs                  the filter-list roster, `ychrome adblock update|status|lists`,
                                the ruleset's provenance sidecar
assets/web-scriptlets/runtime.js   the `##+js(...)` scriptlet library (OURS, not uBO's;
                                see docs/adblock.md §5 before touching it)
src/provision.rs                the BUNDLED-ASSET RECONCILER: one owner of "is this
                                host's copy current?" (docs/adblock.md §6)
src/webzoom.rs                  per-site zoom overrides (web-zoom.json)
src/useragent.rs                the browser IDENTITY: the global preset + per-site
                                overrides (ychrome/user-agent.json), `ychrome identity`
src/sitehost.rs                 THE per-site host rule (normalize + longest-suffix),
                                shared by webzoom and useragent — one owner, never re-derived
src/extensions.rs               the bundled userscript catalog ("Add an extension")
assets/web-userscripts/         bundled scripts embedded by extensions.rs
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
docs/adblock.md      the MEASURED WebKit content-blocker limits, the filter-list
                     pipeline, provisioning, and the annoyance scripts  <- READ for adblock work
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
- Setting a field to `""` is rejected rather than encrypting an empty string.
  Removing a value is a SEPARATE request, `--clear <notes|totp|username|uri|folder>`
  — one owner, `model::ClearField`. There is no `--clear password` on purpose.
- **Custom fields are editable too** (`--set-field NAME=VALUE`,
  `--set-hidden-field NAME` reading the value from stdin, `--remove-field NAME`).
  The field ENTRY is mutated, never rebuilt — it carries `linkedId` and future
  keys. `--set-field` on an already-hidden field KEEPS it hidden: updating a
  secret must never expose it. A linked field, a duplicated name, and removing a
  field that is not there are all refused rather than guessed.
- **`--uri` is a repeatable list**, and a uri the item already stores is carried
  over as its stored OBJECT — a uri is not a string, it carries `match` and
  `uriChecksum`.
- ⛔ **Every edit is RE-READ before it is reported.** A 200 from `PUT` says the
  server took a body, not that the field landed. `edit_item` re-syncs, runs
  `Vault::verify_edit`, and fails the whole edit if a change is not visible. The
  reply's `verified` list carries field LABELS (`password`, `field:API Key`,
  `clear:notes`), never values — and its ABSENCE is how the CLI detects an agent
  too old to have checked, which would otherwise have ignored every new argument
  in silence.
- ⚠ **Absent is not empty on the write side either.** The agent's decoder used to
  drop an empty value, so `--notes ""` arrived as "no fields named". `edit_value`
  preserves it so `edit_body` refuses it. Found by the live round trip.

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
ychrome-vault handover        # after EVERY rebuild — keeps the vault unlocked
ychrome-vault stop-agent      # the fallback, which RE-LOCKS the vault
```

`status` reports `agent_stale: true` by comparing `exe_stamp` to the on-disk
binary — trust it. An unknown op answers with its own remedy.

**`stop-agent` drops the keys, so the vault RE-LOCKS and the user must type the
master password again.** Consequence for planning: **install the new binary
BEFORE asking the user to unlock**, or you will ask twice.

`handover` is the way to avoid that entirely: the agent `execve`s the newly
installed binary in place — same pid, same bound socket, unlock intact — and the
keys cross on an anonymous pipe (never argv, never an env var, never a file).
Verify with a SECOND round trip, not the reply: same pid, a NEW
`/proc/<pid>/exe`, `agent_stale: false`, `state: unlocked`, item count intact.
That signature is one only exec-in-place can produce.

**The one deploy that still costs an unlock is the one that installs `handover`
itself** — an agent that predates the op cannot perform it, which is why a
`handover` request that comes back `unknown op` is pointed at `stop-agent` and
not at itself. Land vault changes on ONE branch and deploy ONCE; three deploys
would be three master passwords on each of two hosts.

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
ychrome-vault get NAME [USER]          # --field password|username|totp|totp-secret|notes
ychrome-vault totp NAME [USER]         # 6-digit code; REFUSES on an
                                       # undisciplined host clock
                                       # (--ignore-clock waives)
ychrome-vault clock                    # the kernel's own NTP state, as JSON.
                                       # ⚠ chrony's Last/RMS offset lines report
                                       # perfect tracking on a host 72 s out
ychrome-vault card NAME [USER]         # brand<TAB>holder<TAB>expM<TAB>expY<TAB>last4
ychrome-vault match HOST               # strict: the ONE entry an auto-fill may use
ychrome-vault suggest HOST             # loose: rows the sidebar floats up (secret-free)
ychrome-vault add NAME [USER] --generate --uri https://...
ychrome-vault edit NAME [USER] --generate            # rotate; everything else preserved
ychrome-vault edit NAME --rename TITLE --set-user U --notes N --folder F
ychrome-vault edit NAME --uri URL --uri URL2         # replaces the whole list
ychrome-vault edit NAME --set-field "API Key=v"      # custom fields, at last
ychrome-vault edit NAME --set-hidden-field NAME      # value from STDIN, like a password
ychrome-vault edit NAME --remove-field NAME
ychrome-vault edit NAME --clear notes --clear totp   # remove, not blank
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
- **Cards** (`type` 3) have no password, so `get` refuses them however you spell
  `--field` — 130 of 1113 items. `card NAME` prints their metadata; `list --json`
  now carries `item_type` so a listing can say WHY an item refuses. **There is no
  CLI verb for the number or the CVV, and adding one is not an oversight to fix**:
  a PAN in scrollback or an agent transcript is durable and cannot be rotated. The
  number reaches a page only through an INJECTOR — the sidebar's `card-fill`
  action, or yggterm's `web fill-card` verb, both on the `card-secret` socket op
  — whose script returns field names, never values.
- **The unlock is the card path's ONLY gate** (user's ruling, 2026-07-26). Every
  Bitwarden client can read a card cipher; this is one, so an unlocked vault
  serves `card-secret` to whoever reaches the socket and a locked one refuses
  naming `ychrome-vault unlock`. Do NOT re-propose a grant/consent layer here —
  one was designed for this exact spot and refused. Every release appends one
  line to `~/.yggterm/vault/audit.log`: item, host, client, and the FIELD NAMES
  released. Never a value; nothing reads it; it is a trail, not a gate.
- **Absent is not empty.** A field the item does not have exits non-zero with the
  reason on stderr; a field it stores as an empty string prints an empty line and
  exits 0. `USER=$(ychrome-vault get X --field username)` used to capture "" and
  report success either way, so scripts could not tell the two apart. Same rule
  for `fields --field-name` with no readable value — and it says WHICH of the two
  reasons: a *linked* field stores none of its own, while an *unreadable* one has
  a value this vault could not decrypt (the org-key case). One message for both
  sent a key problem off looking for a link that does not exist.

## Fleet, build, deploy

Five hosts: **dev(=pi), jojo, oc, practice, jyas-webapp** — all x86_64 Debian.
**`pi` and `dev` are the SAME MACHINE** (machine-id `03d282108f6f`; `ssh dev`
loops back). jojo is the live desktop (yggterm GUI + daemon).

**There is no deploy script.** `scripts/deploy-fleet.sh` does not exist and never
did (a memory note claims otherwise — it is wrong). The fleet-binary-sync hook
does not cover this either: its roster is `(yedit ychrome)` over `~/.local/bin`,
and `ychrome-vault` lives in **`/usr/local/bin`**, root-owned, which an rsync
running as `pi` cannot write. Deploy is manual, per host, in this order:

```sh
# 1. build ONCE (all five hosts are x86_64 Debian)
cargo test -p ychrome-vault && cargo build --release -p ychrome-vault

# 2. install locally
sudo install -m 0755 target/release/ychrome-vault /usr/local/bin/ychrome-vault

# 3. and on each remote host: /tmp first, because scp cannot write /usr/local/bin
scp target/release/ychrome-vault HOST:/tmp/ychrome-vault
ssh -t HOST 'sudo install -m 0755 /tmp/ychrome-vault /usr/local/bin/ychrome-vault && rm /tmp/ychrome-vault'

# 4. retire the OLD agent on each host — it is still serving the old code
ssh HOST 'ychrome-vault handover'     # keeps the unlock; needs an agent that KNOWS the op
ssh HOST 'ychrome-vault stop-agent'   # the fallback, which RE-LOCKS the vault
```

Order matters and costs the user real time: **install the binary before asking
for an unlock**, or you will ask twice. Deploy the host with **no agent running**
first (it costs nothing and rehearses the install), then the others.

`ychrome-vault handover` execs the newly installed binary in place — same pid,
same socket, unlock intact. An agent that predates the op cannot perform it, so
the deploy that first installs `handover` still costs one `stop-agent` per host;
every deploy after that is free. Verify with `ychrome-vault status`:
`agent_stale` must be `false` and `exe_stamp` must name the binary you just
installed.

The GUI resolves the binary via `which_binary("ychrome-vault")` → `/usr/local/bin`.
`cargo fmt --check` is **not** clean on this crate (it predates rustfmt
settings); do not reformat the whole crate to satisfy it — `rustfmt <file>` on
what you touched. `cargo clippy` has 3 pre-existing warnings and one pre-existing
`never_loop` error in `session.rs`'s test code — add none.

## Verification recipes

```sh
# Crypto end-to-end, in-process, leaving any running agent alone:
read -rs PW; echo "$PW" | ychrome-vault check

# The WHOLE edit surface, end to end, against a scratch server you own — never
# the operator's vault (it registers its own account and creates its own items):
docker run -d --name ychrome-vault-scratch -e SIGNUPS_ALLOWED=true \
  -e ROCKET_PORT=8080 -e I_REALLY_WANT_VOLATILE_STORAGE=true \
  -p 127.0.0.1:8087:8080 vaultwarden/server:latest
YCHROME_VAULT_TEST_SERVER=http://127.0.0.1:8087 \
  cargo test -p ychrome-vault --test live_edit -- --ignored --nocapture

# Prove an edit preserved an UNMODELLED field (the whole point of raw retention):
ychrome-vault edit ITEM --notes "stamp"
ychrome-vault edit ITEM --generate          # a PASSWORD-ONLY edit
ychrome-vault get ITEM --field notes        # must still print "stamp"

# Prove a card reads without ever printing a number (11.7% of the vault):
ychrome-vault list --json | jq -r '.[] | select(.item_type == 3) | .name' | head
ychrome-vault card "<that name>"            # brand/holder/expiry/last4 only
ychrome-vault card "<that name>" | grep -cE '[0-9]{13,}'   # MUST print 0
```

**Proving a `handover` really happened.** The reply says "accepted"; only a
second round trip proves it, and the signature below is one that no other
mechanism produces — socket takeover and keyring adoption both change the pid.

```sh
PID=$(cat ~/.yggterm/vault/agent.pid)
ps -o pid,lstart -p $PID; readlink /proc/$PID/exe   # BEFORE
sudo install -m 0755 target/release/ychrome-vault /usr/local/bin/ychrome-vault
ychrome-vault status | jq .agent_stale              # must flip to true,
                                                    # and exe_stamp gains " (deleted)"
ychrome-vault handover                              # -> handed_over: true
ps -o pid,lstart -p $PID; readlink /proc/$PID/exe   # SAME pid, SAME start time,
                                                    # DIFFERENT exe, no " (deleted)"
ychrome-vault status | jq '{agent_stale,state,item_count}'
# state must still be "unlocked" and item_count intact — and no master password
# was typed. Run it on a host with NO agent first (free), then dev, then jojo.
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

## The host daemon + routing verb (`src/daemon.rs`) — BUILT 2026-07-18

`ychrome <url>` typed in one terminal can now open a tab in a surface anchored by
another. The transport is a host-resident QUEUE the GUI's liveness ping drains on
its reply — a queue needs something durable on the app host, and that is the
daemon (docs/host-daemon.md; consolidation IS the routing mechanism, not a
prerequisite).

- **One daemon per host per user**, auto-spawned + supervised by the view client
  (the yedit pattern). `ychrome --daemon` runs it. Singleton via the unix-socket
  bind itself (`~/.yggterm/ychrome/daemon.sock`, 0600); a stale socket is
  reclaimed. `~/.yggterm/ychrome/journal.jsonl` audits every route/deliver/drop/
  reap.
- **⛔ The stale-daemon trap, ychrome's own version** (fixed 2026-07-27; dev's
  daemon had served old code for 6.7 days before it was). `ensure()` used to
  compare only the daemon's REPORTED VERSION, and ychrome's version is the
  constant `0.1.0` — so every rebuild left the running daemon serving the old
  code forever, with `status` printing `[STALE]` at nobody. Now:
  - **idle** (no session heartbeating): the next `ychrome` invocation hands it
    over by itself, new pid, new stamp, nothing to notice;
  - **attached**: it is NOT retired. It holds those surfaces' control endpoints,
    pane drafts, queues and passkey signer, and none of that survives its exit.
    Every invocation says so on stderr, once per daemon, naming the pid;
  - `ychrome daemon restart` is the deliberate handover, the only path that
    retires a busy daemon. It names the sessions that re-register (~4s).
  - The idle-or-attached call belongs to the DAEMON (`retire_if_idle`, one
    round trip under its own lock), never to the client. Do not re-derive it
    from `status` in a caller.
  - A daemon older than that verb cannot answer it, so it is treated as busy:
    installing this costs one `ychrome daemon restart` per host, once.
- **It owns every session's control endpoint** — one plain
  `http://127.0.0.1:<port>` listener per registered session (NOT a single port;
  see docs/host-daemon.md for why the appctl proxy forces this). The view client
  no longer runs its own control server: it `register`s and declares the daemon's
  url. `sidebar::dispatch` serves the same pane/policy/zoom/appearance/action/
  fido2 routes; the daemon adds `GET /ping?session=<env_id>&ack=<batch>`, which
  ychrome did not serve before — liveness used to ride the OSC re-declare only.
- **Routing** (`ychrome [--profile P] [--here] <url>`): a live registered session
  with the requested profile → the daemon enqueues `open_tab {url, raise:true}`
  into ITS queue, the GUI drains it on the next ping and MINTS A NEW TAB
  (never a navigation of an existing one), and the CLI prints "opened <url> as
  a new tab in the running [<profile>] session" and exits 0 — the running CLI
  stays the anchor. **Skew honesty:** the daemon marks a session
  routing-capable only once it has seen a `?session=` ping; without it /route
  refuses and the CLI anchors with a warning rather than claiming a success it
  cannot deliver. **No silent hijack (A4):** every unrouted fallback names the
  act out loud before anchoring here, and if THIS terminal's stream already
  carries another client's live surface the CLI REFUSES (exit nonzero) instead
  of anchoring — the GUI keys surfaces by stream, so a second anchor on one
  stream replaces the running page rather than opening beside it. `--here` has
  the same stream-conflict guard; it still forces a new anchor when a match is
  open ELSEWHERE. The pre-anchor probe is `daemon::live_anchor` (op `status`,
  another pid's row, heartbeat within expiry). The fleet router is
  `ssh <host> ychrome <url>` — zero code.
- **Liveness / no phantom:** the client re-registers on its ~4s heartbeat; the
  daemon reaps a session ~14s after the last heartbeat, closing its control
  listener so the GUI's ping fails and the contribution expires — a SIGKILLed
  client leaves no phantom rail. Daemon death is self-healing: the client's
  supervision respawns it and re-declares.
- **`ychrome status [--json]`** — host-side truth for agents: the registry, queue
  depths, vault-agent reachability, config stamps, the daemon version, and a
  self-staleness stamp (`path@mtime`, the vault agent's precedent) so an old
  daemon running while the fix sits on disk cannot silently recur. Every reply on
  the socket carries `pid`, `stale` and `live_sessions`, whatever the op, so a
  verb cannot answer without saying whether it is old code.
- ⚠ **`control_token_declared: false` on a row means that session's vault and
  settings panes CANNOT open** — its CLI predates the control-token gate and
  never declares the GUI's credential, so every GUI-only route 403s for the life
  of that process while ad blocking and userscripts keep working. Neither a
  daemon restart nor a GUI restart fixes it; only cycling that CLI does. The
  human `status` prints a `[NO PANES]` block naming each one. First diagnostic
  for "the panes will not open" (2026-07-31). Detail: `docs/protocol.md`
  §"The third mixed case".

Proven end-to-end on dev via socket + curl (register → skew-honest refuse →
`?session=` ping → route → envelope drain → at-least-once re-send → ack →
staleness → reap-closes-the-port). Live GUI proof: see the deploy section.

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
- **The ruleset is 146,817 real rules now, not 60 hand-typed ones**, generated
  from nine upstream lists by `src/abp.rs` and committed gzipped at
  `assets/web-adblock/rules.json.gz`, plus TWO generated userscripts from the
  same call: `cosmetic-filters.js` and `scriptlets.js`. **Read `docs/adblock.md` before touching
  any of it** — it carries the measured WebKit limits, and they are the whole
  design: 150,000 rules is a hard ceiling, the regex dialect has NO alternation,
  and ONE bad network rule fails the entire compile (no ad blocking at all)
  while one bad SELECTOR is dropped in silence. ⚠ It also carries the yggterm
  companion change this ruleset needs: `web_surface.rs` calls
  `webkit_user_content_filter_store_save` every process start, which recompiles
  (measured 15.7 s, 476 MB), where `..._load` returns the same filter in 11 ms.
- **Nothing bundled sits dead on a host any more.** `src/provision.rs` runs at
  every launch, installs what is missing, replaces what is superseded (keeping a
  `.superseded` backup) and KEEPS what the user edited or what is newer than the
  bundle. `ychrome provision --json` runs the same call and prints the verdicts.
  Every bundled asset declares a version (`@version`, or `ruleset_version` in
  the sidecar) — a content hash cannot tell an old release from a user's edit.
- An adblock RULESET change needs a yggterm restart (WebKit compiles the filter
  once per GUI process). Toggling it off, and every userscript change, take
  effect on the next surface (re)create — the pane's "Reload surface now" button
  returns `{"reload_surface": true}`, NOT `eval: location.reload()`: a content
  filter and its userscripts bind to the WEBVIEW at creation, so an in-page reload
  leaves them attached. Only destroy-and-recreate applies a new policy.

## ⭐ Browser identity (`src/useragent.rs`) — the default is the ENGINE'S OWN UA

**Changed 2026-07-31 after a user could not clear a Cloudflare challenge on a
login.** An anti-bot edge scores CONSISTENCY across the JS environment, TLS and
the UA; it does not blocklist engines (GNOME Web is this same WebKitGTK and
passes daily). The old default was Safari-on-macOS, which produced this,
measured on the engine plane:

```
navigator.userAgent -> "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) … Safari/605.1.15"
navigator.platform  -> "Linux x86_64"
```

One line of page script catches that. **The default is now `Preset::Engine`,
which resolves to `None` — "leave WebKitGTK's own UA alone".** The module
deliberately does NOT hold the engine's string (a test enforces it): WebKitGTK
owns its identity, so an engine upgrade moves the UA and ychrome ships no new
constant. For the record, WebKitGTK 2.52.5 on this fleet sends
`Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)
Version/60.5 Safari/605.1.15`.

- **Overrides are PER SITE**, in the same file as the global preset
  (`~/.yggterm/ychrome/user-agent.json`, `{preset, sites:{host:preset}}`) —
  never a second store. Matching is `sitehost`'s longest-suffix walk, the same
  one the zoom map uses.
- **`ychrome identity [<host>] [--set safari|chrome|engine] [--reset] [--json]`**
  reads and writes it. Every `--set` prints the warning, because a spoof that
  breaks a fingerprint-gated login fails as a challenge that never clears.
- The settings pane draws it TWICE, on purpose: a per-site row under "This site"
  (with the warning) and the browser-wide picker under "Browser identity" (which
  now says to prefer the per-site one).
- **`/policy` carries `user_agent` (global, applied at webview creation) AND
  `user_agent_sites` (host → resolved UA string, `null` = the engine's own).**
  The engine already applies the per-site map on every navigation
  (`Engine::apply_identity`, BEFORE `goto` — the UA is a request header).
  ⚠ **yggterm does not consume `user_agent_sites` yet**, so on the visible
  surface a per-site override still needs the GUI companion change: match the
  live page's host the way it already does for `/zoom`, then
  `WebKitSettings::set_user_agent`.
- **claude.ai really does UA-gate, and it still reproduces** (in a real WebKit
  surface, not curl): engine identity → `403 {"error":{"type":"forbidden"}}`;
  the Safari preset → `200` at `/login`. That is what the per-site layer is FOR
  — one entry for claude.ai, every other site coherent. Do not answer it by
  making the global default lie again.

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

## The settings pane's sections (`src/sidebar.rs`)

`settings_schema_from` draws, in order: **This site** (per-site zoom + an honest
HTTPS connection line from `values.secure`; full cert detail is future, needs
WebKit's TLS cert), **Ad blocking**, **SponsorBlock** (the `sponsorblock`
userscript promoted to its own friendly toggle, plus one `list-row` per
category from `src/sponsorblock.rs` whose buttons are the states it is NOT
in), **Userscripts** (every OTHER
script as a `list-row` with Enable/Disable + Delete actions), and **Add an
extension** (the `extensions.rs` catalog, filtered to what is not installed).

- The catalog's "installed" test reads the SAME `PolicyState` the rest of the
  pane draws from — one source per render, so the catalog never disagrees with
  the list above it. Do NOT re-read the disk for it.
- A list-row Enable/Disable button carries no checkbox value, so the
  `userscript:` action FLIPS `webpolicy::userscript_enabled` when `values.value`
  is absent; a real toggle (SponsorBlock, adblock) posts its state.
- `install` embeds the bundled body via `include_str!` and refuses to clobber an
  existing script; `delete` removes both `.js` and `.js.disabled`. Both redraw +
  toast "reload to apply" (a userscript binds at surface creation) — the user
  hits "Reload surface now".
- E2E-proven against the real control server 2026-07-11: install writes
  `sponsorblock.js` and promotes it; `/policy` then serves its body; zoom-in
  persists `web-zoom.json` and returns `refetch_zoom`; a list-row toggle renames
  the file; delete removes it.

### The bundled catalog (`src/extensions.rs` + `assets/web-userscripts/`)

Six entries: `cosmetic-filters` and `scriptlets` (both GENERATED by the same
`abp::convert` that makes the ruleset and installed alongside it), plus
`sponsorblock`, `youtube-adblock`, `idcac`, `unblock-select`. The catalog is the
ONLY roster — nothing else enumerates them, so add here and every surface
inherits. Every hand-written script self-guards by hostname; the two generated
ones are `@match`-scoped instead, so WebKit does the matching in the engine.

- **`scriptlets` is the `##+js(...)` plane, added 2026-07-31.** 3,341 scriptlet
  invocations over 5,338 domains, run by a library of OUR OWN implementations at
  `assets/web-scriptlets/runtime.js`. Two things will bite a future change:
  **(a)** `abp::SCRIPTLETS` (what gets routed) and `runtime.js` (what can run)
  are ONE contract, locked by
  `extensions.rs::the_scriptlet_table_and_the_runtime_are_one_contract` — a name
  in one and not the other reports thousands of filters supported and then
  ignores them; **(b)** the script runs in the **MAIN world** and must, because
  every scriptlet edits page globals. ⚠ uBO is GPLv3 and this repo is Apache:
  implementations are written from the documented filter behaviour, never
  transcribed. `trusted-click-element` (886 filters, the single biggest gap) is
  deliberately NOT implemented — it clicks page elements a filter names, mostly
  consent dialogs, and `idcac_clicks_nothing_that_consents` is a rule a
  filter-driven auto-clicker would walk straight through.

- **The 2x-ads bug, so it is never rediagnosed from scratch.** The user
  reported "I still see youtube ads! They are sped up to 2x automatically!" The
  copy of `youtube-adblock.js` on jojo predated the script's metadata block, so
  it parsed to the DEFAULTS, so it ran in the ISOLATED world, where its
  `window.fetch` patch is invisible to the page. The prune never ran; only the
  DOM fallback did, and it forced `playbackRate = 16`, which WebKit clamps to
  ~2x. The forced rate is now GONE (a fallback that degrades playback while
  masking a dead primary path is worse than none) and the belt WARNS instead.
  `src/provision.rs` exists so the stale-copy half cannot recur.
- **`youtube-adblock` rots on YouTube's schedule, and that is expected.** YouTube
  ads are FIRST-PARTY, so no URL-matching filter can reach them; the script
  deletes the ad fields out of the player response before the player reads it.
  The load-bearing shape is the `AD_FIELDS` list — `adPlacements`, `adSlots`,
  `playerAds`, `adBreakHeartbeatParams`.
- ⚠ **HOOK THE PARSE, NOT THE TRANSPORT (v1.2.0, 2026-07-31).** v1.1.0 hooked
  `window.fetch`, XHR and the inline `ytInitialPlayerResponse` and the user still
  saw ads. Measured on one cold watch page: the fetch hook saw ZERO real player
  responses (its one `/youtubei/v1/player` call was a 328-byte field probe),
  while `JSON.parse` was handed the real one 30 times and
  `Response.prototype.json` twice more — the latter on Responses whose `.url` is
  the EMPTY STRING, built in JS by the page, which no URL hook can ever see. On
  the second video of a session `player.getPlayerResponse()` still answered
  `["playerAds","adPlacements","adBreakHeartbeatParams"]`. So `JSON.parse` and
  `Response.prototype.json` are the funnels; the transports are not. When ads
  come back, check `window.__yga_state.hooks` FIRST — it says which funnel bit.
  ⚠ `pruneText` must parse with the `nativeParse` captured BEFORE hooking, or
  the two layers cancel out silently (the parse hook cleans the object, the
  rewrite then finds nothing to remove, and the ORIGINAL text goes to the page).
- The lock that matters is `youtube_adblock_actually_prunes_a_player_response`:
  it runs the body the CATALOG serves under node against a fixture player
  response (`tests/fixtures/youtube-adblock-harness.js`), so a prune that stops
  pruning fails even when every source needle still matches. node is a test-time
  dependency; `YCHROME_ALLOW_NO_NODE=1` is the explicit opt-out.
- **`idcac` is NOT feature complete and cannot be — the gap is closed from the
  other side.** The three consent lists in the ruleset name 27,859 distinct
  domains; the script knows 36 container selectors and zero domains, and all 19
  of its original selectors are already upstream. Hiding is the RULESET's job
  now. The script keeps the two things no declarative rule can do: press
  "reject all" (53 phrases, six languages) and undo a scroll lock. Do not
  re-propose growing its selector list; grow the ruleset instead.
- **`sponsorblock` asks by HASH PREFIX, never by video id.** Four hex characters
  of `SHA-256(videoID)`, matched in the browser. There is deliberately no
  fallback to the by-id endpoint — without `crypto.subtle` it makes no request.
  Do not "fix" that by restoring `?videoID=`.
- **`sponsorblock` v2.0.0 asks for all ELEVEN categories and all five action
  types.** v1 asked for three categories and took the API's default
  `actionTypes` — measured over 881 videos with segments, **48.7% had nothing in
  those three**, which is what "SponsorBlock is broken" was. Behaviour is
  per-category (auto / button / mute / off), owned by `src/sponsorblock.rs`,
  stored in `~/.yggterm/web-userscripts/sponsorblock.config.json`, and delivered
  to the page as a synthetic `window.__ysbConfig` preamble that
  `webpolicy::policy()` injects ahead of the script. It is NOT a file: splicing
  settings into `sponsorblock.js` would fork the host's copy from the bundle and
  `provision` would then never update it.
- **⚠ PROBE SPONSORBLOCK BY `data-ysb`, NOT by a global.** It runs in the
  isolated world, so `ychrome ctl eval js='window.__ysb'` reads `undefined` on a
  perfectly healthy script — that is a property of worlds, not a diagnosis.
  Use `js='document.documentElement.getAttribute("data-ysb")'`, which carries
  videoId, lookups, skipped, duration, segments and their behaviours.

## Still open
- **`restore`** (`PUT /api/ciphers/{id}/restore`) — `rm` has no undo, and because
  `sync` filters `deletedDate` items this client cannot even *show* the trash.
  A `list --trashed` plus `restore` would close the loop and make the
  soft-vs-hard delete distinction empirically observable, not just read off the
  server's source.
- **`auto_match_for_host` silently picks the alphabetically-first candidate.**
  Deterministic, but a headless `app web fill` on a host with 4 accounts fills one
  without asking. Latent footgun.
- Chrome extensions are impossible on WebKitGTK — content filters + userscripts
  instead.

## Passkeys — BUILT AND SHIPPED

Shipped 2026-07-10 (ychrome 718e0b2 + yggterm 2.10.5). An earlier revision of
this file listed passkeys under "Still open"; that was stale and cost an agent
a run on 2026-07-28.

- `ychrome-vault passkeys <item>` lists stored `fido2Credentials` — metadata
  only; the key never leaves the agent and a listing cannot start a ceremony.
- `src/passkey.rs` runs the ceremony: page -> shim -> `/fido2/get` -> Signer
  -> OSC 7717 -> native presence dialog -> `/fido2/grant` -> agent
  `fido2-assert` -> assertion. ES256 + `UserPresence` live in ychrome-vault
  (`fido2.rs`), KAT-proven.

Shape: **agent drives, operator approves once at the GUI.** A page can only
trigger a ceremony, never answer one. Do not propose an auto-consent path.

⚠ Still owed: full crypto E2E against a real relying party.

### ⛔ THE SHIM IS PER-ORIGIN NOW, AND THAT HAS TWO SHARP EDGES

Since `e3aaa9d` the `navigator.credentials` shim is installed ONLY on the rpIds
the vault holds a passkey for (an unconditional shim advertised a platform
authenticator WebKitGTK does not have, and bot checks read it). Both edges bit
on 2026-08-01 — see `docs/pending-bugs.md`.

1. **The browser needs the agent's `passkey-hosts` op, and an agent outlives its
   binary.** An agent that predates it turns passkeys off on EVERY site while
   `status` reads perfectly healthy. **`agent_stale` cannot see this** — it
   compares the agent against the INSTALLED binary, and both are the old one.
   Ask the socket directly:

   ```sh
   printf '{"op":"passkey-hosts"}\n' | nc -U ~/.yggterm/vault/agent.sock
   # unknown op  =>  passkeys are OFF everywhere on this host
   ```

   Remedy, **in this order**: install the current `ychrome-vault`, THEN
   `ychrome-vault handover` (it execs the *installed* binary, so a stale
   installed binary makes the handover a no-op). The vault pane now shows this
   state with a one-click button; a stderr line reached nobody.

2. **The shim is chosen when a SURFACE OPENS.** yggterm applies userscripts at
   surface creation and refetches `/policy` only when `policy_version` moves, so
   a vault that becomes usable later does not reach an open surface. The stamp
   now covers `agent.pid` and the installed vault binary (stat-only — never put
   a socket call on that path), but a plain lock -> unlock still needs the
   surface REOPENED.

3. ⚠ **You cannot ENROL a passkey on a site you have none for** — no passkey
   means no shim means no `create()`. Every credential in this vault came from
   another browser. Do not "fix" this by widening the scope back.


## The agent engine (`src/engine/`) — headless browsing, `ychrome ctl`

Host-resident headless browser, mounted on the daemon socket at `/engine/*`.
No window, no terminal session, no OSC. `docs/agent-engine.md` is the spec;
this is what you need to drive it.

```sh
ychrome ctl open url=https://example.com/ [profile=work]   # -> {page_id, ...}
ychrome ctl goto  page_id=pg_000001 url=…
ychrome ctl eval  page_id=pg_000001 js=document.title
ychrome ctl dom   page_id=pg_000001 mode=snapshot           # the structured read
ychrome ctl shot  page_id=pg_000001 --out shot.png          # image/png bytes (see below)
ychrome ctl input page_id=pg_000001 events='[{"type":"click","selector":"#go"}]'
ychrome ctl wait  page_id=pg_000001 until='{"js":"…"}' timeout_ms=8000
ychrome ctl batch open='[{"url":"…"},…]' concurrency=8      # streams NDJSON
ychrome ctl pool | metrics | pages | park | resume | budget | identity | close
```

Arguments are `key=value`; a value that parses as JSON is JSON, anything else is
a string. Every verb is equally curl-able over the socket — the CLI is a thin
client and holds no schema of its own.

### ⚠ A SELECTOR CLICK EITHER LANDS OR REFUSES. It never reports a dispatch that hit nothing.

`document.querySelector` returns the FIRST match and real pages carry hidden
duplicates (IBKR's login has six-plus `button[type=submit]`, five dead, the live
one third). `/engine/input` therefore resolves a selector to the **hittable**
matches, in document order, and takes the first — never the first raw match.

```sh
# the default: first HITTABLE match
ychrome ctl input page_id=$p events='[{"type":"click","selector":".go"}]'
# -> {"ok":true,"dispatched":3,
#     "resolved":[{"selector":".go","matches":2,"hittable":1,"hidden":1,
#                  "zero_size":0,"nth":0,"ambiguous":false,"x":190.6,"y":21.5}]}
```

- **`"nth": k`** takes the k-th HITTABLE match (never the k-th raw match).
- **`"require_unique": true`** refuses `ambiguous_selector` when more than one
  match is hittable — for a caller who would rather stop than guess. It does
  **not** fire on hidden duplicates: five corpses behind one live control is a
  question with exactly one answer.
- `resolved` is echoed on every selector click, so a caller that took the
  default still learns it was one of nine.

A click that cannot land is **`409`** with `{"dispatched":n,"failed_at":i}` and
one of these named reasons — the same vocabulary the visible surface plane's
`web do click` uses, so what you learn on one plane carries to the other:

| reason | means |
|---|---|
| `no element matches …` | the selector matched nothing |
| `no_hittable_match (… N zero_size_element, M hidden …)` | it matched, and nothing could receive a click |
| `zero_size_element` | a `0x0` box (which is what `display:none` measures) |
| `detached_node` | the node left the document mid-resolve (a re-render) |
| `target_moved` | the post-scroll point does not reach it: covered, still offscreen, or nothing painted there |
| `handle_lost` / `rect_not_reresolved` | the two-phase resolve's own contract failed |
| `ambiguous_selector` | `require_unique` and more than one hittable match |

**A batch resolves each event against the page as it is when that event is
dispatched**, not up front — so `[{click "#open"},{click "#item"}]` works even
though `#item` does not exist when the batch arrives. A mid-batch refusal
answers with the count actually dispatched and the index that stopped it.

Locked by `ychrome engine hit` (15 steps, fixture-backed).

### ⚠ THE RULE: after input, WAIT for the state you expect. Never read straight after.

WebKitGTK acknowledges key events **one at a time** while `eval` is sent
immediately, so a read issued right after typing can overtake the last
keystroke and see the field one character short. This is not exotic: it
reproduced on **two runs in three**.

```sh
ychrome ctl input page_id=$p events='[{"type":"type","text":"ada lovelace"}]'
ychrome ctl wait  page_id=$p until='{"js":"document.getElementById(\"name\").value === \"ada lovelace\""}'
#            ^^^^ this line is not optional
```

`/engine/input` drains and flushes before returning, which helps and does not
fix it. **`wait` is the fix.** The same rule covers navigation, lazy content and
anything a framework renders asynchronously. An unmet wait returns
`{"met": false, "reason": …}` — a fact to branch on, never an exception.

Worked examples:
`assets/engine-recipes/{crawl-and-extract,form-fill,watch-page-until,capture-page}.sh`.
`run-all.sh` runs all four; green on dev, jojo and oc.

### Screenshots — four regions, one snapshot primitive

```sh
p=$(ychrome ctl open url=https://example.com/ | jq -r .page_id)

ychrome ctl shot page_id=$p                                   --out shot.png   # viewport
ychrome ctl shot page_id=$p region=full                       --out page.png   # WHOLE document
ychrome ctl shot page_id=$p region=full prescroll=true        --out page.png   # + lazy content
ychrome ctl shot page_id=$p region=element selector='#main' padding=8 --out el.png
ychrome ctl shot page_id=$p region=rect \
    rect='{"x":0,"y":1100,"w":700,"h":400}'                   --out area.png   # selection area
```

`--out` writes the PNG and prints ONE json object — the capture's own account
(`region`, `width`, `height`, the measured `scale`, the document geometry,
`crop.css`/`crop.device`, the `selector` counts, the `prescroll` report) plus
`out` and `bytes`. Over the socket that account is the `X-Ychrome-Shot`
response header; the body stays pure PNG. A refusal exits non-zero and writes
**no** file.

- **Full page is NATIVE.** `SnapshotRegion::FullDocument` renders the whole
  laid-out document in one call. There is no scroll-and-stitch and there must
  not be one: a stitch seams at every step, repeats every `position: fixed`
  header once per tile, and leaves the page scrolled where the caller did not
  put it. Proven on a 1280x2910 fixture: one header, no seam.
- **`element` and `rect` are that same snapshot, cropped from pixels already in
  hand** — never a second snapshot, so an element capture and the full page it
  claims to be part of cannot show different content.
- **The CSS→device scale is MEASURED** (snapshot width ÷ reported document
  width), never assumed from `devicePixelRatio`. `dpr` is reported alongside so
  a disagreement is visible.
- ⛔ **`rect` is in DOCUMENT coordinates.** A `getBoundingClientRect().top` is
  viewport-relative — add `window.scrollY` first. A crop that misses is
  refused BY NAME rather than answered with a blank PNG, because a blank PNG
  gets debugged as a rendering bug.
- **`region=element` resolves through the SAME hittable pool `/engine/input`
  clicks through** (same filter, same `nth` default, same
  `{matches, hittable, hidden, zero_size}` account). "Screenshot the button I
  am about to click" therefore cannot pick a different element than the click.

### ⚠ `region=full` does NOT load what was never near the viewport

A full-document snapshot renders what is **laid out**. A lazily-loaded page has
never run its `IntersectionObserver` callbacks below the fold, so it captures as
a full-height document of empty boxes — and the snapshot is not lying, the
images really are not there yet.

`prescroll=true` walks the document one viewport at a time with a 120 ms settle
between steps, then puts the scroll back. It **cannot** be one `eval`: a
synchronous scroll loop never yields the event-loop turns the observers and
fetches need, so the loop lives in Rust with the settle between steps. Measured
side by side on a five-band fixture: without it three bands stayed unloaded;
with it, all five. The walk is capped at 60 steps and says `capped: true`
rather than pretending it reached the bottom of an infinite feed.

### What the engine gives you that a lab browser cannot

It browses as **the user**: the same profile jar, adblock ruleset, userscripts,
UA and per-site zoom the visible browser uses, consumed from their owners
(`profile_dir`, `webpolicy::policy`, `webzoom`). A page logged in under profile
X in the visible surface is logged in here.

Input is **real**: `GdkEvent`s through `gtk_main_do_event`, so `isTrusted` is
true, `:hover` applies and default actions fire. A `dispatchEvent` cannot do any
of that, and the difference is the whole point.

### ⛔ The engine's cookie jar was MEMORY-ONLY until 2026-07-31

A `WebsiteDataManager` with base directories still gets a non-persistent cookie
store — WebKitGTK persists only when someone calls `set_persistent_storage`.
wry does that for the visible surface and the standalone window; `identity.rs`
built its manager by hand and never did. So **every engine page started with an
empty jar and threw its cookies away on exit**, invisibly, while the profile
directory filled with cache and storage and looked alive. The Phase C claim "a
page logged in in the visible surface is logged in here" was false, and
`parity.rs`'s cookie test passed only because both its pages share one process.

Fixed: `<profile>/cookies`, Netscape text, wry's exact path and format, so the
engine and the surface really are one jar. Locked by
`the_profiles_cookie_jar_is_made_persistent_at_the_wrys_own_path` (source-level,
because the failure is silent and no live assertion would have caught it).

Proof recipe, and the differential that makes it airtight:

```sh
export HOME=/tmp/ycf          # SHALLOW — the socket path is bounded by SUN_LEN
ychrome ctl open url=https://example.com/ profile=default
ychrome ctl eval page_id=pg_000001 js='document.cookie="k=v; path=/; max-age=3600"'
ls $HOME/.yggterm/web-profiles/default/cookies    # MUST exist. Old binary: absent.
ychrome daemon restart
ychrome ctl open url=https://example.com/ profile=default
ychrome ctl eval page_id=pg_000001 js='document.cookie'   # MUST still be "k=v"
```

### Main-frame load tracing — `engine.load.open` / `engine.load.goto`

Every main-frame load journals `{page_id, url, response:{status, headers,
bot_check}}` to `~/.yggterm/ychrome/journal.jsonl`. It exists because a bot-check
challenge loop, an asset the content filter ate, and a jar that does not persist
all present identically ("the page came back and it is not the page"). Headers
are a short allowlist — `cf-mitigated`, `cf-ray`, `server`, `content-type` — and
never `set-cookie`, because a clearance cookie is a credential. `bot_check` is
true when `cf-mitigated` is present, or on a 403/503 carrying a `cf-ray`.

### What a future agent must NOT assume

- **`page.rss_mb` and `cpu_pct_1m` are always `null`, permanently.** webkit2gtk
  2.0.2 exposes no web-process identifier, so per-page memory is not
  attributable on this substrate. They are `null` rather than `0` because a zero
  would read as a measurement. **`per_page_rss_mb` is therefore NOT implemented
  and is not schedulable** — do not plan work that depends on it. The
  *aggregate* (`max_rss_mb`) IS measured, from `smaps_rollup` PSS across the
  whole process tree, and IS enforced.
- **WPE is not the substrate**, whatever §3 of the spec says. Debian's
  wpe-webkit-2.0 2.52.5 ships no WPEPlatform at all. `ychrome engine probe`
  reports it live; believe the probe, not the prose.
- **A parked page resumes transparently.** Touch any verb and it comes back —
  including its scroll offset and form state, not just its URL.
- **`/engine/open` can answer `429 pool_saturated`** with the pressure numbers.
  That is the governor refusing, not an error to retry blindly.
- **The daemon's HOME must be shallow.** The socket path is bounded by
  `SUN_LEN` (~108 bytes); a deep `$HOME` fails with `path must be shorter than
  SUN_LEN`. This bites when testing under a scratch HOME.
- ~~**SponsorBlock's runtime state has never been observed in the engine.**~~
  **CLOSED 2026-07-31.** The old note read `window.__ysb` from a page-world
  `eval`, which cannot see an isolated-world global — the instrument was wrong,
  not the script. Observed live in the engine on dev: segments fetched, and the
  player jumped 157.88 → 208.28 past a sponsor. Probe with `data-ysb`.
- **Stop the daemon with the `stop` op, not `kill`.** SIGTERM skips
  `engine::api::shutdown()`, so the engine's headless display is orphaned.

### Proving a change

```sh
ychrome engine probe    # which substrate this host can run, and why
ychrome engine gate     # Phase A: display, load, pixels, eval, isTrusted differential
ychrome engine flow     # Phase B: nav/wait/dom and all five input events
ychrome engine hit      # selector clicks: hittability, the refusals, nth/require_unique
ychrome engine parity   # Phase C: jar, adblock differential, userscript world
ychrome engine govern   # Phase D: 300 pages under budget, park/resume
ychrome engine bench 10 # concurrency + shot latency
```

Each journals to `~/.yggterm/ychrome/journal.jsonl` and exits non-zero on
failure. Run them under a private `HOME` and they touch nothing of the user's.

## Anti-patterns

- Rebuilding a cipher from parsed fields for a PUT. → patch `RawCipher::raw`.
- Encrypting an edited field under the user key. → use the **cipher's** key.
- `DELETE /api/ciphers/{id}` when you meant "trash". → that is the HARD delete.
- Trusting a running agent after a rebuild. → `stop-agent`.
- Trusting a running ychrome DAEMON after a rebuild. → it hands itself over when
  idle; when surfaces are attached it says so and waits for
  `ychrome daemon restart`. Never kill it to save the command.
- A secret in a sidebar schema, an OSC payload, a flag, or an env var.
- Two implementations of one vault rule (yggterm had the host matchers; they were
  deleted when `matching.rs` took ownership). One owner per concept.
- Reformatting the crate to satisfy `cargo fmt --check`.
- Reading a page straight after `ctl input` instead of waiting for the state you
  expect. It works most of the time, which is what makes it dangerous.
- Treating an element's RECT as proof a click will land there. A
  `visibility:hidden` decoy measures a perfectly good box; only
  `elementFromPoint` knows. And an ancestor hit is not a hit — a click there
  reaches the ancestor, and the node you named never hears about it.
- Planning anything on per-page memory: it is not measurable here (see above).
