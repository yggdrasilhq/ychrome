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

## unmapped-surface-has-no-raf-reload-is-not-the-fix · WORKS
task: sign-in on an unrevealed surface for the androiddeveloper flow
model: claude-opus-5
date: 2026-08-07
tags: 

Correction and root cause for the "SPA view swap stalls" entry above, measured
2026-08-07 on the `service=androiddeveloper` sign-in (Play Console signup).

## The reload fix does NOT hold for this flow

The earlier entry says "the fix is `location.reload()` after each navigation". On this
flow that is WRONG and costs the login: `/v3/signin/challenge/pwd?TL=…` carries a
SINGLE-USE token, and reloading it bounces straight back to `/v3/signin/identifier`
with the typed identifier lost. Measured twice, both times a clean bounce.

## The real cause: an unmapped surface has no animation frames

On a `--no-activate` surface `document.visibilityState === 'hidden'` and
**`requestAnimationFrame` never fires** (measured: no callback in 3 s; timers and
promises still run). Google's view swap is frame-driven, so it half-completes:
the incoming screen is in the DOM but its `c-wiz` keeps the inline `display: none`
the swap would have cleared, which is why the inputs measure 0x0 with
`offsetParent: null`.

## The fix that works, without revealing the surface

Do what the missing frame would have done:

    // 1. land every frozen animation/transition
    document.getAnimations().forEach(a => { try { a.finish() } catch(e){} })
    // 2. finish the half-done view swap: keep the LAST c-wiz.A77ntc, hide the rest
    const wz=[...document.querySelectorAll('c-wiz.A77ntc')]
    const incoming=wz[wz.length-1]
    wz.forEach(w=>{ if(w!==incoming) w.style.display='none' })
    incoming.style.removeProperty('display')
    // 3. the transition scrim eats clicks even at opacity .5 — let them through
    document.querySelectorAll('div.ZQxJQe').forEach(s=>s.style.pointerEvents='none')

After that the real password box measures 376x52 and takes real keys, and
`Try another way` becomes hittable (before step 3 it answers `target_moved
(the point lands on div)` — that div IS the scrim, not a bad selector).

## Two more things this run proved

- **`fill-vault` can silently drop characters.** The verb answered `chars: 40` while the
  page held **32**. Clear the field and refill; the second attempt landed all 40. The
  ONLY trustworthy signal is a page-side `.value.length` read — the verb's own count is
  what it MEANT to type, not what arrived.
- `web totp --entry … --user …` fills the code but answers with EMPTY fields
  (`chars: null`); the page-side read (`#totpPin` length 6) is the only confirmation.
- 2FA routing: when the account is passkey-default, `Try another way` lists the lanes.
  The device-prompt lane can be present but DEAD — it renders as
  "Tap Yes on your phone or tablet — **Device can't be reached right now**". Read that
  line before choosing a lane; the vault's Authenticator/TOTP lane needs no owner.
