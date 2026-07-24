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

## file-itr1-end-to-end-with-everify · WORKS
task: file and e-verify an ITR-1 through the surface
model: claude-opus-4-8
date: 2026-07-24
tags: 

A full ITR-1 was filed and e-verified end to end through the surface (AY 2026-27,
nil tax, refund case). The path:

`File Now` → AY + `Online` → `Start New Filing` → status `Individual` → `Proceed`
→ ITR form select (`#select_itr_form`) → `Proceed With ITR - 1` → `Let's Get
Started` → filing-reason questionnaire (`#radio-input` = "taxable income is more
than basic exemption limit", usually pre-selected) → **Return Summary** with five
cards, each needing its own `Confirm`: Personal Information, Gross Total Income,
Total Deductions, Tax Paid, Tax Liability → `Proceed To Verification` → tax
summary (states the refund) → `Preview And Submit` → declaration (tick the
`#ConfirmVerificationDetails` box, fill `Verification.Place`) → `Proceed To
Validation` → `Preview` → `Proceed To Validation` again (upload-level) →
`Proceed To Verification` → `E-Verify Now` → "OTP on mobile registered with
Aadhaar" → consent + `Generate Aadhaar OTP` → six `#otp_<i>_<nonce>` boxes →
`Validate` → **`Submit` in the "Confirm Submission of Return" modal** → `Proceed`
→ "You have successfully filed and verified your return!".

**Traps, all paid for:**

1. **The modal stack is the main hazard.** Entering a section pops up to three
   overlapping notices (prefill / new-regime / "informative purposes only"). They
   need REAL clicks — `element.click()` does nothing on them. Measure the button
   with `getBoundingClientRect()` and use `web do click --x --y`, then SCREENSHOT
   to confirm it actually went away.
2. **Never dismiss modals by matching button text.** A "Continue" that looks like
   a modal button belonged to the hidden *Help me decide which ITR Form* wizard;
   clicking it navigated out of the return, and the flow could not simply be
   walked back — the summary stopped rendering and the draft was gone (nothing is
   saved until the first section is confirmed, so `Resume Filing` stays disabled).
   On that disclaimer modal, **`Cancel` is the safe answer**, not `Continue`.
3. **Editing a confirmed section un-confirms every section after it.** Fix one
   thing in Gross Total Income and Deductions / Tax Paid / Tax Liability all
   revert to "Provide your confirmation". Re-confirm in order.
4. **Validation error `Description — Minimum 1 characters are required` names no
   schedule.** It is the other-sources "Any Other" row: select the row's
   checkbox (`…OthersIncDtlsOthSrc.<n>_cb`), click its `Edit`, and the nature
   `Any Other` reveals a **textarea** (`…OthSrcOthNatOfInc`) that must be filled.
   It is a `textarea`, not an `input` — an `input[id$=…]` selector silently
   matches nothing.
5. **Personal Information will not confirm** until "Is the secondary address same
   as primary address?" is answered. The control is not on that page: open the
   Contact card's `Edit` (`#Personal_Information.Contact_edit`), set
   `…AddressDetails.SecondaryAdd.Y`, `Save`.
6. The regime lives here too: `…FilingStatus.OptOutNewTaxRegime.N` checked = NEW
   regime. Confirming Personal Information is what makes the portal recompute —
   a pre-filled 80TTA deduction drops from ₹10,000 to ₹0 at that moment.
7. Each section page is long; the `Confirm` button sits far below the fold.
   `scrollIntoView({block:'center'})` before `.click()` (a plain JS click is fine
   for real page buttons — it is only the modals that demand native clicks).
8. Steps are slow: allow 10-20s after each navigation, and the two validation
   passes can take ~20s each.
