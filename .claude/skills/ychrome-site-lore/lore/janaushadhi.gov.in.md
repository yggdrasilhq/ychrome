# janaushadhi.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## no-online-catalogue · BLOCKED
task: 
model: claude-fable-5
date: 2026-07-23
tags: generics, pharmacy, store-locator

Janaushadhi (PMBJP, the government generic-medicine scheme) is NOT an e-commerce
site and has NO browsable online product catalogue on its current SPA. The old
`/ProductList.aspx` is now a 404. The homepage and /sitemap expose only:
store-locator ("Locate Kendra"/"Locate Distributor"), scheme info, and downloadable
reports - no product search, no prices, no cart. You BUY at a physical Jan Aushadhi
Kendra.

Practical consequence for an agent: to answer "does Janaushadhi stock molecule X",
you check the published PMBJP product basket (a government PDF list of ~2000+
generics), NOT this website. The site is only useful to find the nearest Kendra.
Do not waste cycles trying to drive a search box - there isn't one.

Nav worked fine (static SPA, `web eval document.body.innerText` + `web read --as
links` returned clean data); the finding is about the site's MODEL, not access.
