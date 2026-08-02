# emudhradigital.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## turnstile-600010-blocks-agent-signup · BLOCKED
task: 
model: claude-opus-5
date: 2026-07-31
tags: 

The eKYC signup at `CreateNewUser.jsp` (the URL IP India links for trademark eSign,
`?src=c44ead5fdecd48ecac24f` — keep that src param, it ties the account to the IPO
integration) CANNOT be driven from ychrome/WebKitGTK. The wall is Cloudflare Turnstile.

## What happens, and why it looks like a different bug

The form itself is trivially drivable:

- `#authenticatemobile` — mobile, `maxlength=10`, native-setter fill works
- `#btnauthenticatemobileOTP` — the "Get OTP" anchor (`href="#"`)
- `#authenticatemobileOTP` — OTP box, **`maxlength=4`** (a 4-digit code, not 6)
- `#btnauthenticatemobile` — Login
- `#lblindividual` / `#lblindian` already carry `active` — Individual + Indian are the
  defaults, no toggling needed

Click "Get OTP" and the UI enters a **loading spinner** in `#SpanGetOTP`
(`images/accordionloading.gif`), `#otpTimer3Block` ("Resend OTP in NN seconds") stays
`display:none`, and **no SMS ever arrives**. There is NO error message anywhere.

Do not read that as a silent app failure or a dead click — the handler ran. The send is
gated on a Turnstile token that never exists:

- `performance.getEntriesByType('resource')` is the verb that finds this. An XHR/fetch
  monkey-patch shows NOTHING (zero captures) because the request is never made at all,
  which misleads badly. The resource list shows
  `challenges.cloudflare.com/turnstile/v0/api.js?onload=prewarmTurnstile`.
- `window.turnstile` exists, but the widget host `#turnstile-captcha` is **0x0, never
  rendered**, and the hidden `cf-turnstile-response` input is **empty**.
- Rendering it explicitly (`turnstile.render('#turnstile-captcha', {sitekey})`, sitekey
  read from the hidden `#reCaptchaSiteKey`, `0x4AAAAAAB…`) fails with
  **error `600010`** — Turnstile's challenge-execution failure. **Reproducible on a
  clean page load**, not a transient prewarm glitch.

⇒ Cloudflare's fingerprinting rejects the WebKitGTK environment. There is no fix on our
side, and defeating a Certifying Authority's fraud control is not something to attempt.

## Correction to older fleet lore

**ychrome does NOT present a nonexistent-browser UA.** Measured here:
`Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)
Version/18.5 Safari/605.1.15` — a clean, plausible Safari string. So a UA-spoofing fix is
pointless; Turnstile is fingerprinting deeper than the UA.

## The working division of labour

The human steps cluster anyway — this signup, the **live video KYC**, and the **plan
payment** are all irreducibly his. So hand him the whole eMudhra side in his own browser,
and keep the agent on `ipindiaonline.gov.in`, which has **no Turnstile** and is fully
curl-drivable once its own TLS chain fix is in (see that site's lore).

What to ask him to bring back: the **eSign ID / signer ID**. That is the only artefact the
IPO portal needs — it is entered once during IPO registration.

## turnstile-gone-signup-fully-drivable · WORKS
task: 
model: claude-opus-5
date: 2026-08-02
tags: 

**SUPERSEDES the 2026-07-31 `turnstile-600010-blocks-agent-signup` entry above.**
Re-tested on a clean load: Turnstile now **solves itself in ychrome/WebKitGTK**. The
hidden `cf-turnstile-response` input carried a full token on first paint, with no
`turnstile.render()` call and no error 600010. "Get OTP" fired for real: the anchor
flipped to "Resend OTP", `#otpTimer3Block` went `display:block` with "Resend OTP in 57
seconds", the resource log showed the POST to `https://emudhradigital.com/CreateNewUser`,
and the SMS arrived (sender `AD-eMuDSC-S`, **4 digits**).

**Lesson worth more than the recipe: a Cloudflare verdict is a SERVER-SIDE POLICY, not a
property of the browser.** Two days turned BLOCKED into WORKS with no change on our side.
Re-probe a Turnstile/WAF wall before believing your own lore; the cost is one page load.

## Signup, end to end (all agent-driven, ~3 minutes)

URL: `CreateNewUser.jsp?src=c44ead5fdecd48ecac24f` (keep `src` — it is `allianceID`, and it
is what ties the account to the IP India trademark e-filing integration).
`#userTypeValue=1` (Individual) and `#resedentialValue=1` (Indian) are already the defaults.

1. `#authenticatemobile` <- 10-digit mobile, native-setter fill + input/change/keyup/blur.
2. `#btnauthenticatemobileOTP` — plain `el.click()` is enough (`onclick="getMobileOTP()"`).
   No pointer-event sequence needed.
3. Read the 4-digit code off the phone. `#authenticatemobileOTP` is `type=password`,
   maxlength 4; same native-setter fill.
4. `#btnauthenticatemobile` (Login) -> **account created**, redirect straight to
   `makePayment.jsp?X=<opaque>`. There is no separate "create account" screen.

## The billing screen (also fully agent-driven)

Title "Make Payment: eMudhra eKYC Portal". Plan is FIXED by the alliance link, no selector:
**One Year 100 Transactions, Rs250 + taxes** (= Rs295 with 18% GST). This CONFIRMS the
100-transaction tier still exists — a 2021-guide figure that had been flagged unverified.

- `#gstno` radio is pre-checked (individual -> No), `#gstNumber` stays empty
- `#retailBillingEmailID` — text
- `#billingAddress` — TEXTAREA (use `HTMLTextAreaElement.prototype` for the native setter,
  not `HTMLInputElement.prototype`)
- `#billingCountry` — India is value `1`, already selected
- `#billingPincode` — text; **filling it auto-selects `#billingDistrict`** (Kolkata = 15)
- `#billingState` — West Bengal is `WB19`; codes are `<2-letter><number>`, not plain names
- Submit is `#proceedBtnPriceSummary` -> `submitBillingDetails()` -> PayU
  (cards / net-banking / UPI / wallets; a voucher field exists there)

## Camera and mic WORK in the ychrome surface — the video KYC needs no other browser

Measured on this page after `server app open <session> --view preview`:
`getUserMedia({video:true,audio:true})` returned live tracks
`Integrated Camera (V4L2)` + the host's analog audio. `window.isSecureContext` true.
So the whole eMudhra lane — signup, payment, enrolment, live video KYC — can run in ONE
co-browse surface with the operator only doing the payment and showing his face/documents.

⚠ `web await` needs `--await-timeout`, NOT `--wait-timeout` (that flag belongs to `web
wait`). An unrecognized flag silently falls back to ~15s and the camera probe reads as
`await_timeout`, which looks exactly like a hung permission dialog.
⚠ A surface can be live and `mapped:true` while the SESSION still shows the terminal — the
page is a tab nobody is looking at. `server app open <session> --view preview` is what puts
it in front of the operator.

## What the IPO portal needs back

`ipindiaonline.gov.in/trademarkefiling/user/How-To-Register.aspx`, verbatim:
"Steps to Register for eFiling with Esign — 1. Procure an esign(signerid) from Web URL to
procure esign." So the ONE artefact to carry out of eMudhra is the **signer ID**.

## ekyc-enrolment-aadhaar-otp-route · WORKS
task: 
model: claude-opus-5
date: 2026-08-02
tags: 

Continues `turnstile-gone-signup-fully-drivable`. Everything from the payment return to the
video page, measured live. **Only the video is human.**

## After PayU: `PaperlessIndividual.jsp` — FOUR KYC routes, not the guide's two

The 2021 eSign User Guide (linked from IP India) documents Aadhaar-XML and PAN. The live portal
offers four, and the best one is not in the guide:

| Mode | radio id | What it costs |
|---|---|---|
| **Aadhaar eKYC (OTP)** ⭐ | `#rdoAadhaarOtpBased` | one Aadhaar OTP + a video. **No XML, no uploads.** |
| Aadhaar eKYC (Biometric) | `#rdoAadhaarBioBased` | a UIDAI-compatible reader; skips the video |
| Aadhaar eKYC (Offline XML) | `#rdoAadhaar` | the guide's route: a UIDAI ZIP + share code, then a video |
| PAN | `#rdoPan` | upload PAN + address proof, then show BOTH **in original** on camera |

⛔ **Do not build the XML route.** It needs `resident.uidai.gov.in/offline-kyc`, which **no longer
resolves at all** (it is `myaadhaar.uidai.gov.in/offline-ekyc`, an SPA behind a captcha). The OTP
route makes the whole detour unnecessary.

`#authenticateAadhaarOTP` navigates to **`aadhaarauth.e-mudhra.com/index.jsp`**:
`#txtAadhaar` (password, ml 19, auto-formats to `XXXX XXXX XXXX`) · `#chkConsent4` (the UIDAI
consent declaration — a legal attestation, get the operator's word before ticking it) ·
`#btnOnlineAadhaarOTP` (GET OTP) · `#txtOnlineAadharAuthOTP` (**6** digits) ·
`#btnAuthenticateOnlineAadhaar`. Sender `BV-ADHAAR-G`. Success navigates BACK to
`PaperlessIndividual.jsp` with a NEW `X=` token and UIDAI's name/Aadhaar filled in read-only.
UIDAI also mails an "Authentication Successful" notice — a free independent confirmation.

## Applicant details, then credentials

Revealed after the Aadhaar hop: `#Male|#Female|#Others` (gender arrives UNCHECKED even though
UIDAI knows it) · `#txtOnlineAadhaarEmail` · `#txtOnlineAadhaarPanNumber` ·
`#onlineAadhaarNameInDscPAN|Aadhaar` · `#txtOnlineAadhaarNameAsInPAN` ·
`#userdateAadhaar` / `#usermonthAadhaar` (value `1` = January) / `#useryearAadhaar` ·
submit `#btnAuthOnlineAadhaarApplicantDetails`.

⚠ **`Name (As in PAN)` is MANDATORY even when the DSC name is taken from Aadhaar.** Selecting
"As in Aadhaar" does not hide it; submitting without it raises a modal "Please enter the Name as
in PAN." The `?` beside it only opens a help modal — there is no lookup. **Ask the operator for
the exact printed name; do not infer it from the Aadhaar name.**

Next screen: `#txtLoginUsername` (ml 24) · `#txtLoginDesiredPswd` + `#txtLoginConfirmPswd`
(**the 6-digit signing PIN** — guide s.3: every future eSign authenticates PIN + OTP, so this is
the credential that makes later filings agent-drivable; generate it and vault it, read-back
verified) · `#txtEmailID` · `#btnGetOTP` · `#LoginmobileOTP` (**4** digits, despite the id, and
it is the EMAIL otp) · `#btnAuthloginAndOtpDetails`.

⭐ **The email OTP is mail, not SMS** — from the site's own do-not-reply sender, subject "Email
Verification OTP - KYC Enrolment", 10-minute validity. A mail CLI reads it with no phone
involved. Only the two MOBILE otps need the handset.

## `VideoVerification.jsp` — the one human step

Allots the **Signer ID** (shown under "Enrolment Information", alongside a 3-digit **Video Code**
the applicant must read aloud). 40 seconds, `#VRM_startRecordingbutton`, preview before submit.
Their stated rejection criteria: unclear audio/video, or a cap/headgear/eyeglasses/headphones.
Originals held at the edges, 4-5 seconds each.

## Re-login and the status dashboard

The session drops quickly. `Login.jsp` is mobile + OTP again, but the OTP box here is a
**SEGMENTED four-input** (`#otp1`..`#otp4`, submit `#authenticateMobileOTP`) — fill each box and
dispatch `input`/`keyup` per box, or use `web do fill --selector-set`. A "sent" msgBox with an
OK button covers the form first; dismiss it.

Lands on **`TrackOrderStatus.jsp`**: application number, a percentage, and two lists —
KYC Enrolment (Enrolment Information, Mobile Verification, Email Verification, Video Recording,
Photograph, Credential Setup) then Approval and Download (**eMudhra: Approval of Application**,
then **Applicant eSign**). At 80% with all six Completed, everything left is theirs. The header
badge reads "Pending from Subscriber" even while the body says "Pending for eMudhra Approval" —
**believe the step list, not the badge.**

⚠ Every field on this dashboard is masked (name and PAN both starred out), so it confirms
WHICH values were used only to someone who already knows them.
