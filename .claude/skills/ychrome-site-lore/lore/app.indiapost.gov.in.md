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

## drop-off-booked-and-paid-surcharge-and-input-rungs · WORKS
task: 
model: claude-opus-5
date: 2026-08-07
tags: 

Second full run of Click-n-Book, **DROP OFF branch, booked AND PAID** on 2026-08-07 04:57-05:40 IST.
Driven on the **yggterm `web` plane on the GUI host** (ychrome profile `agent-ipbook`), not the `ctl`
engine, because the card rail is a `server app web` verb and `ctl` has no card verb.
Article `EY492675435IN`, Rs 23 tariff, **Rs 23.25 debited** (see THE SURCHARGE).
Everything below was measured this run and several items CORRECT the 2026-08-07 entry above.

## ⛔⛔ THE SURCHARGE — app.indiapost.gov.in is a UPI-FIRST portal, not a card-first one

The gateway page states in its own words: **"Note: No charges for UPI & Debit cards"**, and the
credit-card leg then debited **Rs 23.25 against a quoted Rs 23.00** — a credit-card-only
convenience fee of **Rs 0.25 (~1.09%)**. The owner's ruling at the moment of payment:
**"Next time we go UPI route for Indiapost."**
⇒ Under the standing instrument order (card first *unless it carries an extra charge the others do
not*), **UPI is the correct first choice here** and the card is the fallback. Cost moves you down
the list. The `ePay` tile expands to **UPI · Credit Card · Debit Card · Net Banking**.

## ⛔ THE LOGIN PAGE DOES NOT HYDRATE ON FIRST LOAD — and it looks exactly like a bad password

On first entry to `/customer-selfservice/login`: **no DOM node carries a `__reactFiber$` key**, the
Login button is `disabled: true` forever, and typing moves `input.value` while React state never
follows. **`location.reload()` fixes it** — after the reload the fibers attach and the button
enables the instant both fields hold values. Diagnose it with
`Object.keys(document.body.children[0]).filter(k=>k.startsWith('__react'))` — an empty array means
reload, not re-check the credentials. Same family as the known `Loading chunk NNNN failed`.

## ⛔⛔ THE PINCODE MODAL: which of its four steps need TRUSTED input, exactly

The existing "it is a modal launcher" note is right but too coarse. Per step:

| step | rung that works | rung that FAILS |
|---|---|---|
| open the modal | **trusted** `web do click --selector '#<field>'` | page-side pointer sequence: focuses the field, **no modal** |
| type the pincode | **trusted** `web do fill --mechanism real-keys` | native-setter injection → the modal auto-searches and **hangs on `Searching...` forever**, and then will not close |
| press Search | trusted click on `button` text `Search` | — |
| pick the result row | **page-side pointer sequence** on `tr > td:first-child button` | a trusted click is usually refused `target_moved … outside the viewport` (the row sits below 800px and the dialog scrolls internally, so `scrollIntoView` on the cell does not help) |

⛔⛔ **AND THE TRAP THAT COSTS A WHOLE FORM: a PAGE-SIDE select fills the field but LEAVES THE MODAL
OPEN**, while a trusted select closes it. **An open modal stays bound to the field that opened it**,
so the next field's click lands on the stale modal and the next selection **writes into the WRONG
field.** Measured: a destination search wrote 700001 into `dropOffPincode` AND `originPincode`.
⇒ **After a page-side select, close the dialog with a TRUSTED click on its own `Close` button, then
assert the modal is gone before touching another field.** Detector:

```js
[...document.querySelectorAll('.fixed')].some(d => d.innerText.includes('Enter pincode or office name'))
```

⚠ Also: the old "click the result row ~30px in from its left edge" is **too imprecise**. On a row
whose left edge was 161 the Select button's centre was **x≈201**; a click at 191 hit cell padding
and produced the misleading toast **`Error fetching pincode data`**. Address the button, not a pixel.

⚠ **The 700001 search returns a DIFFERENT ROW SET run to run** — sometimes ONE row whose Office Name
is `Kolkata GPO DC`, sometimes ~10 (`KOLKATA GPO`, `Lalbazar SO`, `Customs House SO` …) that all
share `Idc Id 33840013` / `Idc Name Kolkata GPO DC`. **Match on the Idc Name column** or the matcher
fails at random. Whichever row is picked, `boId` resolves to `Kolkata GPO DC`.

## The input rung is PER-WIDGET on this page — there is no single answer

- **Radix `[role=combobox]`** (Mail Service Type, Mail Shape) **REFUSES `web do click`**: it answers
  `accepted:true, delivered:false` while the element hit-tests clean and is not disabled. The
  **page-side pointer sequence opens it first try**, and its `[role=option]` children take page-side
  clicks too.
- **The Suggested Services card needs a TRUSTED click**; a page-side sequence on it does nothing.
- **The `div.cursor-pointer` Pick Up / Drop Off cards, the T&C `button[role=checkbox]#terms`, the
  declaration checkboxes and the icon-only Next button all take page-side pointer sequences.**
- **Sender/recipient text fields take a page-side native-setter injection cleanly** — all 13 filled
  in one script and survived submit validation.
⇒ Expect to switch rungs per widget and **read back the effect every time**; `delivered:false` here
is an honest refusal, not a transport failure.

## ⭐ Mail Service Type is DISABLED until the tariff round-trip finishes, and BLUR is the trigger

The tariff fetch fires on the weight field's **blur**, not its input. Set `#physicalWeight` then send
`Tab`; the API answers

```json
{"chargeable_weight":20,"local_flag":true,"price":19,"base_tariff":19,
 "cgst":2,"sgst":2,"igst":0,"gst":4,"total_with_tax":23}
```

and **the Suggested Services modal opens by itself**. An agent that clicks the combobox before the
blur reads a disabled control and concludes the page is broken. (Confirms the GST-inclusive
arithmetic: Rs 19.50 + 18% ≈ Rs 23, quoted as base 19 + GST 4.)

## ⛔ CORRECTION: Submit → the T&C dialog → OK is the WHOLE add-to-cart

The earlier entry says "Submit raises the dialog, then a SECOND Submit adds the article". **On the
drop-off branch that is wrong and dangerous**: after pressing OK the footer already read
`View Cart (1)` / `Complete Booking with (Rs.23)`. **A second Submit would have added a second
article.** Verify the cart counter instead of pressing Submit twice.

## ⛔⛔ THE PAYMENT LEG OPENS A NEW TAB VIA `window.open`, AND YOU MUST HOOK IT

`Complete Booking` → `ePay` → `Credit Card` calls `window.open('/pmtgw/unified//<hex>')` — note the
**double slash** — and the parent page then shows only *"We have opened the payment page in a new
tab"* with a **10-minute timer** and a `Verify Payment` button. Facts:

- ⛔ **The popup itself is BLOCKED on a yggterm surface** — after the click the parent shows its
  "we have opened the payment page in a new tab" copy, and `server app state | jq .web_surface_tabs`
  still lists **only tab 0**. The page's `window.open` returned null. (Client bug, filed:
  `yggterm/docs/pending-bugs.md` § *WEBKIT'S OWN POPUP BLOCKER*.) ⇒ **hook `window.open`, capture
  the URL, and open it yourself** — that is the working route today, and it is what the hook below
  is for.
- Relaunching the captured URL with `ychrome --profile <same> <url>` makes it **`tab_id 1` of the
  SAME yggterm session**, and it becomes the active tab, so `web eval --session <path>` addresses it
  with no extra flag. ⚠ **There is no verb to switch back to tab 0** — plan to finish in the payment
  tab.
- To capture the URL, hook `window.open` **before** triggering the gateway:
  `window.open=function(u){window.__opened.push(String(u));return {closed:false,close(){},focus(){}};}`
  Cancelling the "Payment Gateway Opened" dialog returns you to the method list, so the gateway can
  be re-triggered with the hook armed. **Re-triggering costs nothing — it mints a payment session,
  not a charge.**
- ⚠ `ychrome --profile <p> <url>` from a SECOND yggterm session does **not** create a second surface:
  it hands the URL to the already-running ychrome for that profile, which opens it as a tab on the
  FIRST session's surface. The second session then answers `no_declare` to `web ensure` — a correct
  refusal, not a broken tool.
- The success page carries **Transaction ID · DoP Order No · Bank Reference No · Amount**, and its
  URL query is a **JWT whose payload holds `amount`, `status`, `txnId`, `refId`, `indentid`,
  `txnprocessedat`** — decode it for a machine-readable receipt.

## ⚠ TAKE A LONG SURFACE LEASE — the reclaim eats the whole form

Twice the surface was reclaimed mid-form (`web ensure` → *"page was unresponsive; a rebuild is
queued"*), each time dropping the SPA back to `/home` and **losing every field entered**. It stopped
after **`web ensure --session <s> --ttl 3600`**. ⇒ on any multi-step form take a long lease up front
and re-`ensure` between steps. Three full restarts were paid for this.

## Confirmed unchanged, and worth restating

- **The article barcode is allotted at CART-ADD, before payment** (`#articleId` held
  `EY492675435IN` as soon as the sender step rendered).
- Deep-linking an inner route is still REFUSED — navigate by clicking the tile's `<a href>`.
- Name fields are **letters and spaces only** (so `State Public` / `Information` / `Officer`), and
  `recepientMobileNumber` must start 6-9, so an office landline is refused.
- **Drop Off shows its cutoff as soon as the office is chosen**: *"Please Drop off the article before
  1500 Hrs at Paschim Putiari SO"*. No pickup charge on this branch.
- **My Bookings → filter combobox → `Domestic Booking`** is where a paid article is verified; it
  defaults to `International Booking` and shows *No Data Available*, which reads like a lost booking.
- ⛔⛔ **CORRECTION, same day, from the operator driving it by hand: the `Receipt` and `Label`
  buttons are NOT broken and the site is fine — THE POPUP IS BEING BLOCKED.** In a normal browser
  the webapp says so out loud (*"popup is getting blocked"*); on a yggterm surface the agent sees
  **nothing at all** — no `window.open` on a monkey-patched hook, no dialog, no download — and this
  entry originally recorded that silence as "the buttons produce nothing". **That reading was
  wrong.** ⇒ **On this plane, treat "a button that opens a document did nothing" as a BLOCKED POPUP
  until proven otherwise.** Cause and the fix are filed as a client bug, not a site fact:
  `yggterm/docs/pending-bugs.md` § *WEBKIT'S OWN POPUP BLOCKER EATS `window.open`* — nothing sets
  `javascript_can_open_windows_automatically`, WebKit defaults it false, and it then refuses any
  `window.open` outside a live user-activation window, which an `await` spends. The same cause ate
  the payment gateway's own popup (below). Until it lands, the receipt cannot be captured from a
  surface — every identifier needed to re-fetch it is in the booking row.
