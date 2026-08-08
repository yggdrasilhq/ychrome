# ychrome — pending bugs

Entries are removed in the same commit as their verified fix. Newest first.

> **This file is the ONE answer to "what is open" for ychrome.** Open items
> only; an entry is deleted in the same commit as its verified fix and git
> remembers it. The law, the owner table for every other question, and how to
> search the archive are in `yggterm/docs/docs-ssot.md`.

## ⚠ VAULT PANE vs THE BITWARDEN EXTENSION — the parity gap, itemised

**Status:** OPEN. Owner directive 2026-08-08: *"Make our vault GUI and implementation on par
with Bitwarden's extension."* Tonight closed the defects that broke FUNCTION or that he could
see; this entry is the honest remainder, so nobody reads the green ticks as parity.

**Closed 2026-08-08** (do not re-open): passkeys were invisible to every ceremony
(credential-id spelling); `icon:copy`/`icon:eye`/`icon:dice` printed as literal text over the
value; row icons too small and un-grounded; a stored passkey shown nowhere on the edit form or
in the list.

**Still missing, roughly in the order a user meets them:**

1. **No passkey autofill, and no picker.** THE big one. A passkey is only usable when the SITE
   starts a ceremony; the Fill tab lists logins only, so there is no "sign in with this
   passkey" affordance and no way to CHOOSE among several for one site. Bitwarden offers it
   from the item. ⛔ Blocked behind the presence-request bug above — a picker is pointless
   while no grant can be delivered.
2. **No passkey removal.** An item's passkey can be read but not deleted from the pane.
3. **A created passkey confirms nothing.** `fido2-create` mints and stores correctly, but the
   sidebar says nothing afterwards, so the user trusts a silent success — the same class the
   owner's own razor names: *a status field without a readback is decoration*.
4. **`clear_notes` / `clear_totp` are standing toggles**, not per-field actions. They read as
   two duplicated delete switches while scrolling and were reported as such. Bitwarden removes
   a value where the value lives.
5. **Cards and identities have no real editor.** `item_type` 3 and 4 render, and a card fills,
   but neither can be edited — and 130 of ~1125 items in this vault are not logins. ⚠ CREATE is
   no longer part of this gap: the Add tab makes logins, secure notes and cards as of
   2026-08-08 (`add_tab_widgets`). The EDIT form is still login-shaped.
7. **An IDENTITY (`item_type` 4) can be neither created nor read.** The create path refuses it
   deliberately — nothing in this client decrypts an identity, so an item written from a form
   would store fields the user could never see again, and the save would report success. The
   two halves must land together: a reader (`Vault::identity`, the twin of `Vault::card`) and
   then the form. Until then the pane offers Login / Note / Card and says so by offering
   nothing else.
6. **The GUI's modal state is unobservable.** `server app state` exposes no `pending_fido2`, so
   neither an agent nor a test can tell whether a ceremony is actually in front of the user or
   was dropped. That is what made the presence-request bug take a full session to corner.

⚠ **Sequencing:** items 1–3 all sit behind the presence-request fix. Do that first or the work
cannot be verified end to end — which is exactly the trap this session fell into.

## ⛔⛔ A PASSKEY CAN NEVER BE APPROVED: the presence request is written to the DAEMON'S `/dev/null` stdout

**Status:** OPEN — this makes `navigator.credentials.get()` unusable on every daemon-served
surface, which is all of them.

Measured on guihost 2026-08-08, driving a real Google sign-in end to end.

`passkey::emit_fido2_request` publishes the ceremony as an OSC on **stdout**:

```rust
let mut stdout = std::io::stdout().lock();
let _ = write!(stdout, "\u{1b}]7717;fido2;request;{encoded}\u{7}");
```

That is correct for a ychrome launched **inside a yggterm row**, where stdout IS the row's PTY
and yggterm parses the sequence into `PendingFido2Dialog`. It is wrong for the architecture we
actually run:

```
ychrome --daemon (pid 1191332)   /proc/PID/fd/1 -> /dev/null      <-- serves the surfaces
foreground ychrome (pid 1145528) /proc/PID/fd/1 -> /dev/pts/12
```

**The daemon serves the pages, and the daemon's stdout is `/dev/null`.** So the request is
written into nothing, no modal is ever raised, and the ceremony parks on the `Signer` condvar
for the full `CEREMONY_TIMEOUT` (120 s) waiting for a `/fido2/grant` that nobody was ever asked
to send. The page then shows Google's "Something went wrong".

**Three independent confirmations that nothing downstream is at fault:**

1. The shim IS installed on the page (`navigator.credentials.get` stringifies to our JS).
2. The vault RESOLVES the passkey — after the 2026-08-08 credential-id fix the call returns no
   error at all, where it previously answered `no passkey in this vault answers that request`.
3. `~/.yggterm/vault/audit.log` contains **zero** `fido2` lines, ever. `fido2-assert` is only
   reached after a grant, so its absence proves the grant never arrives — the failure is upstream
   of the vault, not in it.

**What the fix has to do:** route the presence request over a channel that survives the daemon,
the same way everything else the GUI needs already does — the per-session control endpoint the
surface already declares (the `sidebar` declaration `/fido2/grant` is POSTed back to). The OSC
must be emitted on the OWNING SESSION'S stream, not on whatever stdout the emitting process
happens to hold. `emit_fido2_request` already takes a `session` argument and currently uses it
for diagnostics only ("the GUI routes the OSC by the STREAM it arrived on, not this field") —
under a daemon that comment is precisely the bug.

**Do not confuse this with daemon orphaning** (that one is fixed — `daemon list`/`daemon reap`,
`docs/host-daemon.md` §6.2). A `ychrome daemon restart` ALSO leaves live
surfaces pointing at the retired daemon's control port (observed: `connect 127.0.0.1:41459:
Connection refused`, GUI toast "Web policy unavailable … Its surfaces open unprotected", which
additionally strips the userscripts and therefore the shim). That is a second, separate defect
on the same path; fixing either one alone still leaves passkeys broken.

## ★ THE VAULT PANE STILL WAITS FOR YOU TO PRESS SYNC

**Status:** OPEN — only (c), the sync SCHEDULE, survives. Everything else in
this entry shipped and is live-proven on the GUI host.

✅ **2026-08-04 — the DEPLOY half is closed.** Both binaries are installed on the
GUI host, the vault agent was handed over (unlock preserved, 1122 items, fresh
`last_sync_unix`) and the ychrome daemon restarted, so the pane the user is
looking at is the current one. The row's `✎` is gone: a row OPENS the entry now,
into a Bitwarden-shaped View Login (masked secrets throughout, password history
expanded, `Edit` leading an action bar with archive and delete). Archive was
exercised against the live server and reversed, leaving the vault as found.

✅ **2026-08-04 — and the reason it could not be exercised before is fixed too.**
A contributed pane could be OPENED from the control plane and not PRESSED; the
new `server app pane <pane-id> <action> [value]` verb (yggterm 3.0.24) routes
through the same owner a click uses. Without it this pane's affordances are
pointer-only, on a desktop where absolute pointer injection does not map to
screen pixels.

User-tested the sidebar against the real Bitwarden extension side by side,
2026-08-02, with screenshots. Three gaps; two are closed, one remains.

### 1. Edit could not SHOW a stored value — ✅ CLOSED 2026-08-02, twice, live-proven

The user's words: *"I cannot edit the existing fields or see them to manually
copy paste."* The pane was a REPLACE form, not an EDIT form, so you could not
check what was stored or copy it by hand — the fallback every other client gives
you when autofill misses.

**The first fix** put a separate "Stored values" section under the form: one row
per value the entry HOLDS, each with a `👁` and a `⧉`.

**The user reported again the same evening**, side by side with Bitwarden:
*"I cannot see passwords in edit mode and our vault UX looks so lifeless with
dullness everywhere."* A list of rows beneath a column of blank boxes is not what
edit mode looks like in any client — the mask, the eye and the copy belong ON the
field.

**What landed (final).** The form is Bitwarden's Edit Login: **Item details ·
Login credentials · Autofill options · Additional options**, each a card. The
password box shows mask dots at rest with `👁`, `⧉` and a `⟳` (arms a roll for
the next save; it replaced the "Roll a new password instead" toggle, so the flag
has one owner) inside its trailing edge. Save is pinned in the pane footer,
wearing the accent. Custom fields stay rows — their values have no in-place write
path, and a typeable box that does nothing on save would lie about the
affordance.

⛔ The empty-box rule was a real invariant, not laziness, and it is intact — and
now held by CONSTRUCTION rather than by care. The mask dots are a **placeholder**
(`stored: true` on the widget), so they cannot be submitted or read back; and
yggterm keeps a `stored` field's declared value OUT of the form draft, so a
revealed password cannot be re-sent on the next action. A revealed value is still
a **parameter**, not state — `PaneState` cannot hold one, `GET /pane/vault`
builds through the no-reveal owner, and the next render is built without it.
**A secret is never in a schema AT REST or in a LISTING.** Locked by
`the_schema_route_cannot_carry_a_revealed_value`,
`a_revealed_value_lives_in_exactly_one_render`,
`the_edit_form_declares_every_secret_box_empty` and
`the_edit_form_puts_the_mask_the_eye_and_the_copy_on_the_field`.

⚠ **Needs the yggterm side.** The mask, the inline verbs, the cards and the
pinned footer are yggterm schema fields (`stored`, `actions`, `card`, footer
`primary`) shipped on `lane/dev/youthful-inputs`. On an older GUI the schema
degrades to plain boxes — the form still works, it just draws flat.

### 2. No copy actions on a row — ✅ CLOSED 2026-08-02, live-proven

Bitwarden's row overflow offers **Copy username · Copy password · Copy
verification code**. Ours offered fill ⧉, totp ⏱ and edit ✎ — every affordance
"put it in the page", none "give it to me".

**What landed.** Those three are in the row's right-click `menu` (yggterm's own
row vocabulary, so a menu item can say what it does in words where a fourth glyph
in a 300px rail could not), offered only for what the listing says exists. The
value is handed to the GUI as an `eval` calling `navigator.clipboard.writeText`
— the clipboard belongs to the GUI's host, so it takes the injector road a fill
takes. There is no OSC 52 spelling: that would put the secret in the scrollback
ring.

✅ **THE CLIPBOARD RUNG IS MEASURED, NOT ASSUMED.** The open question was whether
WebKitGTK grants `writeText` to a GUI-injected eval at all (user gesture, focus).
It does: on a live surface at `https://example.com/`, "Copy password" from a
row's menu left the page saying *"Copied the password to the clipboard."* and
`navigator.clipboard.readText()` read back **20 characters** — the scratch item's
generated password, by length, never printed. Same for the notes (**18**).

⚠ **One honest limit remains.** `navigator.clipboard` exists only in a **secure
context**, so on a plain http page there is nothing to write with; the page
notice names that and points at the eye. That branch has not been exercised on a
real http page. A hidden-textarea `execCommand('copy')` fallback was refused on
purpose: it puts the secret in the page's own DOM, where the page can read it,
and a user copying a password on some unrelated site did not consent to that.

⛔ A card still has neither verb. Number and CVV reach a page only through
`card-fill` — a PAN in a transcript is durable and cannot be rotated (settled
2026-07-26).

### 3. The sync line is a warning where a fact belongs — and the cause is ours

The pane shows: *"This host's vault agent does not report when it last synced,
so nothing here can tell you whether it is current. Install the current
ychrome-vault and hand the agent over."* The user: *"our sync now option should
have last synced time and then sync now button … A warning text is
non-confidence inspiring."*

**Root cause, measured 2026-08-02 — and the advice in that warning does not
work.** `status_json` (the ONE status builder, `agent.rs:1772`) always sets
`last_sync_unix`. The RUNNING agent's status has no such key at all — proven by
asking `~/.yggterm/vault/agent.sock` directly, not the CLI. So the agent
predates the field. But:

- `agent_stale` reads **false**, because it compares the agent against the
  INSTALLED binary and cannot see a missing FIELD;
- ⛔ **`ychrome-vault handover` REFUSES**: *"is the binary this agent is ALREADY
  running — nothing to hand over"*. It compares the PATH, and an in-place binary
  replace leaves the path identical while the code differs. **So the one
  documented zero-cost remedy is unavailable exactly when it is needed**, and
  the pane's own advice sends the user in a circle.

**Fixes owed:** ~~(a) `handover` must compare the installed binary's IDENTITY~~
✅ **(a) DONE 2026-08-02** (`ce0a7ec`): `exe_stamp` now captures the running
binary's stamp ONCE, pinned at `serve_on`, instead of re-reading the path's
mtime on every call — which after an in-place install described the SUCCESSOR
and made the identity check blind to the change it exists to detect.
⚠ **Honesty about this one: the reported refusal did NOT reproduce.** On Linux a
replaced binary readlinks as `<path> (deleted)`, so the stale agent stamped
itself `(deleted)@0`, `agent_stale` read **true**, and `ychrome-vault handover`
went through **with the unlock preserved** (verified live on guihost: 1120 items,
no master password re-entry). The fix removes the dependency on that procfs
detail, which does not hold if an installer overwrites in place and keeps the
inode.
✅ **The agent-side half of (b) is also cleared**: the post-handover agent
reports `last_sync_unix` (measured `1785682003`).
✅ **(b) PANE HALF DONE 2026-08-02**: a current copy reads **"Last synced 1
minute ago"** as a muted fact with the Sync now button under it, and carries no
`⚠` at all. The warning survives for the two cases that earn one — a copy over 30
minutes old, and an agent that cannot report the fact — and the second now offers
the one-click **"Hand the agent over (keeps it unlocked)"** button instead of
prose telling the user to run a command that used to refuse.
**(c) still owed:** sync on a schedule/staleness rule rather than making the
user press it. Today the only automatic pull is on pane OPEN, once the copy is
over 30 minutes old (`refresh_if_stale`), so a pane left open goes stale in
silence.

✅ **PIXEL-PROVEN 2026-08-02, ON THE LIVE GUI HOST, WITHOUT TOUCHING ANYTHING
OF HIS.** The proof rig is the one this repo already had for exactly this:
`provision_a_scratch_vault_for_a_live_proof` against a throwaway vaultwarden in
docker, a scratch `HOME` (its own ychrome daemon, its own vault agent, adblock
off so the GUI never recompiles a ruleset), a probe row created with
`--no-activate`, and a SHADOW client. Observed, in that order:

1. the pane paints **"Last synced 1 minute ago"** as a muted fact with `Sync now`
   under it and no `⚠` anywhere — part 3, on screen;
2. `✎` opens the Edit tab with a **Stored values** section: Password ·
   Verification code · Authenticator key · Notes, plus the custom field below;
3. `👁` on Notes paints the stored value with *"Shown once. The pane keeps
   nothing"* under it;
4. a plain `GET /pane/vault` refetch immediately after shows the row **bare
   again** — the one-render invariant, in pixels rather than in an assertion;
5. right-clicking a row opens **Copy username · Copy password · Copy verification
   code · Edit, and read what is stored**;
6. clicking Copy password toasts *"Copying git.example.org's password — the page
   confirms the clipboard, or names the refusal."* and the clipboard really holds
   it (see above).

⚠ **WHAT IS STILL OWED: THE USER'S OWN DAEMONS.** The pane is served by the
ychrome DAEMON, and both of this fleet's daemons were holding live surfaces
(dev: 5, including the linked WhatsApp Web session; the GUI host: 3, two of them
mid-task). Retiring one is the operator's call, not an agent's, so
the binary is installed on both hosts and **the running daemons still serve the
old pane**. One `ychrome daemon restart` per host adopts it; every session on
that host re-registers in ~4s and its page reloads.

⚠ **And on dev the vault AGENT still predates `last_sync_unix`**, so the pane
there will show the warning branch until it is handed over. That is now a
one-click button in the pane itself ("Hand the agent over (keeps it unlocked)"),
which is deliberately where it was left: a handover that fails costs a master
password, and it is his to spend.

⚠ Same family as three other findings today: a version-gated hot-restart that
cannot swap a same-version binary, `ctl --help` exiting 0 on a build without the
verb, and `agent_stale`. **An identity check that cannot see the change it
exists to detect.**


## ★★ A CLIENT-SIDE VIEW SWAP STALLS: the URL changes, the screen does not

**Status:** OPEN

Found 2026-08-02 driving the Google sign-in end to end. After each step Google
navigates its own URL (`/v3/signin/challenge/pwd`, then the 2SV screens) and the
**visible view never swaps**: the body still renders the PREVIOUS screen with
Google's own "Loading" prefix, while the next screen's inputs are present in the
DOM at **`0x0` with `offsetParent: null`**.

**This is layout, not paint** — the measurements come from `web eval`, so the
element genuinely has no box; a stale-pixel explanation is falsified. Google's
client-side swap did not complete.

`web wait --until load:finished` answers `met: true, elapsed_ms: 1`, which is
correct and unhelpful: the document load DID finish. Waiting does not help
(measured: still `0x0` after 60 s of polling).

**`location.reload()` renders the real screen every time**, so the flow is
drivable — at the cost of one reload per step, which is why it reads as "the
Google auth flow cannot be completed" to anyone who does not know the trick.

⚠ Downstream, this makes an HONEST refusal look like a selector bug:
`fill-vault` answers `no_hittable_match (… matched 1 element(s) and NONE could
receive a click)`. That is the tool correctly refusing to dispatch into a 0x0
target — do not "fix" it by loosening hittability.

**Worth knowing before diagnosing:** the passkey shim is NOT the cause. It was
the first hypothesis (Google's identifier field carries
`autocomplete="username webauthn"`, so a conditional-mediation call into a shim
that ignores `options.mediation` would block on the ceremony timeout) and it is
FALSIFIED: `performance.getEntriesByType("resource")` shows **zero** `/fido2/*`
and zero control-endpoint requests on the stalled page. The shim was never
called. ⚠ The `mediation` gap is real anyway —
`isConditionalMediationAvailable()` answers `false` while `shim.get()` ignores
`options.mediation` entirely, so a site that asks regardless would hang on a
ceremony no agent surface can approve. Worth closing on its own merits.


## ★★★ THE PASSKEY SHIM PATCHES `navigator.credentials` ON EVERY PAGE, INCLUDING A CHALLENGE PAGE

**Status:** OPEN
**Found 2026-07-31 while investigating why a Cloudflare challenge on a
brilliant.org login would not clear.** Not the cause of that report (the UA and
the engine's cookie jar were, both fixed) but a real, measured incoherence that
a bot check can read.

`sidebar.rs`'s `/policy` prepends `passkey::shim_userscript()` to EVERY
surface's userscripts, main world, document-start, unconditionally. On a page it
touches:

- `navigator.credentials` becomes a plain `Object.create(native || {})`, whose
  methods stringify to JS source rather than `[native code]`;
- `window.PublicKeyCredential` is DEFINED, and
  `isUserVerifyingPlatformAuthenticatorAvailable()` answers `true`.

**Measured on the engine plane (which does not install the shim), WebKitGTK
2.52.5:** `typeof window.PublicKeyCredential === "undefined"` and
`navigator.credentials === undefined`. WebKitGTK has no WebAuthn at all. So on
the visible surface we claim a platform authenticator that this engine cannot
have — an anomaly no real GNOME Web ever shows.

**Why top-frame-only does not save it.** The shim is already `all_frames:
false`, so the `challenges.cloudflare.com` iframe is clean. But an interstitial
managed challenge is served as the TOP-FRAME document at the site's own URL, and
its collector runs in exactly the environment the shim has already patched.

**Why there is no cheap fix, stated so it is not re-attempted.** Since the
engine genuinely lacks WebAuthn, ANY presence of these APIs is the anomaly —
making the shim "look native" cannot work, and a URL-pattern exclusion cannot
help because the challenge is served at the normal page URL. The only correct
shape is **per-origin installation**: build the shim's `Userscript::matches`
from the set of rpIds the vault actually holds passkeys for, so a page for a
site you have no passkey for sees a pristine `navigator`.

### IT REACHED THE USER, 2026-08-01 — measured, and the cause is NOT what it looked like

**Report:** a Forgejo 2FA page answered *"Could not read your security key. Your
browser does not currently support WebAuthn."* — *"How am I supposed to use the
passkey?"*

**Two independent faults, both measured on the GUI host. Neither was the documented
"the vault holds no passkey for this host" case:** `ychrome-vault passkeys
<host>` returns a real credential for it.

1. **The running agent does not know `passkey-hosts`.** Asked directly on
   `~/.yggterm/vault/agent.sock` it answers `unknown op "passkey-hosts"`, in
   0.2 ms, five times out of five. That is the documented deploy-ordering
   hazard below, and it happened: the browser shipped ahead of its agent.

   ⚠ **The stderr announce was not evidence either way, and reasoning from its
   absence sent a first diagnosis in the wrong direction.** ychrome's stderr is
   the PTY it was launched in — it is not in `~/.yggterm`, not in the GUI's
   `app-launch-logs`, and not anywhere anyone greps. **FIXED:** the vault pane
   now renders the state (`sidebar::passkey_shim_widgets`) with a one-click
   `handover`, so the browser says it where the user already is when a passkey
   login fails.

   ⚠ **`agent_stale` is structurally incapable of catching this**, which is why
   the pane was silent: it compares the agent against the INSTALLED
   `ychrome-vault`, and both were the same six-day-old binary. `status` read
   `unlocked`, `agent_stale: false`, 1116 items — a perfectly healthy vault —
   on the same socket that refused the op. Only ASKING for the op finds it.
   The remedy is also ordered: `handover` execs the *installed* binary, so a
   stale installed binary must be replaced FIRST or the handover is a no-op.

2. **The policy stamp was blind to the shim, which made it permanent.**
   `webpolicy::policy_version` stamped adblock, the UA, SponsorBlock and
   userscript FILES — nothing about the vault — while `/policy` prepends a shim
   whose scope comes from the vault. The GUI refetches only when that stamp
   moves, and yggterm applies userscripts at surface CREATION. So a surface kept
   whatever shim decision was true when it opened, for life.

   Measured, one unchanged `policy_version` (`ebc219f7d40ddc53`):
   `sidebar_contribution/policy` recorded `userscripts: 6` at 14:53 and
   `userscripts: 5` from 16:07 onward, across the deploy. **FIXED:**
   `passkey_shim_stamp()` folds `agent.pid` (rewritten by `serve_on` on every
   start AND every handover, since an `execve` keeps the pid) and the installed
   binary's stamp into `policy_version`, stat-only so it stays off the socket.

   ⚠ **Still open:** a plain lock → unlock touches no file, so a surface opened
   over a locked vault still needs REOPENING. The pane says so in words. A
   vault-published scope stamp would close it properly.

### ⚠ ENROLMENT ON A SITE WITH NO PASSKEY — THE ARM IS BUILT, THE PROOF IS OWED

The shim's match patterns come only from rpIds the vault ALREADY holds
credentials for, so a site you have no passkey for sees a pristine `navigator`
— exactly right for the fingerprinting fix, and it also meant
`navigator.credentials.create()` could never be called there. Every passkey in
this vault was enrolled in some other browser. The user hit this on a Google
sign-in: *"I cannot enter the passkey when anyone requests me … there is no
clicking to give passkey or save a passkey."*

⛔ The fix is NOT to widen the scope back, and it was not: **the user arms one
host from the vault pane** ("Enrol a passkey here"), the anomaly exists only on
a page a human deliberately armed, and arming is per-process so a browser
restart forgets it. Built 2026-08-02, unit-locked and mutation-proven (an armed
host must reach the shim's real match patterns; a wildcard or empty host is
refused at the door).

**Owed:** the end-to-end proof on a real page — arm a site, reopen the tab,
and watch `navigator.credentials.create()` succeed. ⚠ The pane says the tab must
be REOPENED, because the shim is installed when a surface is built; if that
turns out to be wrong on this path, the notice is what needs correcting.

### IMPLEMENTED 2026-08-01, NOT YET PROVEN ON A REAL PAGE

All four steps are done and unit-locked: the `passkey-hosts` agent op (metadata
only — rpIds, no credential ids, no keys), the `sidebar.rs` call through
`ychrome-vault-proto`, no-shim-on-a-locked-vault, and the probe kept off the
`policy_version` heartbeat. Match patterns are `*://<rp>/*` PLUS `*://*.<rp>/*`,
because WebKit's `*://*.example.com/*` does not admit the bare host while
WebAuthn scopes a credential to the rpId and its subdomains. The user-presence
invariant is untouched and locked: scoping decides only WHERE
`navigator.credentials` is patched.

**What is NOT yet demonstrated, and why this entry stays open:**

1. **No end-to-end proof on a real page.** Nobody has loaded a site the vault
   holds a passkey for and confirmed the shim is present, then loaded one it
   does not and confirmed `navigator.credentials` is pristine. That is the
   observation that would actually close this.
2. **The running vault agent predates the op**, so on dev today the browser
   installs the shim NOWHERE. Verified live: the agent answers `unknown op
   "passkey-hosts"`. The code says so loudly on stderr, once, rather than
   letting it look like the healthy case — but until the agent is handed over
   (`ychrome-vault handover`) passkeys are off.
3. ⚠ **DEPLOY ORDERING IS PART OF THIS FIX.** The vault agent must be handed
   over before, or with, the browser. Shipping the browser alone turns passkey
   logins off everywhere.

---

## ★★ (yggterm) A PROFILE WHOSE WRITE-LOCK IS HELD ELSEWHERE OPENS WITH NO JAR

**Status:** OPEN
**Not in this repo — filed here because it is the other half of the cookie-jar
failure ychrome fixed on its own engine plane, and an ychrome user meets it as
"the login will not stick".**

`yggterm-shell/src/shell.rs` commented that a surface whose profile write-lock
is held elsewhere "opens READ-ONLY (ephemeral, no jar)". Ephemeral is not
read-only: an ephemeral `WebContext` reads NOTHING from the jar and writes
nothing back. A second surface on a profile another surface already holds
therefore starts logged out and cannot keep a cookie — including a bot-check
clearance cookie, which is why a challenged login can loop forever.

Two things were owed. **One is done** (yggterm `lane/dev/ychrome-bugs-docs`):
the silence. `WebSurfaceJarMode` now owns the decision, the spelling and the
words — `persistent` / `ephemeral_by_request` / `no_jar_lock_held_elsewhere` —
the mode is on the `profile_write_lock` trace line, and a degraded profile
raises ONE notice (per profile, not per tab) that talks about being logged out
and about the bot-check cookie rather than about write locks. The misleading
"READ-ONLY" comment is gone. A `debug_assert` pins the mode to the jar it
describes.

⚠ **The other half is a design call, not a mechanical fix, and that is why it
is still here.** WebKitGTK has no read-only jar mode, so "genuinely read-only"
means giving the loser a private COPY of the profile's cookies (a Netscape text
file — mechanically easy) and its local storage. Editorially it is not easy:
**every shadow surface an agent opens on a profile the user holds would then
duplicate that profile's live session cookies to a second place on disk.** In a
browser that carries the operator's brokerage sessions, spreading cookie jars is
his decision to make. Options, so the next reader does not start from scratch:

1. copy-on-open into a scratch dir, wiped at teardown, with a startup sweep for
   crash leftovers — full fidelity, maximum secret spread;
2. copy the `cookies` file ONLY, which fixes the reported symptom (the login
   sticks for the surface's life) at a fraction of the exposure;
3. decline, and keep the notice as the whole answer.

⚠ **Not live-proven.** The notice needs two live clients contending for one
profile on the GUI host, which needs a yggterm GUI + daemon deploy — and
`yggterm/docs/pending-bugs.md` still carries "REMOTE ROWS WEDGE IN
`RemoteBootstrap` AFTER A DAEMON VERSION HANDOVER", which wedged 15 rows on the
last bump. The rule and the words are unit-locked and mutation-proven; the
pixel is owed.

## ★★ THE `dream-control-surfaces` ITEMS ARE BUGS, NOT ASPIRATIONS

**Status:** OPEN
`docs/dream-control-surfaces.md` is filed as a dream document. The operator's
ruling, 2026-07-31: **these are bugs.** Framing a missing capability as a dream
puts it outside every process that would fix it — it is not in a bug list, it has
no reproduction, and nobody triages it. Several of its items have now cost real
sessions:

- **§2 Headless surface-create — the OSC must not depend on window focus.** This
  is precisely the gap that sent an agent toward revealing surfaces on the
  operator's screen; it is the subject of `dream-detached-agent-surfaces.md` and
  of the detached-by-default rule in `yggterm/docs/agent-surface-attachment.md`.
- **§1 Unlock request** and **§6 Autofill-from-vault.** The vault is the standing
  friction: it is LOCKED on a headless host and dev (as root it answers `not_configured`) and
  resolves only on the GUI host, and `fill-vault` needs a mapped surface so it is
  unavailable on exactly the detached surfaces agents are told to prefer. Every
  run pays this toll by hand.
- **§3 OTP from the data-fabric** and **§5 Extract surface** — the same shape.

**Ask:** promote each numbered section into this file with a reproduction and an
acceptance test, or state explicitly which are declined and why. The `dream-*`
documents should hold *design*, never *the only record that something is
missing*. Same applies to `dream-detached-agent-surfaces.md`, whose §5 control
and §7 table are already bug-shaped.

## ⚠ FLAKY TEST — `daemon_staleness` fails on a busy host, and it is NOT the code

**Status:** OPEN
**Characterised 2026-08-01 on dev, after it cost a session real time twice.**
Two tests in `tests/daemon_staleness.rs` fail intermittently:

```
a_pre_gate_client_is_named_as_such_by_the_refusal_and_by_status
a_gated_route_survives_a_daemon_handover_on_what_the_client_re_declares
  → panicked at tests/daemon_staleness.rs:326: a control response:
    Os { code: 11, kind: WouldBlock }        ← a 5s read timeout, not a bug
```

`control_get` gives a spawned real binary **5 s** to answer over TCP. dev is a
32-core LXC that regularly sits at load 45-112 (the yggterm test binaries alone
were measured at 1208% and 1144% CPU), and under that the budget is simply not
enough.

**Proven to be load, not a regression, by a controlled A/B under the same load:**
at load 112 the *baseline* (HEAD with the change stashed) failed **3/3**; at
load ~44 the same tree passed **8/8 in 8.2 s**. Anything that changes the
daemon's startup path will look like it caused this. It did not.

**Want:** make the budget generous (or adaptive) rather than a wall clock a
loaded CI host cannot meet — a timing test that fails on a busy machine teaches
agents to ignore red, which is worse than the flake.

---
