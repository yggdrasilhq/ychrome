# Dream: control surfaces for browsing without the operator

ychrome was built bottom-up to give an agent unusual control over a real browser —
a native webview it can read, script, resize, route, and screenshot. This doc is
the forward look: the control surfaces that would let an agent **get the
information without the operator's assistance**, the same ground-up re-imagining
that produced agentic browsing in the first place.

It is grounded, not speculative. Every item below is a place that live co-browse run
actually stopped and needed a human, or forced a hand-rolled workaround
that should be a first-class verb (live run: 2026-07-18, on the live desktop host,
driving example.com and a Facebook login under a `meta` profile). The measure of
success for each surface: one fewer reason to tap the operator on the shoulder.

## What already works (the substrate to build on)

- **Read/script the page** — `app web eval '(function(){…})()'` runs JS in the
  page DOM and returns a value. Proven: read `document.title`, enumerate inputs,
  and commit a value into a React-controlled field via the prototype value setter
  + an `input` event (a bare `.value=` does not stick).
- **See the page** — `app screenshot --backend os` is the faithful pixel (default
  backends are blind to the native webview); `app web screenshot` grabs the page.
- **Route a tab** — the host daemon + `ychrome <url>` opens/routes a tab into a
  surface anchored elsewhere; identity is isolated per profile.
- **Isolate identity** — one profile per persona (`meta`, `itr`, a bank) keeps
  cookies and logins apart.

Those are primitives. The dream is the layer above them — the verbs an agent
reaches for by intent, not by hand-writing DOM plumbing every run.

## The friction map (where a run stops for a human)

| Stop | What happened live | Re-imagined surface |
|---|---|---|
| Vault locked | Could not read Meta creds; the master password is the operator's | **Unlock request** (§1) |
| Surface didn't open | The web-surface OSC is deferred while the GUI window is backgrounded; had to foreground first | **Headless surface-create** (§2) |
| 2FA / OTP | An SMS/authenticator code lands on the operator's phone | **OTP from the data-fabric** (§3) |
| CAPTCHA / passkey tap | A human-presence challenge the agent may never auto-solve | **Human handoff** (§4) |
| "Read the page" | Hand-wrote `querySelectorAll` maps every time | **Extract surface** (§5) |
| "Fill the login" | Hand-wrote the prototype-setter fill | **Autofill-from-vault** (§6) |
| Capture for a case | No one verb to snapshot page-as-evidence | **Evidence capture** (§7) |
| Splits overflow | A surface can overflow its pane on cold first paint | **Always-clamped surfaces** (§8) |

## 1. Unlock request — turn a hard stop into a one-tap grant

The master password is, and stays, the operator's to type (stdin only; an agent
must never hold or auto-consent it). But today "vault is locked" is a dead end: the
run halts and the agent has to compose a message and wait. Re-imagine it as a
**request the agent can raise and the operator answers in one gesture**:

- `ychrome-vault request-unlock --reason "<what for>" --scope <profile|host>` posts
  a request the GUI renders as a push-notification / titlebar prompt. The operator
  taps it, a secure prompt takes the password (still stdin-equivalent, never
  through the agent), and the agent's next `status` poll sees `state: unlocked`.
- **Time-boxed and scoped**: the grant can carry a TTL and a scope ("unlocked for
  the `meta` profile for 30 min"), so a co-browse run gets exactly the authority it
  needs and it lapses on its own.
- The agent never sees the password; it sees only the state transition. The human
  stays in the loop for the secret, but the loop is one tap instead of a paragraph
  and a wait.

## 2. Headless surface-create — the OSC must not depend on window focus

Live, the web-surface OSC that opens a native webview is **deferred while the GUI
window is backgrounded** — the surface only appeared after a screenshot forced the
window forward. An agent driving from ssh cannot assume the window is focused. The
daemon + command queue already exist (routing rides them); extend them so a surface
**create** is queued and drained regardless of window state, exactly as a route is.
`ychrome <url>` should reliably yield a live, registered surface whether or not a
human is looking at the screen. Focus is a rendering concern, not a control one.

## 3. OTP from the data-fabric — the single biggest unassisted win

Most Indian logins that matter for the triage work (banks, income-tax, and Meta
checkpoints) send a one-time code by **SMS**, and those SMS already land in the
personal data-fabric (`~/data/androidfs/<u>/SMS-import-export`, and WhatsApp for
some services). The operator's phone is not actually a required human — the code is
already in a store the agent can read.

Re-imagine an **OTP resolver** wired into the co-browse login flow:

- After submitting a login the agent expects a code, it calls
  `otp-wait --source sms --from <sender-pattern> --since <t> --timeout 60s`, which
  polls the SMS store for a fresh code matching the sender/format, and returns it.
- The agent injects it into the OTP field (same commit-to-state fill as §6).
- TOTP authenticator codes are already covered directly — `ychrome-vault totp
  <item>` generates them locally. The new piece is the **SMS/inbound** path.

This turns "the code is on my phone, wait for me" into a self-service step. It is
the clearest example of the dream's definition: information the agent can get
itself instead of asking. (Guardrail: read-only against the fabric; the resolver
returns the code, it never sends or deletes anything.)

## 4. Human handoff — make the unavoidable human gate cheap

A CAPTCHA and a passkey user-presence tap are gates an agent **may never**
auto-solve. Today hitting one is an ugly stall. Re-imagine a **handoff surface**:
the agent calls `handoff --reason captcha` (or the shell detects the challenge),
which (a) surfaces the exact challenge to the operator — a cropped screenshot of
the widget plus a one-line instruction, via push-notification — and (b) **polls the
DOM for the post-challenge state and auto-resumes** the run the instant the human
clears it. The human does the 3 seconds only they can do; the agent does everything
before and after. No babysitting, no "tell me when you're done."

## 5. Extract surface — structured reads, not hand-rolled DOM maps

Every run I hand-wrote `[...document.querySelectorAll(...)].map(...)` to read the
page. That plumbing should be a verb. `app web extract --mode <…>`:

- `readable` — the article/main text, boilerplate stripped (a built-in readability
  pass), for "what does this page say".
- `tables` — every `<table>` (and ARIA grid) as JSON rows, for statements, order
  histories, AIS/26AS pulls.
- `forms` — a **form map**: each fillable field with a *stable* handle (name, id,
  aria-label, label text, the CSS path as a fallback), its type, current value, and
  validation/disabled state. This is what §6 fills against, and what a site-lore
  entry should record.
- `links` — the link inventory with visible text, for navigation planning.

One verb, deterministic JSON out. Site-specific quirks (a field keyed by `name` not
`id`, a button disabled until blur) get logged to the **site-lore** skill so the
next agent starts from the map instead of rebuilding it.

## 6. Autofill-from-vault — the login as one intent

The login dance — resolve the credential, find the fields, commit values that stick
to the framework's state, stop before submit — is the same shape on every site and
was hand-assembled live. Make it a verb: `app web autofill --host <h>`:

- Resolves the **one** vault entry `ychrome-vault match <host>` permits (strict
  match; never the alphabetically-first of several — that latent footgun is called
  out in the vault skill).
- Uses the §5 form map to locate username/password/OTP fields.
- Fills via the prototype-setter + `input`/`change` events, so React/Dioxus
  controlled inputs actually commit and the submit button enables.
- **Stops before submit by default** — a state-changing action on a real account
  is operator-confirmed, same stance as a vault write. `--submit` is explicit.
- The password reaches the page only as the injected eval, never a literal in a
  command line, a log, or site-lore — the existing "no secret in a schema" rule,
  extended to autofill.

## 7. Evidence capture — page-as-evidence in one verb

The grievance/Meta co-browse feeds **dossierGraph**, where a screenshot alone is
weak evidence. Re-imagine `app web capture --case <id>`: one verb snapshots the
current page as a court-usable bundle — full-page PNG, the rendered DOM HTML, the
final URL (post-redirect), an ISO timestamp, and optionally an MHTML/WACZ archive
that replays offline — and drops it straight into the dossierGraph intake with
provenance. Capturing a filed complaint's acknowledgement, a bank dispute's status,
or a Meta message thread becomes a single reproducible step instead of a manual
screenshot-and-file.

## 8. Always-clamped surfaces — never overflow, so splits are safe

A surface born from a single `[data-ws-page]` rect sample can be measured before the
layout settles and briefly overflow its pane on cold first paint (the old
GTK-webkit look). For the operator's stated goal — several ychromes in window-like
4-pane splits — a surface must **never** overflow its allotment. The fix is
create-on-stable: don't build the webview until the page-area rect is identical
across two consecutive reconcile measurements, and clamp every applied rect to the
window bounds. A surface that is always exactly its pane is the precondition for
trusting multi-pane co-browsing.

## Sequencing (rough)

1. **§3 OTP-from-fabric** and **§6 autofill** — the two that most directly remove
   the operator from the common login path. §5 extract is their shared dependency.
2. **§1 unlock request** and **§4 human handoff** — turn the two unavoidable human
   gates into one-tap, auto-resuming steps.
3. **§2 headless create** and **§8 clamped surfaces** — substrate reliability that
   everything else rides on.
4. **§7 evidence capture** — the dossierGraph payoff verb.

None of these ask the browser to do anything a human at the keyboard could not; they
ask ychrome to let the *agent* do those same things, and to reserve the human for
exactly the moments — a secret, a presence check — that are genuinely theirs.
