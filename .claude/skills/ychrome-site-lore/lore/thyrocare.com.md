# thyrocare.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## curl-blocked-reseller-open · WORKS
task: price + verify Thyrocare package contents without a browser
model: claude-opus-5
date: 2026-07-29
tags: 

thyrocare.com itself 403s every curl (bot protection) on / and /india/<city>, and the app APIs are dead to curl too: velocity.thyrocare.com/master/GetProductsAPI = connection failure, b2capi.thyrocare.com/api/ProductsMaster/Products = 404, www.thyrocare.com/api/products = 403.

DO NOT reach for a browser at that point. The Thyrocare RESELLER/affiliate sites are plain WordPress and serve full package contents + prices to a normal curl with a desktop UA. healthcareoffers.in is the best of them:
  - package page: https://healthcareoffers.in/thyrocare-packages/<slug>/
  - site search:  https://healthcareoffers.in/?s=<query>   (returns priced result cards)
  - test profiles live under /test-profiles/<slug>/
Strip tags and grep; the parameter list, the 'Current offer price' and the 'Regular Offer Price' are all in the served HTML. Verified 2026-07-29: 'Complete Health Checkup with Vitamins' 122 params Rs 1,569 — confirmed ApoA1/ApoB/ApoB-A1 ratio/Lp(a)/hs-CRP present as a 'Cardiac Risk Markers Panel (5)', Gamma-GT present in LFT, and FERRITIN CONFIRMED ABSENT (the package has iron studies — Iron/TIBC/UIBC/transferrin sat — which is NOT ferritin; do not let a marketing 'iron deficiency studies' line stand in for it). Testosterone also absent.
Reseller prices are negotiated and differ from Thyrocare direct — treat them as reseller quotes, not Thyrocare list.
Only the CART/SLOT step actually needs a rendered surface; everything up to it is curl-able.
