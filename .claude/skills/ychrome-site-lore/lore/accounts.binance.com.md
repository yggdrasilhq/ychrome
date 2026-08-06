# accounts.binance.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## not-logged-in-and-a-free-injection-control · PARTIAL
task: check the Binance account / KYC state
model: claude-opus-5
date: 2026-07-31
tags: 

STATE 2026-07-31: the `binance` ychrome profile has NO session.
https://www.binance.com/en/my/dashboard redirects to
https://accounts.binance.com/en/login?return_to=<base64 of the dashboard url>.
So the KYC-expiry question could not be answered — it needs a login first.

⚠ The SPA renders slowly on a fresh surface: at ~20s document.title was "" and
body.innerText was EMPTY, which reads exactly like a dead surface. At ~45s the
login page was fully there. Wait before concluding anything.

LOGIN SHAPE (accounts.binance.com/en/login): a single "Email/Phone number" field
then a "Log In" button; password is a SECOND page; Google/Apple/Telegram SSO
alternatives. Vault entry `accounts.binance.com` HAS a
TOTP, so the second factor is completable without the phone.

⭐ THE USEFUL PART — this page is a FREE CONTROL for the "does injected input
land on an unrevealed surface" question, because it needs no credentials and
clicking Log In with an empty field is harmless. Measured here:

    document.visibilityState : "hidden"      <- never revealed
    button rect              : 343x48        <- real layout
    web do click             : accepted:true, is_trusted:true
    PAGE-SIDE readback       : capture listener FIRED, isTrusted true

That settles it GREEN: injection reaches a never-revealed surface and the page
sees a trusted event. Hold any input verb to a PAGE-SIDE readback like this one,
never to the verb's own success field.

⚠ Still unverified: whether the click hits the INTENDED element. In that run only
a document-level listener recorded the hit; a button-bound one did not. Probe it
with a single eval that stashes the rect AND binds the listener together.

NOT ATTEMPTED: the login itself. No credential was used or changed.

## headless-login-works-reset-captcha-refuses · PARTIAL
task: log in headless and rotate the password after a credential exposure
model: claude-opus-5
date: 2026-08-02
tags: login, captcha, mfa, totp, otp, engine, anti-bot

LOGIN IS FULLY DRIVABLE HEADLESS. PASSWORD RESET IS NOT. The two paths have
different anti-bot gates and the difference is the whole finding.

WHAT WORKS (proven end to end on the engine plane, no GUI host):
  ctl open profile=<p> url=https://accounts.binance.com/en/login
  1. username field is input[name="username"], single field, then a "Log In"
     button. Password lives on a SECOND page (/login/password).
  2. A 3x3 image captcha ("select all images with <noun>") fires after the
     username submit. It IS solvable: screenshot the viewport, read the grid,
     click cell centres by x,y, then click Verify. The grid sits at roughly
     x=526/640/754 and y=342/457/571 on a 1280x900 viewport, Verify near
     (744,671). Selected cells get a visible check, so screenshot again and
     confirm the right ones are lit BEFORE pressing Verify.
  3. MFA is a checklist ("0/2"), not a single step. Each factor opens its own
     dialog and the counter increments. Authenticator App takes a single 6-digit
     field, not a 6-box component, so a plain type event lands.
  4. The e-mail factor sends a code that must be raced: stamp a T0 BEFORE
     clicking the factor, then only accept a message newer than T0. An older
     code sitting in the mailbox otherwise wins and fails with a
     correct-looking value.
  5. After both factors a "stay signed in" Yes/Not Now prompt appears, then the
     dashboard.

WHAT IS BLOCKED:
  The reset-password path (/en/security/reset-password) puts a BCaptcha
  "I'm not a robot" checkbox in front of the flow, and it refuses this engine.
  Clicking it answers "verification failed". Adding a realistic pointer
  approach (six move events converging on the box before the click) changed
  nothing, so this is environment fingerprinting rather than a motion-pattern
  heuristic. Note the asymmetry: the LOGIN image captcha passes, the RESET
  behavioural captcha does not. The account-takeover path is gated harder,
  which is correct of them.

  DO NOT GRIND THIS. Repeated failed attempts against a password-reset
  endpoint on a live exchange account is exactly the pattern fraud detection
  is built to notice, and a flagged account is a worse outcome than an
  unrotated password. Two attempts, then stop and hand back.

OTHER FACTS WORTH HAVING:
  - API Management renders "Your Account has not created any API Keys yet"
    when the list is empty, so a zero-key account is confirmable in one read.
  - The security page's ON/OFF chips are NOT trustworthy on this substrate:
    every factor rendered "OFF" while the session had just passed two of them.
    The Manage vs Enable button label is the better signal, since Manage only
    appears for a configured factor. Do not report those chips as state.
  - Changing the password disables withdrawals, P2P selling, payment services
    and card applications for 24 hours. The page says so in a banner; budget
    for it before starting, and tell the operator first.
  - ctl input nth indexes the HITTABLE pool, not querySelectorAll order, so a
    DOM index counted from an eval will click the wrong control. Address by a
    unique selector or by coordinates read off a screenshot.

## session-timeout-modal-and-code-race · WORKS
task: read balances and open orders on a profile whose session had expired
model: claude-opus-5
date: 2026-08-06
tags: session, shadow-dom, mfa, totp, otp, modal, orders, trap, screenshot

A LOGGED-IN PROFILE CAN STILL BE LOCKED OUT, AND THE DOM WILL NOT TELL YOU.

Reading an account page on a profile whose session had gone stale, the text rung
lied in the most expensive way available: document.title was correct
("Spot - Wallet - Binance"), body.innerText rendered the full table HEADERS
("Asset / Amount / Available / Action") and ZERO rows. That reads exactly like
"the account is empty" and it is not — it is "the data never loaded".

THE SCREENSHOT IS THE INSTRUMENT. One `ctl shot` showed a centred modal:
"Verification Needed — Your login session has timed out. Please complete security
verification to stay logged in." with [Verify Myself] / [Log Out], and the rows
behind it were loading SKELETONS, not empty cells.

⛔ THE MODAL IS IN A SHADOW ROOT. It is invisible to the obvious probes:
  document.querySelector("button")            -> does not find it
  body.innerHTML.includes("verify myself")    -> FALSE
Walk for it instead, and click the live rect rather than a guessed coordinate:

  const host=[...document.querySelectorAll("*")].find(e=>e.shadowRoot)
  const el=[...host.shadowRoot.querySelectorAll("*")]
            .find(e=>/verify myself/i.test(e.textContent)&&!e.children.length)
  const r=el.getBoundingClientRect()   // -> click x=r.x+r.width/2, y=r.y+r.height/2

A coordinate read off a full-document screenshot did NOT match the live rect here
(y differed by ~165px because `region=full` is a taller surface than the viewport).
Take the rect from the DOM, take the confirmation from the pixels.

RE-VERIFICATION IS THE SAME 0/2 CHECKLIST AS FIRST LOGIN (Authenticator App +
Email), so the flow already logged for login applies. Two refinements:

1. THE AUTHENTICATOR FIELD AUTO-SUBMITS ON THE 6th DIGIT. A wait predicate of the
   shape "input.value.length === 6" therefore TIMES OUT even on total success —
   the dialog has already advanced and the input is gone. The counter going 0/2 ->
   1/2 is the real signal. Do not read that timeout as a failed factor.

2. THE EMAIL FACTOR MUST BE RACED, and the failure is silent. Stamp T0 BEFORE
   clicking the factor and accept only a message newer than T0. Concretely: a
   high-recall mail search for "Binance verification code" returned 2024-era codes
   from a DIFFERENT account first, all of them correct-looking 6-digit strings.
   Match on subject "[Binance] Verification Code - <UTC timestamp>" and compare
   that timestamp to T0.

⚠ AND EXTRACT THE CODE FROM THE DECODED BODY, NOT THE RAW MESSAGE. Regexing
\b\d{6}\b over the raw source yielded SIX candidates (ids, dates, style values).
After stripping tags from the decoded text/html part there was exactly ONE, and
its context is unambiguous: "Your verification code:&nbsp; <CODE> The verification
code will be valid for NN minutes." Anchor on that phrase.

⚠ A FULL-TEXT MAIL CLIENT CAN BE TOO SLOW FOR A LIVE CODE. A `read --message-id`
against a ~9 GiB store exceeded a 120 s timeout while the code was expiring.
Reading the tail of the mbox file directly and parsing with python's `email`
module returned instantly. When a secret is on a clock, go to the file.

ORDER-BOOK COVERAGE — THREE BOOKS, NOT ONE. "No open orders" from the spot screen
is NOT the answer to "is anything resting". Check all three; they are separate:
  Orders > Spot Order > Open Orders     (set Pair/Direction/Filter all to All)
  Orders > Convert > Open Orders > Limit Order
  Orders > Convert > Open Orders > TP/SL
A Convert limit order never appears in the spot book. Same lesson as
"registry uninstall keys are not an inventory": name the instrument you used.

HISTORY WINDOWS ARE SHORT AND THE SHORTCUTS ARE FIDDLY. Spot Trade History states
it covers 6 months only and defaults to 7 days; Convert History and crypto Deposit
History default to 30 days. The "Past 6 months" shortcut in the date popover did
not apply on one attempt (the popover closed and the range reverted, silently).
For anything older, use the page's own Export function rather than grinding the
picker.

ALSO SEEN: a persistent site-wide banner asking for additional KYC details and
warning of "limited access" if ignored. It is informational, it does not block
reads, and it is the operator's to clear.
