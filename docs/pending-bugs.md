# ychrome — pending bugs

Entries are removed in the same commit as their verified fix. Newest first.

> **This file is the ONE answer to "what is open" for ychrome.** Open items
> only; an entry is deleted in the same commit as its verified fix and git
> remembers it. The law, the owner table for every other question, and how to
> search the archive are in `yggterm/docs/docs-ssot.md`.

## ⚠ `daemon restart` RETIRES ONLY THE DAEMON ON THE LIVE SOCKET — the rest accumulate forever

**Status:** OPEN

Measured on `dev` 2026-08-07 by the graph-router orchestrator, immediately after a clean
`ychrome daemon restart` (pid 1667242 retired → 2999778 serving, 4 surfaces re-registered in ~4 s —
**the restart itself worked exactly as documented**).

⛔ **Two OTHER ychrome daemons survived it**, both running **binaries deleted from disk**:

```
pid  742285  up  30h   /home/user/gh/ychrome/target/release/ychrome --daemon   (deleted)
pid 3022293  up 153h   /home/user/gh/ychrome/target/release/ychrome --daemon   (deleted)   ~6.4 days
```

Neither is an empty husk: **7 sockets and 3 child processes each**. The older one listens on
`/tmp/ycc/.yggterm/ychrome/daemon.sock` — **a different socket root** from the installed daemon's
`~/.yggterm/ychrome/daemon.sock`.

**The mechanism, and it is not a bug in the restart:** `daemon restart` retires the daemon bound to
the socket it resolves. A daemon bound **elsewhere** — a dev build, a test fixture root, an older
`$YGGTERM_HOME` — is invisible to it and therefore immortal. Every development run can leave one,
and nothing ever collects them.

⚠ **Why this matters beyond tidiness:** each one holds sockets and children, they run **code that no
longer exists on disk**, and they make any capability probe on the host **ambiguous** — the same
shape as yggterm's own long-standing daemon proliferation (28 daemons on `dev`, 26 on deleted
binaries, oldest up 24 days). A probe that asks "what does ychrome do here" can be answered by a
six-day-old build.

**Owed:** a reaper — or a `daemon list` — that reasons about **socket path + binary liveness +
ownership**, not just the current socket. ⛔ It must not simply kill by age: these hold children, and
one of them may legitimately serve a fixture root a test is using. **Identify, then retire; do not
count and kill.**

**Falsifier:** `pgrep -af "ychrome --daemon"` on a host after a restart. If only one daemon remains
and no `(deleted)` binary appears in `/proc/<pid>/exe`, this is wrong.

## ⭐ A CARD ITEM IS UNREADABLE AND UNEDITABLE IN THE SIDEBAR — the View pane shows nothing, `edit` has no card options

**Status:** OPEN. **Reported by the operator 2026-08-07**, in his own words, while he was paying an
India Post booking by card on guihost: *"ychrome-vault edit/or see details of card is broken and no
detail other than note can be seen in the GUI sidebar."* Relayed from the atlasStore lobe;
booking run log: `~/data/atlasStore/graph/notes/indiapost-rti-a-booking-run-2026-08-07.md`.

**Two halves, and neither is a secrets-policy question — that is what makes this a bug and not a
boundary.**

### 1. The View pane renders NO card facts, though the agent already answers them secret-free

`view_tab_widgets` (`src/sidebar.rs`) has one card branch and it is a dead end:

```rust
let is_card = detail["item_type"].as_u64() == Some(u64::from(CIPHER_TYPE_CARD));
// … "Login credentials" section …
text: if is_card {
    "This is a card. Its number and CVV reach a page only through the card fill button — they are never shown here."
}
```

After that line the builder falls through to Autofill / Notes / History. **There is no card
section**, so brand, cardholder, expiry month, expiry year and **last4** are all invisible — and
those five are exactly what `ychrome-vault card <name>` already prints on the CLI:

```
$ ychrome-vault card 'IDFC WOW AVIKALPA' | awk -F'\t' '{print NF}'
5      # brand, cardholder, expMonth, expYear, last4 — all PRESENT, all non-empty
```

⇒ **The CLI and the pane disagree about the same five fields**, and the CLI is right: a PAN and a
CVV are secrets, a **last4 and an expiry are not** — the whole point of `ychrome-vault card`'s
docstring is that it prints metadata and never the number. So the pane is withholding data its own
agent classifies as safe, and the operator's practical loss is real: **with two IDFC WOW cards in
the vault (his `IDFC WOW AVIKALPA` and his sister's `IDFC Credit Card WOW bon`), last4 is the only
thing that tells them apart at a glance** — and picking the wrong one charges the wrong person.
That confusion has already cost a session once (see `[[reference_idfc_wow_card_item]]`).

**Ask:** a **"Card" section** in `view_tab_widgets` when `is_card`, carrying brand · cardholder ·
`MM/YYYY` · `•••• last4` as ordinary rows, sourced from the same `card` op the CLI uses. The number
and the CVV keep their masks and their eye-less treatment — nothing about this asks for a PAN in a
schema. Keep the existing sentence as the section's muted footer, where it reads as an explanation
rather than as the entire content.

### 2. `edit` cannot change a card at all — not from the pane, not from the CLI

```
$ ychrome-vault edit --help | grep -iE 'card|brand|cardholder|exp|number|code'
(nothing but --set-field's help text)
```

`edit` models `--rename / --set-user / --uri / --totp / --notes / --set-field / --folder`. **A card
has none of those as its real content**, so the only edits reachable on a card item are its title,
its notes and its custom fields — which is precisely the operator's *"no detail other than note"*.
An expiring card (they all expire) currently cannot be updated by this client at all; it has to be
edited in the Bitwarden web vault, which is the one thing this CLI exists to avoid.

**Ask:** `--card-brand / --card-holder / --card-exp-month / --card-exp-year / --card-number /
--card-code`, with the two secret ones **read from stdin like `add`'s password**, never from argv
(argv reaches `ps` and shell history). Same re-read-after-write proof `edit` already gives every
other field. The pane's Edit tab then grows the same fields under its existing "empty box means
leave this alone" rule, which already handles secrets correctly.

⚠ **Do not fix half of it.** Being able to SEE a card's expiry without being able to CHANGE it just
moves the dead end one screen later; the operator hit both in the same minute.

## ⭐ THE ENGINE CANNOT PAY BY CARD — `ctl fill-card` does not exist

**Status:** OPEN. Raised by the operator 2026-08-07 via the atlasStore lobe; triaged here from
`yggterm/docs/agent-cobrowse-gaps-2026-08-07.md`, which holds the full field report and the
measurement table.

```
$ ychrome ctl fill-card
{"error":"unknown engine verb \"fill-card\"","ok":false}      # engine replied 404
```

⛔ **This defeats the engine's own premise.** `ctl` exists so agent browsing never touches the
operator's machine (`agent-engine.md` §4: "how agent browsing finally stops touching guihost"), and
the co-browse doctrine prefers dev for exactly that reason. But `server app web fill-card` needs a
registered GUI client — dev has **0**, guihost has **1** — so **every card payment is forced onto the
laptop the human is working on.** The one class of task with the strongest reason to stay off his
machine, entering payment credentials, is the one class the engine cannot finish.

⚠ **Not a fleet non-uniformity, and that was checked first** because it was the operator's
suspicion: `ychrome-vault card` works on dev, and dev and guihost run a **byte-identical** yggterm
binary with an **identical `web` verb set including `fill-card`**. Nothing is missing from dev's
install. The gap is one missing verb on one driver.

**Costed twice in 24 h:** an RTI fee payment took the netbanking rail (and died on a stale bank
password) because the card rail was not reachable from the driver in use; and an India Post
Click-n-Book booking was driven end to end on `ctl` — login, five-step wizard, pincode modals,
cart — then handed to a second agent on guihost at 05:00 to pay **Rs 23**.

**Ask:** `ctl fill-card page_id=<p> item=<name> field=<number|expiry|code|holder> target=<sel>`,
mirroring `server app web fill-card` over the **same vault agent `card-secret` op** (never the CLI,
which prints no PAN), answering `{item, field, chars, matched}` — a length, never a value — and
leaving the same one line in `~/.yggterm/vault/audit.log`. A companion `ctl fill-vault` closes the
same hole for password-gated flows. ⛔ **The PAN boundary is correct and no ask here touches it:**
no verb prints a card number, the secret stays behind the vault agent's `card-secret` op gated by
the unlock alone. Keep that exactly as is.

## ★★ THE ENGINE CANNOT REACH INTO A CROSS-ORIGIN FRAME, AND THAT BLOCKED A PAYMENT

**Status:** OPEN. Found by the atlasStore lobe, 2026-08-06 (run 5, ychrome
`190da86`), driving BillDesk's Embedded SDK v2 on a live RTI fee payment.

`ctl eval` runs in the TOP document only. A payment UI that lives in a
cross-origin `<iframe>` therefore cannot be read or driven at all: no selector
resolves into it, `window.frames[0].document` throws `SecurityError` by design,
and there is no `frame=` or `frame_selector=` argument on any verb.

The lobe's workaround was to re-POST the gateway form with `target=_self` so the
bank's UI renders TOP-LEVEL, where ordinary `eval` works. That reached the Net
Banking bank list and the IDFC login page, so it is a real workaround — but it
is only available when you can rebuild the submit by hand, and it changes what
the merchant page thinks happened.

⛔ **The alternative they correctly REFUSED** was blind coordinate clicking on a
payment page, because `ctl shot` was also unavailable to them at that moment
(see the entry below, now fixed). A driving surface with neither frame reach nor
a screenshot is one an agent must not use to spend money.

**Shape of the fix**, so it is not re-derived: `webkit_web_view_evaluate_javascript`
takes a `world_name`, not a frame, and `WebKitFrame` exists in the web-process
extension API only. A `frame=` selector on `/engine/eval` and `/engine/input` is
the verb that is missing.

### ✅ THE COST QUESTION IS SETTLED: NO WEB-PROCESS EXTENSION IS NEEDED

**Measured 2026-08-06, `ychrome engine frames`, 8/8 on dev.** The entry used to
end "whether it can be done without a web-process extension is UNPROVEN —
establish that first". It is now established, by measurement, and the answer is
the cheap one: **two UI-process APIs the engine already links are enough.**

1. **`UserContentInjectedFrames::AllFrames`** — `identity::attach_script`
   already honours a userscript's `@all-frames`, and WebKit really does inject
   it into a cross-origin child. That is the load-bearing half: a bank's
   document will never cooperate, so the only question was whether our own code
   can run inside it. It can.
2. **`postMessage`** — cross-origin by design. The copy of the bridge in the
   child talks to the copy in the top frame, and ordinary `eval` (which reaches
   the top frame) reads the result off a global.

The fixture is two real loopback origins with an **inert** child page — no
script, no listener, no beacon — because a child that helped us would prove
nothing about a real gateway. Measured, in this order: the child is genuinely
cross-origin (`THREW:SecurityError`); the bridge runs inside it; ⭐ **the
mutation control** — the same script in the same world with the same `@match`
and no `@all-frames` is `undefined` in the child and a `number` in the top; the
child's DOM reads back; a listener installs inside the child; an element
measures inside the child.

⭐ **And it drives, not just reads.** `/engine/input` was never the missing
piece — WebKit already hit-tests a real `GdkEvent` through the frame tree. What
was missing is the **coordinate**, because selector resolution runs in the top
document and cannot see into the child. A rect measured inside the child plus
the iframe's own rect is that coordinate. Proven: a click at the translated
point lands on the child's own `#otp` with `isTrusted: true`, focuses it, and
`424242` typed after it reads back **from inside the child**. A `#decoy` band
occupies exactly where an untranslated point would land, so this cannot pass by
arithmetic accident.

**Mutation-proven**: deleting `@all-frames` from the bridge turns exactly the
seven dependent steps red and leaves the cross-origin control green.

**What is still OPEN**, and why this entry stays: the verb itself. `frame=` on
`/engine/eval` and `/engine/input` is not built. The probe's bridge is an
instrument, not the design — it is installed for the run, removed on every exit
path including a panic, and gated on a per-run token so a leftover copy is inert
to any page. ⛔ **Do not lift that script into `src/`**: it accepts `eval` off
the page's own message channel, which a shipped verb must not.

⚠ Do NOT "fix" this by loosening origin checks. The frames are cross-origin
because banks intend them to be — and nothing above does: the child reads its
OWN document, in its own frame, and reports a value.

### ⛔ `rustfmt <file>` IS NOT FILE-SCOPED, AND THAT IS THE FORMATTER TRAP IN A NEW SHAPE

The skill says to use `rustfmt <file>` rather than `cargo fmt` (which reformats
the whole workspace and buried a 385-line change on 2026-08-06). That advice is
incomplete and cost a revert the same day: **`rustfmt` follows `mod`
declarations.** `rustfmt src/engine/mod.rs` reformatted `api.rs`, `host.rs`,
`js.rs` and `substrate.rs` — four files that change had never touched.

**Use `rustfmt --skip-children <file>`**, and audit `git diff --stat` after
formatting, every time. A formatter that silently widens its own blast radius
looks exactly like a clean run until someone reads the diff.

---

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

## ★★★ A DAEMON HANDOVER STRANDS THE GUI ON A DEAD CONTROL PORT, AND SURFACES SILENTLY LOSE ADBLOCK

**Status:** OPEN
**User-reported 2026-07-31, immediately after a deploy + `ychrome daemon
restart`.** The GUI raised:

> **Web policy unavailable** — The app did not serve its ad-block and userscript
> policy (connect 127.0.0.1:42687: Connection refused (os error 111)). Its
> surfaces open unprotected.

**The notice is honest and correct — that part is good design.** The defect is
what it is honest *about*.

### What is actually true, measured on the live host

- The GUI was trying **42687**, a port from a daemon generation that no longer
  exists. Nothing listens there.
- **The policy was available the whole time.** The current daemon (pid 486744)
  serves the session's control endpoint on the port recorded in the journal's
  latest `register` — for `oi cfa` that is **34419**, and `curl
  127.0.0.1:34419/policy` returns the full new ruleset (`/ping` → 200).
- ⚠ **Do not misread the `ss` output**, as I did first: the ports the journal
  registers are owned by the **daemon process**, not by the per-session
  `ychrome --profile <name>` CLI. That is the design — the daemon hosts the
  control endpoint. The CLI's own listener answers **404** and is not it.

### The bug

**A daemon handover invalidates every control port the GUI has cached, and the
GUI does not re-resolve — it caches the dead port, warns once, and leaves the
surface unprotected.** The remedy exists in the daemon's own journal (the new
`register` line carries the live port); nothing consults it.

Consequence, and why this is ★★★: the user's browsing continues with **no
adblock and no userscripts** while the ruleset sits correctly installed and
served. The failure is silent after the single toast, and it survives for the
life of the surface.

### PROVEN IN A SHADOW SESSION, 2026-07-31 — and the consequence is worse than "no rules"

Probed with `server app web eval` against a real YouTube page in a backgrounded
probe session (the user's viewport never moved). The page contained **every one
of yggterm's own shims** — `__yggtermClipboardImagePasteShim`,
`__yggtermCloseShim`, `__yggtermScrollNavShim`, `__yggtermThemeColorShim` — and
**not one ychrome userscript**: `window.__ytAdDefense` `undefined`,
`window.__ysb` `undefined`, `window.fetch` still native. The surface reported
`policy_gate: "absent"`.

So the userscript plane is not merely late, it is **absent for the life of that
surface**, which is exactly what `yggterm/docs/web-surfaces.md:341-346` already
says happens:

> *"A surface opened before its contribution exists is created unblocked and
> runs without userscripts for its whole life. After
> `MAX_POLICY_FETCH_ATTEMPTS` failed fetches the gate opens anyway — a page with
> no adblock beats no page — and the user is notified."*

**That is the whole chain**: binary swap → daemon handover → GUI holds a stale
control port → policy fetch fails → gate opens unblocked → **no userscripts,
permanently** → YouTube pre-rolls play (they are served from googlevideo.com,
the same host as the video, so no network rule can touch them — the main-world
`adPlacements` strip is the only defence) and SponsorBlock never runs.

**Two fixes are owed, not one:**
1. Re-resolve the control port (above). Necessary, not sufficient.
2. ⭐ **"For its whole life" is too harsh a punishment for a transient fetch
   failure.** A surface that opened unblocked must be REPAIRED when the policy
   later becomes reachable — recreate it, or re-run the userscript attach —
   rather than staying degraded until the user happens to close the tab.
   Reloading the page does NOT fix it: `init_script` is bound at webview
   creation, so the surface must be rebuilt.

⚠ Interim remedy for a user in this state: **close and reopen the ychrome tab**
(not reload) once the policy endpoint answers. Verify with
`curl 127.0.0.1:<registered-port>/policy`.

⚠ Honest limit: a follow-up attempt to prove the cure by recreating the surface
was INCONCLUSIVE — the second `ychrome` invocation did not mint a new surface
(the href never changed), so "a fresh surface gets its scripts" is expected but
**not yet demonstrated**. Demonstrate it before closing this entry.

### The mechanism, traced 2026-08-02 (the expensive half is done)

Read on the yggterm side, `crates/yggterm-shell/src/shell.rs`:

- A contribution holds `policy`, `policy_attempts` and `declared_control_url`.
- `sidebar_contribution_matches_declare` ALREADY handles a moved endpoint: a
  declare carrying a different control url returns false, and the caller tears
  the contribution down so the declare re-creates it. **So re-resolution exists
  for a fresh declare** — which is why a session whose ychrome CLI is cycled
  recovers, and why cycling the clients is the working remedy today.
- What has no owner is the REPAIR. `fail_sidebar_policy` counts to
  `MAX_POLICY_FETCH_ATTEMPTS` (3) and `web_surface_policy_gate` then answers
  `Abandoned`, so the reconciler builds the surface with no userscripts and no
  ruleset. When the policy later becomes reachable, `apply_sidebar_policy` fills
  `contribution.policy` — and **nothing rebuilds the webview**, which still
  holds the empty `init_script` set it was created with. That is the "for its
  whole life" clause, and it is a missing edge rather than a wrong decision.

**Shape of the fix:** when `apply_sidebar_policy` lands a policy for a session
whose surface was created under `Abandoned`, reclaim that session's tabs so the
reconciler recreates them with the scripts attached —
`selecting_a_reclaimed_tab_recreates_its_webview_with_the_saved_url` is the
existing proof that a reclaimed tab comes back at its saved url, so the
machinery is already there and only the trigger is missing.

⚠ A rebuild is visible (the page reloads), and that is unavoidable:
`init_script` binds at webview creation, so there is no silent repair. Prefer
rebuilding a BACKGROUND tab immediately and an active one at the next
opportunity, rather than yanking the page the user is reading.

### The fix

On a policy-fetch connection failure the GUI must **re-resolve the endpoint**
(ask the daemon, or honour the next OSC re-declare) and retry, rather than
pinning the port it first learned. A refusal is only honest when re-resolution
is genuinely impossible. Consider also having the daemon push a policy-changed
signal on handover so surfaces refresh without a fetch failing first.

### Operator note — the deploy step that was skipped

`docs/pending-bugs.md` in **yggterm** already warns that after any ychrome
deploy the daemon is stale by definition and must be handed over **together with
its clients** ("clients and daemon together per the round-29 mixed-version
note"). The 2026-07-31 deploy handed over the daemon and did **not** cycle the
clients, which is exactly how this surfaced. Until the fix lands, the manual
remedy is to cycle each session's ychrome CLI so it re-declares its endpoint —
at a moment the user is not mid-task, because it reloads their page.

⚠ **Measured while fixing the sibling token bug, 2026-07-31 — one fact that
narrows this one.** The CLIENT does re-declare the moved control url on every
~4s heartbeat, and has since 2026-07-18; a real client was driven through a real
`ychrome daemon restart` on dev and followed the new port every time. So the
dead-port window is one heartbeat, not the life of the session, and the durable
damage named above ("no userscripts, permanently") comes from the SURFACE that
was created unblocked during that window rather than from a declare that never
arrives. That makes fix (2) — repair a surface that opened unblocked — the
load-bearing half. Which sessions are still on a pre-gate CLI is now visible in
`ychrome status`; that marker is about the token, not about this port, but it
answers "did anyone forget to cycle the clients" in one command.

---

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
