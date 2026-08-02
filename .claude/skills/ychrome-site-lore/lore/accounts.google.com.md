# accounts.google.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## vault-login-totp-and-passkey · WORKS
task: 
model: claude-opus-5
date: 2026-08-02
tags: 

Full sign-in driven end to end from the vault on a shadow surface, 2026-08-02:
identifier -> password -> 2-Step Verification via the vault's TOTP -> authenticated
(`myaccount.google.com` renders the account UI). The operator's viewport never moved.

## Selectors that are actually right

- identifier: `#identifierId` — **`type="text"`, NOT `type="email"`**. A
  `input[type=email]` selector matches NOTHING here and the refusal
  (`no element matches`) reads like a broken verb. Its
  `autocomplete="username webauthn"` is Google offering passkey autofill.
- password: `input[type=password][name=Passwd]` (a second, hidden
  `name=hiddenPassword` exists — do not target it).
- TOTP: `#totpPin`, `type="tel"`, `name="totpPin"`, `autocomplete="off"`.
- Buttons are addressed by text: `Next`, `Try another way`, `Continue`.

## ⛔ THE ONE THAT COSTS AN HOUR: the SPA view swap stalls, and a reload fixes it

After every step Google navigates its own URL (`/v3/signin/challenge/pwd`,
then the 2SV pages) but **the visible view does not swap** — the body still
renders the PREVIOUS screen, prefixed with Google's own "Loading", while the
next screen's inputs exist in the DOM at **0x0 and `offsetParent: null`**.

`web wait --until load:finished` answers `met: true, elapsed_ms: 1` — the load
really did finish; it is the client-side view swap that never ran. So the wait
verb is not lying and waiting longer does not help (measured: still 0x0 after
60 s).

**The fix is `location.reload()` after each navigation.** The reloaded URL
renders its real screen immediately. Budget one reload per step.

⚠ Because of this, `fill-vault` correctly refuses with
`no_hittable_match (… matched 1 element(s) and NONE could receive a click)`.
That refusal is the tool being honest about a 0x0 target — it is the SPA stall,
not a selector mistake. Reload, then fill.

## The vault steps that work

    web fill-vault --item accounts.google.com --field username \
      --user <account> --selector '#identifierId' --session <s>
    web fill-vault --item accounts.google.com --field password \
      --user <account> --selector 'input[type=password][name=Passwd]' --redact --session <s>
    web fill-vault --item accounts.google.com --field totp \
      --user <account> --selector '#totpPin' --redact --session <s>

Each was verified by a PAGE-SIDE readback of `.value.length` (23 / 40 / 6),
never by the verb's own `accepted`. `--redact` keeps the secret out of the reply.

⚠ `--item accounts.google.com` alone matches 8 accounts and is refused by name;
always pass `--user`.

## 2FA routing

The default challenge is the PASSKEY screen ("Use your passkey to confirm it's
really you"). `Try another way` lists the alternatives; the TOTP one is
**"Get a verification code from the Google Authenticator app"**. Click it by
text, reload, then fill `#totpPin`.

Passkeys DO work here: `google.com` is in the vault's rpId set and
`*://*.google.com/*` covers `accounts.google.com`, so the shim is installed and
`navigator.credentials.get` stringifies to OUR JS rather than `[native code]`.
The ceremony ends at a native presence dialog in the operator's GUI, which is
deliberate and cannot be answered by an agent.

⛔ **A tab opened while the vault was LOCKED has no shim for its whole life** —
userscripts bind at surface creation. Unlock first, then open the tab.
