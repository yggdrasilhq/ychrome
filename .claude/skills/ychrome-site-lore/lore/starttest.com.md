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


## prepexam-console-read-the-client-not-the-screen · WORKS
task: Measure the whiteboard, the low-time alert, the minimise controls and the keyboard map
model: claude-opus-5
date: 2026-08-09
tags: measurement, jquery, iframes

⭐⭐ **The single biggest lever on this site: the delivery client's own function bodies are
readable, and they are a better measurement than the screen.** `String(window.someFn)` returns
the source. That turned three separate "we would have to reproduce the condition" problems into
one eval each:

- **A timed alert you would otherwise wait for.** `timeRemainingWarning()` contains its own
  message, title and button list as literals — *and* you can then **call it** to render and
  measure the real dialog. No need to drain 15 minutes of section clock to see a 5-minute
  warning.
- **State machines.** `displayWhiteboard()` / `closeWhiteboard()` / `toggleTimeCell()` state
  their guards outright (`if(vPopUpOpen || vAlertOpen || clockwarning == 1) return;`), which is
  the behaviour you would otherwise infer from failed clicks.
- **`Object.keys(window).filter(k=>/…/i.test(k))` is the index.** One regex over the globals
  found the whiteboard, the calculator, the minimise pair and every timer verb.

⛔ **DO NOT TRUST THE SITE'S OWN HELP TEXT AS A MEASUREMENT.** Two of four behaviours documented
in this client's Help are wrong about the client. Help says the timer minimises "by clicking on
them"; `$._data(el,"events")` shows **no handler on the timer at all** — only on the icon
button beside it. Help says "Alt + underlined shortcut letter"; there is **no `accesskey`
attribute in the document**, the underline is a `<span class="underline">`, and the shortcut is
a `keyCode` comparison inside the client's own handler.

⭐ **`$._data(element,"events")` is the instrument that settles "what is actually clickable".**
jQuery keeps its bindings off-DOM, so `onclick`, `[accesskey]` and inline attributes all read
empty on a page that is fully wired. Enumerate candidates and ask jQuery, rather than clicking
around to find out.

⭐ **A hidden control's markup is readable while it is hidden.** Zero-size buttons keep their
`innerHTML`, so one `document.querySelectorAll(".underline")` sweep yielded the shortcut letters
for **fifteen** controls across screens this session never visited (break, score report, section
review). Harvest the whole vocabulary from one screen.

## Driving notes that cost time this session

- ⛔ **`ychrome ctl input` has no `mousedown`/`mouseup`** — the types are `click|move|scroll|
  type|key`. A jQuery-UI **drag** therefore cannot be done with `ctl input`; dispatch
  `new MouseEvent("mousedown"…)` on the handle and then `mousemove`/`mouseup` **on `document`**
  (that is where jQuery UI listens), with `buttons:1` on everything but the mouseup.
- ⚠ **A `ctl input` click dispatches three events**, so a control whose handler is a TOGGLE can
  end up back where it started. Seen once on `#Whiteboard`: the click after a close reported
  `isWhiteboardToggled:false`. Re-clicking worked. **Read the toggle flag back and re-click**
  rather than concluding the button is broken.
- **Inside `wbFrame`, use the frame's own `PointerEvent`/`MouseEvent` and `view`** — the same
  rule as `ElementDisplayFrame`. Sending `pointerdown`+`mousedown` to the canvas, then
  `pointermove`/`pointerup` to the frame's `document`, drew a real stroke (Undo and Clear went
  from `disabled` to enabled, which is the cheap proof it registered).
- **jQuery-UI handles can exist and still be unreachable.** `#whiteboardContainer` gets
  `.ui-resizable-e/s/se` children, yet each computes to `800x0` with `cursor:auto` because the
  resizable stylesheet is not loaded. ⇒ **measure a handle's computed box before reporting a
  feature as present.** The same trap in a different shape as Help's `ui-resizable` class with
  no handles at all.
