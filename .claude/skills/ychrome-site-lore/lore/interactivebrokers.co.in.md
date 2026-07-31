# interactivebrokers.co.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## login-blocked-on-a-hidden-surface · PARTIAL
task: log in to IBKR Client Portal from a detached agent surface
model: claude-opus-5
date: 2026-07-31
tags: 

FIRST CONTACT 2026-07-31. Reached the login page and filled it from a DETACHED
(never-revealed) surface; could not submit. Recording the shape so the next
agent does not re-derive it.

THE FORM (https://www.interactivebrokers.co.in/sso/Login, Angular):
  #xyz-field-username      text
  #xyz-field-password      password
  #toggle1 / paperSwitch   checkbox  <- LIVE vs PAPER right on the login page.
                                        Paper has its OWN username+password;
                                        this toggle is how you choose which.
  button.xyz-button-login  submit

WHAT WORKS on an unrevealed surface:
  - `web ensure --session <path>` MATERIALISES it. ⚠ `app open --client <shadow>`
    does NOT — the surface stays unregistered and every verb answers
    "session has no web surface". `ensure` answers reason:"healed".
  - `web eval` / `web await` read and write the DOM fine.
  - Filling: a native value-setter + input/change events LOOKS like it worked
    (el.value has the right length) and Angular ignores it — the form submits
    empty and the page resets. `document.execCommand('insertText')` after
    focus+selectAll does commit. Same family as the React/Lexical composer
    finding in the whatsapp lore: ACCEPTED IS NOT COMMITTED, read the field back.

WHAT BLOCKS:
  - `web do click --selector .xyz-button-login` ->
    "matched a zero-size element". The surface is fine and reports a 1920x1200
    viewport, but document.visibilityState === "hidden" and the SPA defers
    layout while hidden, so the button has a 0x0 rect. Every rect-resolving verb
    is useless on this site while detached; eval-backed ones are unaffected.
  - A programmatic .click() on the submit button does not navigate either, so a
    trusted (isTrusted) click is likely required as well.

CONCLUSION: completing this login today needs the surface REVEALED on the active
client (the operator's viewport) — ask first. The durable fix is the one named in
yggterm docs/agent-surface-attachment.md §4a: a detached surface must report
itself VISIBLE, or rung 2 silently degrades and everything falls to rung 3 on the
human's screen.

NOT ATTEMPTED: the paper-account password reset itself (Client Portal ->
Settings -> Paper Trading Account). No credential was changed.
