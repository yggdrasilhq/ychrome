## The settings toggle: client vs server rendering (filed 2026-09-12, zcode seat) — AWAITING A DECISION

**The ask (owner, 2026-09-11):** "ychrome should have a settings option on top toggle for
client or server rendering."

**What exists today, measured:**
- **Client rendering** is the only VISIBLE path: thin-client surfaces render in the yggterm
  GUI's WebKit child webview; network egress stays on the session host through the per-surface
  SOCKS proxy (architecture.md's egress rule).
- **Server rendering** exists in two INCOMPLETE forms: `yggterm-wpe` (the headless WPE engine
  that "renders on a server host [and] costs the GUI host exactly zero") is a standalone
  library with **no GUI integration, no consumers** — its own lib.rs says so; the agent engine
  (`ychrome ctl`) renders pages offscreen with PNG frame readback and click/type input, but has
  no visible-surface pipeline (no frame stream into the viewport, no input routing from the GUI).

**So the toggle cannot be honest yet.** A settings switch with only one working arm is a lie
button. What the owner asked for is real: page rendering on the session host with frames
shipped to the GUI is the architecture's destination (it removes the GUI host's WebKit cost,
the under-glass/widget bug classes, and makes remote-row browsing first-class). The build is:
wpe view per surface on the session host → frame streaming over the existing substrate →
GUI composites frames where the webview would sit → `ctl`-style input routing back.

**Decision needed (owner):** confirm "server rendering" means wpe/engine frames rendered on the
session host composited into the GUI viewport (the build above), vs something lighter. Then
this becomes a lane: phase 1 the settings schema + persistence + the surface-side read path
(the toggle's plumbing, client arm live), phase 2 the wpe frame pipeline (the real arm).

**Toggle plumbing sketched (phase 1, ready when decided):** `settings_schema_from` gains a
top-level toggle widget (client = thin-client webview, today's behavior; server = reserved
until phase 2 lands, greyed with the named reason rather than a silent no-op), persisted
per-host next to the ad-block/userscript settings, read by the surface open path.
