# indiapost.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## online-booking-exists-click-n-book-self-service · PARTIAL
task: 
model: claude-opus-5
date: 2026-08-04
tags: 

First pass, 2026-08-04, prompted by the operator: *"From my last Indiapost visit I saw
they have online booking. But I have to deposit the letter (without standing in queue)
in the post office."* He is right that it exists. What follows is what is ESTABLISHED
versus what still needs one attended session, kept separate on purpose.

## ✅ ESTABLISHED: online booking is real and dates from 01-10-2025

The Department of Posts changed Speed Post effective **1 October 2025**, and the change
set includes **online booking of an article and online payment for it**, alongside
OTP-verified delivery, SMS delivery notifications, real-time tracking, and an
addressee-specific delivery registration. So the counter is no longer the only booking
surface, and the operator's reading of what he saw is correct.

## ✅ ESTABLISHED: the surface is the Customer Self Service Portal

- Landing: `https://www.indiapost.gov.in/featuredservices/self-service-portal`
- **Portal: `https://app.indiapost.gov.in/customer-selfservice/login`** (HTTP 200,
  reachable from an ordinary host, no VPN needed)
- India Post's own name for the feature is **"Click and Book"**.
- The login page offers THREE doors: **Registered Customer** (Customer ID + Password),
  **Guest Login**, and **Register Yourself**.
- India Post's own stated limits: *registered users only*, *available for selected
  cities*, and *pick up will not be available on Sunday or closed holidays*.

⭐ **The `Guest Login` door is the interesting one** and is the first thing to try,
because it may reach a booking flow without creating an account. Untested.

## ⚠ NOT ESTABLISHED — do not assume, and do not tell the operator it is solved

1. **Whether "book online, then DROP the article at the counter without queueing" is
   actually a supported flow.** India Post's own copy talks about *pick up*, which is
   the opposite motion: they collect from you. The operator watched something at
   Alipore HO and described a drop. Both can be true (a booked article may have a
   deposit lane) but the drop flow is UNVERIFIED.
2. **Whether Kolkata / Alipore HO 700027 is one of the "selected cities".**
3. **Whether a statutory RTI cover can go this way at all.** Our RTI dispatches need
   **Speed Post with the s.27 General Clauses Act presumption of service**, and a
   court-fee stamp is affixed to page 1 inside. Nothing about online booking should be
   assumed to preserve either until seen.
4. **Whether it can be driven headlessly.** Unknown; expect a captcha and an OTP.

## ⛔ TRACKING IS NOT SOLVED EITHER, and two guesses are already dead

- `apiportal.indiapost.gov.in` — **does not resolve.** Invented, not real.
- `app.indiapost.gov.in/api/v1/track/<ref>` — **404.**
- `www.indiapost.gov.in/_layouts/15/DOP.Portal.Tracking/TrackConsignment.aspx` —
  **timed out (curl 000)** from a plain host; it is also captcha-gated in a browser.
- Third-party trackers exist (ClickPost, Parcel Monitor, RapidAPI) but a consignment
  number is a live evidentiary identifier, so **do not hand our refs to a third-party
  tracker** to save a co-browse session.

⇒ Tracking currently needs the ychrome plane against the official page. That matters
because **India Post purges tracking records in about three months**, and delivery
confirmation for a statutory dispatch is the artefact that proves service. There are
four live refs from 27-07-2026 whose proof will expire around late October 2026.

## The next session's plan, in order

1. `Guest Login` on the self-service portal; screenshot what it offers.
2. If guest is thin, `Register Yourself` — that is an account creation, so it is an
   operator-confirmed write, not something to do unasked.
3. Establish, in writing: does a booked article get deposited at a counter, is Kolkata
   in scope, and does the booking preserve Speed Post + POD?
4. Solve tracking on the same visit and harvest the four 27-07 refs while they exist.

## Why this matters beyond convenience

The 2026-07-27 dispatch cost **about 1.5 hours at the counter** for four articles, with
UPI-only payment and a counter that refused a front-printed sender block. If online
booking removes the queue, it removes the single largest time cost in the postal lane,
which is the lane every WB state RTI must use because **no West Bengal online RTI portal
exists** (`rtiwb.gov.in`, `wbrti.gov.in`, `rti.wb.gov.in` do not resolve; `wbic.gov.in`
and `wb.gov.in` time out).

## shipping-label-convention-and-no-autoprint · WORKS
task: how to address a Speed Post cover India Post accepts, and when to print a label at all
model: claude-fable-5
date: 2026-08-07
tags: 

⛔⛔ **THE OWNER'S STANDING ORDER, 2026-08-07: NEVER AUTO-PRINT A SHIPPING LABEL.**
> *"do not print the shipping label from now on. It wastes paper and you make mistakes …
> We print only if we have a batch and we use one page CORRECTLY for the shipping labels."*

**Print a label sheet ONLY when (a) there is a BATCH of articles, and (b) the sheet is laid out to
FILL an A4.** A single label on its own page is refused: the RTI-A sheet on 2026-08-06 occupied
about 10% of an A4 and the whole page went in the bin. For one article he addresses the envelope
himself and it is faster than being handed a wrong label.

## ✅ THE ACTUAL LAYOUT INDIA POST ACCEPTS — settled from the primary artefact

Photographed 2026-08-07, the RTI-A cover as accepted (atlasStore
`icccc-kolkata-police-complaint/evidence/rti-postal/ENV-RTI-A-AS-ADDRESSED-2026-08-07.pdf`,
`ev-20260806-fce39b26`). A DL long white envelope, landscape:

- **FROM — upper LEFT, on the FRONT.**
- **TO — lower RIGHT, on the FRONT**, offset down and across from the sender block.

⛔⛔ **THIS CORRECTS THE 2026-07-27 ENTRY IN `indiapost.gov.in` LORE, WHICH SAID "FROM GOES ON THE
BACK, TO ON THE FRONT" AND THAT A FRONT-PRINTED SENDER BLOCK WAS REFUSED.** The delivered
article carries BOTH blocks on the front and was accepted. Whatever happened at that 2026-07-27
counter, it is not a general rule and must not be applied again. **The photograph is the
authority here, not the earlier recollection.**

## ⛔ THE THREE DEFECTS IN THE PRINTED LABEL — all corrected by hand at dispatch

1. **NO RECIPIENT PHONE WAS PRINTED.** He had to handwrite the addressee's published
   switchboard number beside the To block at dispatch time. **Both phones are mandatory** per the counter's posted orders,
   and the template only ever carried the sender's. ⇒ **A label with one phone number on it is an
   incomplete label.** Sender: the registered number. Recipient: the authority's own published
   switchboard number - look it up before rendering, never leave the field out.
2. ⛔ **HIS E-MAIL MUST NOT APPEAR ON THE OUTSIDE.** It serves no postal purpose whatsoever, and
   the reply address already sits INSIDE the application where the authority actually needs it.
   Printing it on the cover is a gratuitous identifier leak to every handler. **Remove it from
   every template.**
3. ⛔ **DO NOT MARK THE COVER "RTI APPLICATION".** It announces the contents to everyone who
   touches the envelope — including staff of the very authority the request is aimed at — and it
   buys nothing, because **the addressee line already reads "Report (RTI) Section"**, so routing
   is done by the address itself. `BY SPEED POST` is likewise redundant once the article carries a
   Speed Post barcode. Both were the owner's calls on 2026-08-07 and both are correct.

⇒ **The label carries exactly: From block (name, address, PIN, phone) · To block (designation,
office, address, PIN, phone). Nothing else.** No email, no contents marking, no internal codes
(that last one was already law — tray codes and slugs never print).
