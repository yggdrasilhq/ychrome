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

## paper-cost-announce-before-printing · WORKS
task: how much paper a label run will cost, and what to say before spending it
model: claude-fable-5
date: 2026-08-07
tags: 

⛔⛔ **OWNER ESCALATION 2026-08-07 — ANNOUNCE THE PAPER COST, ASK, THEN PRINT.**
> *"With so much paper wastage I feel like crying to be honest. We should really optimize
> shipping labels in one page or use bad one-sided pages. I need you to tell me these next
> time we print."*

This supersedes the earlier "do not auto-print" line by making the obligation POSITIVE. Before
any print job, state four things and wait:

1. **How many sheets, and how full the last one is** — a percentage, not an impression.
2. **What could ride along.** A batch one article short is worth waiting for; the pickup/postal
   batching law applies to paper too.
3. **Whether a one-side-used sheet will do.** He keeps paper already printed on one side, and
   drafts, labels and working copies belong on the blank back: `-o sides=one-sided`, manual feed.
   ⛔ Never duplex a reused sheet; never send one out of the house.
4. **Whether it needs printing at all.** One envelope is hand-addressed faster than a label sheet
   can be rendered, checked and corrected.

⛔ **Under about 60% page fill, printing without saying so first IS the failure.**

## What the tooling now does about it

`atlasStore/git/graph-manager/scripts/labels.py`:
- prints its own **`⚠ PAPER COST: N sheet(s), about X% full`** line and, under 60%, tells the
  operator to offer batching or a reused sheet before printing;
- **refuses** a single-article sheet without `--force` (the 2026-08-06 sheet used ~10% of an A4);
- **refuses** any recipient with no phone number, the exact field that had to be handwritten;
- packs **two columns per row via a `<table>`**.

⚠ **WeasyPrint does not paginate flex containers.** A `display:flex; flex-wrap` grid with
`page-break-inside: avoid` rendered an **empty first page** and spilled 5 labels across 3 sheets.
A table paginates row by row and fits about 10 articles per A4. Use tables for anything that must
break across pages.

⚠ **Counting `/Type /Page` tokens to get a page count returns nothing on WeasyPrint output** —
which silently suppressed the paper-cost line, the one line the tool exists for. Shell out to
`pdfinfo` instead.

⇒ **The general form, past paper:** when an action consumes something of his that does not come
back — a sheet, a bank OTP, a one-shot captcha, a non-refundable booking — **state the cost in
units before spending it, not after.**

## track-without-captcha-via-server-actions · WORKS
task: 
model: claude-opus-5
date: 2026-08-07
tags: 

⭐⭐ **TRACKING NEEDS NO CAPTCHA, NO BROWSER, AND NO GUI HOST. The captcha on the home page is
CLIENT-SIDE ONLY.** This closes the "⛔ TRACKING IS NOT SOLVED EITHER" entry above, which was
correct about its three dead guesses and wrong about the conclusion.

Every earlier session paid for tracking twice: once to solve the image captcha in a browser, and
again days later when the result URL had expired (`/track-result/article-tracking/<token>` is
scoped to the search SESSION, not the article — re-requested three days on, all four answered
*"Tracking session has expired"*). There is no durable per-article URL. There did not need to be.

## The mechanism

`www.indiapost.gov.in` is a Next.js app and the tracking work is done by two **server actions**,
neither of which takes a captcha argument:

    getTrackingToken()            -> a short-lived signed token  ({"exp":…,"nonce":…}.<sig>)
    trackArticle(token, article)  -> booking details + the FULL scan chain, as JSON

The captcha is checked in the client component before it calls them:

    if (!r && !T && !a) { setCaptchaError("Please complete the captcha verification"); return false }

⇒ Call the actions directly. Two POSTs, no session, no image, no GUI:

```sh
UA='Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/126 Safari/537.36'
TOK=$(curl -sS -X POST https://www.indiapost.gov.in/ \
        -H 'Next-Action: 00b4f44fd7cd8e5a9d969100904e4880581555ea21' \
        -H 'Content-Type: text/plain;charset=UTF-8' -H "User-Agent: $UA" \
        --data-raw '[]' | sed -n 's/^1:"\(.*\)"$/\1/p')

curl -sS -X POST https://www.indiapost.gov.in/ \
     -H 'Next-Action: 60d4c45fc5727f9c4c3efc1c93b182559c9cedbaaf' \
     -H 'Content-Type: text/plain;charset=UTF-8' -H "User-Agent: $UA" \
     --data-raw "[\"$TOK\",\"EY492675435IN\"]" | sed -n 's/^1://p'
```

```json
{"data":{"booking_details":{"article_number":"EY492675435IN","article_type":"SP_INLAND_DOC",
 "booking_date":"2026-08-07T05:36:44Z","booking_office_name":"Paschim Putiari SO",
 "booking_pin":"700041","destination_office_name":"Kolkata GPO DC","destination_pincode":"700001",
 "weight_value":"20","delivery_confirmed_on":null},
 "tracking_details":[{"date":"2026-08-07T05:36:44.109Z","office":"Paschim Putiari SO",
 "eventcode":"ITEM_BOOK","event":"Item Booked","officeid":"33660261","remarks":""}],
 "success":true,"message":"data retrieved successfully","error":null}}
```

## The four traps, each one measured

1. ⛔ **READ THE FLIGHT LINE BY PREFIX (`1:`), NEVER BY POSITION.** The response is a React-Flight
   stream and it carries the whole RE-RENDERED PAGE alongside the answer — 142 kB of it, on line
   `0:`. "Last line", "longest line" or "parse the body as JSON" all pick the page. The action's
   return value is the line beginning `1:`.
2. ⚠ **THE ACTION IDS ARE BUILD HASHES.** They change when India Post redeploys. A 404/500 or an
   empty token means STALE IDS, not a dead route — re-derive them from the live JS bundles:
   `grep -oE 'createServerReference\)\("[0-9a-f]{20,}"[^)]*"(getTrackingToken|trackArticle)"'`
   over everything under `/_next/static/chunks/`. `scripts/track.py --rediscover` in the
   atlasStore manager does exactly this and prints CHANGED/unchanged per action.
3. ⚠ **THE Z ON THE TIMESTAMPS IS A LIE — the values are IST wall-clock.** `booking_date`
   `2026-08-07T05:36:44Z` is a booking made at 05:36:44 **IST**. Shifting by +5:30 invents a
   booking that never happened. Render as given.
4. ⛔ **"Item Booked" IS A PAYMENT EVENT ON THE ONLINE RAIL, NOT A CUSTODY EVENT.** EY492675435IN
   was booked (paid) online at 05:36:44 and physically handed to the counter at ~13:10 the same
   day; the scan chain shows only the 05:36:44 row, overstating India Post's possession by
   **7 h 34 min**. At a counter the two coincide. ⇒ Never cite the booking scan as a dispatch
   date, and never measure transit from it.

Other tabs on the same widget use plain REST rather than server actions — `BOOKING_REF_TRACKING_URL`
(`?reference=<ref>` → the article numbers under a booking reference), `MONEY_ORDER_TRACKING_URL`,
`COMPLAINT_ID_TRACKING_URL`. The captcha backend is `app.indiapost.gov.in/becaptcha`; nothing here
touches it.

⛔ A consignment number is a live evidentiary identifier: it goes to India Post's own endpoint and
nowhere else. Third-party trackers exist and must not be used, however convenient.

⇒ Because it is free, **track often instead of once**. atlasStore
`scripts/track.py [--all]` appends every new scan event to
`graph/notes/tracking/<ARTICLE>.json` with the moment it was first observed — which both defeats
the ~3-month purge (the ledger is itself the durable capture; the 2026-08-03 harvest survived only
as screenshots) and turns "did this article get delayed?" into a series instead of an argument.

## ⛔ A RAIL'S NATIONAL AVAILABILITY IS NOT ITS AVAILABILITY AT A GIVEN OFFICE

Owner field report, 2026-08-07, dropping a Click-n-Book prepaid article at **Paschim Putiari SO
(700041)**, his own sub-office:

> *"It would been less hassle if I had not booked online. I am the first online customer and the
> counter lady didnot know what to do with the envelope and was started at the label. Finally
> after 10-15mins back and forth with calls, the postmaster … she informed me 'please leave your
> phone number … and we do not yet have the power given to us for pickup'."*

- **Click-n-Book DROP-OFF: accepted, but no procedure.** First online customer at that office;
  ~15 minutes and the postmaster to resolve. Budget a conversation, not a drop-and-go.
- ⛔ **Click-n-Book PICKUP: NOT ENABLED at that office** — and **the portal does not know it.**
  It offered a pickup slot (date locked, `min == max`) and quoted the Rs 50 under-Rs-500 pickup
  charge for an office that cannot perform one. A dispatch planned around that pickup would have
  been paid for and stranded.
- The rail did buy something real: the portal booked the article FROM that sub-office
  (`booking_office_name` confirms it), making the office 400 m away a valid Speed Post origin.

⇒ Record rail readiness **per office**, and read it before planning a dispatch. atlasStore keeps
it in `graph/postal-offices.toml` and `scripts/postal.py` prints it above the packing list — the
FORESEEABLE logic applied to the rail instead of the queue. The general form: **when an action
spends something that does not come back — a sheet, an OTP, a non-refundable booking, a trip to a
counter — state the cost in units before spending it, not after.**
