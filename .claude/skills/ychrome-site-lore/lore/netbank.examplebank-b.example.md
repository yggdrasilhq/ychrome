# netbank.examplebank-b.example

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## netbanking-login-masked-field-and-otp-not-delivered · PARTIAL
task: Read-only netbanking access for a banking automation lane: reach balances/statements headless
model: claude-opus-5
date: 2026-08-10
tags: 

**Reached: password ACCEPTED, stopped at SMS-OTP delivery.** Both credential steps drive cleanly
headless on `ychrome ctl` from dev. The gate is that the OTP never arrived on the handset.

## The host moved (RBI `.bank.in` migration)

`my.netbank.examplebank-b.example` **302s to `netbank.examplebank-b.example/login`** — that is the live portal. Example Bank A
moved the same way (`retail.examplebank-a.example` is now NXDOMAIN, live host `netbank.examplebank-a.example`).
⇒ **Resolve before you drive; assume any pre-2025 Indian bank URL is dead.**

## ⛔ THE VAULT'S STRICT HOST-MATCH RESOLVES TO THE WRONG PERSON HERE

```
ychrome-vault match my.netbank.examplebank-b.example  → item "example bank b",    user "<a DIFFERENT person>"
ychrome-vault match netbank.examplebank-b.example     → item "netbank.examplebank-b.example", user "<the owner's number>"
```

Two humans' Example Bank B logins live in this vault, and the *strict* rule picks the other one for the
`my.` host — the exact host the portal redirects you to. **On a shared/family vault, never let
`match` choose a banking identity by host alone; name the entry explicitly and confirm the
username before typing.** A wrong username here spends a stranger's login attempts.

⚠ Related: `ychrome-vault match` **prints the password in cleartext on stdout**. Fine for a
disambiguation you pipe to `jq`, bad in an agent transcript. Prefer
`ychrome-vault get <item> --field password` into a shell var.

## ⛔ THE MOBILE FIELD IS MASKED — a fast keystroke burst is MANGLED

`input[name=mobileNumber]` reformats as you type (`NNN NNN NNNN`). One `{"type":"type"}` event
carrying all ten digits lands **wrong**:

```
events=[{"type":"type","text":"<10 digits>"}]
  → {"landed":false,"before":0,"after":12,"grew_by":12,"want":10}
  page-side readback: "NNN N       "     ← four digits lost, spaces injected
```

**Send one event per character** and it lands perfectly:

```sh
EV=$(python3 -c 'import json;print(json.dumps([{"type":"type","text":c} for c in "<10 digits>"]))')
ychrome ctl input page_id=$p events="$EV"
# page-side: "NNN NNN NNNN"  ✅  (strip spaces to compare)
```

⚠⚠ **`landed` is a FALSE NEGATIVE on any masked field.** All ten per-character events above
reported `"landed":false` while every digit landed correctly — the flag is computed from
length-growth, and a mask that inserts a space makes growth ≠ 1. ⇒ **On a formatted input (phone,
card, IFSC, amount, date) judge by a page-side value readback, never by `landed`.** An agent that
retries on `landed:false` doubles the input.

Compare the plain password field, where the same verb is honest:
`{"landed":true,"before":0,"after":15,"grew_by":15,"want":15}`.

## ✅ THE RECIPE THAT WORKS (to the OTP wall)

```sh
p=$(ychrome ctl open url=https://my.netbank.examplebank-b.example/login profile=agent-fin-bank | jq -r .page_id)

# step 1 — mobile number, ONE EVENT PER CHARACTER
ychrome ctl input page_id=$p events='[{"type":"click","selector":"input[name=mobileNumber]"}]'
ychrome ctl input page_id=$p events="$per_char_events"
ychrome ctl eval  page_id=$p js='document.querySelector("input[name=mobileNumber]").value'
#   verify the DIGITS (strip spaces). Do NOT trust `landed`.
ychrome ctl input page_id=$p events='[{"type":"click","selector":"button","nth":3}]'   # "Proceed to login"

# step 2 — password. ONE field, no honeypots (unlike Example Bank A).
PW=$(ychrome-vault get "netbank.examplebank-b.example" --field password)     # NOT `match`, NOT the my.* host
[ ${#PW} -eq 15 ] || exit 1
ychrome ctl input page_id=$p events='[{"type":"click","selector":"#login-password-input"}]'
ychrome ctl input page_id=$p events="$typed"
ychrome ctl eval  page_id=$p js='document.querySelector("#login-password-input").value.length'   # ⇒ 15
ychrome ctl input page_id=$p events='[{"type":"click","selector":"button","nth":3}]'   # "Login securely"
```

Selectors: `input[name=mobileNumber]` · `input[name=customerUserName]` (the OR-branch) ·
`#login-password-input` · `input[name=otp]`. **Buttons carry no ids and no stable classes**
(styled-components hashes: `sc-hIufae`, `CallToAction-sc-11yre54-0` — these WILL churn on deploy).
Address them by index off a fresh enumeration each run:

```sh
ychrome ctl eval page_id=$p js='JSON.stringify([...document.querySelectorAll("button")]
  .map((b,i)=>({i,t:b.innerText.trim(),vis:!!b.offsetParent})))'
```
⚠ `{"type":"click","selector":"button","nth":N}` answers `"ambiguous":true` and clicks anyway —
that is the opt-in behaviour, so **re-derive N every run**; a hard-coded index is a wrong click.

## ⛔ WHERE IT STOPS: "OTP sent to your registered mobile number" — and none arrives

Password accepted → `input[name=otp]`, a `Resend OTP` button and `Verify`, with a 0:25 countdown.
Measured against the owner's real SMS store, twice, with a stamped `--since-ms` window and one
explicit resend click: **no OTP arrived, over ~5 minutes.**

The absence was verified rather than assumed, because an absence cannot describe its own boundary:

- the Termux lane answers (`READS_OK`), phone clock correct to the second;
- `termux-telephony-deviceinfo` → `sim_state: ready`, `JIO 4G`, `data_state: connected`,
  `network_roaming: false`, not airplane; dual-SIM (`phone_count: 2`);
- SMS from this bank's own sender family (`XX-EXBANKB-S`, `XX-EXBANKB-S`, `XX-EXBANKB-S`) **does**
  normally reach this handset — seven messages between 2026-08-07 and 2026-08-08;
- the newest message on the device throughout was hours old.

⇒ The lane is not the problem and the handset is not the problem. **Cause unknown — recorded as
UNSEEN, not as "the bank did not send".** Candidates, untested: server-side dispatch failure, a
DLT/telco delay longer than the page's own validity window, an OTP routed to a channel other than
SMS, or a rate-limit from repeated requests.

## ⛔ INSTRUMENT TRAP found while proving that absence

`termux-sms-list -l N` omits **`received_ms` on every message** (40/40). The fabric's
`termux-sms.py` derives it from `received`, so `--since-ms` is sound *through the script* — but
any hand-rolled reader that sorts or filters on `received_ms` gets `None` and either throws or
silently mis-orders. It misread this very run for one round. **Sort on `received`, or use the
script.** The documented "newest N, oldest-first" contract does hold: `msgs[0]` is the OLDEST of
the window, `msgs[-1]` the newest.

## Boundary

Reads only. Nothing here authorises a transfer, payment, standing instruction or card action.
