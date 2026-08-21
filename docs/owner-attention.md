# ychrome — decisions that are the owner's, not the lane's

Each entry: **the question · the options · THE RECOMMENDATION · what was done meanwhile ·
how to reverse it.** A lane does not stop on these; it routes around them and carries on.
An entry with no recommendation is unfinished.

Read at the end of a campaign, not mid-relay. When one is settled, it is deleted in the
same commit as the work it unblocked — same law as `pending-bugs.md`.

---

## 1. Four tests are red on the trunk and they belong to another lane

**Filed 2026-08-21.**

**The question.** `cargo test --bin ychrome` fails 4 of 389, deterministically, on a clean
tree. Commit `0c6b80e` added a fourth vault item type (`Identity`) and did not update the four
`sidebar::tests` that enumerate the item types. Should this lane edit them?

**RECOMMENDATION: no, and it did not.** Editing another lane's assertions until they match
their code is how a genuine regression gets laundered — those assertions are the only thing
currently checking that feature. The author should say which side is right. Filed at ⛔ in
`pending-bugs.md` with the four names, because until it is settled nobody can use the suite as
a green gate, and every session must either know the four names or suspect its own change.

**What was done meanwhile.** Worked around it: this lane ran targeted tests and reports its own
counts against the same 4 pre-existing failures (382→389 passing, same 4 failing).

---

## 2. Two stale rows in the sidebar that a sweep should take, not you

**Filed 2026-08-22.** Not a decision so much as a thing that looks like a defect and is not.

The wedged first successor (`70c2ae44…`) is **dead** — no process, `live_processes: []`, and
`session remove` accepted twice. Its row **still appears in the listing**, on the
`remote-cc://dev/` spelling and as a project-folder entry. That is the row plane's own
documented law: *the verb reports the request, not the effect*.

**RECOMMENDATION: leave it for a sweep.** Re-removing an already-removed row files a second
DELETION against someone's row files, which is worse than a stale entry. The live successor is
`9eb57932…`, seat 11.24.1.

**What was done meanwhile.** The cause is recorded in the fleet skill's §11 register on
`lane/dev/11.24-omnibox`: `--no-activate` parks a queued `remote-cc` resume on *"Daemon PTY:
request main viewport terminal stream"* forever, and every check the spawn contract prescribes
passes anyway.

---

## 3. Nothing distinguishes campaign work from work handed to a row directly

**Filed 2026-08-22**, from the owner asking whether *"subscribe to 11.0"* is the right way to
describe a row that carries both orchestrated work and tasks he gave it personally.

**It is not, and the reason is worth recording: there are THREE mechanisms and no fourth.**

| what | mechanism | what it actually does |
|---|---|---|
| liveness | `ygg-booter.py subscribe --row <path> --campaign <token> --max-hours N` | boots the row when it goes idle, so long work survives |
| placement | the `--campaign` token + the seat number in `outline_prefix` | says which campaign a row belongs to |
| reporting | a message to the orchestrator's row | how a row tells 11.0 anything |

⇒ A row subscribes to the **booter**, not to 11.0. 11.0 is a row, and rows are not
subscribable. The accurate sentence is *"seated as 11.24 in campaign 11, subscribed to the
booter under that campaign, reporting to 11.0"*.

⛔ **The real gap his question found:** work the owner hands a row DIRECTLY arrives as an
ordinary turn in that row, and **nothing anywhere records that it happened.** The booter
record carries `campaign` and `kind`, never a task list. So:

- 11.0 cannot see that a row is carrying owner-assigned work it did not route;
- a sweep or a reassignment cannot know that retiring a row would drop that work;
- and the only trace is the row's own transcript, which is exactly the artefact everything
  else in this fleet refuses to treat as authoritative.

**RECOMMENDATION.** The cheapest honest fix is to use the field that already exists: the
booter record's `note`. A row given work directly should write it there in one line, because
it is durable, machine-readable, and already read by the sweep path. This row now does that.
⇒ A heavier alternative — a per-campaign task file that 11.0 owns — is more correct and more
work; do not build it before the `note` convention has been tried and found wanting.

**What was done meanwhile.** This row's booter `note` names its assigned work, both spawned
successors, and its exit condition, so a sweeper reading only that file knows what dropping
this row would cost.

### ⇒ WHAT HE ACTUALLY MEANT, AND IT IS SIMPLER THAN THE MECHANISM QUESTION

*"I meant since you are part of 11.0 orchestrator let us preserve it while you do more tasks.
I do not know the right vocabulary to say that."*

⛔ **He was not asking about subscription mechanics — he was asking for the row to be KEPT
ALIVE and IN THE CAMPAIGN while he keeps handing it work.** The first answer to this
explained three mechanisms and did not do the one thing being asked for, which is a failure
mode worth naming: *a question phrased in the wrong vocabulary still has a concrete request
inside it, and answering the vocabulary is not answering the request.*

The concrete thing was a number. The row's coverage was `max_hours: 12.0` with **7.8 hours
left** — it would have lapsed mid-session while he was still assigning work. Re-subscribing
with `--max-hours 48` and a `--note` naming the owner-assigned tasks fixed it in one call, and
the read-back confirms 48 h coverage with the watcher armed.

⇒ **The `note` recommendation above is no longer untried.** It was exercised here and it
works: one call carries the extension, the campaign, the kind and the task list together, and
`ygg-booter.py status <row>` reads it back. Prefer it to building a task file until something
it cannot express actually turns up.

⭐ **The vocabulary he was looking for:** *"extend my booter coverage and keep my seat in
campaign 11"* — or, in the fleet's own terms, *"re-subscribe me under campaign 11 with a
longer max-hours."* There is no verb for "subscribe to 11.0" because 11.0 is a row, and the
thing that keeps a row alive is the booter, not the orchestrator.
