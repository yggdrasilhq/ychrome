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
   `+1 (912) 345-6789` — the country code is swallowed into the US area code and you are left with a
   plausible-looking wrong number (example digits invented). ⇒ Click the flag button (the only
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

## post-editor-navigation-and-publish-gate · WORKS
task: Find the reliable route to a blank post editor, and establish whether the free tier can publish with no payment account connected
model: claude-opus-5
date: 2026-08-14
tags: 

Navigation traps found while driving a fresh account. Both cost a wrong instruction that would
have stranded a human mid-task.

## ⛔ "Start writing" on the Posts page does NOT open an editor

On a new publication the empty Posts page offers a single **Start writing** button. It routes to
`/posts/template-library` — the template *manager*. On a new account that page has **no templates
and no start-from-scratch control**, so it is a dead end. Measured twice.

**The controls that DO open a blank post editor:**

- **`New`** in the top-left of the dashboard — opens `/posts/<uuid>/edit` directly. This is the
  one to put in any human-facing instruction.
- `/posts/new` as a direct URL — same result.

⚠ Both *create a draft immediately*, before you type anything. There is no "unsaved" state to
back out of, so a navigation probe leaves an artefact. Delete it via the row's overflow button on
the Posts list → `Delete` → confirm; the confirm dialog's own wording is "This is a permanent
action that cannot be undone."

## The editor is a five-step wizard, and Publish lives at the end

`Compose → Audience → Email → Web → Review`. Useful facts for anyone automating up to a gate:

- **Audience** offers `Email and web` / `Email only` / `Web only`, plus tier and segment selection.
- **Review** renders `Publish to Email and Web` and `Schedule`. ⛔ On a free-tier account with **no
  payment account connected**, both are **enabled** — a payment account gates monetisation
  (paid recommendations, ads, payouts), not publishing. Confirmed against the platform's own
  Payment accounts page, which describes itself in exactly those terms.
- ⚠ So a saved draft parks one enabled click away from a public post plus an email send. If you
  stage a draft for a human to approve, know that nothing further protects it.

## Reaching the editor by URL then describing a button is how the instruction went wrong

The first written procedure said "Posts → Start writing", because the author had reached the
editor via the direct URL and then narrated the visible button without following it. ⇒ **If an
instruction will be executed by a human, walk the path you are about to write, in the order you
are about to write it.**

## load-an-article-into-the-post-editor · WORKS
task: Load a finished article into the beehiiv post editor with its quotes and code blocks intact
model: claude-opus-5
date: 2026-08-14
tags: 

Loading a finished article into the post editor. The route works, but pasting HTML LOSES
STRUCTURE SILENTLY, and the loss is invisible to every obvious check.

## ⛔ The editor's schema has no `blockquote`

Pasting `<blockquote><p>…</p></blockquote>` into the TipTap surface keeps the words and throws
away the quote. No error, no warning. The schema (read live off the editor instance) has no
`blockquote` node at all — it carries a custom trio instead:

    blockquoteFigure   content = "quote quoteCaption"     (BOTH required, in that order)
    quote              content = "(paragraph|bulletList|orderedList)+"
    quoteCaption       content = "text*"                  (may be empty)

`<hr>` is dropped the same way; the node exists but is called `horizontalRule`.
Code blocks, headings and inline `code`/bold/italic DO survive an HTML paste.

⚠ **The check that misses it:** `innerText.length` was **9346 before the fix and 9346 after** —
identical, because only the containers were lost. Word count, a text diff and reading the prose
all pass. **Count the structural nodes you sent**, not the characters.

## ✅ The route that works: build the document as ProseMirror JSON

Reach the editor instance by walking the React fiber up from `.tiptap.ProseMirror`, looking for
`memoizedProps.editor` with a `.schema`. Stash it (`window.__ed`), read
`Object.keys(editor.schema.nodes)` to learn what the platform actually supports, build the doc
against those names, then `editor.commands.setContent(doc, true)`. Verify by counting
`[data-type=blockquoteFigure]` in the DOM afterwards.

Carry long content in as **base64** and decode page-side — it removes every shell/JSON quoting
hazard from prose containing quotes, em dashes and apostrophes.

## ⚠ A React field readback in the same tick reports a failure that did not happen

Setting the title via the native setter + `input`/`change` and reading `t.value` in the SAME
eval returned the seeded `"New post"`. Read again two seconds later and it was the real title —
the write had landed and React simply had not committed when the readback ran. The usual law
here is "never trust a verb's own success field"; this is the inverse and it costs a pointless
re-run. **Re-read after a tick before concluding a React write failed.**

## Navigation and state

- `/posts/new` opens `/posts/<uuid>/edit` and CREATES THE DRAFT IMMEDIATELY.
- The editor header reports save state as `Draft | Synced <n> words` — that string is the
  honest indicator; confirm persistence with a reload before walking away.
- Session persists in the profile jar across runs: reopening `app.beehiiv.com` on a profile that
  logged in earlier raised NO device-confirmation challenge.
