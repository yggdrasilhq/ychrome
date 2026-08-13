# instagram.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## export-via-accounts-centre · PARTIAL
task: obtain and read Instagram message/profile data; login itself not exercised
model: claude-opus-5
date: 2026-08-11
tags: export, dyi, archive, parsing, encoding

Not driven directly. This records what was established about Instagram's data while working
the shared Meta Accounts Centre and reading two Instagram DYI archives on disk. Scope stated
so the next agent knows what is measured and what is not: the export flow below is verified,
the instagram.com login is NOT.

EXPORTS GO THROUGH THE SHARED ACCOUNTS CENTRE, NOT THROUGH instagram.com.
Full flow in the `facebook.com` lore, slug `accounts-centre-export-dyi`. The profile chooser
there lists every linked profile with its platform, so an Instagram export is requested from
the same place as the Facebook one.
⛔ ONE PROFILE PER EXPORT. Facebook and Instagram cannot be covered by a single request.
⭐ That chooser is also the cheapest authoritative answer to "which Instagram accounts does
this login actually own today", which beats inferring it from archives on disk.

WHAT AN INSTAGRAM HTML EXPORT ACTUALLY CONTAINS, measured on two of them:
- Layout: `your_instagram_activity/messages/inbox/<handle>_<threadid>/message_1.html`.
- ⛔ HTML exports carry essentially NO MEDIA. Both archives held exactly ONE media file, the
  profile photo, in a ~1.1MB tree. No posts, stories, reels or shared photos came down.
  If media matters, request JSON with media quality set to higher, and expect a much larger
  archive.
- Useful identity files, all under `personal_information/` and
  `security_and_login_information/`:
    personal_information/personal_information/personal_information.html   username, name, email, DOB, private flag
    security_and_login_information/login_and_profile_creation/signup_details.html   ACCOUNT CREATION timestamp
    personal_information/personal_information/profile_changes.html        username/email change history
    connections/followers_and_following/{followers_1,following}.html
  ⭐ `signup_details.html` is the file that dates an account. It is ABSENT on older archives,
  and its absence is itself evidence that the account predates the export format.
  `profile_changes.html` can show a username set LATER than the earliest message, which means
  the account predates its own current name. Do not read a handle as an account identity.

PARSING: the HTML is a flat block sequence with a huge inline <style> prelude. A regex strip
returns the CSS as content. Use a real parser that drops <style>/<script>, then read
timestamps of the shape "Mon D, YYYY H:MM:SS am/pm".

ENCODING: these HTML exports are CLEAN UTF-8. A known-Bengali thread held 184 Bengali
codepoints and zero mojibake runs. ⛔ Do NOT apply the latin1/utf8 round-trip repair to them;
it would corrupt correct text. That repair is for the JSON exports.

## dyi-download-needs-instagram-side-session · PARTIAL
task: collect an Instagram archive: the cross-property refusal, login-with-facebook, 2FA, and the re-auth that blocks
model: claude-opus-5
date: 2026-08-13
tags: dyi, export, download, login, 2fa, totp, reauth

Collecting a finished Instagram archive. The REQUEST is made from the shared Accounts Centre and
the existing `export-via-accounts-centre` entry covers that correctly. **The DOWNLOAD is not
symmetric with the request, and that is the whole finding.**

⛔⛔ AN INSTAGRAM ARCHIVE CANNOT BE DOWNLOADED FROM THE FACEBOOK-SIDE ACCOUNTS CENTRE.
The Facebook-side panel at `accountscenter.<facebook host>/info_and_permissions/dyi/` LISTS the
Instagram archive, with its size, its expiry and a live `Download` button, exactly like the
Facebook one. Pressing it opens the file dialog, and pressing the inner Download refuses:

    "Switch to Instagram to continue
     You enabled two-factor authentication for this account. Enter Accounts Centre from
     Instagram and try again."   [Close]

⚠ So the button is not disabled and the listing is not a preview. **The refusal only appears at
the last click**, after two dialogs of apparently normal progress. Budget for it: an agent that
reads "Download" on the panel and reports the archive as reachable is reporting a control that
will refuse. This is a 2FA-conditional restriction, so an account WITHOUT two-factor may not hit
it at all; do not generalise the absence of this refusal from an unprotected test account.
⇒ The Instagram archive requires a session on `accountscenter.<instagram host>`, reached by
logging in at the Instagram property. Same Accounts Centre, same archive list, different origin.

GETTING THE INSTAGRAM-SIDE SESSION, on a cold instagram.com jar:
    ctl goto url=https://accountscenter.<instagram host>/info_and_permissions/dyi/
redirects to `instagram.com/accounts/login/?next=...`, which is the correct entry point.

SELECTORS on that login form, by NAME (they do not match the visible labels):
    identifier : input[name=email]     (labelled "Mobile number, username or email")
    password   : input[name=pass]
    submit     : a DIV[role=button] with the text "Log in"
⚠ `ctl fill` reports its own target names, and they were `email`/`pass`. A readback written
against the guessed `input[name=username]` / `input[name=password]` returns undefined and looks
like a failed fill. **Read back using the names the fill reply names**, not the ones you assumed.

⭐ "LOG IN WITH FACEBOOK" IS THE RELIABLE ROUTE WHEN THE PROPERTIES ARE LINKED, and it needs no
Instagram password at all. With a live Facebook session in the same profile jar, clicking it went
straight to the Instagram two-factor challenge with the account already resolved. That is one
click and zero credential guesses, versus a login form whose stored password may have rotated.

⛔ A `key: Return` ON THE PASSWORD FIELD DOES SUBMIT, BUT THE PAGE GIVES NO IMMEDIATE SIGN.
The form still showed both values and no error, so the natural read is "the Return did nothing"
and the natural next move is to click the button. Do NOT: the login was already in flight, and
the engine then correctly refused the click with

    target_moved (the resolved point no longer hits "[data-agent-target]" - `svg` is what
    receives a click there)

That `svg` is the button's SPINNER. ⇒ Before re-submitting anything, ask the button whether it is
disabled and whether a spinner exists. `aria-disabled="true"` plus a loading node means IN
PROGRESS, and a second submit risks a duplicate attempt against a 2FA-protected account:

    b.getAttribute("aria-disabled")                                   // "true" while working
    document.querySelectorAll("[data-visualcompletion=loading-state]").length

TWO-FACTOR: the authenticator route works and needs no human.
The challenge reads "Go to your authentication app ... Enter the 6-digit code", with one bare
`input[type=text]`, a "Trust this device and skip this step from now on" checkbox, and Continue.
A TOTP read from the vault entry for this host cleared it first try. Expand the code inside the
argument so it never reaches a transcript; the `type` event's `grew_by` is a real readback.
⚠ THE TRUST CHECKBOX DID NOT TAKE. `ctl input` dispatches three events per click, and on this
checkbox that landed back at `checked:false`. Read it back and re-click if you want the device
remembered; treat an untrusted device as the default outcome.

⛔ THE DOWNLOAD RE-AUTH WANTS THE INSTAGRAM ACCOUNT'S OWN PASSWORD, EVEN WHEN YOU LOGGED IN VIA
FACEBOOK. The password overlay that guards the download is NOT satisfied by the Facebook
credential that established the session; it answered "Incorrect password. Please try again."
⇒ Logging in through the linked property gets you IN, but it does not get you PAST the re-auth.
Those are two different credential checks and only the first one is satisfiable by the link.
⚠ STOP AFTER TWO FAILURES. A checkpoint on a 2FA-protected account costs the archive outright,
and the archive is on a short expiry clock. A stale stored password is a fact to report, not a
thing to brute-force through the other entries for the same host.

⭐ A VAULT ENTRY CAN BE HALF RIGHT, AND THE HALVES FAIL DIFFERENTLY. The entry used here had a
STALE PASSWORD but a CORRECT TOTP SEED and correct backup codes. The TOTP cleared a live
challenge minutes after the password was rejected. ⇒ A rejected password does not mean the wrong
entry: seeds do not rotate when passwords do, and the entry carrying the TOTP and the recovery
codes is still the authoritative account record. Report the drift against the FIELD, not the item.

⚠ Backing out safely: the archive rows carry `Download` and `Cancel` SIDE BY SIDE and Cancel
destroys the archive. To leave a refused or half-driven dialog, re-`goto` the panel URL. Never
reach for a nearby control to dismiss something. Re-read the panel afterwards and confirm the
archive is still listed, which is the only proof nothing was cancelled.

## identify-the-account-before-spending-a-credential · WORKS
task: get past the Instagram download re-auth, and establish which account you are on before guessing a password
model: claude-opus-5
date: 2026-08-14
tags: dyi, download, reauth, credential, identity, totp, archive

Collecting an Instagram archive after the request has already been made. The request side is in
`export-via-accounts-centre`; the session and two-factor side is in
`dyi-download-needs-instagram-side-session`. This entry is about the LAST gate, and about the move
that should be made BEFORE touching it.

## The asymmetry, stated plainly because it is not guessable

Both archives are listed on the same Accounts Centre panel, in the same shape, with the same
controls. They do not behave the same way at the last click:

| | Facebook-side archive | Instagram-side archive |
|---|---|---|
| download from the Facebook-side panel | works | **refused**, "Switch to Instagram to continue" |
| download from the Instagram-side panel | n/a | reaches the file dialog |
| password re-auth at the final Download | **none** | **always** |

⇒ An agent that has just downloaded the Facebook archive with no credential prompt will assume the
Instagram one behaves the same way. It does not. Budget a credential for it, and read the next
section before spending one.

## ⭐ LOOK AT THE RE-AUTH BEFORE YOU TYPE INTO IT. The read costs nothing.

The overlay reads *"Please re-enter your password"* and, measured, contains exactly:

    one input[type=password]      (no name, no placeholder, no aria-label)
    a Continue button
    a "Forgot password?" link

**and nothing else.** In particular there is **no send-a-code-to-email alternative**, which is the
route that solves most re-auth walls on other properties without any stored secret. Enumerate the
overlay's inputs and links first:

    document.querySelectorAll("input")     // types, names, labels
    document.querySelectorAll("a")         // is there a code route at all

That one read decides whether a credential is required or merely offered, and it spends nothing. An
agent that types first learns the same thing at the cost of an attempt it cannot get back.

⛔ **STOP AT TWO FAILED ATTEMPTS.** A security checkpoint on a two-factor protected account costs
the archive outright and is not recoverable inside the expiry window. Two is the budget.

## ⭐⭐ THE MOVE THAT MAKES THE STOP RULE AFFORDABLE: date the account before guessing

When a stored password is refused, the reflex conclusion is "the entry is stale, try the next one".
That conclusion is usually **untested**, and on a host where several entries exist for the same
domain it invites exactly the brute-force the stop rule forbids.

**There is a rung-1 answer.** If any archive of this account already exists on disk, it carries its
own identity papers and they settle the question for free:

    personal_information/personal_information/personal_information.html
        -> current handle, registered email, date of birth, private flag
    personal_information/personal_information/profile_changes.html
        -> every handle and email change, with dates
    security_and_login_information/login_and_profile_creation/signup_details.html
        -> WHEN THE PROFILE WAS CREATED

`signup_details.html` is the decisive file. It is **absent from older exports**, and that absence is
itself evidence that the account predates the format.

**The inference that saved an attempt:** if the account was **created after** the credential record
was written, then the password for it was **never stored**, rather than stored and later rotated.
Those are different facts with different fixes, and only one of them is repaired by trying another
entry. Cross-check the handle: if no entry on the host names the account's handle, and the account
has never been renamed (read `profile_changes.html` to confirm), then no entry holds its password
and every further attempt is a guess.

⚠ Do not read a handle as an account identity on its own. `profile_changes.html` can show a handle
set LATER than the earliest message in the same archive, which means the account predates its own
current name.

## ⭐ A credential record binds to an account by its TOTP SEED, not by its username field

This is the reason a refused password must not be read as "wrong record". Seeds do not rotate when
passwords do. An entry whose stored TOTP clears a **live** two-factor challenge on the account is
that account's record, even when:

- its username field names something the account no longer uses, and
- its password is refused at the same session, minutes apart.

⇒ Report the drift against the **field**, not the item. Saying "this entry is wrong" sends the next
session hunting for a better entry that does not exist; saying "this entry's password field is
empty of anything current, its seed is correct" tells them exactly what to fix.

## The profile chooser lists more accounts than the two you came for

The Accounts Centre change-password chooser enumerates every profile the login administers, and on
a linked pair that is **three** rows, not two: the per-app profile for each property, plus a
**platform-level account** that owns them both. That third account is easy to miss because no
per-app surface ever mentions it, and it may have no credential record anywhere.
⭐ This chooser is also the cheapest authoritative answer to "which profiles does this login
actually own today", which beats inferring it from archives on disk.

## What is NOT lost when this gate holds

The archive expiry kills the built file, not the data. With the session established, the account's
own message list is readable directly, and a fresh export can be requested at any time.
⇒ **Do not reset a live credential to beat the clock.** "Forgot password?" is sitting right there in
the overlay and it is the wrong door: it changes a working credential, may sign the owner's other
devices out, and buys a file that could have been re-requested for free.

## direct-api-read-and-the-limit-100-truncation · WORKS
task: read the whole DM corpus from a logged-in session when the DYI archive is gated, and the silent truncation that nearly halved it
model: claude-opus-5
date: 2026-08-14
tags: direct, api, messages, pagination, truncation, encoding, session

When a DYI archive is gated behind a credential you do not have, **the logged-in session can still
give you the message corpus**, through the site's own private API rather than its DOM. This is rung 1
work dressed as rung 3: no scrolling, no virtualised list, no screenshots.

## The route

The web app's own API answers a same-origin `fetch` from inside the page, because the session cookies
ride along automatically. It needs exactly one header.

    // the app id is in the page's own HTML; scrape it rather than hardcoding
    document.documentElement.innerHTML.match(/"X-IG-App-ID":"(\d+)"/)

    GET /api/v1/direct_v2/inbox/?persistentBadging=true&thread_message_limit=20&limit=25
    GET /api/v1/direct_v2/threads/<thread_id>/?limit=20[&cursor=<oldest_cursor>]
        headers: { "X-IG-App-ID": "<the id from the page>" }

The inbox reply carries `viewer.pk` (your own user id, which is how you tell your messages from
theirs) and one entry per thread. Page a thread with `oldest_cursor` while `has_older` is true.

⚠ **Take the thread id as a STRING.** The numeric `thread_id` exceeds JavaScript's safe integer
range, so `JSON.stringify` of it silently collapses distinct threads to the same value and every
thread looks identical. Use `thread_v2_id`, or `String(x.thread_id)` taken before any serialisation.
This is not a subtle failure once you see it, and it is invisible until you notice every row printing
the same id.

⚠ An engine `eval` cannot return a promise. Kick the async work off, stash the result on `window`,
poll with `wait`/`eval` until it is set, then read it out in slices.

## ⛔⛔ THE FINDING: `limit=100` SILENTLY TRUNCATES AT 75 AND THEN LIES ABOUT IT

Measured on two independent threads, and this is the whole reason this entry exists.

    GET .../threads/<id>/?limit=100
      -> 75 items, has_older: FALSE, oldest_cursor present
    GET .../threads/<id>/?limit=20   (paged on oldest_cursor)
      -> 273 items across 14 pages

**The same thread is 75 messages long or 273 messages long depending on the page size you asked
for.** The large request does not error, does not warn, and does not set `has_older`. It returns a
plausible number and declares the thread finished.

⇒ Across a twelve-thread mailbox the first extraction reported **231 messages** and the corrected one
**473**. A run that trusted `limit=100` would have written an archive **missing 51% of the corpus,
with every internal consistency check passing**, because nothing inside the result is wrong. Only its
completeness is.

⭐ **What caught it was not suspicion of the API.** It was that **two independent threads both came
back at exactly 75**. A round number appearing twice is the kind of coincidence worth one probe, and
the probe is cheap: re-request the same thread at a smaller page size and compare totals.

⇒ **PAGE AT `limit=20` AND VERIFY BY RE-COUNTING AT A DIFFERENT PAGE SIZE.** Treat `has_older: false`
as advisory rather than authoritative on this endpoint. The general law it belongs to: **a pagination
flag is a claim by the server, and a server that truncates has already demonstrated it will make
false claims.**

⚠ Pace the requests. 400 to 600 ms between pages and threads was accepted without a single non-200
across ~30 requests; there is no reason to find the rate limit.

## Encoding, which differs from the export path

⭐ **The live API returns clean UTF-8.** A mojibake scan over 473 messages found **zero** mangled
runs. ⛔ **Do NOT apply the latin1/utf8 round-trip repair to API output** — that repair is an artefact
of the DYI *export* pipeline, and applying it here corrupts correct text. Same rule as the HTML
exports.

## Timestamps

Message timestamps are **microseconds**, not milliseconds. Branch on magnitude rather than assuming,
and render in the account's own local zone: rendering as UTC from a positive-offset corpus moves
every boundary message backwards and can delete the last day of a span.

## When this is the right tool

- The archive is gated, expired, or still building, and the content is wanted now.
- You need a diff against an older export rather than the whole history.
- ⛔ **It is not a replacement for an export.** It returns the account's CURRENT state: nothing
  deleted, nothing from a disabled account beyond what the thread still holds, and no media files.
  A thread whose counterparty account no longer exists comes back with an **empty user list**, so it
  has no name and can only be identified by its thread id.
