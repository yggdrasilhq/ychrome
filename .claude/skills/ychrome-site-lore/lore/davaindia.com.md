# davaindia.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## client-only-search · PARTIAL
task: 
model: claude-fable-5
date: 2026-07-23
tags: generics, pharmacy, nextjs, search

Davaindia (davaindia.com, Zota Health Care's generic-pharmacy chain) is a heavily
client-rendered Next.js/Tailwind store. KEY GOTCHA: search is 100% client-side.
Neither `?s=metformin&post_type=product` (WooCommerce-style) NOR `/search?q=`,
`/medicines?search=`, `/products/search?q=` route to results - they all render the
homepage or a 404 shell. `document.querySelectorAll('input')` returns [] on load:
the search input only mounts AFTER you click the "Search" trigger (a bare
`<div>` with text "Search" near the hero, found via
`Array.from(document.querySelectorAll('*')).filter(e=>e.children.length===0 &&
e.textContent.trim()==='Search')`). So driving the search needs: click the trigger,
wait for the input to mount, `web do type`, `web do key Enter`. A blind
coordinate `web do click` on an UNREVEALED headless surface is dangerous - one such
click navigated the page and triggered a surface reap (see agent-control-plane gaps).

What IS readable without interaction (single `web eval document.body.innerText` on
the homepage): the category taxonomy (Heart Care, Sugar Level Care, Sexual Wellness,
Kidney Care, ...), and the "Super Saving Deals" / house-brand best-sellers with
prices - these are DavaIndia's own branded softgel combos (Smarty Man, Long Vision,
Skin/Nail/Hair Support, Multivitamin Effervescent), typically shown at ~50-80% off
a high printed MRP. The pharma molecules (metformin, tadalafil, statins) are in the
catalogue but are NOT the homepage best-sellers.

NEXT TIME: prefer the interactive click-to-reveal-input path, or find the internal
JSON API via `web eval fetch(...)` from the page origin (untested - the /api route
was not probed this run).
