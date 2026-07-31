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
src/provision.rs                the BUNDLED-ASSET RECONCILER: one owner of "is this
                                host's copy current?" (docs/adblock.md §5)
src/webzoom.rs                  per-site zoom overrides (web-zoom.json)
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
ychrome-vault totp NAME [USER]         # 6-digit code
ychrome-vault card NAME [USER]         # brand<TAB>holder<TAB>expM<TAB>expY<TAB>last4
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

Five hosts: **dev(=pi), the GUI host, oc, practice, jyas-webapp** — all x86_64 Debian.
**`pi` and `dev` are the SAME MACHINE** (machine-id `<machine-id>`; `ssh dev`
loops back). the GUI host is the live desktop (yggterm GUI + daemon).

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
# was typed. Run it on a host with NO agent first (free), then dev, then the GUI host.
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
- **The ruleset is 146,748 real rules now, not 60 hand-typed ones**, generated
  from nine upstream lists by `src/abp.rs` and committed gzipped at
  `assets/web-adblock/rules.json.gz`. **Read `docs/adblock.md` before touching
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
userscript promoted to its own friendly toggle), **Userscripts** (every OTHER
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

Five entries: `cosmetic-filters` (GENERATED — the cosmetic rules WebKit cannot
express, `:has-text()` and `:style()`, 1,122 of them over 704 domains, produced
by the same `abp::convert` that makes the ruleset and installed alongside it),
`sponsorblock`, `youtube-adblock`, `idcac`, `unblock-select`. The
catalog is the ONLY roster — nothing else enumerates them, so add here and every
surface inherits. Every script self-guards by hostname, because the injection
plane has no matching: document-start, top frame, MAIN world, every page.

- **The 2x-ads bug, so it is never rediagnosed from scratch.** The user
  reported "I still see youtube ads! They are sped up to 2x automatically!" The
  copy of `youtube-adblock.js` on the GUI host predated the script's metadata block, so
  it parsed to the DEFAULTS, so it ran in the ISOLATED world, where its
  `window.fetch` patch is invisible to the page. The prune never ran; only the
  DOM fallback did, and it forced `playbackRate = 16`, which WebKit clamps to
  ~2x. The forced rate is now GONE (a fallback that degrades playback while
  masking a dead primary path is worse than none) and the belt WARNS instead.
  `src/provision.rs` exists so the stale-copy half cannot recur.
- **`youtube-adblock` rots on YouTube's schedule, and that is expected.** YouTube
  ads are FIRST-PARTY, so no URL-matching filter can reach them; the script
  deletes the ad fields out of the `/youtubei/v1/player` response before the
  player reads it. The load-bearing shape is the `AD_FIELDS` list —
  `adPlacements`, `adSlots`, `playerAds`, `adBreakHeartbeatParams` — hooked into
  `window.fetch`, `XMLHttpRequest.prototype.open/send`, and a setter on
  `window.ytInitialPlayerResponse` for the inline copy a cold load ships. When
  ads come back, read a live `/youtubei/v1/player` response and update that list
  FIRST; the DOM skip-button layer below it is a belt, not the mechanism.
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
