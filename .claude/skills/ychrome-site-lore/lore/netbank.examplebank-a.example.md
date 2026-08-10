# netbank.examplebank-a.example

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## netbanking-login-honeypot-fields-and-app-code-2fa · PARTIAL
task: Read-only netbanking access for a banking automation lane: reach balances/statements headless
model: claude-opus-5
date: 2026-08-10
tags: 

**Reached: credential ACCEPTED, stopped at 2FA.** Login itself is fully drivable headless on
`ychrome ctl` from dev. The gate is the second factor, not the password.

## ⛔ THE DOMAIN MOVED — every stored Example Bank A URL is stale (RBI `.bank.in` migration)

| host | resolves? | what it is |
|---|---|---|
| `retail.examplebank-a.example` | **NXDOMAIN** | the OLD netbanking host. Dead. |
| `examplebank-a.co.in` | **NXDOMAIN** | dead. |
| `omni.examplebank-a.co.in` | ✅ | entry point, **302s to the one below** |
| `netbank.examplebank-a.example` | ✅ | **THE live portal** — `/examplebankaretailbanking/`, "Login | ExampleBankAOmniChannel" |

RBI mandated Indian banks onto the `.bank.in` TLD; Example Bank B moved the same way
(`my.netbank.examplebank-b.example` → `netbank.examplebank-b.example`). ⇒ **Assume every Indian bank URL older than
2025 is dead, and resolve before you drive.** A vault item NAMED for a dead host still holds a
good credential — the stale name is not evidence of a stale secret.

## ⛔⛔ THE PASSWORD FIELD IS A HONEYPOT SANDWICH — vault autofill lands in the DECOY

The form carries **three** password inputs, in DOM order:

```
input[name=password0]   display:none   autocomplete="new-password"   ← DECOY
input#pass              visible        autocomplete="off"            ← THE REAL ONE
input[name=password1]   display:none   autocomplete="new-password"   ← DECOY
```

Example Bank A put those there deliberately to absorb password-manager autofill. `ychrome ctl fill` walks
`input[type=password]`, takes the first match, and writes the secret into **`password0`**.

⛔ **And it reports total success while doing it.** Measured:

```
ctl fill page_id=… entry='netbank.examplebank-a.example'
  → {"filled":"filled", "ok":true, "secret_field_count":3,
     "fields":[{"field":"username","want":9,"got":9,"ok":true,…},
               {"field":"secret","want":14,"got":14,"ok":true,
                "target":{"name":"password0",…}}]}     ← wrote the DECOY, verified the DECOY
```

Independent page-side readback at that same instant:

```
{"cust":"<the customer id>", "passlen":0, "h0":14, "h1":9}
        the REAL field is EMPTY ↑
```

⇒ The 2026-08-07 `unverified` hardening does not save you here: it reads back **the field it
wrote**, so a wrong-target write verifies perfectly. `want == got` proves the value survived; it
says nothing about whether the target was right. Also note `h1:9` — the *username* was pushed into
the second decoy too.

**A submit in that state sends an empty password = one consumed attempt. Example Bank A locks on repeated
failures. This is the trap that costs an account, not just a session.**

## ✅ THE RECIPE THAT WORKS

```sh
p=$(ychrome ctl open url=https://omni.examplebank-a.co.in/ profile=agent-fin-bank | jq -r .page_id)
ychrome ctl wait page_id=$p until='{"load":"finished"}'

# 1. NEVER `ctl fill` this form. If you already did, clear the decoys first:
ychrome ctl eval page_id=$p js='(()=>{const s=Object.getOwnPropertyDescriptor(
  HTMLInputElement.prototype,"value").set;
  for(const n of ["password0","password1"]){const e=document.querySelector(`[name=${n}]`);
    if(e){s.call(e,"");e.dispatchEvent(new Event("input",{bubbles:true}));}}
  return "cleared";})()'

# 2. customer id — plain, unmasked
ychrome ctl input page_id=$p events='[{"type":"click","selector":"#custid"}]'
ychrome ctl input page_id=$p events='[{"type":"type","text":"<customer id>"}]'

# 3. password — click the REAL field BY ID, verify focus, then type
ychrome ctl input page_id=$p events='[{"type":"click","selector":"#pass"}]'
ychrome ctl eval  page_id=$p js='document.activeElement.id'     # must be "pass"
PW=$(ychrome-vault get "netbank.examplebank-a.example" --field password)
[ ${#PW} -eq 14 ] || exit 1                                     # length guard before you spend an attempt
ychrome ctl input page_id=$p events="$(PW="$PW" python3 -c \
  'import json,os;print(json.dumps([{"type":"type","text":os.environ["PW"]}]))')"

# 4. ⛔ THE GATE THAT DECIDES: all three lengths, together
ychrome ctl eval page_id=$p js='JSON.stringify({pass:document.querySelector("#pass").value.length,
  h0:document.querySelector("[name=password0]").value.length,
  h1:document.querySelector("[name=password1]").value.length})'
#   REQUIRE {"pass":14,"h0":0,"h1":0}. Any other shape ⇒ DO NOT SUBMIT.

ychrome ctl input page_id=$p events='[{"type":"click","selector":"#APLOGIN"}]'
```

Selectors, all stable and id-bearing (Angular Material, `ng-tns-*` classes churn — use the ids):
`#custid` · `#pass` · `#APLOGIN` (Login) · `#FGTPASS` (Enable login ID) · `#resendOTP`.

Two login MODES exist, as tabs: **`#tab-0` Login ID / Customer ID** (the above) and **Debit Card
No.** — the second is presumably what a card-number + 4-digit-PIN credential is for. Untested.

## ⛔ THE 2FA IS AN IN-APP CODE — no SMS, and this is where an unattended run ends

After a correct password the page becomes **"Two-Factor Authentication | ExampleBankAOmniChannel"**:

> *Enter Mobile App Code for Login. For robust security of your account, you need to generate a
> Login / Transaction OTP using your Example Bank A Mobile App.* Step 1: open the Example Bank A Mobile App → *Mobile
> App Code* on its login page. Step 2: the code appears; enter it here.

Six unnamed `input[type=text]` boxes (a per-digit OTP component), a `Confirm` button, and a
`RESEND OTP` span (`#resendOTP`) behind a ~3:00 countdown.

⚠ **`RESEND OTP` is misleading — it does not put an SMS on the handset.** Measured against the
owner's real SMS store (Termux lane, clock verified, `sim_state: ready`, JIO 4G, not roaming, not
airplane): **zero** messages arrived in the whole window. Bank SMS from this sender family does
normally reach this handset, so the lane was not the problem.

⇒ **The Example Bank A factor is generated inside an app on the handset, behind its own MPIN/biometric.**
That is a genuine human gate of the same class as the passkey presence dialog — not a ychrome
limitation, and not something a better selector fixes. Everything up to it is automatable; the
code itself must come from the phone.

## Boundary

Reads only. This lore covers reaching the logged-in state; it authorises nothing beyond looking.
No transfer, no payment, no standing instruction, no card action.
