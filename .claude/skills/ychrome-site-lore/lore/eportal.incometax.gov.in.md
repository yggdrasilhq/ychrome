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

## file-itr1-end-to-end · WORKS
task: 
model: claude-opus-5
date: 2026-07-27
tags: 

Filed an ITR-1 end to end — prepared, submitted and e-verified — on a `--keep`
session from `data-fabric/scripts/itr-portal.sh`. Reads were already proven; this
is the write path working. Credentials come from the vault entry keyed by this
domain; never inline them.

FLOW: login -> #/dashboard/fileIncomeTaxReturn -> pick AY + Online -> Resume or
Start New -> the five-section summary (P Info / GTI / Tot Deduct / TP / Tax Liab,
each must show "Modify if required" before Proceed) -> Proceed To Verification ->
Preview And Submit -> declaration -> Proceed To Validation -> Preview ->
Proceed To Validation (again, upload-level) -> Proceed To Verification -> pick
E-Verify Now -> Aadhaar OTP -> Validate -> "Confirm Submission of Return" modal ->
Submit. The ack number is at e-File > Income Tax Returns > View Filed Returns.

TRAPS, in the order they bite:

1. NOT EVERY ACCOUNT HAS A LOGIN OTP. Where e-Filing Vault higher security is off,
   the password alone lands on the dashboard. The OTP-request JS still "succeeds"
   (it returns its label whether or not a button was found), so a watcher blocks
   its full timeout and the run reports "no OTP arrived" — indistinguishable from a
   dead phone. Check location.hash for /dashboard right after the password step.
   Aadhaar OTP for E-VERIFICATION is a separate flow and does still fire.

2. A SAVED DRAFT CAN BE STALE AND WRONG. A month-old draft carried an interest
   figure that predated the SFT filings; filing it would have understated income by
   ~4.1L against the department's own AIS. "Resume Filing" preserves the rest, so
   resume — but diff every figure against a fresh AIS/TIS pull first. "Start New
   Filing" deletes the draft.

3. ANGULAR MATERIAL: JS .click() beats coordinates for expansion panels, mat-options
   in a CDK overlay, and row checkboxes. But real input (web do click) WAS required
   for the TIS<->AIS label-styled tab and the AY mat-select. Keep both, expect to
   switch. And scrollIntoView() in the measuring eval + click in a LATER call = the
   layout shifts between them and the click silently lands on nothing. Measure and
   click in one eval, or use JS click.

4. NUMBER INPUTS need the native-setter injection (getOwnPropertyDescriptor on
   HTMLInputElement.prototype "value" .set, then dispatch input/change/blur). Plain
   el.value= leaves the Angular model stale and the running total never moves.

5. EDITABLE ROW TABLES (Income from Other Sources): tick the row checkbox -> _edit ->
   fields become inputs -> _save -> _add_another. The nature-of-income _select writes
   a hidden code (SAV = savings, IFD = interest from deposit).

6. MANDATORY FIELDS FAIL LATE AND POINT AT THE WRONG PAGE. "Is the secondary address
   same as primary address?" unanswered, and a secondary mobile holding the literal
   "00" ("cannot begin with '0'"), both blocked Confirm on Personal Information — but
   both are only editable inside the Contact sub-editor.

7. PREFILLED "Nature of Employment" can be stale from a previous employer. Check it
   against the salary actually being declared.

8. SESSION IS 15 MIN IDLE, and time spent on the Compliance Portal (AIS) does NOT
   count as activity here — expect to re-login after an AIS walk. Setting
   location.href triggers a logout confirm dialog; answer No and use in-app nav.

9. The dashboard's "Recent Filed Returns" accordion did not expand; go through the
   e-File menu instead.

VERIFICATION: "E-Verify Now" -> "OTP on mobile number registered with Aadhaar" ->
tick consent -> Generate Aadhaar OTP -> six separate #otp_<i>_<suffix> boxes, which
need real input (web do type) one digit at a time; eval cannot drive them. Stamp the
since-timestamp BEFORE clicking Generate so a stale code cannot win.

## cpc-itr-grievance-filing · WORKS
task: 
model: claude-opus-5
date: 2026-08-06
tags: 

Filing a CPC-ITR grievance end to end, unattended, from a `--keep` session
(proven 2026-08-06, acknowledgement 26390914, AY 2025-26 refund chase).

## Route
Top-nav **Grievances -> Submit Grievance -> CPC-ITR** -> Category / Applicable Act /
Sub-Category -> **Continue** -> AY, Grievance Description, attachments -> **Submit Grievance**.

- `Continue` EXPANDS the same route (`#/fo-greivance/submit/fillDetails`); it does not navigate.
  Do not wait for a hash change or you will conclude the click failed.
- Menu items: click the element's `closest("a")`, and take the LAST element whose exact text
  matches -- the label text appears on several nested nodes and the outer one is inert.
- The department is a plain div tile, not a radio.
- Dropdowns are `mat-select` (`#mat-select-N`), options are `mat-option` in a CDK overlay.
  Sub-Category stays EMPTY until Applicable Act is chosen -- an empty option list here means
  "a prerequisite field is unset", not "the control is broken".
- Close a stray overlay with `.cdk-overlay-backdrop`.

## ⛔ The description field silently rejects `>`
No error text, no hint: the control just stays `ng-invalid` and Submit stays disabled forever.
Probed one character at a time: `"` `/` `:` `,` and newlines ALL PASS; only `>` fails.
So a menu path like `e-File > View Filed Returns` must be written with commas.
There is no `pattern` attribute -- the validator is in code, so the DOM tells you nothing.

## ⚠⚠ Never read `ng-invalid` synchronously after setting a value
Angular updates the class in a LATER tick. A same-tick read reports INVALID for a field that is
perfectly fine, and it does so for every candidate you probe -- which reads as "the value never
reached Angular" and sends you hunting a wiring bug that does not exist. This cost ~15 minutes
and three wrong diagnoses here. Use `web await` and sleep ~500ms before reading the class.

## Setting the value
- `document.execCommand("insertText", ...)` on the focused textarea is EXACT and reaches Angular.
- The native-setter injection also reaches Angular (control goes `ng-dirty ng-touched`).
- `web do type` reaches Angular too but DUPLICATES THE FINAL CHARACTER (typed "test grievance",
  got "test grievancee") -- same stray-char class as `#panAdhaarUserId` on the login page.

## Form shape
- There is **no subject field** -- fold the subject into the first line of the description.
- Description cap is 3000 chars; the "Remaining Characters" counter tracks the DOM value and can
  read correct while the control is still invalid, so it is not a validity signal.
- Applicable Act: AY 2025-26 is FY 2024-25 => **Income Tax Act 1961**, not the 2025 Act also
  offered (that starts FY 2026-27).

## ⛔ Attachments cannot be filed by an agent today
The form offers Form 16/16A, Challan Copy, Order Copy from department, Other documents
(pdf/jpeg/zip, 10MB total). There is **no file-upload verb on the web surface** -- `web do` is
click/type only -- and a multi-MB DataTransfer injection through an eval is not viable.
Workaround that made the filing worth sending anyway: quote the JOIN KEYS in the body (order DIN,
rectification ARN, challan CIN + BSR + serial, DRN) -- those are what CPC matches on -- and keep
the PDF staged for when they ask.

## Confirming
The success toast prints `Grievance submitted sucessfully!` (the department's own typo) with the
acknowledgement number, then clears. **Screenshot immediately or you lose it** -- a capture taken
~60s later shows only the reset form. Confirm INDEPENDENTLY under **Grievances -> View Grievance
Status**, which lists ack no / department / category / sub-category / date logged / status, and
screenshot that instead: it is the durable artefact.
