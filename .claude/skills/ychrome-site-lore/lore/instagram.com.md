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
