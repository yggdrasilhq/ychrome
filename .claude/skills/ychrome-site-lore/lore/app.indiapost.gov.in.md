# app.indiapost.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## click-n-book-full-flow-and-pincode-modal-trap · WORKS
task: book a Speed Post pickup for an RTI cover; rotate the portal password
model: claude-fable-5
date: 2026-08-07
tags: 

Driven end to end 2026-08-07 03:30-05:00 IST on the ychrome ENGINE (`ychrome ctl`, headless on
dev, profile `agent-indiapost`). The 2026-08-04 PARTIAL entry's open questions are now ANSWERED.

## ✅ ANSWERED: all four unknowns from the 2026-08-04 pass

1. **Kolkata IS in scope.** 700041 (Paschim Putiari SO) books, and pickup slots are offered.
2. **BOTH motions exist as a first-class choice on step 1:** `Pick Up` (they collect) and
   `Drop Off` (you deposit at any post office). The operator's counter sighting was real.
3. **Speed Post + POD survive online booking.** Service `Inland Speed Post Document`
   (`SP_INLAND_DOC`) and a POD value-added service are both selectable, so the s.27 General
   Clauses Act presumption + proof of delivery are preserved. A **Speed Post article barcode is
   allotted at cart-add, BEFORE payment** (`EY492674996IN` on the test booking).
4. **It IS headlessly drivable, and there is NO captcha anywhere.** Login is
   customerid+password; the only OTP is on the password-reset flow.

## The route map (Next.js app; deep-linking is REFUSED, navigate by clicking)

`/customer-selfservice/login` -> `/home` -> `/self-booking` -> `/self-booking/domestic`
Also `/self-booking/{international,bulk-booking,domestic-moneyorder,check-payment-status}`
and `/self-booking/my-bookings`.
⛔ **`ctl goto` to any inner route answers `load of ... failed: Load request cancelled` and
lands on `/home`.** The router intercepts. Click the tile's `<a href>` instead.
⛔ **`el.click()` on a tile NO-OPS** (the whole card is a div inside the anchor). `ctl input`
`{"type":"click",x,y}` at the card centre routes correctly; so does a full pointer sequence.
⚠ **`Loading chunk NNNN failed`** happens on first entry to `/self-booking/domestic`; the chunk
is fine over curl. Its own `Retry` button does NOT fix it — `location.reload()` does.

## THE FIVE-STEP BOOKING FLOW (measured, in order)

**1. Pickup / Drop off.** Cards are `div.cursor-pointer`; `ctl input` click selects (border goes
red). `Enter Pickup Address` opens a modal: names, mobile, address x3, pincode, date, slot.
⭐ **Pincode auto-resolves office+city+state, but ONLY on a `keyup`+`blur` after the value is
set** — a native-setter injection with input/change alone leaves it blank and the form then
fails validation for a field the user can see is filled.
⛔ **Address Line 2 and Line 3 are capped at 30 CHARACTERS** and the error appears only after a
save attempt. "P.O. Paschim Putiary, P.S. Haridevpur" (37) is REJECTED; split it.
⚠ **Pickup date is locked to TODAY** (`min == max == today`). Slots offered on a 03:30 booking:
`10:00-13:00`, `13:00-16:00`. There is a T&C checkbox at the modal foot that must be ticked
before `Save Address`. Saved state reads back: *"Pickup will be scheduled by <SO> on <date>
between <slot>"*.

**2. Article Information.** `originPincode` / `destinationPincode` / `physicalWeight`.
⛔⛔ **THE DESTINATION PINCODE FIELD CANNOT BE TYPED OR INJECTED — it is a MODAL LAUNCHER.**
Setting `.value` silently reverts to `""` and typing does nothing; clicking it opens a
`Pincode Search` modal (search box + `Search` button + a results table). **Click the RESULT ROW
(x = row.left+30) to select** — that is what fills the pincode AND the PO name. The origin
pincode behaves the same way on the sender step. This burned four attempts; the field looks
broken and is not.

**3. Service Selection.** Choosing `Mail Service Type` opens a **`Suggested Services` modal**
priced for the entered weight. At 20 g, Kolkata->Kolkata: **Speed Post Document Rs 23** (Air) ·
India Post Parcel Rs 34 (Surface) · Speed Post Parcel Rs 34. Then `Mail Shape` =
`Document (Envelope)`, and the VAS checkboxes: POD, Registration, OTP Based Delivery,
Insurance, COD.
⭐ **TARIFF ARITHMETIC, confirmed against the counter tariff:** the quoted Rs 23 is
**Rs 19.50 base + 18% GST**; POD adds **Rs 10 + GST = Rs 12**, giving **Rs 35**. So the portal
quotes GST-INCLUSIVE and there is no online surcharge on the article itself.

**4. Sender / Recipient.** `sender*` and `recepient*` (note the misspelling in the DOM).
⛔ **Name fields accept LETTERS AND SPACES ONLY** — no dots, slashes, or digits, and the error
names the wrong field: a designation like "State Public Information Officer" must be split
across first/middle/last. **`recepientMobileNumber` must start 6-9**, so an office LANDLINE is
rejected outright and the sender's own mobile is the only workable value for an office
addressee. Both pincodes go through the same modal as step 2.

**5. Declarations + Submit.** Two checkboxes (prohibited-items + T&C), then `Submit` raises a
**`Terms & Conditions` dialog (OK)** — *"No cancellation/refund of booking is allowed after
successful completion of transaction"* — and a SECOND `Submit` adds the article to the cart.
Cart shows the allotted barcode, sender/receiver, tariff.

## ⛔ THE PICKUP CHARGE — the fact that decides pickup vs drop-off

> *"Pickup charge of Rs.50/- will be applied for any order with a total tariff below Rs. 500/-
> inclusive of all taxes. Pickup will be free for orders with a total tariff above Rs. 500/-."*

So a single RTI cover pays **Rs 35 article + Rs 50 pickup = Rs 85**, and the footer states it
plainly: `Complete Booking with (Rs.35) + Pickup Charge: Rs.50.00`.
⇒ **The pickup fee is PER ORDER, not per article**, so batching every postal instrument into ONE
booking amortises it to nothing — which is the same law the atlasStore postal queue already
enforces for counter trips. Drop Off avoids the Rs 50 entirely at the cost of the walk.

## Password reset (`/itrolemgmt/verify-mobile/customer`) — PARTIAL, and it FAILED to apply

`Forgot/Reset Password?` is an `<a href="/itrolemgmt/verify-mobile/customer">` that only
navigates under a **full pointer sequence** (plain `.click()` and `ctl goto` both fail); it
redirects to a long `/rp/<hex>` token URL. Customer ID -> `Send OTP` -> **six single-char OTP
boxes** (`maxlength=1`; `ctl input type` with the whole code auto-advances correctly; sender
`JX-INPOST-S`) -> `Verify OTP` -> Change Password (rules: 1 upper, 1 lower, 1 special, 1 digit,
8-30, and not any of the last 3).
⛔ **The change did NOT take, twice.** First attempt: `Bad Request`. Second attempt on a fresh
token/OTP: `Updating...` for ~30 s then **`Load failed`**. A single verification login with the
new password answered `Invalid Customer ID or Password`, proving the old password is still live.
⚠ On the password screen the inputs **lose their `name` attributes after re-render**, and a
native-setter injection then throws *"The HTMLInputElement.value setter can only be used on
instances of HTMLInputElement"* — real `ctl input` click+type is the only route that works there.
⇒ Unresolved: whether this is a portal-side defect or a rejected character class. **Do not
retry blind** — each attempt burns an OTP and risks a lockout on an account with a live booking.
