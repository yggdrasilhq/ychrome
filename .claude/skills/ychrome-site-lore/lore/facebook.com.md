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

## export-submit-reauth-and-status · WORKS
task: actually submit the export: the re-auth overlay, and reading the queue back
model: claude-opus-5
date: 2026-08-11
tags: dyi, export, reauth, dialog, click, meta

Completes the `accounts-centre-export-dyi` entry above, which stopped at the confirm button.
Both a Facebook and an Instagram export were submitted and are queued. Two mechanisms only
show up once you actually press the button, and one of them wasted a diagnosis.

⭐⭐ PRESSING "Start export" OPENS A PASSWORD RE-AUTH OVERLAY, AND IT LOOKS LIKE A NO-OP.
A SECOND `[role=dialog]` is appended on top of the confirm dialog:
    "Please re-enter your password. For your security, please re-enter your password to
     continue."  [Password] [Continue] [Forgotten password?]
⛔ THE TRAP: the FIRST dialog still reads "Confirm your export ... Start export", unchanged.
So a caller that re-reads `document.querySelector("[role=dialog]")` (the FIRST match) sees the
identical screen and concludes the click silently failed. It did not. Always read the LAST
dialog:
    const ds=[...document.querySelectorAll("[role=dialog]")]; ds[ds.length-1]
The overlay has exactly ONE `input[type=password]` and no username field, so `ctl fill` is safe
here despite the known decoy defect. It reports `username: {ok:false, present:false, got:-1}`
for the absent field and fills the secret correctly; that mixed response is success, not
failure. Then click "Continue" in the LAST dialog.
⚠ The re-auth is asked ONCE per session. The second export submitted straight through with no
password prompt, so do not wait for an overlay that will not come; branch on
`document.querySelectorAll("input[type=password]").length`.

⛔ CORRECTION TO `tag-and-click-nested-react`: TAKE THE ROLE-BEARING ELEMENT, NOT `c[0]`.
That entry says to take the outermost match. **For ROWS that is right; for BUTTONS it is
wrong.** "Start export" resolved to 10 nested elements, of which THREE shared the exact
button geometry (560x44 at the same y). Only ONE carried `role="button"` `tabindex="0"`, and
the outermost same-size match was an inert wrapper. Clicking the wrapper dispatches happily
(`ok:true, dispatched:3`) and nothing happens.
⇒ For a control, filter to `[role=button],button` FIRST and match text within that set:
    const el=[...d.querySelectorAll("[role=button],button")]
             .find(b=>(b.innerText||"").replace(/\s+/g," ").trim()===want);

⭐ HOW TO TELL "CLICK DID NOTHING" FROM "SOMETHING OPENED ON TOP", in one eval. Ask what is
actually at the button's own centre point:
    const r=el.getBoundingClientRect();
    const top=document.elementFromPoint(r.left+r.width/2, r.top+r.height/2);
    ({occluded: !el.contains(top) && top!==el, topText:(top.innerText||"").slice(0,40)})
That returned `"Please re-enter your pass..."` and settled it immediately. Relatedly, the
engine's own refusal is a real signal here: re-clicking answered
`no_hittable_match (... 1 hidden ...)` for an element that is plainly visible, which means
OCCLUDED, not absent. Read a `hidden` count on a visible element as "something is covering it".

AFTER SUBMIT, the panel becomes a status list worth reading back as proof:
    "Requested — Your information is being prepared for export."
    <profile> (<platform>) · Available information · Export to device · Once · JSON
    · Requested on DD/MM/YYYY · [Cancel]
⭐ Each queued export carries its own **Cancel**, and there are "Current activity" and "Past
activity" tabs. So a request is reversible until it completes, and past exports are
enumerable, which is the cheap way to answer "has this account ever been exported before"
without going to the mailbox.

⚠ SCOPE, stated by the page itself and worth quoting to anyone expecting a full social graph:
"Your export won't include information that someone else shared, such as another person's
photos that you're tagged in."

## dyi-download-the-archive · WORKS
task: download a ready DYI archive headlessly: the signed URL, the two-dialog flow, the 0x0 background trap
model: claude-opus-5
date: 2026-08-13
tags: dyi, export, download, curl, engine, ctl

Downloading a finished Download Your Information archive, driven headlessly end to end on
`ychrome ctl`. Completes the pair `accounts-centre-export-dyi` (request) and
`export-submit-reauth-and-status` (submit); this one is the collection step, and it has three
mechanisms none of the earlier entries could see because the archive did not exist yet.

⭐⭐ THE ARCHIVE URL IS SELF-AUTHENTICATING. FETCH IT WITH curl, NOT THE BROWSER.
Pressing the final Download hands the page a signed URL on `bigzipfiles.<host>` carrying
`file_secret=` and `hash=` query parameters. **It needs no cookies at all.** Verified with a
cold range request from a process with no session:

    curl -r 0-2047 -o probe.bin -D headers.txt "<signed url>"
    HTTP/2 206
    accept-ranges: bytes
    content-range: bytes 0-2047/<total>
    content-disposition: attachment;filename=<platform>-<name>-DD-MM-YYYY-<token>.zip
    content-type: application/zip

⇒ Capture the URL, then download with `curl -C - --retry 5`. That buys resume, a byte count you
can assert against `content-length`, and no 500 MB round trip through a headless WebKit whose
download handling is not part of the engine contract. A multi-hundred-MB browser download with
no progress surface and no landing path is the fragile option, not the safe one.
⚠ The signed link carries its OWN expiry in an `ext=<epoch>` parameter, and it was LATER than
the archive's advertised expiry date. So once you hold the link it outlives the page's deadline.
Decode it rather than assuming: `ext` is a plain unix timestamp.

HOW TO CAPTURE THE URL: hook XHR/fetch RESPONSE BODIES, not navigation.
Nothing navigates and no `<a href>` is ever created, so a `window.open` / anchor-click hook alone
catches nothing. The URL arrives inside a GraphQL response. Install before clicking:

    const oo=XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open=function(){
      this.addEventListener("load",function(){
        const m=(this.responseText||"").match(/https?:[^"\\ ,)]{20,}/g)||[];
        m.filter(u=>/bigzipfiles|\.zip/i.test(u))
         .forEach(u=>window.__cap.push(u.replace(/\\\//g,"/")));});
      return oo.apply(this,arguments);};

⛔ Escape the JSON backslashes: the URL comes back JSON-encoded, so `\/` must be unescaped or the
link is malformed in a way curl reports as a DNS failure.

THE FLOW IS TWO DIALOGS DEEP, NOT ONE BUTTON.
  1. The panel lists each ready archive with `Download` and `Cancel` side by side.
     ⛔ They are ADJACENT and identically sized. Resolve which is which by walking UP from the
     button to an ancestor carrying the profile label, never by index.
  2. Clicking `Download` opens a file chooser dialog: "Download your files ... File 1 of N".
     Read N. A large archive CAN be split, and a partial download looks exactly like a complete
     one. A ~500 MB archive came down as File 1 of 1, so splitting is not automatic at that size.
  3. The `Download` INSIDE that dialog is the one that mints the signed URL.
  4. A password re-auth overlay guards it, exactly as it guards the submit.

⛔⛔ NEW TRAP, AND IT COSTS A CYCLE: WHEN THE MODAL OPENS, THE PAGE BEHIND IT COLLAPSES TO 0x0
BUT STAYS INSIDE THE SAME `[role=dialog]`.
The advice in `tag-and-click-nested-react` to scope to the last `[role=dialog]` does NOT
disambiguate here, because there is only ONE dialog element and it contains both the modal and
the whole collapsed page. A naive `.find(text==="Download")` inside it returns a BACKGROUND
button measuring 0x0, and the engine then refuses:

    no_hittable_match ("[data-agent-target]" matched 1 element(s) and NONE could receive a
    click: 0 zero_size_element, 1 hidden)

That refusal is correct and is telling you that you tagged a collapsed element, NOT that your
selector is wrong. ⇒ **FILTER ON GEOMETRY, ALWAYS.** Add `r.width>0 && r.height>0` to every
candidate filter on this site. It is one clause and it makes the whole family of nested-React
selector problems disappear on this panel:

    [...document.querySelectorAll("[role=button],button")].filter(x=>{
      const t=(x.innerText||"").replace(/\s+/g," ").trim();
      const r=x.getBoundingClientRect();
      return t==="Download" && r.width>0 && r.height>0;})

Diagnostic that names it in one call: dump every button with its rect. A screen showing one
visible Download that reports five matches, four of them 0x0, is this trap and nothing else.

THE RE-AUTH IS PER PROPERTY, NOT PER LOGIN SESSION.
`export-submit-reauth-and-status` says the overlay is asked once per session. That holds WITHIN
one property. The download re-auth fired again on a later day in the same profile jar, so treat
"once per session" as "expect it, branch on it", never as "it will not come":

    document.querySelectorAll("input[type=password]").length

The overlay still has exactly one password field and no username, so `ctl fill` is safe and its
`username: {ok:false, present:false, got:-1}` line remains success rather than failure.

⚠ THE EXPIRY IN THE EMAIL AND THE EXPIRY ON THE PAGE DISAGREE. BELIEVE THE PAGE.
The notification mail says a flat "4 days from this email". The panel states a **per-archive**
expiry date, and two archives requested minutes apart on the same day expired on DIFFERENT days.
Read `Expires on DD/MM/YYYY` off each row; do not compute it from the mail.

VERIFY WHAT LANDED, three cheap assertions and all three are worth it:
  bytes == the `content-length` from the probe (a truncated zip is the failure mode here)
  `unzip -l` returns a central directory and a plausible file count
  the first four bytes are `PK\x03\x04`
Download to a `.part` name and rename only after those pass, so an interrupted transfer can never
be mistaken for a finished archive by the next session.

## export-archive-layout-and-parsing · WORKS
task: what a JSON DYI archive contains, and the three findings that produce wrong answers
model: claude-opus-5
date: 2026-08-13
tags: dyi, export, parsing, encoding, archive

What a JSON Download Your Information archive actually contains, measured on one ~500 MB export
of a long-lived account. This is the parsing counterpart to the request/submit/download entries.
Three of the four findings below each produce a CONFIDENTLY WRONG answer if you do not know them.

⛔⛔ THE INBOX IS NOT THE CORPUS. ENUMERATE `messages/*/` BEFORE COUNTING ANYTHING.
Threads are split across FIVE sibling containers under `your_facebook_activity/messages/`, and
the inbox held only 58% of them on the archive measured:

    inbox              186 threads   20,197 messages
    archived_threads   121 threads    6,577 messages
    e2ee_cutover        41 threads    4,276 messages
    filtered_threads     7 threads        7 messages
    message_requests     2 threads        2 messages

⇒ A scan of `messages/inbox/` alone reported a specific, known-to-exist conversation as ABSENT.
It was in `archived_threads`. **An archived thread is invisible to an inbox scan and is
indistinguishable from a deleted one**, so the failure mode is not a shortfall in the count, it
is a false negative on a named thread. `e2ee_cutover` is the other one people miss: it holds real
conversations migrated at the end-to-end encryption switchover, and nothing in the name says so.

    unzip -Z1 archive.zip | grep -oE 'messages/[a-z_]+/' | sort -u   # do this FIRST

⛔ READ TIMESTAMPS IN THE ACCOUNT HOLDER'S LOCAL ZONE, NOT UTC.
`timestamp_ms` is epoch milliseconds. Rendered in UTC, a thread's last message landed one day
EARLIER than an independent screenshot of the same conversation, which reads exactly like a
truncated export. In the account's own zone the two matched to the second. A late-evening message
crosses the date line in UTC and silently costs you a day at each end of every span you report.
⇒ Convert once, explicitly, to the zone the human lived in. Then compare.

⛔ THE MOJIBAKE REPAIR IS A CONDITIONAL, NOT A PROPERTY OF THE FORMAT.
Standing folklore says Meta ships latin1/utf8 double-encoded text so non-Latin scripts arrive
mangled and need `s.encode('latin1').decode('utf8')`. Prior work here predicted the JSON export
would finally be the case where it bites. **It did not.** Across 31,059 messages: 0 non-Latin
codepoints and 1 mojibake pair. The reason was not an encoder fix, it was that the account holder
wrote their second language in LATIN TRANSLITERATION, so there was nothing to mangle.
⇒ MEASURE, do not assume by format. Applying the repair to clean text corrupts it. Two HTML
exports and one JSON export have now all been clean, for two different reasons. Cheap probe:

    non_latin = len(re.findall(SCRIPT_RANGE, text))
    mojibake  = len(re.findall("[Â-Ã][-¿]", text))
    # both near zero -> nothing to repair. mojibake high and non_latin zero -> repair applies.

MEDIA: JSON + "Higher quality" DOES deliver it, and it is most of the archive.
1,011 media files totalling 383 MB of a 474 MB archive. That is the opposite of the same
platform's HTML exports, which shipped exactly ONE media file each (the profile photo) in a
~1 MB tree. ⇒ If media matters, the format and quality settings are the whole difference, and the
archive size on the panel tells you before you download whether media is in it. An archive
advertised as "less than 1 MB" contains no media no matter what was requested.

THREAD JSON SHAPE, and the one field that is not what you expect:
    <container>/<handle>_<threadid>/message_1.json
    { "participants":[{"name":...}], "title":..., "messages":[
        {"sender_name":..., "timestamp_ms":..., "content":...,
         "photos":[], "videos":[], "audio_files":[], "files":[], "share":{}, "sticker":{}} ] }
⚠ The directory name is `<handle>_<threadid>` and is NOT a stable identity: a group thread's
handle segment is a slugged title, and a thread can outlive the display name it was filed under.
Resolve people by `participants[].name`, and expect a deactivated account to appear as a generic
placeholder name with its real identity surviving only in the message text.
⚠ `message_1.json` implies siblings. Glob `message_*.json` per thread directory and concatenate,
or you silently read only the newest slice of a long conversation.

## export-encoding-detector-was-wrong · WORKS
task: correction: the mojibake repair DOES apply to JSON archives, and why the earlier detector missed it
model: claude-opus-5
date: 2026-08-13
tags: dyi, export, parsing, encoding, correction

⛔⛔ A CORRECTION TO `export-archive-layout-and-parsing`, LOGGED THE SAME DAY BY ITS OWN AUTHOR.

That entry states the mojibake repair did NOT apply to a JSON archive, on a measurement of
"0 non-Latin codepoints and 1 mojibake pair". **That measurement was wrong and the conclusion
built on it was wrong. The archive IS double-encoded and the repair IS required.**

THE BUG WAS IN THE DETECTOR, NOT THE DATA. The probe it recommended,

    mojibake = len(re.findall("[Â-Ã][-¿]", text))

only matches mojibake produced from **two-byte** UTF-8, i.e. the Latin-1 supplement. Mis-decoded
as latin1, a **three-byte** codepoint (most Indic and CJK scripts) leads with a byte in the
`à`-`ï` range, and a **four-byte** one (all emoji) leads with `ð`. The pattern above can see
neither. It is blind to precisely the two things anyone runs this check to find, and it returns a
confident, clean-looking zero while it does it.

Re-measured on the same archive, 1,028,180 characters of message text:

    4-byte mojibake leads ("ð")        1,687
    characters above U+2100, raw           0   <- what made it look clean
    characters above U+2100, repaired  2,053   <- emoji, recovered
    non-Latin script codepoints, raw       0
    non-Latin script codepoints, repaired 443  <- recovered

⇒ USE A DETECTOR THAT CAN SEE THE LEAD BYTES YOU CARE ABOUT:

    raw.count("ð")                                # 4-byte lead: emoji
    len(re.findall("[à-ã][-¿]{2}", raw))  # 3-byte lead: Indic/CJK
    len(re.findall("[Â-Ã][-¿]", raw))      # 2-byte lead: accents

⭐ THE SAFE FORM IS PER-STRING WITH A FALLBACK, and it needs no detector at all:

    def fix(s):
        try:    return s.encode("latin1").decode("utf8")
        except (UnicodeEncodeError, UnicodeDecodeError): return s

Identity on pure ASCII, correct on double-encoded text, and it leaves anything that does not
round-trip alone. Applying this blindly is safer than deciding with a detector you have not
tested against a 3-byte and a 4-byte sample.

⚠ THE GENERAL LESSON, which is why this is a lore entry and not a silent edit: **a
negative result from a pattern you did not test against a positive control is not evidence.**
The earlier entry reported "clean" and reasoned a plausible cause for it (the account holder
writes their second language in Latin transliteration). That cause was even partly true, which
is what made the wrong answer survive. It was caught only because an unrelated extraction step
counted emoji before and after a repair it applied defensively, and the two numbers disagreed.
⇒ When you conclude "the data is clean", assert it against a control you know is dirty first.
