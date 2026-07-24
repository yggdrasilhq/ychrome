# ais.insight.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## sso-form-post-from-efiling-then-read-tis · WORKS
task: reach the Compliance Portal AIS/TIS and read it
model: claude-opus-4-8
date: 2026-07-24
tags: 

The Compliance Portal (AIS / TIS / e-Campaign / e-Verification) has no login of
its own: you arrive by SSO from the e-filing portal. Clicking **AIS** in the
e-filing nav builds a hidden form and POSTs `param1/param2/param3` (an encrypted
one-shot token) to `https://ais.insight.gov.in/portal/access` with
`target="_blank"`.

**What works:** patch `HTMLFormElement.prototype.submit` to force
`this.target='_self'` BEFORE clicking the AIS link, then click it. The hand-off
then lands in the same surface at `/complianceportal/ais/instructions`.

**What does not, and why — all measured:**

- `window.open` is a no-op in a ychrome surface, so the untouched `_blank` form
  goes nowhere and the click looks dead.
- **Replaying the captured token from any other origin gets `Access Denied !!`**
  — a local `http://127.0.0.1:<port>` page that auto-submits the same three
  params reaches the server and is rejected. The POST must originate from
  `eportal.incometax.gov.in`. (It also burns the token.)
- `fetch()`ing the endpoint fails with `TypeError: Load failed` — no CORS, by
  design. It is a navigation, not an API.
- The token is one-shot and short-lived: every AIS click mints a new one and
  invalidates the last. Capture and use within seconds, or just let the real
  click submit.
- A plain GET to `/portal/access` answers **HTTP 425** (CloudFront/WAF). That is
  normal, not a block on you.

**Reading the data once inside:** `AIS` in the nav -> the year selector shows
`F.Y. 2025-26` style tax years -> two tiles, **TIS** (Taxpayer Information
Summary — the aggregated per-category totals, the fastest read) and **AIS** (the
detail). The AIS detail has tabs: `TDS/TCS Information` (Part B1), `SFT
Information` (B2 — bank interest arrives here as SFT-016, per bank), `Payment of
Taxes` (B3 — advance/self-assessment challans, and it shows PRIOR years too),
`Demand and Refund` (B4), `Other Information` (B7). Per-row TDS amounts are
hidden until you click the row's expander; it then loads async ("Fetching
information, Please wait ...") so re-read after a few seconds. Everything is
plain DOM text — `web read --as text` or an innerText eval is enough, no download
needed.

Note the portal's own advice: where AIS and TRACES Form 26AS disagree on TDS,
rely on 26AS.
