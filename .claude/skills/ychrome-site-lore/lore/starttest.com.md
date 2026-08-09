# starttest.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## prepexam-official-practice-exam-console · WORKS
task: Reach and drive the ITS PREPEXAM delivery client to measure its Help surface
model: claude-opus-5
date: 2026-08-09
tags: 

The PREPEXAM Official Practice Exams are delivered by **Internet Testing Systems (ITS)**, not
Pearson VUE. Reaching the delivery client is a three-hop chain and only the first hop is a
login.

## Getting in

1. `https://www.mba.com/service/login` — fields `#txtEmailAddress` / `#txtPassword`, submit
   `#btnlogin`. `ychrome ctl fill entry=<the mba.com vault item>` fills both and reports the
   per-field byte counts; read the DOM back anyway. Submit with a real engine click on the
   button's measured centre — the form posts, then bounces
   `login → login-redirect → /my-account`. Poll for the URL to leave `/login`; it takes 3-6 s.
2. On `/my-account` the link *Access PREPEXAM Official Prep* points at `http://prepexam.example.test/`,
   which redirects into `starttest.com/starttest2/13.2/router?...&cmd=HomePage`. Navigating
   straight to that href in the same page works; the SSO rides on cookies.
3. ⛔ **The Resume/Start control does NOT navigate.** It is
   `<a href="#OnClickHandledByScript" class="launchitdwindow" url="router?...&cmd=ITDRestartTest">`
   and its handler opens a NEW WINDOW, which the engine does not surface as a page. Clicking it
   looks like nothing happened. **Read the anchor's `url` ATTRIBUTE and `goto` it yourself** —
   that lands in the delivery client (`ITDVersions/24.3.0.0/ITDStart.aspx?...&res=1280x900`).
   The `code` in that URL is minted per render, so re-read the attribute rather than reusing one.

## Driving the delivery client (ITD 24.3.0.0)

The chrome is a single document: `#InfoPanelFrameID` (header), `#ElementDisplayFrameID` holding
the `ElementDisplayFrame` iframe, `#ControlPanelFrameID` (footer). Three more zero-size frames
exist and matter: `infoPopUpFrame`, `wbFrame` (whiteboard), `VariableFrame`.

- **Footer controls are plain top-level buttons** — `#Help`, `#Pause`, `#Save`, `#Next`,
  `#Return`, `#EndExam`, `#Review`. Inactive ones are present at zero size, so the DOM lists the
  whole control vocabulary of every screen without visiting them.
- ✅ **Engine coordinate clicks (`ctl input`) DO fire the footer handlers.** Older notes from the
  yggterm web-surface plane say selector clicks on `.cpButton` do not work; that is a different
  driver. `ychrome ctl input events='[{"type":"click","x":..,"y":..}]'` at a button's measured
  centre worked for `#Help` and `#Pause` first try.
- ⚠ **Coordinate clicks still do not reach INSIDE `ElementDisplayFrame`.** For anything in the
  item area, dispatch the full pointer sequence built with the *frame's own* `PointerEvent` /
  `MouseEvent` constructors and `view`, on the element in `contentDocument`. A bare `el.click()`
  ticks a radio visually without the client registering the response.
- **The client exposes its own globals, and they are the reliable rung**: `displayHelp()`,
  `closeHelp()`, `isHelpToggled`, `nextConfirm()` (advances an item), `showInfoPopUpTool()`,
  `closeAllPopups()`. `Object.keys(window).filter(k=>/help|popup|info/i.test(k))` enumerates them.
- ⚠ **`ctl eval` shares one global scope across calls**, so a second script declaring `const h`
  fails with *"Can't create duplicate variable"*. Wrap every eval in an IIFE.

## Measuring without spending anything

Takes are **"1 of unlimited"** on Practice Exam 1, so a retake costs only score history — the
scarcity worry does not apply to that product. But *"you will be limited to one active exam take
at a time"*, so a fresh take cannot start while one is open, and `#EndExam` is present-but-zero-
size on the question and pause screens.

⭐ **`Pause` STOPS the section clock** (verified twice 12 s apart, unchanged). That is how you
leave a take frozen for the next session instead of letting a live 45-minute timer drain while
nobody is driving. Pause first, then close the page.

## Boundaries

- The account holds a real PREPEXAM registration. Reads are fine; an **account write** (the forced
  password change this account once presented) is the owner's call, not an agent's.
- **Exam 2 stays pristine** by owner instruction — it is his genuine diagnostic. Only the
  already-discarded take on Exam 1 is measurable.
- No exam content leaves the session: measure geometry, tokens and control vocabulary, never
  items, and never commit a screenshot with an item in it.
