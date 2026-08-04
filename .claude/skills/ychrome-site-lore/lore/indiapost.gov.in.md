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
