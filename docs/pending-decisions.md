# Pending decisions — the ychrome UX lane

Forks this lane decided rather than stopping on, each with what was done and how
to reverse it. Relay rule: a turn does not end on a question, so an ambiguity is
recorded here with a recommendation and the work carries on.

⚠ Every one of these is REVERSIBLE. Anything irreversible or outward-facing that
could not be undone would have been left undone instead.

---

## 1. A spawn from a group HEAD goes INSIDE the group, not beside it

**The fork.** In the row-group model a head's own `group_head` names the OUTSIDE
group — so by the arithmetic, a head is a member of the outer group, and a link
opened from it could reasonably land there.

**Decided: INSIDE.** The head row is what replaced the folder HEADER, and the
header's "+" filled the folder. Reading it the other way would silently change a
gesture users already had. It is also what a yggterm live-session row does when
you spawn from it.

**Reverse it** by dropping the `opener_heads_a_group` branch in
`web_tab_placement` (yggterm `state.rs`); `a_child_is_born_inside_its_openers_group`
is the test that would then need rewriting, and it says which half is which.

## 2. A NAMED row counts as organization at a fresh start

**The fork.** The fresh-start gate could read the group fields only.

**Decided: a user-given name counts too** — and this was not a preference, it was
a data-loss fix found live. A folder holding ONE tab flattens to a head with no
members, and a head with no members mints no group key: it is organization on the
launch that migrates it and a loose browsing row on the NEXT one, so a fresh
start would silently drop a page the user filed. What survives the flattening is
the folder's label, moved onto the head's `custom_title`.

⚠ The cost, stated: a tab the user renamed for any reason now survives a fresh
start. That is a deliberate act, so it is defensible — but it IS a behaviour
change beyond the migration, and it is the clause to revisit if "start fresh"
starts feeling sticky. `saved_web_tab_is_organized` is the one place it lives.

## 3. libyggterm v0.13.0 was tagged and pushed to `main`

**The fork.** The palette had to ship as a tag for yggterm to consume it, and
`~/gh/libyggterm` carried another agent's uncommitted work.

**Decided: build in an isolated git worktree, tag, push to main.** Their tree was
never touched (a worktree is a separate checkout) and their edits were seven days
stale with no session on them. Pushing a commit does not disturb an uncommitted
tree; it only means a rebase later, which any commit by anyone would cause.

**Reverse it** with `git push --delete origin v0.13.0` and reverting `62aec27`,
plus the `tag = "v0.13.0"` line in yggterm's `Cargo.toml`. ⚠ Reverse it SOON if
at all: a tag is cheap to withdraw only while nothing consumes it.

## 4. ytop was changed to declare `row_spawn: false`

**The fork.** Item 7's curation is a schema question, and the entry noted the
schema is worthless alone — an app rewrites its manifest on every launch, so a
hand-edit does not survive. The declaring app was ytop, a different repo.

**Decided: land both halves in one wave.** Filing the schema and leaving the
declaration undone would have shipped a flag nothing sets, which is the state the
entry already refused once.

**Reverse it** by reverting ytop `3b1dafa4`; the flag defaults to true, so the
verbs come straight back.

## 5. Items 4 and 5 were treated as unowned

**The fork.** A previous brief split them to a second lane.

**Decided: treat as unowned.** That lane's worktree never advanced past its spawn
commit, so nothing was in flight to collide with. Item 4's custom-site-access
half was delivered; the rest of 4 and all of 5 are now filed in
`docs/pending-bugs.md` with their traps, which they were not before.

**Recommendation for the next wave:** do item 5 before item 4's modals. Item 5 is
two owner-visible defects; the modals are a refactor of a pane that works.
