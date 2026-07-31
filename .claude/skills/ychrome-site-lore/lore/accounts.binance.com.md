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
alternatives. Vault entry `accounts.binance.com` (user the account named in the vault entry) HAS a
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
