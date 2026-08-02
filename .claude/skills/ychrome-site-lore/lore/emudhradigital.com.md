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
