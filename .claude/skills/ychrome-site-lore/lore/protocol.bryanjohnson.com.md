# protocol.bryanjohnson.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## full-page-innertext · WORKS
task: 
model: claude-fable-5
date: 2026-07-23
tags: supplements, longevity, blueprint, scrape

The full Blueprint protocol (diet, supplements, Rx stack with dosing and timing) is
a SINGLE long static HTML page at https://protocol.bryanjohnson.com/ (title
"protocol - DON'T DIE"). No login, no paywall, no JS gating. `web read --as readable`
or `web eval 'document.body.innerText'` returns the entire ~53k-char page in one shot.

The itemized daily stack lives under the "My daily routine" section (search the text
for "5:25 am", "I'll take the following pills", and "My Rx stack" / "Rx / Prescriptions").
The two Rx lists differ slightly (the noon "My Rx stack" list vs the later
"Rx / Prescriptions" section) - the later section is the more current one and adds
Candesartan 8mg while dropping the older ones; reconcile both.

Nav: bare-expression `web eval` fails ("Return statements are only valid inside
functions" if you write `return ...`; a bare expression like
`document.body.innerText` or a `JSON.stringify({...})` works). URL-navigate with
`web eval "location.href='...'; 'nav'"` then `web wait --until load:finished`.
