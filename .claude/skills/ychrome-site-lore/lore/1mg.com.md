# 1mg.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## url-search-price-cards · WORKS
task: 
model: claude-fable-5
date: 2026-07-23
tags: pharmacy, prices, search, india

Tata 1mg (1mg.com) is the RELIABLE lane for live Indian medicine prices in an
agent co-browse. URL search WORKS directly: https://www.1mg.com/search/all?name=<q>
(URL-encode spaces as %20). No login needed to read prices. After navigate, `web
wait --until load:finished` then `web eval 'document.body.innerText'`.

Parsing: product cards appear in innerText as three lines -
  <Brand name>\n<pack e.g. "strip of 10 tablets">\nDiscounted Price: ₹<N>
so a regex like
  /([A-Z][A-Za-z0-9 \-\+\.%&]{2,45})\n(strip[^\n]*|bottle[^\n]*|[0-9][^\n]*)\nDiscounted Price: ₹([\d\.]+)/
pulls (name, pack, price) cleanly. The page also lists a "Generic drugs (salts)"
block (the molecule + its combos) and a brand filter sidebar (every brand for that
salt with counts) - useful to enumerate cheap alternatives to the innovator brand.

Proven this run (2026-07): acarbose (Glucobay 50 ₹166/10), empagliflozin (Jardiance
10 ₹389 vs generic Empacip 10 ₹77.5/10), candesartan (Candestan 4 ₹17.3/10),
rosuvastatin (Rozutin 5 ₹47.9/10), metformin SR (Metform SR 500 ₹14.6/10),
tadalafil (Megalis 5 ₹239/10). Prices are per strip; note the pack size in the
second line (some tadalafil packs are strips of 4, not 10).
