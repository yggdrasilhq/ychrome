# beehiiv.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## account-signup-headless-free-tier · WORKS
task: Register a newsletter account end to end headlessly on the free tier, no card, email + SMS verification, and prove the stored credential opens it from a cold profile
model: claude-opus-5
date: 2026-08-14
tags: 

Signup and first login driven end to end, headless, on the `ychrome ctl` engine (dev). No GUI
host touched, no operator involvement at any step. Roughly 12 minutes wall clock, most of it
waiting on a mail sync.

## The route

`app.beehiiv.com/signup` -> "Continue with email" -> a single form (firstName, lastName, email,
password, agree_to_terms) -> EMAIL code -> PHONE + SMS code -> persona -> publication name +
subdomain -> a marketing survey (has a Skip) -> a plan wall.

## The five things that cost time

1. **`/create-account` REDIRECTS TO `/login`.** The real path is `/signup`. The login page's own
   "Create one" link points at `/signup?plan=max` — following it pre-selects the paid plan, so
   go to bare `/signup`.

2. **`button[type="submit"]` MATCHES NOTHING on the first page.** `e.type` in a DOM read returns
   the PROPERTY, which defaults to "submit" for a `<button>` with no type attribute — so the
   attribute selector finds zero elements while your own probe says the button is a submit. Click
   the bare `button` instead. This is a general trap for any attribute selector built from a
   property read.

3. **The submit stays DISABLED and the reason is only in the form's TEXT.** Nothing sets
   `aria-invalid`, there is no `role=alert`, and no error node exists. The password policy renders
   as a checklist where met items carry a tick and unmet ones carry a bullet:
   `✓ Between 8 and 100 characters ... • 1 or more special characters`. ⇒ When a button will not
   enable, read `form.innerText` and diff the ticks. A vault password generated with
   `--no-symbols` fails this and nothing tells you.

4. **The phone field is `react-international-phone` and it FIGHTS a typed country code.** It is
   seeded with `+1 `, Ctrl+A does not clear it, and typing `+<cc><national-number>` is reformatted into
   `+1 (912) 345-6789` — a plausible-looking wrong number. ⇒ Click the flag button (the only
   non-submit `<button>`), `scrollIntoView` the `li[data-country="<iso2>"]` in the 218-row list, click
   it, THEN type the national digits. Verify the field reads `+<cc> <formatted national number>` before submitting.

5. **The publication-name field is PRE-SEEDED** with "<FirstName>'s Newsletter", so typing
   appends rather than replaces. Same for the subdomain, which auto-derives from the name. Clear
   both with the native-setter + `input`/`change` dispatch; plain typing produces
   ""<First>'s Newsletter<the text you typed>"".

## The plan wall — free is reachable, but not by the button that says so

Two options only: "Get started" (paid Max) and "Start 14-day trial". There is NO plain
"continue on the free plan" control. The trial is the free route: the page itself states **"No
credit card required"** and "After your trial, you'll be moved to our free Launch plan, good for
up to 2,500 subscribers". Taking it lands on the dashboard with the underlying plan ALREADY
Launch at $0/month and billing reading "last payment N/A, next payment due N/A". ⇒ Do not read
"trial" as "this will charge someone" — with no card stored, expiry just removes the Max
features.

## ⛔ Every new device is challenged — plan for it

After a correct password from a fresh profile, beehiiv asks for a **device confirmation code**,
6-7 uppercase alphanumerics (e.g. a 6-7 char alphanumeric), valid 60 minutes, emailed to the account address.
This fires on EVERY new browser profile, so it will fire for the human's own first login too.
⚠ The code is in the BODY, not the subject — unlike the signup code, whose subject is literally
""Confirmation code <the code>"".

## Verb notes

- **`ctl fill` answers `filled:"no-fields"` on a two-step login** where the password input does
  not exist yet. That is correct behaviour, not a vault failure: type the identifier, advance,
  and call `fill` again once the password field is in the DOM. It then fills both.
- **`ctl fill` lands the values but does NOT satisfy React** — the submit stays disabled until an
  `input`/`change` event is dispatched. Re-set each field through the native setter (set "", then
  set back) after filling.
- **`ctl eval` shares ONE global scope across calls**, so a second call declaring the same
  `const` dies with "Can't create duplicate variable". Wrap every script in an IIFE.
- **An auto-submitting code field makes a SUCCESSFUL `input` report 409** ("the page kept none of
  them") because the readback happens after the navigation the typing caused. Check
  `location.href` before believing it.
