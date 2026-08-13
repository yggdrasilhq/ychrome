# consumerhelpline.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## ingram-grievance-headless · WORKS
task: Lodge a National Consumer Helpline grievance end to end, headless, with an evidence upload
model: claude-opus-5
date: 2026-08-14
tags: 

INGRAM, the grievance system behind the National Consumer Helpline. Driven headless end to end
through the `ychrome ctl` engine: login, a single-page grievance form, an evidence upload, and a
docket number verified from the account's own history.

## Why this lane is worth knowing

It is **free, has no limitation period, and needs no lawyer** — so it stays available long after a
consumer-commission window under s.69 of the Consumer Protection Act 2019 has closed. For an old
claim it is often the only live route left.

## Login

`/user/` → mobile number + image captcha → **Generate OTP** → 6-digit SMS OTP → **Verify OTP**.
Lands directly on `/user/register-complaint.php`. Straightforward; no traps found.

- Captcha: `img#captchaImg`, fields `#email` (mobile, despite the id) and `#captcha_code`.
- OTP: `input[name=otp]`, submit `input[name=verify_otp]`.

## The form is CASCADING — fill it in order or the later dropdowns stay empty

`complainttype` → `GrivClassification` (Goods/Services) → `company` → `industry` → `category` →
`fop` (Nature of Grievance). Each step populates the next on `change`, so dispatch a real change
event and **wait** before reading the next dropdown's options.

⭐ **The company list holds ~2000 firms; if yours is absent choose `Other`** (value literally
`other`) and a text field `#icn` appears for the name. Selecting `Other` also unlocks the rest of
the cascade normally.

⭐ **A second wave of fields appears only after the cascade completes** — product type, order
number, transaction id, date of purchase, amount paid, grievance amount, company reference. They do
not exist in the initial DOM, so an early field enumeration will miss them entirely and the form
will look far smaller than it is.

⛔ **`#Datepurchase` is a TEXT input with a date widget that MANGLES input silently.** Writing
`06/09/2023` produced **`2006-09-20`** — a real date, wrong by seventeen years, with no error.
Write ISO (`2023-09-06`) and **read the value back**; this is the single most dangerous field on
the page because the result is plausible rather than obviously broken.

## Upload

`#file1` / `#file2` / `#file3`. The form is plain PHP `multipart/form-data` with **no
UpdatePanel and no onsubmit interceptor**, so a `DataTransfer`-injected File plus an ordinary
`input[name=submit]` click uploads correctly. (Contrast with ASP.NET portals, where a partial
postback silently drops the file.)

## Confirmation

On success the page renders **"Your Grievance has been successfully lodged and your docket
number:NNNNNNN"** plus a feedback star widget.

⛔ **Verify from the account's own history, not from that banner** — and the history link is
**`/user/manage-complaints.php`**. Guessing `/user/grievance-history.php` from the menu label
returns a bare **`Forbidden`**, which reads like a permissions failure and is only a wrong URL.
The history row shows grievance number, state, name, registration timestamp and status
(`In Process`).
