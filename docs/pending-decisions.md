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

## 6. Deploying to unstick the adblock plane, rather than only reporting it

**The fork.** The YouTube adblock failure turned out to be a stale asset plane, not a
code defect: a three-week-old ruleset, cosmetic filters to match, and the scriptlet
companion absent entirely. The lane could have filed that and stopped, or deployed.

**Decided: deploy.** A current binary was built and installed, and the three stuck
copies were moved aside so the reconciler would rewrite them. All six assets now read
`current` and the block is live-proven. The reasoning: the mandate asks whether the
adblock works, and that question is unanswerable on a host whose blocker is three weeks
stale — the deploy is not a side quest, it is the precondition for the measurement.

**Reverse it** by restoring the previous binary and the three asset copies, both kept
aside during the session. Nothing in the repo changed to achieve it.

⚠ **What this does NOT do:** it fixes one host. The `@version` defect that stuck those
assets is untouched, so the same freeze returns on the next same-day regeneration, and
every other host is still stuck. That fix is a code change filed at the top of
`docs/pending-bugs.md` and was deliberately not attempted here.

## 7. Restoring SponsorBlock from the repo after provisioning de-listed it

**The fork.** Moving the stuck `sponsorblock.js` aside made it *not installed*, and
provisioning only refreshes extensions that are already installed — so it vanished from
the asset list rather than being rewritten.

**Decided: reinstall it from the bundled asset directly**, which is what the settings
pane's install action does. The host copy is now byte-identical to the repo's, which is
the end state that was wanted anyway: it is the first copy anywhere to carry `5aa909f`.

**Reverse it** by deleting the file; it is opt-in and the pane can reinstall it.

## 8. Not building the modal widget kind

**The fork.** Item 4 asks for per-extension modals. Measurement says a contributed pane
cannot raise one at all: the protocol's split is that an app informs and cannot ask, and
every modal that exists is a native shell construct.

**Decided: measure, file, route — do not build.** It is a Tier C change to the shell's
widget vocabulary, and the spec admits one only when **two** apps want it. SponsorBlock
is one. Building it here would either mean changing another repo's vocabulary on one
app's say-so, or opening a native surface to dodge the question — which the spec names
as the expensive mistake ("serves one app and charges every app the native-surface tax
forever").

**Recommendation:** raise it with the shell's owner and look for the second caller. If
none exists, the settings pane stays, and the clogging is better answered by collapsing
SponsorBlock's eleven category rows behind one disclosure than by inventing a modal.

## 9. Fixing the generator, and NOT regenerating the bundled asset

**The fork.** `cosmetic-filters.js` runs in the isolated world and reports itself only through
`window` globals, which no probe can read — so it is undiagnosable, in the exact way
`docs/adblock.md` already warns about for `window.__ysb`. The generator now also publishes
`data-ycf` to the DOM. But the shipped asset is a checked-in build artefact, and no test locks
it to the generator, so the two can drift.

**Decided: land the generator change, leave the bundled asset alone.** Regenerating pulls a
month of upstream filter-list changes into a 19 MB asset in one unreviewed step, which is a
different act from adding a state marker and is not what this mandate asked for.

⚠ **Consequence, stated plainly: the marker does not exist on any host until someone
regenerates.** The code is right and the artefact is stale, which is a real gap and not a
tidy ending.

**Recommendation:** regenerate with the documented `ychrome adblock update` recipe as its own
reviewable change. It is safe on any day the bundle has not already been regenerated that day
— the version stamp is the generation date, so a later day mints a new version and deploys
normally.

## 10. Chasing the segfault only as far as the symbols allowed

**The fork.** The crash reproduces every time, and the backtrace is entirely unsymbolised
WebKit frames. Going further means installing debug symbols for `libwebkit2gtk-4.1`.

**Decided: stop at the characterisation and make the crash visible instead.** What is recorded
is worth more than a guess: which process faults, which thread, which library, and four
workarounds ruled out by measurement. The stderr capture means the next attempt starts from
evidence rather than from rebuilding the instrument.

**Recommendation:** install the WebKitGTK debug symbols and re-run the same gdb recipe. It is
one package and the reproduction is 5 seconds, so the next session should get a named frame
cheaply. ⚠ Do not re-try the four workarounds in the queue entry; they are measured negatives.

## 11. Not upgrading the system WebKit, though it is the most likely fix

**The fork.** The engine segfaults inside `libwebkit2gtk` 2.52.5-1, and 2.52.6-1 is published.
Upgrading is both the obvious remedy to try and the only route to a symbolised backtrace — the
only `-dbgsym` published is 2.52.6, and symbols must match the binary exactly.

**Decided: do not upgrade; recommend it instead.** Two reasons, and neither is caution for its
own sake. It is shared system infrastructure that every GTK web app on the host links,
including other live sessions. And **2.52.5-1 is no longer downloadable** — absent from the apt
cache and gone from the archive — so rollback is snapshot archaeology and dependency juggling,
not one command. An unrollbackable change to shared infrastructure is the owner's call.

⚠ **I also added and then removed a Debian debug apt source while establishing this.** The
system is left exactly as found; the recipe to re-add it is in the queue entry.

**Recommendation:** upgrade, then re-run the reproduction, which takes five seconds. If it
still crashes, install the matching `-dbgsym` and re-run the gdb recipe for a named frame.

⚠ **And the reason this entry exists at all:** the first version of the queue entry said
2.52.5 was the newest available. `apt-cache policy` had answered from stale package lists. One
`apt-get update` produced a different answer and a different conclusion. ⇒ A cache reports the
last time you looked, not the world — which is the same failure as the stale asset plane in
entry 6 and the always-zero falsifier, three times in one session.
