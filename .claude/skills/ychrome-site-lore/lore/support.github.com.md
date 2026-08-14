# support.github.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## file-repository-ticket-on-agent-engine · PARTIAL
task: File a repositories support ticket end to end, headless on the ychrome agent engine
model: claude-opus-5
date: 2026-08-01
tags: github, support, ticket, agent-engine, totp, free-plan-deflection

Filed a repository support ticket end to end on the **agent engine** (`ychrome ctl`), fully
headless. No yggterm session, no surface, no shadow client, nothing on the operator's screen.

## The engine is the right tool here, and it is genuinely detached

Probe with a REAL verb (`ctl --help` exits 0 on old binaries and lies):

    ychrome ctl pool --json      # a daemon answer => new binary + live engine

Isolation proof, worth repeating before any run on a GUI host: the engine's page process
carries `DISPLAY=:90 GDK_BACKEND=x11` with **no** `WAYLAND_DISPLAY`, and `DISPLAY=:90
xwininfo -root -children` shows the only `ychrome` windows living on that Xvfb. The
operator's `wayland-0` surfaces are untouched.

    p=$(ychrome ctl open url=https://github.com/login profile=<per-agent> | jget page_id)

## Login: password + TOTP, both landed with trusted input

Fields are plain server-rendered inputs: `#login_field`, `#password`, submit with
`{"type":"key","key":"Return"}`. Then GitHub routes to
`/sessions/two-factor/app` where the field is `#app_totp`; entering six digits
**auto-submits**, no Verify click needed. Whole login is ~4 verbs.

⛔ **`ychrome ctl fill` does not exist** — the engine has no `/engine/fill` route despite
`docs/agent-engine.md` §4 listing `POST /fill`. So vault autofill is unavailable on the
engine plane. Keep the secret out of argv and out of the transcript by POSTing the events
body straight to the daemon socket from a 0600 file:

    ychrome-vault get <entry> <user> --field password | python3 -c '...json.dumps...' > body.json
    curl -s --unix-socket ~/.yggterm/ychrome/daemon.sock -X POST http://localhost/engine/input \
      -H 'Content-Type: application/json' --data-binary @body.json

Same shape for the TOTP, done as ONE remote command so the code cannot expire mid-flight.
Read back page-side by **length only** (`value.length > 8`), never the value.

## Two input traps that cost real time

1. **`{"type":"type"}` does not deliver newlines.** A body containing `\n\n` stops dead at
   the first newline (405 of 629 chars landed); Enter is intercepted by the comment
   component. It did NOT submit anything prematurely, but compose **single-paragraph** text
   or the tail is silently dropped. Always compare the field length to the intended length.
2. **The multi-step form persists in `sessionStorage`** under
   `contact-next:<category>-form:contact_next[<field>]`. Navigate away, come back, retype,
   and you get the value **twice** (body doubled to exactly 2x460 chars). Clear first with
   `{"key":"a","mods":["ctrl"]}` then `{"key":"BackSpace"}`, then verify byte-for-byte
   against the intended string. `sessionStorage` is also the honest place to confirm the
   whole draft survived the route change before hitting Submit.

## ⛔⛔ THE DOM SNAPSHOT DOES NOT SHOW CONDITIONAL SUB-FORMS — SCREENSHOT BEFORE SUBMIT

`ctl dom mode=snapshot` taken before a radio is chosen cannot show the fields that radio
unfolds. Choosing **Deletes** on the Repositories form unfolds a repository *deletion*
sub-form: "What is the URL of the repository you would like to delete?" (required) plus
"Please confirm your action. Once the repository is purged, it cannot be restored." with
Delete / Don't Delete. A ticket about deleting unreachable *objects* would have been filed
as a request to delete the *repositories*. Only `ctl shot region=full prescroll=true`
revealed it. **Screenshot the filled form before every submit on an unfamiliar site.**

## The contact taxonomy (2026-08-01) — there is no "Other"

Six top-level categories only, identical for a personal account and an org: Copilot,
Codespaces, Repositories, Education, Sign-in issues, Billing and payments. Repositories
offers nine topics, and none is a catch-all:

| topic | required extras |
|---|---|
| Deletes | repo URL to delete + irreversible purge confirm ⛔ |
| Repository features | a feature checkbox (Templates/Releases/Insights/Branches) + URL |
| Log Request | repo + date range + public/private |
| Migration and locked state | migration method + locked repo URL |
| Restoration | repo name + 90-day window + private? |
| Remove LFS Objects | repo + "can you delete and recreate?" |
| **Repository Access Issues** | **one neutral field: "the affected repository"** |

`Repository Access Issues` is the least-lying choice for anything that is not one of the
other eight: its single extra field is neutrally worded and nothing destructive is implied.
Final step adds a required "Type of Issue" `<select>` (`low` = general question/feature
request, `normal` = errors/problems); set it with the native `HTMLSelectElement.prototype`
value setter plus `input`+`change`, then confirm Submit un-disables.

## ⛔ THE ACTUAL FINDING: free-plan tickets are auto-closed by a bot in seconds

A ticket filed through the normal contact form for a **Free User Plan** org came back
**Closed** within seconds, with an automated reply signed "Cora" pointing at Community
Discussions / Docs / Skills and saying the resources "cover most technical questions ... for
free-plan users". The submission itself was perfect; nothing about the form was wrong.

**The route that demonstrably reaches a human on this plan is the
"Clear cached views with our Virtual Agent" button at the top of the Repositories form.**
Two earlier tickets created that way ("Clear Cached Views") were answered by named support
engineers who ran the reference check, removed `refs/pull/N/head` + `refs/pull/N/merge`
references and ran cache clearance so the old commit URLs 404. Same account, same class of
request, opposite outcome. If the goal is GC / cached-view removal after a history rewrite,
**start at the Virtual Agent, not the contact form.**

A closed ticket is not a dead end: the detail page carries a `Reopen and comment` button
(textarea `#js-comment-<ticketid>`), and using it flips the ticket back to **Open** and adds
the comment. Verified live.

## Reading the result

Ticket list at `/tickets` (redirects to `/tickets/personal/0`) lists every ticket including
org ones, as `#<id> <status> created for <account>`; detail at `/ticket/personal/0/<id>`.
`document.body.innerText` is enough — no need for a screenshot to read the ID.

## virtual-agent-vs-copilot-and-the-status-banner · PARTIAL
task: Reach the Clear Cached Views virtual agent to request GC on nine repos
model: claude-opus-5
date: 2026-08-01
tags: github, support, virtual-agent, copilot, status-incident, git-tags

Second pass on the same job, after the contact-form ticket was auto-closed. Two support
"assistants" exist and **they are not the same thing** - that confusion cost a full
investigation.

## The two assistants, and which one can actually open a ticket

| surface | what it is | can it file a ticket? |
|---|---|---|
| `<virtual-agent-container>` on the Repositories contact form ("Clear cached views with our Virtual Agent") | guided flow, writes a ticket titled **Clear Cached Views** with a fixed body (repo URL, offending SHA, customer justification) | **YES** - this is the route that reached human engineers on a Free plan |
| `https://support.github.com/copilot` ("Copilot in GitHub Support") | RAG docs assistant over GitHub Docs, cites sources | **NO** - it answers with documentation and points back at the portal |

So "ask the assistant to file it" fails if you land on `/copilot`. Only the contact-form
widget writes tickets.

## ⛔ The Virtual Agent widget hides itself when its backend is degraded

Symptom: the blue "Clear cached views" button simply is not on the page. Not greyed, not
erroring - **absent**. `ctl dom` still lists a `SUMMARY|with our Virtual Agent`, so the
element exists; it is the `<details class="details-reset details-dialog-close ...">` that is
`display:none`, inside a `<virtual-agent-container>` custom element whose ancestors are all
visible and sized. Its innerText already contains the string "Sorry, a connection error
occurred" **at all times** - that is template text, not evidence of an error, and reading it
as one sends you the wrong way.

Diagnosis that actually worked, in order:

1. `input` refused with `no_hittable_match ... 1 zero_size_element` - a page-state answer,
   per the standing rule, not a reason to reveal anything.
2. Walking `getComputedStyle` up the ancestor chain pinned it to the `<details>` itself.
3. A `window.fetch` / `XMLHttpRequest.prototype.open` / `WebSocket` interceptor installed
   page-side, then forcing `details.open = true`, logged **zero requests**. The widget was
   not failing a call, it was declining to start.
4. `https://support.github.com/copilot` renders a **GitHub status banner**, and that is
   where the answer was: *"Copilot AI Model Providers is currently status yellow."* The
   timestamp matched the minute the widget stopped rendering, and it had rendered fine
   ~25 minutes earlier in the same session and profile.

**Lesson: when a GitHub Support widget vanishes, read the status banner on `/copilot`
before suspecting the engine, adblock or WebKit.** Nothing local was wrong. The resource
timing entries were a red herring too - `transferSize: 0` across almost every asset just
means cache hits, not blocked requests.

## Framing decides whether the request is actioned

Copilot's own answer, quoting the docs: Support will dereference or delete PR refs, run
server-side GC, remove cached views and purge orphaned LFS objects - but **"GitHub Support
only assists with removal of sensitive data"**, and the docs do not promise GC for
non-sensitive commits merely because they are still fetchable by SHA. The tickets in this
account that succeeded named the exposure explicitly; the one that was auto-closed did not.
Name the exposed data class.

## Gathering the fields WITHOUT a browser (rung 1, and much better evidence)

The guided flow wants repo + offending SHA + justification. Get all of it from the API:

    # every force-push, with the pre-rewrite SHA, straight from the activity log
    gh api "/repos/OWNER/REPO/activity?per_page=20" \
      --jq '.[] | select(.activity_type=="force_push") | "\(.timestamp) \(.ref) before=\(.before)"'

    # is that object still fetchable?  200 = yes, 422 = already collected
    gh api "/repos/OWNER/REPO/commits/<sha>"

    # is it truly detached?  "No common ancestor" is the strongest possible answer
    gh api "/repos/OWNER/REPO/compare/<sha>...main" --jq '.status'

`422` is the confirmation signal that a previous clearance worked - a SHA cleared on an
earlier ticket flipped from 200 to 422, which is how you prove the operation succeeded.

⛔ **Checking whether a tag keeps old objects alive: peel the tag first.** `git ls-remote
--tags` returns the **annotated tag object** SHA, and the commit is on the companion
`^{}` line. Filtering those out with `grep -v '\^{}'` and testing the remaining SHAs
reports almost every tag as "not in the new graph", which is a **false alarm** - I produced
one and had to retract it. Peel, then test with `git merge-base <commit> origin/HEAD` in a
`--filter=blob:none --no-checkout` clone: no merge base means the tag really does point into
the pre-rewrite graph and will block collection. Corrected result on a 191-tag repo:
**189 connected, 1 genuinely disconnected**, not the 172 the unpeeled test claimed.

## One more input trap on the ticket thread

The reply box offers **"Comment and close"** and **"Comment"**, in that document order. A
prefix matcher on `"Comment"` hits **"Comment and close"** first and closes the ticket you
were trying to keep open. Match the trimmed text with `===`, and assert the tagged element's
own text back before clicking.

Long multi-line comments cannot be typed (`type` drops everything after the first newline).
Set them with the `HTMLTextAreaElement.prototype` value setter plus `input`+`change`; the
comment box is a plain named textarea, so the posted body is the DOM value. Verify by length
and by head/tail slices before clicking.

## a-closed-ticket-can-be-closed-with-THEIR-question-unanswered · WORKS
task: Continue an existing support request rather than opening a duplicate
model: claude-opus-5
date: 2026-08-14
tags: github, support, ticket, reopen, triage, stale-claim

## ⛔⛔ THE FINDING: `Closed` DOES NOT MEAN ANSWERED, AND IT DOES NOT MEAN DEFLECTED

A ticket in the list rendering as **closed** was carried in two separate notes as *"bounced
with a form reply, nothing happened"*. Opening it showed **seven comments**, of which one is
a **named support engineer** who had already run the reference check, reported exactly which
pull requests still held references, and **asked which of two remedies was preferred.**

⇒ **The request had not been refused. It had been answered with a question, and the question
was never answered, so it aged into `closed`.** Everything downstream — "already filed", "do
not re-file", "nothing was ever actioned" — was written by readers who trusted the status
word and the first automated reply, and never opened the thread.

⭐ **So: read the whole thread before concluding anything about a ticket, and especially
before filing a second one.** The list view shows a status; the status is a summary of the
last event, not of the state. Cheap: one `goto` + `document.body.innerText` is the entire
thread including every comment.

⭐ **And the same page proves whether a past request WORKED.** A SHA named in an older ticket
now answering `422` instead of `200` is the receipt that a clearance actually ran. Check that
before claiming the route does not work.

## Reopening is one action and preserves the thread

`Closed` tickets render a single control, `Reopen and comment`, over the textarea
`#js-comment-<ticketid>` (name `message`). Posting flips the ticket to **Open** and adds the
comment. ⚠ Once open, the controls become `Close ticket` and `Comment` — the prefix trap
named further up this file is only present in that state, not on the closed one.

⇒ Answering the engineer's outstanding question inside the existing thread is strictly better
than a new ticket: it keeps their own reference-check work, and a duplicate re-enters triage
from zero.

## ⛔ EMAILED REQUESTS ARE DECLINED OUTRIGHT — the portal is the only door

Replying by email, or mailing support directly, returns **"IMPORTANT: Support Ticket
Declined"** with the text *"we now require that new support requests be created using our
Support website"*. ⚠ Nothing is created. **A decline mail in the inbox is easy to mistake for
a filed-then-rejected request**, and one was: it is evidence that **no ticket exists at all**.
⇒ Confirm a ticket exists by finding it in `/tickets`, never by finding mail about it.

## Two engine facts that had drifted in this file

- ✅ **`ychrome ctl fill` EXISTS** and does vault autofill on the engine plane —
  `fill page_id=<id> entry=<name> user=<account>`. It reports per-field `want`/`got`
  **lengths** and never the value, which is the readback you want anyway. With several stored
  accounts it refuses and names them (`matches 4 accounts — name one: …`). The note further
  up saying `/fill` does not exist is **superseded**; the socket-POST recipe is still the way
  to deliver a **TOTP**, which `fill` does not handle.
- **`ctl eval` takes `js=`, not `expr=`** — `expr` returns `eval needs a page_id and js`.
  For a body too large or too quote-heavy for argv, POST `{"page_id":…,"js":…}` to
  `/engine/eval` on the daemon socket, same shape as `/engine/input`.
