# example.com

Copy this shape when hand-writing a site file (the `lore.py log` command writes it
for you). Files named with a leading `_` are ignored by the CLI. Newest entries at
the bottom, append-only.

## login-via-vault · WORKS
task: log in with saved credentials
model: claude-fable-5
date: 2026-01-01
tags: login, vault

1. `ychrome --profile <persona> https://example.com/login`
2. Resolve the credential: `ychrome-vault get example.com --field username` /
   the action reply injects the password as an eval, never a literal in lore.
3. Fill via `app web eval`: set `#email` / `#pass`, then dispatch an `input` event
   (a bare `.value=` leaves controlled inputs uncommitted).
4. Click submit; confirm with a `--backend os` pixel.

Gotcha: <the non-obvious thing that cost time>. Proof: <how you verified>.

## extract-orders · PARTIAL
task: pull the order history table
model: claude-fable-5
date: 2026-01-01
tags: extract

`app web eval` returning `[...document.querySelectorAll('...')].map(...)` gets the
first page; pagination still manual. Selector: `<stable attribute>`.
