# broker-a.example

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## backoffice-otp-then-app-pin · PARTIAL
task: 
model: claude-opus-5
date: 2026-08-20
tags: 

**Reached: OTP ACCEPTED, stopped at a 6-digit app PIN that is not in the vault.** The whole flow
drives headless on `ychrome ctl` from a container host. The gate is a MISSING CREDENTIAL, not a
tooling limitation — nothing here needs a better selector.

## The flow, and every step of it worked

```sh
p=$(ychrome ctl open url=https://<broker-backoffice-host>/ profile=agent-fin-broker | jq -r .page_id)
# redirects to the SSO login; the back-office host no longer has a login of its own
ychrome ctl input page_id=$p events='[{"type":"click","selector":"#mobileNum"}]'
# ⚠ one event per character
EV=$(python3 -c 'import json;print(json.dumps([{"type":"type","text":c} for c in "<10 digits>"]))')
ychrome ctl input page_id=$p events="$EV"
ychrome ctl eval page_id=$p js='document.querySelector("#mobileNum").value.replace(/\D/g,"").length'  # ⇒ 10
ychrome ctl input page_id=$p events='[{"type":"click","selector":"button","nth":1}]'   # "Get OTP"
```

Selectors: `#mobileNum` · `#otpNum` · `#pinCode` · `button` nth-indexed (no stable ids on buttons).

## ⭐ CLOUDFLARE TURNSTILE IS PRESENT AND DID NOT BLOCK

The login page carries a hidden `input[name=cf-turnstile-response]`. The invisible challenge
resolved on its own from the headless engine — **no interaction, no failure, no captcha image.**
Recorded because a hidden Turnstile field looks like a wall in a DOM dump and is not one here.

## ⛔ THE OTP FIELD IS MASKED AND `value.length` LIES BY ONE

`#otpNum` auto-formats a 6-digit code as `DDD-DDD`, so a correct entry reads **length 7**. An agent
checking `length === 6` will conclude its own typing failed, clear the field and retype — which is
how a good code gets thrown away. ⇒ **Judge by the DIGIT COUNT or by a shape mask**
(`v.replace(/\d/g,"D")` ⇒ `DDD-DDD`), never by raw length. Same family as any formatted phone,
card, IFSC or amount field.

## ⛔ `ctl input` CLICK NEEDS A SELECTOR OR x,y — THERE IS NO `text` FORM

`{"type":"click","text":"Continue"}` is refused with *"a click event needs x and y (or, for a click,
a selector)"*. That is the ENGINE plane; the yggterm surface plane's `web do click --text` is a
different API. Do not carry the syntax across.

## ⛔ WHERE IT STOPS: A 6-DIGIT APP PIN, AND THE ONLY WAY PAST IT IS A WRITE

After the OTP the page greets the account holder by name and shows `#pinCode`
(`type=password`, `maxLength=6`) with exactly three affordances: **Forgot PIN? · Continue ·
Switch account?**, plus a QR path requiring the broker's mobile app.

- The PIN is **not in the vault** — the stored entries are 10 and 12 characters and cannot fit a
  6-length field.
- ⛔ **"Forgot PIN?" is a credential RESET, i.e. a WRITE on a live broking account.** A read grant
  does not authorise it. Do not click it.
- ⛔ **Do NOT guess.** A broking login locks, and a lockout costs the account holder real access.

⇒ **The fix is a vault entry, not code.** Ask the owner for the 6-digit PIN, store it, and this flow
completes unattended. Everything upstream is already proven.

## ⭐ WHAT THIS PROVES EVEN THOUGH IT DID NOT FINISH

Reaching a personalised PIN prompt ("Hi <name>… Signed in as <number>") means the mobile is
registered, the OTP was accepted, and **the trading account authenticates** — it is not closed. That
is a real answer to "is this account live", obtained without completing the login.

## Boundary

Reads only. Nothing here authorises an order, a transfer, a mandate, a nomination change, a PIN
reset or an account closure.
