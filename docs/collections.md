# Collections — organising history into things worth keeping

> **Spec of record, 2026-08-01.** The user's last named UX gap: *"I want the
> history picker 'extension' in our ychrome to be a bit better though with a
> collections system … I like our single page history timeline view. But I need
> the session collection mechanism too as an organization mechanism."*

## What exists today, and what stays

`~/.yggterm/web-profiles/<profile>/history.jsonl` — append-only, one
`{ts_ms, url, title}` per visit, per profile. The omnibox reads it, and so does
the history page (newest-first, deduped by URL, capped at 1000).

**That stays exactly as it is.** It is the raw record and it is already the
right shape: append-only, cheap to write, trivially greppable. The single-page
timeline the user likes is unchanged. Collections are a layer *over* it, not a
replacement for it.

## The model — three nouns, and only two stores

| noun | what it is | where it lives |
| --- | --- | --- |
| **Visit** | one page load, automatically recorded | `history.jsonl` (unchanged) |
| **Collection** | a named, organised, durable set of links | `collections/<id>.md` |
| **Snapshot** | a collection nobody typed a name for — made by an event | `collections/<id>.md`, `kind: snapshot` |

**A snapshot IS a collection with a `kind`, not a second store.** Session Buddy
shows them in two rails and that is a fine *view*, but underneath they are one
object with one set of verbs — otherwise "promote this snapshot to a real
collection" becomes a migration instead of editing one field. Same rule the rest
of this project runs on: one owner per concept.

## The file format — Markdown with frontmatter, and that is load-bearing

The user asked for collections that are *"richer with frontmatter texts and
ychrome automation to be touched and shaped by you"*. That single sentence
decides the format. A collection is **a Markdown file**:

```markdown
---
id: quant-reading
name: Quant reading
kind: collection            # collection | snapshot
created_at: 2026-08-01T16:04:12+05:30
updated_at: 2026-08-01T16:41:03+05:30
profile: default
tags: [finance, study]
imported_from: { browser: brave, profile: Default }
---

Anything the user or an agent wants to say. Plain markdown, no schema.
This is the half a bookmark manager never has and the reason a collection
outlives a tab strip.

## Papers

- [Active Portfolio Management — Grinold & Kahn](https://example.org/apm.pdf)
- [How Quants REALLY Find Predictive Trade Signals](https://example.org/x)

## Videos

- [What Nobody Tells You About Being a Quant](https://example.org/y)
```

Why Markdown rather than JSON:

- **An agent can shape it with ordinary tools.** `Edit`, `Write`, a sed one-liner.
  A JSON blob would need a parser round-trip for every touch, and every touch is
  a chance to drop a field.
- **The user can read and edit it in yedit**, which is already in the product.
- **Folders are headings** (`## Papers`), items are list links. Nesting is
  heading depth. No invented structure to learn.
- **It diffs.** A collection in a git repo shows what changed.
- **Frontmatter is the extension point.** New keys cost nothing and old readers
  ignore them.

The parser is deliberately forgiving: an unknown key is preserved verbatim on
rewrite, a malformed item is kept as prose rather than dropped. **A collection
must never lose a link because we could not parse a line.**

## Where they live

```
~/.yggterm/web-profiles/<profile>/collections/<id>.md
```

Per profile, because the user's stated goal is *"organisation by profiles and
sessions and folders"* — profile is the top axis and it already is for history,
cookies and zoom. An import from another browser lands in the profile the user
chose, and records where it came from in `imported_from`.

## Snapshots — what makes one, and what prunes it

An automatic snapshot is written when:

- **the last tab of a surface closes** (Session Buddy's "Browser closed"),
- **on a cadence** while tabs are open (its "Snapshot"), default 60 min,
- **on request**, from the UI or a verb.

Rules, so this cannot become the leak that automations nearly were:

- A snapshot identical to the previous one is **not written**. A browser sitting
  idle for a day should produce one snapshot, not twenty-four.
- Snapshots are pruned by age and count (default: keep 30 days, max 200 per
  profile). Collections are **never** pruned — that is the difference between
  the two kinds, and it is the only difference that matters.
- Promoting a snapshot is `kind: snapshot` → `kind: collection` plus a name. One
  field, no move, no copy.

## The UX, on top of the timeline that already exists

The history page keeps its single-page timeline. It grows:

- **A collections rail**, on the same side as the cwdtree per the mirror setting
  (`ChromeOrientation` already owns which side that is — do not hardcode).
- **Selection in the timeline**: click, shift-click for a range, ctrl-click to
  add. A selection shows one bar: *Add to collection ▾ · New collection · Open
  all*.
- **Drag and drop**: timeline rows onto a collection; items between folders;
  folders into folders. ⚠ Use `yggui::RowDragGesture` — the drag experience is
  already ONE object (ghost card, dim, drop line, spring-loaded auto-expand,
  Escape-to-cancel) and every row list drives it. Do not write a second drag.
- **A collection view**: frontmatter rendered as a header, notes as prose,
  folders as sections, items as rows with favicons — and an **Open all** that
  respects the tab-placement owner rather than inventing one.
- **Save all open tabs** as a collection, which is the single most-used button
  in Session Buddy.

Reuse, do not invent: the rail is the cwdtree component, the rows are
`SessionStyleRow`, the folder glyph is `RowFolderIcon`, the chevron is
`RowDisclosureChevron`. This is the standing rule and this feature is exactly
where an agent would be tempted to break it.

## Import — the actual goal

The user wants *"all my history from all my browsers (chromium based brave,
vivaldi, chromium, google chrome, helium) and firefox based profiles"*.

**`rusqlite` is already a workspace dependency** (bundled), so this is in-process
reading, not a shell-out to `sqlite3`.

| source | file | tables |
| --- | --- | --- |
| Chromium family (Chrome, Brave, Vivaldi, Chromium, Helium, Edge) | `<profile>/History` | `urls(url,title,last_visit_time)`, `visits` |
| Chromium bookmarks | `<profile>/Bookmarks` | JSON tree → folders map 1:1 |
| Firefox family | `<profile>/places.sqlite` | `moz_places`, `moz_historyvisits`, `moz_bookmarks` |
| Session Buddy | its own export | JSON → collections |

Traps that must be handled, because each one silently corrupts a date or a run:

- **Chromium timestamps are microseconds since 1601-01-01**, not Unix. Firefox's
  are microseconds since 1970. Getting this wrong puts every import in the wrong
  century, and it will look plausible.
- **The DB is locked while that browser runs.** Copy to a temp file and open the
  copy read-only; never open the user's live profile read-write.
- **Import is idempotent.** Re-importing the same profile must not double
  anything: dedupe on `(url, visit_time)` for history and on `(url, folder path)`
  within a collection.
- **Bookmarks are folders, history is a timeline.** Bookmarks import as a
  collection with its folder tree preserved; history imports as *visits*, into
  `history.jsonl`, not as a giant collection nobody can read.

## ⚠ Correction to this spec, 2026-08-01: where the code lives

The verbs below were written as `ychrome collection …`. **The implementation
lives in `yggterm`, not ychrome** — `ychrome` does not depend on `yggterm-core`,
and the store sits under `~/.yggterm/web-profiles/<profile>/`, which is
yggterm's, alongside the `history.jsonl` these collections are built from. The
parser is `yggterm-core::web_collection`. Read the verb names below as
`yggterm server app web collection …`; ychrome reaches them the same way it
reaches every other yggterm verb rather than growing a second store.

## Verbs (agent plane)

Every one of these is how an agent shapes a collection, which the user asked for
by name:

```
ychrome collection list [--profile <p>] [--json]
ychrome collection show <id> [--json]
ychrome collection new <name> [--tag <t>]... [--note <text>|--note-stdin]
ychrome collection add <id> --url <u> [--title <t>] [--folder <path>]
ychrome collection add-from-history <id> --since <when> [--match <substr>] [--limit <n>]
ychrome collection move <id> --item <url> --to-folder <path>
ychrome collection rename <id> <name> | tag <id> <t> | note <id> (--text|--stdin)
ychrome collection promote <id> --name <name>        # snapshot -> collection
ychrome collection open <id> [--folder <path>]       # into the tab placement owner
ychrome collection export <id> [--as md|json] [--out <file>]
ychrome collection import --from <chromium|firefox|session-buddy> --path <dir|file>
                          [--profile <p>] [--bookmarks-as-collection]
ychrome collection prune [--profile <p>] [--dry-run]
ychrome snapshot now [--profile <p>]
```

`export` with no `--out` writes the Markdown to stdout, because the file already
IS the export format and a second serialisation would be a second source of
truth.

## Build plan

- **I1 — the format.** ✅ **DONE** — `yggterm-core::web_collection`, 13 tests.
  Round-trip is byte-identical **by construction**: every block keeps its source
  line and renders that back unless something deliberately changed it, rather
  than being carefully re-serialised (which works until someone adds a field).
  Unknown frontmatter keys survive, and a line that looks like an item but does
  not parse is kept as prose — losing a link is the one unrecoverable failure
  this format could have.
- **I2 — the store.** Per-profile directory, atomic writes, id allocation, the
  snapshot dedupe and prune rules. Pure decisions, `now_ms` injected.
- **I3 — the verbs**, one parser, both binaries, as `automation` does.
- **I4 — snapshots**: the close hook, the cadence chore, the identical-snapshot
  refusal.
- **I5 — import**: Chromium first (it covers five of the six browsers named),
  then Firefox, then Session Buddy JSON. Copy-then-open, epoch conversion,
  idempotence — each with a test using a fixture DB.
- **I6 — the UI**: collections rail, timeline selection, the collection view,
  Save-all-open-tabs.
- **I7 — drag and drop**, through `RowDragGesture`.

## Acceptance

1. A collection round-trips through the parser byte-identically, including a
   frontmatter key the parser does not know.
2. Selecting three rows in the timeline and choosing *Add to collection* puts
   exactly those three in it, in order, with their titles.
3. Closing the last tab writes a snapshot; doing it again with the same tabs
   writes nothing.
4. Importing a real Chromium `History` copy lands visits with correct **local**
   dates (the 1601 epoch trap), and importing it twice changes nothing.
5. `ychrome collection add-from-history` and a hand-edit in yedit produce the
   same file, and neither loses the other's fields.
6. `Open all` on a folder opens through the existing tab-placement owner.

Related: `docs/pending-bugs.md`, `docs/product.md`,
`yggterm/docs/web-surfaces.md`, and the standing rule that the cwdtree component
is reused rather than reinvented.
