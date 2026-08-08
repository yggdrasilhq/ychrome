# pay.google.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## close-payments-profile-popup-reauth-wall · BLOCKED
task: close a stale Google payments profile from the payments centre
model: claude-opus-5
date: 2026-08-08
tags: google, payments, reauth, popup, profile-close

Closing a Google **payments profile** from the payments centre, driven on an unrevealed
yggterm web surface, 2026-08-08. Everything up to the last click works headlessly. The last
click does not, and the operator finished it in Chromium.

## ⛔ THE WALL: `Close payments profile` needs a POPUP re-auth, and the parent never resumes

`Settings → Payments profile status → Close payments profile` is
`<a class="O7FmUb" href="" jsaction="click:RxEnNd" jsname="hSRGPd">` — no href, JS only.
Clicking it renders a small dialog, *"To continue, please verify it's you. A new window will
appear for you to sign in"*, and calls `window.open()` on an `accounts.google.com` password
challenge.

Measured four times, identically:

1. the popup opens as a NEW TAB of the same surface and becomes the ACTIVE tab, so every
   `web *` verb addresses it with no extra flag;
2. `web fill --entry accounts.google.com --user <acct>` lands the password —
   **page-side `input[type=password].value.length` = 40**, every time;
3. the submit succeeds: the popup reaches `https://accounts.google.com/CheckCookie`.
   ⇒ **the re-auth is NOT the problem. It passes.**
4. the popup closes, and the opener is back on `/gp/w/home/settings` with `[role=dialog]`
   empty and **every injected global gone** — the parent document was replaced.

**`window.opener` is wired, so that is not the cause.** Proven on this very surface:
`window.open('https://pay.google.com/gp/w/home/activity','testpop')` then, in the popup,
`{hasOpener:true, openerOrigin:"https://pay.google.com"}`.

**What it probably is:** an unrevealed surface is `visibilityState:'hidden'` with no rAF, and
the opener gets **no `focus` and no `visibilitychange`** when the popup closes, so a
continuation gated on those never runs. Shimming `visibilityState`/`hidden`/`hasFocus`/rAF in
the opener made the flow's own dialog lay out larger (448×80 → 560×80), so the frame clock WAS
affecting it — but the flow still did not resume, and **the parent's navigation eats the shim.**
⇒ do not spend a session re-deriving this page-side; it is an engine gap, filed as
`yggterm/docs/agent-cobrowse-gaps-2026-08-08.md`.

**⇒ ROUTE THIS TASK TO A NORMAL BROWSER.** the GUI host carries `/usr/bin/chromium` and `/usr/bin/firefox`;
the operator closed the profile in Chromium in minutes after the agent plane failed four times.

## ✅ What DOES work headlessly here, and is worth reusing

**The view-scope re-auth, start to finish, no owner:** landing on
`pay.google.com/gp/w/home/settings` when the session is cold gives
`/gp/w/home/reauthprompt?rtrp=/home/settings`.

    web do click --text "Verify it's you" --new-batch     -> /v3/signin/challenge/pk (passkey default)
    web do click --text "More ways to verify"             -> /challenge/selection
    web do click --text "Get a verification code from the Google Authenticator app"
    web totp --entry accounts.google.com --user <acct>    -> page-side #totpPin length 6
    web do click --text "Next" --exact --new-batch        -> back on /gp/w/home/settings

⚠ The passkey lane auto-arms itself ("Verifying it's you… Complete sign-in using your passkey")
and ends at the GUI presence dialog, which an agent cannot answer. Go straight to
*More ways to verify*.

**SPA navigation:** the left-nav items are real anchors (`<a href="./home/paymentmethods">`), but
`el.click()` is unreliable — the full pointer sequence (`rclick` via `web await`) on the anchor
and then its parent drives `Activity`, `Payment methods`, `Subscriptions & services`, `Addresses`
and `Settings` reliably.

**Reading which profile owns what, before touching anything** — all four pages answer plainly:
`Payment methods` (empty = "Payment methods in Google Wallet" and nothing else), `Subscriptions &
services` ("No subscriptions yet"), `Activity` (empty), `Addresses`.

**Both payments-profile IDs are in the DOM of any payments-centre page**, inside the (hidden)
switcher `<li role="option" jsname="n1UuX">` rows, as
`<span class="Jcjdrc">Profile ID: NNNN-NNNN-NNNN</span>` beside a country and a name. Regex the
`innerHTML` for `\d{4}-\d{4}-\d{4}` — far cheaper than opening the switcher.

## ⚠ TWO TRAPS THAT COST 20 MINUTES EACH

**1. After the popup closes, the ACTIVE tab is a `no_webview` GHOST and every verb lies.**

    web eval … -> {"accepted":false,"reason":"web surface not live (session backgrounded or not yet revealed)"}

The page is fine. `server app state | .web_surface_tabs.rows` shows `tab 0 stashed webview:true
active:false` next to `tab 1 no_webview active:TRUE`. No `web` verb takes `--tab`.
**✅ RECOVERY: `yggterm server app web close --session <s>` closes the GHOST and re-activates the
real tab.** Do that instead of killing and relaunching ychrome — a relaunch loses nothing but
costs a re-auth, and it leaves ANOTHER dead `no_webview` row behind each time.
⚠ In that state `web ensure` answers `"page was unresponsive; a rebuild is queued — re-run ensure
and compare generation_after"` with both generations `null`, three times running, and no rebuild
ever comes. It is not a signal.

**2. The agent's own `web do click` becomes `seat_input_on_unrevealed_surface` against itself.**

    web fill-vault … -> {"accepted":false,"reason":"seat_input_on_unrevealed_surface","seat_input_count":1,
                         "detail":"…Reveal the session (open its row) before driving it…"}

`seat_input_count: 1` was this agent's own click, one call earlier. **Do not follow the advice to
reveal** — drive page-side with `rclick` through `web await` instead. `web fill --entry` and
`web totp` were still accepted while `do`/`fill-vault` were refused, so the lane is per-verb, not
per-surface-forever.

## The safety check to run BEFORE closing any payments profile

The profile being closed here was a stale second profile in another country. It held **no payment
methods, no subscriptions and no activity** — measured on all three of its own pages — so closing
it could not disturb the live subscriptions and the primary card, which hung off the OTHER
profile on the same account. That is the check worth repeating every time: **open Payment
methods, Subscriptions & services and Activity UNDER THE PROFILE you are about to close, not at
the account level.** A subscription that is cancelled-but-still-valid still holds its card, and a
profile that owns one cannot be cleanly closed.
