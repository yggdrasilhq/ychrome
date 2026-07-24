# eportal.incometax.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## scripted-login-pan-password-aadhaar-otp · WORKS
task: log in and read AIS/TIS for a family member
model: claude-opus-4-8
date: 2026-07-24
tags: 

Full login is scriptable end to end, OTP included — no human in the loop. Flow:
`#/login` (PAN in `#panAdhaarUserId`) -> Continue -> `#/login/password` (tick
`#passwordCheckBox-input`, fill `#loginPasswordField`) -> Continue -> **always**
`#/login/otpOptions` (password alone never logs you in) -> pick the second radio
`#mat-radio-1-input` ("Generate OTP") -> Continue -> `#/login/adhaarOtp` -> tick
the consent box -> "Generate Aadhaar OTP" -> six boxes `#otp_<0..5>_<nonce>` ->
"Login". Credentials live in the vault under this domain, one entry per family
member; the entry's notes also carry the **secure access message** — compare it
with the one the password page shows before typing anything, it is the site's
own anti-phishing check and it matches exactly.

The Aadhaar OTP goes to the Aadhaar-registered mobile. Sender id varies
(`BH-ADHAAR-G`, `BZ-ADHAAR-G`); body is `NNNNNN is OTP for Aadhaar (XXNNNN)
(valid for 10 mins) at Dir of Income Tax.` Match on `OTP for Aadhaar`, not on the
sender. Reading it off the phone is a separate recipe — see the data-fabric
skill's KDE Connect section.

**Traps, each one measured:**

1. **Clicking by coordinates is unreliable here.** The page has a top news ticker
   that changes height, so a rect measured seconds earlier lands on the wrong
   control — a click meant for "Continue" hit "Back" (which calls
   `goToFoPortal()` and throws you out to the public portal). That bounce looks
   exactly like a rejected login but no API call is ever made. Prefer
   `element.click()` in an eval for form buttons; measure and click in the same
   instant if you must use coordinates.
2. **`location.hash = ...` trips the portal's own back/refresh guard** and pops a
   "Are you sure you want to Logout?" modal that then swallows every later click
   (`document.elementFromPoint` returns the modal overlay, not your target).
   Dismiss with the modal's "No" — never "YES" — and navigate by clicking real
   in-app links instead.
3. **`automation-validator.min.js` is a red herring.** It is real, obfuscated, and
   titled "Block the usage of automation tool" — but the whole body is gated on
   `navigator.webdriver`, which WebKitGTK leaves false, and its failure mode is a
   "Permission Denied!!" page, not a redirect. Do not spend time on it.
4. **The AIS hand-off is a cross-origin form POST, not a link.** See the
   `ais.insight.gov.in` lore.
5. **"Dual Login Detected"** appears when a previous session is still registered
   (e.g. after a GUI swap killed the surface). It is a modal with a "Login Here"
   button — click it to take the session over.
6. Session idles out in ~15 min; the header shows the countdown. Logging out is a
   profile-menu "Logout" and lands on a feedback page.
7. The login page's `#panAdhaarUserId` field is re-created on route entry — type
   into it only after the route has settled, or the value lands nowhere and
   Continue silently does nothing.
