# play.google.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## play-console-signup-unrevealed-surface · WORKS
task: Play Console personal developer account signup, driven unrevealed
model: claude-opus-5
date: 2026-08-07
tags: 

Play Console developer signup driven end to end on an UNREVEALED yggterm web
surface, 2026-08-07, up to (and stopped at) the card charge.

## ⛔⛔ THE ROOT CAUSE OF EVERY "STALLED" GOOGLE PAGE: an unmapped surface has no rAF

On a `--no-activate` surface `document.visibilityState === 'hidden'` and
**`requestAnimationFrame` NEVER FIRES** (measured: 0 callbacks in 3 s). Timers and
promises still run, so the page is alive — only frame-driven work is frozen. Every
Google SPA hangs its view swaps, popup positioning and enter-animations on rAF, so:

- the sign-in view swaps stall (`accounts.google.com` lore says "reload fixes it" —
  **that advice does NOT hold for the `service=androiddeveloper` flow**: the
  `/challenge/pwd?TL=…` URL is single-use and a reload bounces to `/identifier`);
- modal dialogs sit at `opacity: 0.013` with inline `opacity: 1` (a frozen animation);
- dropdown popups sit at `transform: matrix(0.0001, …)` so every option is 0x0;
- a full-viewport transition scrim `div.ZQxJQe` (fixed, z-5, `pointer-events: auto`)
  keeps swallowing clicks;
- cross-origin payment iframes stay at their 150 px "loading" size because the child's
  resize postMessage is measured in a rAF that never runs.

**THE FIX, and it is three lines — run it after EVERY navigation and EVERY click that
opens something:**

    document.getAnimations().forEach(a => { try { a.finish() } catch(e){} })   // lands frozen animations
    // kill any leftover scale-to-zero on popup wrappers
    el.querySelectorAll('*').forEach(e => { const t=getComputedStyle(e).transform
      ; if (t && t !== 'none') e.style.setProperty('transform','none','important') })
    document.querySelectorAll('div.ZQxJQe').forEach(s => s.style.pointerEvents='none')

Plus, for a half-done sign-in view swap: hide every `c-wiz.A77ntc` except the LAST one
in document order and clear its inline `display:none`. That is exactly what Google's own
swap would have done.

**Verify with `elementFromPoint`, never with the verb.** After un-freezing, hit-test each
control: `const top=document.elementFromPoint(cx,cy); top && (el.contains(top)||el===top)`.

## Selectors and facts that are right

- account-type chooser: the two "Get started" buttons are identical in text. They are
  told apart ONLY by `button[debug-id="select-personal-account"]` vs
  `button[debug-id="select-organization-account"]`. Do not click by position.
- Angular Material text fields: a native-setter write registers, but the char counter
  updates ONE render late — re-read after ~2 s before calling it lost.
- `input[type=checkbox]` `--nth` is NOT the visual order: hidden boxes shift it (on the
  Apps step "None of the above" was nth **18**, while nth 13 was "Crowdfunding or
  microloan apps" — a FALSE declaration if trusted). Always resolve by label text first.
- the phone field REJECTS a space between country code and subscriber number:
  `+91 <number>` fails validation, `+91<number>` passes. Write it unspaced.
- rapid successive `do` calls answer `preempted`; pass **`--new-batch`** on every
  click/fill after the first.
- the public developer email and the contact email both auto-verify with
  "verified through Google account" when they equal the signed-in account — no code.

## ⛔ THE WALL THAT STOPS AN AGENT: card entry is cross-origin

The buyflow (`payments.google.com/gp/w/u/0/buyflow2`) is a CROSS-ORIGIN iframe inside
`play.google.com`. **`web fill-card` must inject page-side, so it cannot reach it** — and
it takes only `--selector/--role/--target-text`, never `--x/--y`. Coordinate CLICKS do
pass into the frame; only the vault injection cannot.

Ruled out by measurement, do not re-try:
- loading the buyflow URL top-level on a second surface — it renders, but its
  "Add credit or debit card" sub-flow never opens (it needs the parent popup wiring);
- the Payments Center `payment_methods` fragment top-level — renders the list, but
  *Remove* is inert for the same reason.

**The relay that DOES work** (and keeps the PAN out of the agent's context): create a temp
`<input>` in the PARENT document, `fill-card --selector '#tmp'` into it, then read it and
re-type it into the frame by coordinates — all inside ONE ssh pipeline on the host, so the
value never crosses into the agent transcript. Scrub and remove the temp input immediately.

## OR_CCR_123 — the card refusal that ends the run

`Save card` answered: *"The card you are trying to use is already being used for a
transaction in a different currency. Please try using another card. [OR_CCR_123]"*
The same card already sat on the payments profile as an INR-linked instrument flagged
**"Incomplete card information"**, and the fee is quoted in USD. Removing that instrument
is the likely fix but CANNOT be done from an agent surface (see above) — it needs a
normal browser session.

## or-ccr-123-is-the-card-not-the-profile · WORKS
task: completing the Play Console signup and paying the registration fee
model: claude-opus-5
date: 2026-08-07
tags: 

Resolution of the OR_CCR_123 wall in the entry above — the signup DID complete, and the
$25 was paid, on the same unrevealed surface. What actually unblocked it.

## OR_CCR_123 is a CARD problem, not a profile problem

"The card you are trying to use is already being used for a transaction in a different
currency" means exactly that: the card carries a subscription priced in one currency while
the fee is quoted in another. Chasing it through the payments profile is wasted work:

- **Switching "Settings → Payments profile for Google Pay" made it WORSE.** Immediately
  after the switch the signup began answering `OR_AP_05 "An unexpected error has occurred"`
  at the payments-profile step and the wizard could not advance past the developer-name step
  at all — still stuck after 100 s. Reverting the setting fixed it. Do not touch that
  setting to solve a card error.
- **Releasing the card is usually impossible.** Removal answers *"Provide a primary payment
  method … currently used with one or more Google services"*, and there is **no
  set-as-primary control and no complete-card control** on any agent-reachable page
  (Payment methods, Subscriptions, Settings). `Close payments profile` exists in Settings but
  its dialog never opens on an agent surface.
- A **cancelled** subscription still holds the card until its paid period ENDS. One that has
  fully ended releases it; one cancelled-but-valid does not.

⇒ **The fix is to use a different card whose subscriptions have fully ended.** That worked
first try.

## The two gotchas at the buyflow

1. **The saved card appears to vanish.** After `Save card` the dialog re-renders as
   "Add credit or debit card" with no card listed — that view is STALE. Re-open the add-card
   entry and the saved card is there with a **Buy** button. Do not conclude the save failed;
   re-render before judging.
2. **Set the cardholder name to match the card.** Google prefills the account holder's name.
   If the card belongs to someone else, overwrite it: click the field, `do key --key a
   --mods ctrl`, then `do type`.

## Two more measured facts

- The wizard does **NOT** persist across a reload — a reload drops you back to the
  account-type chooser and every step must be redriven. Budget for that before reloading.
- The account-type chooser's first click is usually swallowed (Angular has not bound its
  handler yet). Click `button[debug-id="select-personal-account"]` TWICE, verifying between.
- On success Google shows only *"We've sent a receipt for the registration fee to
  <account>"*; the **order number is in that email, not on screen**. The developer account
  ID is in the console URL: `/console/u/0/developers/<ID>/app-list`.
