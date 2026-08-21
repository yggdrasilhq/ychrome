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
`9eb57932…`, seat 11.25.

**What was done meanwhile.** The cause is recorded in the fleet skill's §11 register on
`lane/dev/11.24-omnibox`: `--no-activate` parks a queued `remote-cc` resume on *"Daemon PTY:
request main viewport terminal stream"* forever, and every check the spawn contract prescribes
passes anyway.
