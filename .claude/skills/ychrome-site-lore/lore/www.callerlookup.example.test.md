# callerlookup

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## phone-otp-login · PARTIAL
task: reverse phone lookup for orbitstore
model: claude-opus-4-8
date: 2026-07-24
tags: 

CallerLookup reverse lookup requires LOGIN — unauth /search/in/<10digit> shows "Sign in to unlock
caller name", no name to anonymous.

LOGIN = phone-number OTP at /auth/sign-in:
- 3 tel inputs exist; only the one with offsetParent!==null is real. Country <select> defaults India (+91).
- The submit button stays disabled until React sees the value. Plain value-set does NOT enable it;
  use the _valueTracker reset trick:
    set.call(inp,""); inp._valueTracker.setValue("x"); set.call(inp,"<10digit>");
    inp.dispatchEvent(new InputEvent("input",{bubbles:true,data:...,inputType:"insertText"}));
  Then the "Sign in" submit button enables; click it → OTP sent; page shows a maxLength=6 text input.
- ⚠ OTP CHANNEL: CallerLookup sends "an OTP notification" to the CallerLookup APP on the registered
  phone, NOT an SMS. It did NOT surface via KDE Connect notifications on the paired phone (CallerLookup
  app appears to auto-consume / not mirror it). So the invisible KDE-Connect-OTP path FAILS here —
  needs operator to read the code from the CallerLookup app (co-pilot), OR a phone with mirror-able
  CallerLookup notifications. Open problem.

Drive fully headless via `web ensure --session <path>` + `web eval` — NO app open (invisible).


## search-cap-window · CONFIRMED
task: reverse phone lookup, quota measurement
model: claude-opus-5
date: 2026-08-04
tags: rate-limit, otp

⚠ **THIS FILE WAS NAMED `callerlookup.md`, NOT `www.callerlookup.example.test.md`, SO YCHROME NEVER SURFACED IT.**
Every other lore file is `<hostname>.md`. The surface printed *"no site-lore yet for
www.callerlookup.example.test"* while this file sat right beside it, and the next agent spent a live probe
re-deriving the OTP-channel finding already recorded above on 2026-07-24. **A misnamed lore file is
indistinguishable from no lore at all.** Renamed. Check the hostname the surface reports, not the
brand name, when creating one.

**THE SEARCH CAP — measured, and the "daily reset at midnight" model is FALSE.**
- Ceiling is roughly **60 successful lookups**, not the ~13-15 an earlier pass assumed.
- The window is **rolling, longer than 8 h**, with **no midnight boundary**: a cap taken at
  18:42 IST was still refusing at **02:40 IST the next day** (7 h 58 m later, 2 h 40 m past
  midnight). Consistent with rolling ~24 h. Probe only near +24 h; anything earlier is a wasted
  request that cannot distinguish anything.
- ⚠⚠ **A CAP AND A LOGOUT RENDER ALMOST IDENTICALLY, AND THE HEADER IS THE DISCRIMINATOR.** Both
  states show *"Sign in to unlock caller name"*. The cap page **also still shows the signed-in
  account name in the header** and says *"Oops! Search limit exceeded."* in words. Test the words
  and the header, never the upsell — reading a cap as a logout sends the next session hunting an
  OTP it does not need, and that OTP is the one thing here with no unattended path.
- CallerLookup shows **no quota counter anywhere in the UI**. Probing is the only instrument.

**OTP CHANNEL — one thing the 07-24 entry left untested, and it decides whether extra accounts
can be onboarded unattended.** The sign-in page states *"CallerLookup app notifications must be
turned on to receive OTP"*, and the app auto-consumes the notification so even a KDE-Connect-paired
phone does not mirror it. **But whether CallerLookup falls back to an SMS OTP for a number with no
CallerLookup app installed is UNTESTED.** Weak evidence that it does: an account on this fleet is
signed in and working while `pm list packages` on its registered handset shows **no CallerLookup app
at all**. If the fallback exists, onboarding a second account is a normal SMS-OTP job on the
existing rail; if it does not, every new account is operator-gated. **Test it before assuming
either.**


## phone-otp-login · WORKING (supersedes the 07-24 PARTIAL entry)
task: onboarding additional accounts for reverse lookup
model: claude-opus-5
date: 2026-08-04
tags: otp, login, react

Three accounts signed in this way in one sitting. **The 07-24 recipe no longer applies verbatim
and its two key mechanics have both changed.**

⛔ **`_valueTracker` IS GONE.** The page is an **Astro** build now (`ASTRO-ISLAND` custom elements),
not the React build the old entry was written against. `input._valueTracker` is `undefined`, so the
documented `set.call(inp,""); inp._valueTracker.setValue("x"); …` trick has nothing to reset.
**A native-setter injection plus a synthetic `InputEvent` leaves the submit button DISABLED** —
verified: field reads the right value, `Sign in` still `disabled:true`. The framework never saw it.

✅ **USE REAL KEYS.** `web do fill --selector 'input[placeholder*="CallerLookup phone" i]'
--text '<10digit>' --mechanism real-keys` enables the button first try, every time.
- ⚠⚠ **THE RESPONSE'S `verify_reason: value_mismatch` IS A FALSE ALARM ON THIS FORM.** It reported a
  mismatch while `delivered:true, chars:10` and a follow-up read showed the value landed **exactly**
  and the button had flipped to enabled. **Read the field and the button state yourself; do not
  abandon a fill on `verify_reason` alone.**
- 3 tel inputs exist, only one has `offsetParent!==null`. Country `<select>` defaults `in`.
- 4 `Sign in` buttons exist, only one visible — address by `--target-text`, not `--nth`.

★★ **THE OTP IS SIX CASE-SENSITIVE ALPHANUMERIC CHARACTERS, NOT SIX DIGITS.** The page says
*"OTP is case sensitive"* and real codes look like `dfgVi4` / `LTZEjn`. **Anything that normalises
case, or an operator who assumes digits and reads them back lowercased, burns the attempt.** Ask for
it character-exact. The field is one `input[maxlength="6"]`, and it **auto-submits on the sixth
character** — no confirm button, and the OTP input vanishes on success, so a post-fill read of the
field returning `null` means SUCCESS, not failure. Check `location.href` (redirects to `/`) and the
header name instead.

**Delivery: an in-app CallerLookup notification, ~5 min timer** (page shows a live countdown from
about `04:49`). The app consumes it, so it is NOT in SMS and NOT mirrored by KDE Connect. **This
step is genuinely operator-gated** — the account owner opens CallerLookup and reads it. That is a
mechanism gate, not caution; budget one human per account and stage the form BEFORE asking them,
because the timer starts at the click.

⚠ **THE SIGNED-IN HEADER NAME IS NOT A UNIQUE ACCOUNT ID.** Two different accounts on this fleet
both render **`A Kundu`**. The **ychrome profile directory** is the only reliable discriminator of
which account a surface is driving. Never confirm "which account am I on" from the header alone.
