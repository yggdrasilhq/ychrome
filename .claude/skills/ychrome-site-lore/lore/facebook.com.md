# facebook.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## login-vault · BLOCKED
task: log in with saved creds
model: claude-fable-5
date: 2026-07-18
tags: login, vault

Blocked: vault locked on the live host; user must run: read -rs PW; echo "$PW" | ychrome-vault unlock. Then #email + #pass via app web eval + input event.

## login-form-fill · PARTIAL
task: fill the Facebook login form via co-browse (creds pending vault unlock)
model: claude-fable-5
date: 2026-07-18
tags: login, react, fill, meta

Facebook /login (2026-07 layout). Verified live on the desktop host, meta profile.

Selectors (by NAME, not id — the old #email/#pass are gone):
- email/mobile: input[name=email]  (type text)
- password:     input[name=pass]   (type password)
- submit:       input[type=submit] / [data-testid=royal_login_button] / the 'Log in' button

FILL that commits to React state (a bare .value= does NOT; the button stays
disabled and the value is wiped on re-render). Use the prototype value setter +
an input event, per field:
  var set=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;
  var e=document.querySelector('input[name=email]');
  set.call(e,'<user>'); e.dispatchEvent(new Event('input',{bubbles:true}));
  e.dispatchEvent(new Event('change',{bubbles:true}));
Proven: set+input readback held the value across a tick (React kept it). Drive it
with: yggterm server app web eval '(function(){ ... })()' (web eval evaluates RAW,
so wrap in an IIFE — unlike app dom-eval which wants a bare return).

Credentials: ychrome-vault get facebook.com --field username|password once the
user unlocks the vault. Inject the password the same way; never a literal.

BLOCKED tail: (1) vault is the user's to unlock (stdin master password). (2) Meta
challenges automated logins — expect a checkpoint / 'save browser?' interstitial /
2FA. 2FA OTP could be auto-pulled from the SMS data-fabric store (see the dream
doc) rather than asking the user. Do NOT submit until the operator is lined up for
a possible checkpoint.

## login-ctl-fill-totp · WORKS
task: cold login end to end on the headless engine, clearing 2FA without a human
model: claude-opus-5
date: 2026-08-11
tags: login, 2fa, totp, react, engine, ctl

Full cold login driven end to end on the headless engine (`ychrome ctl`), no GUI host
involved and no human step. Supersedes the two 2026-07-18 entries above.

WHAT THE 2026-07-18 ENTRIES GOT WRONG, both tails now dead:
- "BLOCKED: vault locked, user must unlock" is not a property of this site. It was the
  state of one host on one day. Check `ychrome-vault status` before believing it.
- "2FA needs the operator" is false. It is clearable from the vault, see step 4.
- `[data-testid=royal_login_button]` NO LONGER EXISTS. The submit control is now a bare
  `DIV[role=button]` with the text "Log in" and no test id at all.
- The manual `Object.getOwnPropertyDescriptor(...).set` + input-event dance is no longer
  needed. `ctl fill` commits to React state on its own (step 2).

1. SELECTORS, still by name, ids are useless
     email/mobile : input[name=email]
     password     : input[name=pass]
   ⛔ The `id` attributes are now EPHEMERAL REACT IDS (`_R_1h6kqsqppb6amH1_` shape),
   regenerated on every render. Never write a selector against one.
   ⛔ `input[type=submit]` exists but is ZERO-SIZE. The engine correctly refuses it with
   `no_hittable_match (... 1 zero_size_element ...)`. That refusal is the site telling you
   the real control is elsewhere, not a bug.

2. FILL, one verb, no incantation
     ychrome ctl fill page_id=$P entry="<vault entry named for the host>"
   ⭐ UNDOCUMENTED AND ESSENTIAL: `fill` takes `user=` to disambiguate a vault entry that
   holds several accounts. Without it you get `vault: "<name>" matches N accounts`.
     ychrome ctl fill page_id=$P entry="<entry>" user="<the account>"
   The reply reports per-field `want`/`got` char counts. Verify page-side anyway; the value
   held across a re-render, so React genuinely took it.

3. SUBMIT
   Prefer keyboard over the button, it survives layout churn:
     events='[{"type":"click","selector":"input[name=pass]"},{"type":"key","key":"Return"}]'
   A wrong identifier answers "The email address or mobile number you entered isn't
   connected to an account" INSIDE the page. A username that used to work may no longer be
   accepted as a login identifier even while the account is fine; fall back to the
   registered email.

4. 2FA, and this is the part that used to be called impossible
   The default challenge is a PUSH to the account's other devices ("Check your
   notifications on another device ... Waiting for approval"), which an agent cannot
   satisfy. Do not wait on it. Click "Try another way", which opens a method chooser:
     [0] Notification on another device   (default, needs a human)
     [1] Authentication app               <- take this
     [2] Backup code                      (works, but is FINITE and single-use)
   Select radio 1, press Continue, then read a fresh TOTP from the vault entry and type it.
   ⭐ Prefer TOTP over backup codes: it is non-consuming and repeatable. Check for one with
   `ychrome-vault totp <entry> <account>` BEFORE spending a backup code. Note that only ONE
   of several entries for this host carried the authenticator secret, so probe them all.
   ⛔ Never echo the code. Expand it inside the argument so it never reaches a transcript:
     events="$(python3 -c 'import json,sys;print(json.dumps([{"type":"type","text":sys.argv[1]}]))' "$CODE")"
   The `type` event reports a real before/after/grew_by readback, so a landed code is
   observable rather than assumed.
   NO SMS OPTION IS OFFERED on this challenge, so the phone SMS plane is not the route here.

5. REMEMBER-BROWSER INTERSTITIAL
   After 2FA: "You're logged in. Trust this device?" with "Trust this device" and "Always
   confirm that it's me". Trusting it persists in the profile jar and avoids re-pushing a
   notification to the human's phone on every later visit. Reversible from security
   settings. Then it lands on the feed.

TIMINGS: allow ~5s after submit and ~4s after each interstitial click. The page navigates
between the eval and the read often enough that `document.body` can be null; that error
means mid-navigation, so re-read rather than concluding failure.

PROFILE: use a dedicated profile (`ychrome ctl open ... profile=meta`), never `default`.
Cold jar is fine; the whole flow above starts from no cookies.

## accounts-centre-export-dyi · WORKS
task: request a Download Your Information export, driven to the final confirm gate
model: claude-opus-5
date: 2026-08-11
tags: dyi, export, accounts-centre, archive, meta

Requesting a Download Your Information export, driven to the final confirm button.
Verified through every configuration step; the submit itself is deliberately NOT exercised,
because starting an export is a state change on the account and that tap belongs to the
account holder. So: the navigation and configuration are proven, the submit is untested by
design rather than unknown.

⛔ THE MENU PATH IN EVERY GUIDE IS STALE. "Settings & Privacy -> Your Facebook Information
-> Download your information" no longer exists as such, and `facebook.com/dyi` is not where
the flow lives now. It has moved to the Accounts Centre and the verb has been renamed from
"Download" to "EXPORT".

DIRECT ROUTE, skips the whole menu tree:
    ychrome ctl goto page_id=$P url=https://accountscenter.<host>/info_and_permissions/dyi/
Then the panel offers "Export your information" and a "Create export" button.

THE FLOW, five screens, each a [role=dialog] replacing the last:
  1. "Create export"
  2. CHOOSE A PROFILE. Lists every profile in the Accounts Centre (the Facebook one, any
     linked Instagram, and a Meta profile) with the platform named under each.
     ⛔ ONE PROFILE PER EXPORT. There is no "all of them" option, so a request covering
     Facebook and Instagram together is NOT possible; that is two separate requests.
     ⭐ Worth reading even when you do not need it: this screen is the authoritative live
     list of which profiles the account actually owns.
  3. "Choose where to export": "Export to device" or "Export to external service". Device
     gives the downloadable archive.
  4. CONFIRM YOUR EXPORT, the settings screen. Defaults are wrong for archival use:
        Customise information : all available information excluding data logs   (good)
        Date range            : Last year        <- CHANGE to "All time"
        Format                : HTML             <- CHANGE to "JSON"
        Media quality         : Medium quality   <- CHANGE to "Higher quality"
     Each row opens a sub-dialog of radios plus a "Save". Read the radios back after
     clicking; select, verify, save, and confirm the summary line changed before moving on.
     "All time" is annotated "May take longer to export".
  5. "Start export" is the final confirm. Nothing is submitted before it.

VERIFYING THE CATEGORY SELECTION, worth doing when a specific category is the point of the
export: open "Customise information". It holds ~54 checkboxes across grouped sections. On
the default, 53 are ticked and the ONLY unticked one is "Data logs". Categories worth
confirming by name (they exist and are ticked by default): Messages, Posts, Comments and
reactions, Friends, Followers, Groups, Events, Stories, Reels, Profile information.
Messages, Posts, Comments and reactions and Groups each carry a "May take longer to export"
annotation.
⚠ The panel's "Back" control did not respond to a click. "Save" with nothing changed is the
working way out, and it commits exactly the selection you just read.

STATED BY THE SITE, both worth planning around:
- Notification goes to the account's registered email when the archive is ready.
- **Only FOUR DAYS to download once ready.** That, not the build time, is the real deadline.

READING THE DIALOG: the whole flow is text-readable, no screenshot needed.
    ychrome ctl eval page_id=$P js='JSON.stringify({d:(document.querySelector("[role=dialog]")||{innerText:""}).innerText.replace(/\s+/g," ").slice(0,900)})'
Use `region=element selector='[role=dialog]'` only for a final human-facing record.

## tag-and-click-nested-react · WORKS
task: address a control in Meta's deeply nested React UI without nth arithmetic
model: claude-opus-5
date: 2026-08-11
tags: react, selectors, click, engine, ctl, technique

How to click things in Meta's React UI without fighting it. Learned while driving the
export flow; applies to any Meta surface and probably to any heavily-nested React app.

THE PROBLEM: Meta renders each clickable row as a stack of nested elements that ALL carry
the same innerText and near-identical boxes. A profile row in the export chooser matched
SEVEN nested elements with the exact text "<name> Facebook". So text is not unique, ids are
ephemeral React ids, and most controls have no test id.

⛔ THE TRAP THAT COSTS THE MOST TIME: `ctl input`'s `nth` indexes the HITTABLE POOL, NOT
DOM ORDER. Compute an index from `document.querySelectorAll` and it will be wrong by
however many non-hittable matches sit ahead of your target. On the 2FA chooser
`[role=button]` had 4 DOM matches but only 3 hittable, so the button at DOM index 3 was at
hittable index 2. The engine says so plainly when it refuses:
    no element matches "[role=button]" at hittable index 3 - it has 3 hittable match(es) of 4
Read that message as an off-by-N correction, not as "my selector is wrong".

⭐ THE RECIPE THAT WORKS: tag by exact visible text, then click the unique tag. One eval,
one click, no index arithmetic, and it survives re-render because you re-tag each time.

    # 1. tag
    (()=>{document.querySelectorAll("[data-agent-target]").forEach(e=>e.removeAttribute("data-agent-target"));
      const want="Export to device";
      const scope=document.querySelector("[role=dialog]")||document;
      const c=[...scope.querySelectorAll("*")].filter(b=>{
        const t=(b.innerText||"").replace(/\s+/g," ").trim();
        const r=b.getBoundingClientRect();
        return t===want && r.height>=40 && r.height<=120 && r.width>60;});
      if(!c.length) return "NOTFOUND";
      c[0].setAttribute("data-agent-target","1");
      return JSON.stringify({n:c.length,tag:c[0].tagName,role:c[0].getAttribute("role")})})()

    # 2. click
    ychrome ctl input page_id=$P events='[{"type":"click","selector":"[data-agent-target]"}]'

WHY IT WORKS AND WHAT TO TUNE:
- Scoping to `[role=dialog]` when one is open removes the entire page behind it from
  contention. Do this always; Meta stacks dialogs.
- The height band is the real discriminator. Row containers land ~64px, buttons ~44px,
  section wrappers are much taller. Passing a band picks the ROW rather than the section.
- Take `c[0]`, the OUTERMOST match. Meta's click handler sits on the outer container; the
  inner text spans are inert. This is the same "handler is on the parent" pattern seen on
  other React sites.
- The returned `n` tells you how many nested duplicates existed, which is a useful sanity
  signal. `n:1` on a row you expected to be nested usually means you matched a text span
  rather than the row.
- `NOTFOUND` is a real answer: the text is not on screen, so re-read the dialog rather than
  retrying blind.

⚠ ONE `ctl input` CLICK DISPATCHES THREE EVENTS. Harmless on radios and rows (idempotent),
but it means a genuine TOGGLE can land back where it started. Always read the control's
state back after clicking it. Radios verified with:
    [...document.querySelectorAll("[role=dialog] input[type=radio]")].map(r=>r.checked)

FOR RADIOS SPECIFICALLY, the plain selector form is fine and needs no tagging, because
radios are all hittable so hittable index equals DOM index:
    events='[{"type":"click","selector":"[role=dialog] input[type=radio]","nth":6}]'
