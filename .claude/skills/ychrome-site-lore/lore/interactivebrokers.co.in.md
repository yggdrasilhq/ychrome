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

## detached-surfaces-DO-take-trusted-clicks · WORKS
task: retract the previous entry's plane-level diagnosis
model: claude-opus-5
date: 2026-07-31
tags: 

⛔ RETRACTS the mechanism in `login-blocked-on-a-hidden-surface` above. That
entry's OBSERVATIONS were real; its EXPLANATION was invented from one data point
and is false. Kept, not deleted — the wrong version is the intuitive one.

IT CLAIMED: a never-revealed surface reports visibilityState "hidden", the SPA
defers layout while hidden, so `do click` resolves a 0x0 rect and rung 2 is
unusable while detached — therefore completing this login needs a reveal on the
operator's viewport.

MEASURED THE NEXT HOUR, same site, never-revealed surface:
    visibilityState: "hidden"        <- still hidden, by design
    .xyz-button-login rect: 414x48   <- laid out fine
    web do click --selector ...   ->  accepted:true, is_trusted:true

Both halves were wrong. A hidden page still lays out, and injected input still
lands: the engine host WAKES a view it hid for the length of an injection burst
and re-hides it after (WebSurfaceHost::engine_webview_for_injection).
yggterm-shell/src/shell.rs:5430 says it outright —
    "Visibility gates RENDERING, never the drive path."

So "matched a zero-size element" is a PAGE-STATE answer (a modal over it, a
spinner, a route mid-transition), not a statement about detached surfaces. Read
it as "the page is not showing that right now", re-observe, and do NOT let it
push you into revealing a surface on the human's screen.

STILL TRUE from the earlier entry, and still useful:
  - `web ensure --session` materialises an unregistered surface (reason "healed");
    `app open --client <shadow>` does NOT, and its "session has no web surface"
    reads like a dead surface when it is only an unregistered one.
  - the form fill DOES commit via execCommand('insertText') after focus+selectAll,
    and survives a click (u/p lengths intact afterwards).
  - #toggle1/paperSwitch on the login page chooses LIVE vs PAPER; paper has its
    own username+password.

WHAT ACTUALLY BLOCKS THE LOGIN IS STILL UNKNOWN. Fields hold their values, the
button is enabled and 414x48, the click is delivered trusted — and the page does
not navigate. The page is neither Angular nor React (window.ng false, no
[ng-version], no react root). A notice renders: "Uppercase characters in
usernames are not supported. Those were automatically converted to lowercase."
Next probe should watch the NETWORK, not the DOM: whether a POST leaves at all
says immediately whether this is a client-side guard or a rejected credential.

No credential was changed. The paper-account reset was never attempted.
