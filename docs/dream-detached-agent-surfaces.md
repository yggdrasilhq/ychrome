# Dream: an agent surface the operator never sees

**Field report from a headless host, 2026-07-31.** Written for the agent finishing the ychrome
work on dev. It is deliberately organised as *established / unresolved / how to
settle it*, because this investigation produced two confident wrong answers
before it produced a useful one, and the wrongness is the most transferable part.

Normative rule this serves: `yggterm/docs/agent-surface-attachment.md`.
User's ask, verbatim:

> *"The shadow sessions of agents need not connect to yggterm client GUI. This is
> already true for yRDP but I want it to be true for all libyggterm apps in
> general including ychrome… for many work it simply pollutes my Live sessions.
> Sometimes I do need to co-browse. For that attach is necessary."*

---

## 1. The shape ychrome is missing

Today there are two shapes, chosen by whether `YGGTERM_SESSION_ID` is set:
**standalone window**, or **take the calling session's viewport**. The shape an
agent wants is neither.

And note the trap the operator caught live: **`--no-activate` is not
detachment.** Spawning a session with `--no-activate` and anchoring a surface in
it keeps their *viewport* untouched — but a **row still appears in their session
list** for the life of the work. Two pollutions; only the first is solved.

> *"Btw, this ychrome still attached to yggterm GUI. No issues, and it attached
> in the bg not whooping my viewport. Just saying — shadow sessions may not
> always need to attach to the GUI if you are going to run it yourself anyway and
> have no reason to populate the yggterm Live session space."*

**The test is not "did the viewport move" but "does a row exist the human did not
ask for."**

The obstacle is structural rather than a missing flag: **the surface is CREATED
by the `open` OSC, and `open` is also what reveals it.** There is no action
meaning *exist but stay out of the session space*. So this is a yggterm protocol
addition — create-without-reveal, plus `attach`/`detach` to promote and demote —
not a ychrome argument.

---

## 2. ⛔ THE ONE CONSTRAINT THAT DECIDES WHETHER THIS IS USABLE AT ALL

**"Detached" must be built as HOST-HIDDEN, never as a hard stash.**

`injection_map_plan` (`yggterm/vendor/dioxus-desktop/src/web_surface.rs`) is the
truth table the whole drive path hangs off:

| surface state | `engine_hidden` | mapped | injection |
|---|---|---|---|
| revealed | false | yes | `Deliver` |
| **host-hidden** — never revealed / soft-stashed / backgrounded | **true** | no | **`WakeAndRehide`** — show the view *we* hid, verify it mapped, inject, re-hide ~400 ms after the last event |
| **hard-stashed / detached container** | false | no | **`Refuse` → `surface_not_mapped`** |

The host may wake **only what it hid itself**; a hard-stashed container has no
parent, so showing its webview would not map it.

**If detached lands on the hard-stash path it is un-driveable by construction.**
No verb work fixes that, because the refusal is correct. Agents will get a
surface they can read and cannot drive, and every one of them will conclude it
must reveal on the operator's screen — the precise pollution this feature exists
to remove, reintroduced by building the feature wrong.

---

## 3. What is ESTABLISHED (measured, on a never-revealed surface)

1. **`web ensure --session <path>` materialises an unregistered surface**
   (`reason: "healed"`). **`app open --client <shadow>` does NOT** — every verb
   keeps answering `session has no web surface`, which reads like a dead surface
   and is only an unregistered one. This cost real time; it deserves a better
   error at minimum.
2. **Eval-backed verbs work fine unrevealed** — `eval`, `await`, `read` read and
   write the DOM, measure rects, and run async bodies.
3. **A hidden page still LAYS OUT.** `document.visibilityState === "hidden"` and
   the target button measured **414x48**. So "hidden ⇒ no layout ⇒ zero-size
   rect" is **false** as a general claim.
4. **Form fill needs `execCommand('insertText')`**, not a native value-setter.
   The setter route leaves `.value` correct and the framework's model empty; the
   form then submits blank and resets. Values set via `execCommand` survived a
   subsequent click (12/26 chars still present).
5. **`"matched a zero-size element"` is a page-state answer** — a sibling of
   `detached_node` and `target_moved`. A modal, a spinner, a route
   mid-transition. Re-observe; do not conclude anything about the plane.

---

## 4. ✅ SETTLED — injection DOES land on an unrevealed surface

**Answered with a page-side readback on 2026-07-31, after two wrong answers.**

The control that settled it needed no credentials and no page of my own: any real
site with a button. On the Binance login page, on a surface **never revealed**:

```
document.visibilityState : "hidden"
button rect              : 343 x 48          ← real layout
web do click             : accepted:true, is_trusted:true
PAGE-SIDE READBACK       : hits:[{ trusted:true }]      ← a capture-phase
                                                          listener FIRED
```

**That is the standard to hold anything to here:** the page's own listener, not
the verb's success field. `injection_map_plan`'s `WakeAndRehide` path works in
the shipped build.

### How this question was got wrong twice — worth 60 seconds

- *Claim A:* "detached can't take a trusted click; hidden ⇒ no layout ⇒ 0x0 rect
  ⇒ **reveal it on the operator's screen**." Built from ONE
  `"matched a zero-size element"` refusal, no counterfactual. The button measured
  414x48. **A wrong plane-level diagnosis manufactures a reason to take the
  human's screen** — which is what this whole feature exists to prevent.
- *Claim B:* "proven — it works." The "proof" was `accepted:true,
  is_trusted:true`, i.e. the verb's own field, which pt14 records as *"the
  injector's ASSUMPTION, not an observation."* Right answer, worthless evidence.

Three unverified instruments in one session: a fetch/XHR tap that never recorded
my own `fetch` (and would never have seen a form POST anyway); a fill "verified"
by `.value.length`, which is the accepted-is-not-committed shape I was
documenting at the time; and the verb field above. **Verify the instrument before
the hypothesis.**

### ⚠ What is still open: does the click hit the INTENDED element?

In the clean run only a **document-level** capture listener recorded the hit; a
listener bound to the button itself did not fire in that same run. A follow-up
probe meant to compare aim-point against the button's rect was malformed and its
output is not trustworthy, so **I am not claiming a targeting bug — only that it
is unverified.** If it is real, the likely family is pt14's cause 2 (widget-local
coords versus an ancestor `event->window` under glass) or the
`css_viewport_to_widget` zoom/scroll mapping.

**The cheap decisive probe:** bind a capture listener that records
`{clientX, clientY, target.tagName, target.id}`, stash the button's
`getBoundingClientRect()` in the same script, click, and compare. Keep the two in
ONE eval so a re-render cannot invalidate the id between them — that is what
broke my attempt.

## 5. How to settle it (the experiment I could not finish)

Site-independent, ~5 minutes, and it isolates the plane from any site's
anti-automation behaviour:

1. Serve a page you own on loopback (`python3 -m http.server`, bind 127.0.0.1) —
   a single big button with a `click` listener pushing `{isTrusted, clientX,
   clientY}` into `window.hits`.
2. Bring it up on a surface **that is never revealed**; confirm
   `document.visibilityState === "hidden"` and that the button has a real rect.
3. `web do click --selector '#b'`.
4. Read back **`window.hits`**, not the verb's reply.

`hits.length === 1` with `trusted: true` settles it green, and that assertion
belongs in the suite as a permanent lock with a mutation (force
`InjectionMapPlan::Refuse` for `(true,false)` and watch it go red).

⛔ **I could not run it because there is no `web navigate` verb.** The web verb
set is `await batch capture-element close cookies devtools do ensure eval fill
fill-card fill-vault find frames lease read reload screenshot totp wait` — a
surface's URL can only be changed by relaunching the app that owns it, and
relaunching ychrome into the same profile re-attached to the existing tab instead
of loading the new URL. **That gap is worth closing on its own**: an agent that
cannot point a surface at a page of its own choosing cannot build a control, and
an agent that cannot build a control writes documents like my first two.

---

## 6. Related open item: IBKR could not be logged in

Attempted as the driving use case (rotate the IBKR **paper** password, per the
paper-only authority boundary). Reached the login form on a detached surface,
filled it, clicked — and the page never moved. **No credential was changed and
the paper-account reset was never attempted.**

Whether that is IBKR's anti-automation or our injection not landing is exactly
the unresolved question in §4, which is why §5 matters before any more time goes
into the site. Site lore: `.claude/skills/ychrome-site-lore/lore/interactivebrokers.co.in.md`
(two entries, the second retracting the first's mechanism).

---

## 7. Summary for the implementer

| item | state |
|---|---|
| create-without-reveal + `attach`/`detach` | **owed** — protocol addition in yggterm, not a ychrome flag |
| detached must be **host-hidden**, not hard-stashed | **hard constraint** — get this wrong and the feature is dead on arrival |
| `app open --client <shadow>` not registering a surface | **sharp edge**, wants a real error |
| no `web navigate` verb | **gap** — blocks building controls |
| does injection land on an unrevealed surface | ✅ **YES** — page-side listener fired, `isTrusted:true`, `visibilityState:"hidden"` |
| does the click hit the INTENDED element | ⚠ **unverified** — see §4, one run saw only a document-level hit |
| geometry epoch for adopt-mode resizes | **built in yRDP**, copy the shape (`yrdp repin` / `screenshot`'s `epoch` / `state`'s `geometry_stale`) |
