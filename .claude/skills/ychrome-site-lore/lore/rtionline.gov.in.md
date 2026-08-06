# rtionline.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## submit-rti-request-curl-plus-browser-payment · PARTIAL
task: File 7 central-government RTI applications (Rs10 each) unattended: map the portal, fill the form, pay the fee, capture registration numbers
model: claude-opus-5[1m]
date: 2026-07-25
tags: rti, government, captcha, waf, curl, payment, sbiepay, billdesk, iframe, state-vs-central

**The whole application flow is drivable with plain `curl` — no browser, no login.
Only the payment leg needs a browser.** Mapped end to end 2026-07-25; one
application reached the gateway's Rs-10 confirm screen.

### Facts that decide the approach

- **No account needed.** The home page says it outright: *"(Note: User Registration
  is not mandatory)"*. Do NOT register; do not look for an `rtionline.gov.in` vault
  item (there is none, and none is needed).
- **Central government ONLY.** *"Please do not file RTI applications through this
  portal for the public authorities under the State Governments, including
  Government of NCT Delhi. If filed, the application would be returned, without
  refund of amount."* State RTIs are postal — see the `state` note below.
- **Fee Rs 10**, waived only for BPL with a certificate attached.
- **Text limit 3000 chars**, and the charset is enforced: *only* `A-Z a-z 0-9` and
  `, . - _ ( ) / @ : & ? \ %`. **No apostrophes, no double quotes, no semicolons,
  no square brackets, no en-dashes.** Sanitise the draft BEFORE filing or the
  submit silently strips/rejects. Longer text goes in a PDF under "Supporting
  document" (filename < 12 alphanumeric chars, no spaces, no Aadhaar/PAN).

### ⛔ The WAF rule that costs an hour if you miss it

Every **POST** dies with `curl: (52) Empty reply from server` unless the request
carries a full Chrome header set. `-A <ua>` alone is NOT enough — the **`sec-ch-ua`,
`sec-ch-ua-mobile`, `sec-ch-ua-platform`** trio is what flips it. Required set:

```
Accept: text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8
Accept-Language: en-US,en;q=0.9
Origin: https://rtionline.gov.in
Content-Type: application/x-www-form-urlencoded     (or multipart for the request form)
Upgrade-Insecure-Requests: 1
Sec-Fetch-Dest: document / Mode: navigate / Site: same-origin / User: ?1
sec-ch-ua: "Chromium";v="126", "Not:A-Brand";v="24"
sec-ch-ua-mobile: ?0
sec-ch-ua-platform: "Linux"
Referer: <the page you came from>
```

Also use `--http1.1`: HTTP/2 to this host intermittently fails
`Error in the HTTP2 framing layer`. The server sets a **malformed**
`Set-Cookie: Path=/tmp/NGINX_cache` — harmless, but strip that pseudo-cookie
before handing the jar to a browser or a strict cookie parser.

### The flow (5 hops)

1. `GET /guidelines.php?request`
2. `POST /request/request_email_check.php?pageid=c4ca4238a0b923820dcc509a6f75849b`
   with `CHECKBOX_1=on` (the undertaking) → lands on the email/mobile page.
3. `POST /request/request_email_check.php` with `Email`, `cell` (10 digits),
   `6_letters_code` → redirects to
   `/request/Request_Check_Otp.php?emailchk=…&cellchk=…&urletoken=…`.
   **The OTP goes to BOTH the e-mail and the mobile** (SMS sender `BT-RTISMS-S`,
   "Your Submit Request OTP is: NNNNNN"), so either the mail plane or KDE Connect
   can capture it. Portal note: *"OTPs do not expire until they are used."*
4. `POST` that same OTP URL with `otp`, `emailid`, `mobile_no`, `6_letters_code`
   → redirects to `/request/request.php?emailchk=…&urletoken=…&time=1`, the real form.
5. `POST` the form (multipart) → `/request/payment.php?requestFromId=<hex>`.

### The captcha — the one thing that needs eyes

- Image: `GET https://rtionline.gov.in/captcha_code_file.php?rand=<n>` (root path,
  not `/request/`). `rand` is cache-busting only; **the code is regenerated and
  stored in the PHP session on every fetch**, so fetch it as the LAST request
  before the POST. No NGINX caching (verified: two fetches of the same `rand`
  return different images, `Cache-Control: no-store`).
- **A FAILED POST CONSUMES THE CODE.** Any retry needs a fresh fetch. Two
  submissions were lost to re-using a burnt code while the reading was correct.
- **tesseract is not reliable here** — 5↔S, 7↔T, 8↔S, 1↔I confusions on 3 of 3
  known samples even with a 16-way threshold×psm vote. The reliable read is to
  **split the 130×30 PNG into 6 vertical bands, upscale each ~12× (LANCZOS), lay
  them out with gaps, and read the strip** with the model's own eye. Case-insensitive.
- Accessibility fallback exists (`/audiofile1.php`, opens in a popup) if a speech
  path is ever wired.

### Request form field map (`frmRequest`, enctype multipart/form-data)

`MinistryId` → AJAX `GET /request/getdepartment.php?ministryId=<id>&&type=ministry`
returns the `DepartmentId` (public authority) `<select>`. **Resolved IDs:**

| Public authority | MinistryId | DepartmentId |
|---|---|---|
| Department of Posts | 28 | 28 |
| Department of Administrative Reforms & PG | 76 | 76 |
| Reserve Bank of India | 1738 (Dept of Financial Services) | 2285 |
| University Grants Commission (UGC) | 61 (Dept of Higher Education) | 2295 |
| National Institute of Technology, Durgapur | 61 | 1191 |

(`/request/allpa.php` is the full ~3110-row public-authority list, 690 KB, useful
for resolving any other authority offline. `getMinistry.php` is the type-ahead.)

Remaining fields: `SerchMinistry` (empty), `Email`, `ConfirmEmail`, `MobileStdCode=+91`,
`cell`, `Name`, `gender=M|F|T`, `address1/2/3` (50 chars each), `pincode`,
`chkCountry=001` (India), `stateId` (2-letter, `WB` = West Bengal), `txtCountry`,
`status=R|U` (rural/urban), `educational_Status=L|I`, `graduate_degree=BT|TP|GD|PG`,
`PhoneStdCode=+91 `, `phone`, `Citizenship=I` (only `I` is accepted),
`life=N|Y` (**"Does it concern the Life or Liberty of a Person?"** — N is normal),
`BPL=Y|N`, `bplCardNo`, `YearOfUssue`, `IssuAuthority`, `Description` (the 3000-char
text), `DocumentFile` (send an empty file part), `chkPdfFileType`,
`hndSessionFromId` (**per-render token — re-read it from the page you are POSTing
from**), `requestSubmit=Submit`.

### Payment leg — the only part that needs a browser

`payment.php` → radio `OnlineMode=I` (fires `getpaymentmode.php?mode=I`) → radio
`AssociateBank=01` → submit `SubmitPayment=Pay` → **SBIePay Lite**
(`merchant.sbi.bank.in/merchant/merchantprelogin.htm`, merchant_code `RTI_GOI`,
returns a `merchant_Ref_No` like `POSTSR2026…` — RECORD IT). Options there, all
zero bank charges: `paySubmit('SBINet')`, `paySubmit('OTHERINB')`,
`paySubmitForCard('SBDebit'|'OtherDebit'|'CreditCard')`, `paySubmitUPI('UPI')`.

- **`OTHERINB` → BillDesk** (`billdesk.com/pgidsk/ProcessPayment`), whose entire UI
  is inside an **iframe** `pay.billdesk.com/web/v1_2/sdk` — a top-document query
  returns nothing and looks like "not offered". IDFC First Bank Limited IS listed.
  Selecting it → `auth.idfcfirst.bank.in` (mobile number, then password — **no OTP
  was required**) → `my.idfcfirst.bank.in/ecom/home` with a `Pay ₹10.00` button,
  account picker, and a 50-char `#payment-remarks`.
- ⚠ **"Submitting `frmPayment` leaves the page at `about:blank`" was the ENGINE, not
  the portal** (2026-08-06, opus-5). The hand-off takes a window of its own, and the
  headless engine had no `create` handler, so the navigation was discarded without a
  byte leaving the host while `ctl input` still answered `{"dispatched":3,"ok":true}`.
  An `alert()` on the submit path wedged the page separately — every later verb
  answered `engine call did not answer within 30s`. Both fixed; `ychrome engine
  gateway` is the proof. **If you meet this again, check the engine build FIRST**, and
  remember `ctl pages` only started reporting the live url in that same fix. Not
  re-verified against the real gateway — the fix was proven on a local two-origin
  fixture, and this portal was deliberately not touched.
- ⭐⭐ **NEXT STEP ON THE SDK, and it is one command** (fixer, 2026-08-06). "The SDK
  constructs its iframe and never navigates it" is **NOT a substrate gap** —
  `ychrome engine embed` walks ELEVEN ways of populating a dynamically created
  cross-origin iframe from a real ES module in a real custom element, and **every
  one works** on this build: `src` before and after append, `srcdoc`,
  `document.write` into the initial about:blank child, `contentWindow.location`
  assign and replace, a form whose `target` NAMES the frame, a postMessage
  handshake, an iframe inside a shadow root, and a module that builds at top
  level instead of in a ready handler. So the SDK is not being blocked; it is
  dying in its own JS before it reaches the navigation.
  ⇒ **On the next live run, the FIRST thing to do on the BillDesk page is
  `ychrome ctl console page_id=<p>`** — new verb, ships with the same fix. It
  returns the page's uncaught errors, unhandled rejections, failed subresources
  and console lines, with file, line and stack. Two fixture cases reproduce your
  exact symptom (frame present, `src` empty) and both are explained by one line
  of that output. Read it BEFORE theorising about iframes.
  ⚠ Also worth one probe: **an `id` is not a `name`.** If the SDK targets a form
  at `sdk-iframe` (the id) rather than a frame `name`, the submit asks for a new
  WINDOW, and WebKit blocks a window nobody clicked for — silently. That would
  match everything you measured. `ctl pages` would show no new page (checked in
  the daemon journal for both your runs: zero `engine.window.create`, so if this
  is it, the popup was blocked rather than mis-routed).
- **Card payments are unusable from automation today**: `ychrome-vault` cannot read
  Bitwarden card-type ciphers (see the yggterm campaign field report), so the card
  number/expiry/CVV are unreachable.
- **UPI is not automatable** — collect-request approval needs the phone's UPI PIN.
- **Session hand-off**: transplanting the `PHPSESSID` from the curl jar into a
  browser works (the payment page rendered the applicant's name and fee), so
  "script it on curl, pay in a browser" is a valid split. Strip the malformed
  `Path` pseudo-cookie first.

### Recovery if a payment fails or is abandoned

The application sits **unpaid but staged** with a merchant ref. `/request/status_pendingPayment.php`
("Reconciliation of unsuccessful RTI request payments") takes email + mobile +
captcha + OTP and lists them for payment. **Check that page BEFORE re-filing
anything** — the portal warns that a paid-but-unregistered request must not be
re-submitted (registration numbers can lag 24–48h behind reconciliation).
`/request/status.php` and `/request/status_history.php` retrieve registration
numbers later by email + mobile + OTP, so a lost confirmation page is not a lost
registration number.

### State RTIs

No West Bengal online RTI portal exists: `rtiwb.gov.in` / `wbrti.gov.in` /
`rti.wb.gov.in` do not resolve; `wbic.gov.in` and `wb.gov.in` time out from both
two separate hosts. WBERC's own RTI menu links only to the central Act text. So WB
applications (WBERC / WBSEDCL / WBUHS) are **postal, Rs 10 by IPO or court-fee
stamp, to the SPIO** — print-ready is the only automatable output.

## application-flow-works-payment-fully-blocked · BLOCKED
task: 
model: claude-opus-5[1m]
date: 2026-07-26
tags: 

Second full run of this portal (2026-07-26). Seven applications prepared; the
application flow was driven end to end repeatedly and reliably, and **the run
still ended with nothing filed, because EVERY fee-payment route is closed to an
agent.** Read the payment section before you plan a filing session — the
application half is the easy half and it is not where you will fail.

## ⛔ THE ONE THAT INVALIDATES THE OLD ADVICE: an unpaid application is GONE

The previous entry said an abandoned application "sits unpaid but staged with a
merchant ref" and told you to recover it at `/request/status_pendingPayment.php`
before re-filing. **That is wrong, and acting on it wastes a session.**

`status_pendingPayment.php` reconciles payments where **money actually left the
account** but no registration number came back. It does NOT list an application
that was abandoned, or one whose payment was attempted and failed before debit.
Tested twice today with a captcha the server accepted (it echoes the e-mail back
into the form, so you can tell acceptance from rejection):

- yesterday's abandoned application  -> `alert("There is no pending transaction.")`
- today's application, driven all the way to a bank OTP prompt that then timed
  out with no debit -> **the same** `alert("There is no pending transaction.")`

⇒ **An rtionline application that is not PAID is unrecoverable. Staging and
payment must happen inside ONE live session.** There is no "stage them all now,
pay them later" strategy; each draft simply evaporates. Do not stage an
application until you know your payment instrument works. The portal session
itself also dies in roughly 20 minutes — a staged `payment.php?requestFromId=...`
returns `302 -> /forbidden.php` after that, so even the direct link is not a
way back in.

## ⛔ `status_history.php` IS NOT A USABLE PROBE — and here is how to prove it

The old entry implies it retrieves registration numbers by e-mail + mobile + OTP.
Today it never even reached an OTP step, and worse, it **cannot distinguish a
wrong captcha from a genuine no-records answer**.

Negative control, worth copying as a method: submit once with a captcha you read
correctly, once with a deliberate `QQQQQQ`, normalise away the captcha nonce
(`captcha_code_file.php?rand=\d+`) and the `Date:` header, and diff. Both
responses came back **byte-identical, 17807 characters**. A page that answers
identically to a right and a wrong credential is telling you nothing, and any
conclusion drawn from its silence is void. (`status.php` is different but
useless here for discovery: it needs a registration number you do not have.)

## The WAF empty-reply is TRANSIENT, not just a missing-header symptom

The old entry frames `curl: (52) Empty reply from server` as purely a
consequence of omitting the `sec-ch-ua*` trio. With the full documented header
set I still hit it on a plain **GET**, and on a correctly-headered POST. It is
flaky on this host today. Wrap every request in a retry with backoff (5 attempts
worked without exception). ⚠ One such empty-reply POST also appears to have
**burnt a captcha code**, so treat a 52 as "code may be spent" and refetch.

## Captcha: read the WHOLE image, and segment by ink — do NOT use 6 equal bands

The old entry's advice to "split the 130x30 PNG into 6 vertical bands" is the
**worst** of the three options: 130/6 = 21.6 px cuts glyphs mid-stroke and
invents letters. What actually works, in order:

1. **Whole-image upscale, 8-11x LANCZOS, greyscale.** Read it directly. This got
   about 4 of 5 correct unaided.
2. **Ink-column segmentation** for the hard ones — the reliable method. Threshold
   at <128, sum ink per column, take runs of non-zero columns as glyph
   boundaries, and split any run wider than ~26 px into equal parts (glyphs
   touch). Render each segment separately at ~16-20x with gaps. This resolved
   every ambiguity it was pointed at.

**Glyph discrimination that actually matters here** (the confusions are real and
each costs a round trip):
- `1` has a distinct top-left flag AND a foot serif.
- `I` is a bare stroke with neither — and it is often **strongly slanted**, so a
  clean diagonal with no top bar and no serifs is an `I`, NOT a `7`. Verified
  the hard way: a bare diagonal read as `7` was REJECTED, and the same shape read
  as `I` was ACCEPTED on a later captcha.
- A real `7` in this font does carry a visible horizontal top bar.
- Codes are 6 chars and case-insensitive.

**A wrong captcha is cheap, so do not agonise.** The request form re-renders with
**every other field retained** — ministry, department, state, all radios and the
full multi-thousand-character Description all survive. You lose one round trip,
not the application.

**The rejection is a plain element, not an alert** — grep for exactly:
`<strong style="color:#CC0000";>Captcha code does not match</strong>`

## Corrections to the flow and the field map

- The undertaking form's action is
  `https://rtionline.gov.in/request/request_email_check.php?pageid=<md5>` — note
  the **`/request/` path segment**. Building the URL from the bare
  `request_email_check.php?pageid=...` string you see in the HTML gives a 404.
  Read the `action="..."` attribute instead of reconstructing it. The `pageid`
  is not stable; it is an md5 of a small integer and it rotates.
- **The request form has a captcha field the old map omits:** `6_letters_code`.
  It also carries `hndSessionFromId`, and **that token IS the `requestFromId`** —
  the value you post back becomes `payment.php?requestFromId=<same string>`.
- `DocumentFile` must be sent as an empty file part exactly as a browser sends an
  untouched file input: `Content-Disposition: form-data; name="DocumentFile";
  filename=""` with `Content-Type: application/octet-stream` and no body.
  Building the multipart body by hand and posting it with `--data-binary` is more
  reliable than coaxing `curl -F` into an empty filename.
- There is **no Subject field**. Put `Subject: ...` as the first line of
  `Description`.
- Ministry/department IDs re-verified live and unchanged: Posts 28/28, DARPG
  76/76, RBI 1738/2285, UGC 61/2295, NIT Durgapur 61/1191.
- `graduate_degree` values are BT=Below 12, TP=12, GD=Graduate, PG=Above Graduate.
- OTP SMS sender is **not stable**: seen as `BV-RTISMS-S` and `BH-RTISMS-S`. The
  body is `Your Submit Request OTP is: NNNNNN` — **it does not contain the string
  "RTI"**, so a watcher matching on "RTI" silently never fires. Match on
  `Submit Request OTP`. The OTP also goes to e-mail, which is the better channel
  when the SMS reader is unhealthy.

## Payment: mapped completely, and every route is shut to an agent

`payment.php` -> radio `OnlineMode=I` (fires `getPaymentGateway`) -> radio
`AssociateBank=01`, which is the ONLY gateway offered -> `SubmitPayment` ->
**SBIePay Lite**, which stamps a `POSTSR...` ref and runs a **5-minute on-page
countdown** for the whole remaining journey. Budget for that timer: it is not
generous once a bank OTP is involved.

**Netbanking (`paySubmit('OTHERINB')`) -> BillDesk -> IDFC FIRST.**
- BillDesk's SDK is a **cross-origin iframe**. `web eval --frame` refuses it
  cleanly with `frame_cross_origin`, and `web read` returns nothing for it, so
  the page looks empty while rendering fine. Drive it with **blind coordinates**
  (`web do --x --y`); at a 1280x800 viewport with dpr 1 the iframe covers the
  whole viewport so screenshot pixels map 1:1 to click coordinates.
- ⚠ On that cross-origin iframe the verb reports **`delivered: false` even when
  the input lands**. Do not trust it — confirm by screenshot diff. A batch
  reporting `succeeded: 2, delivered: false` had in fact typed the text.
- `web batch` may refuse the first time with `reason: preempted` ("the user took
  this surface"). `--new-batch` clears it, and the refusal is clean — no partial
  state is applied.
- ⛔ **The old claim "no OTP was required" for IDFC netbanking is FALSE.** Login
  needs no OTP, but after Pay -> Confirm the bank demands a **transaction OTP with
  5-minute validity**. The earlier run never saw it only because it stopped at the
  Pay button. Budget for it: SBIePay's own 5-minute countdown is running at the
  same time, so the OTP must be read and entered fast.
- ✅ **The netbanking layer itself WORKS.** This run failed to complete it, but
  NOT because of the bank: the OTP had in fact been delivered to the handset and
  our READER did not show it (see the canary rule below). Do not record netbanking
  as broken. A later re-login in the same session was refused once with "Please
  enter valid credentials" using a credential verified correct minutes earlier -
  that is an **unexplained single instance**, not a lockout and not a credential
  fault. Still: stop at one failed bank login and escalate, because the cost of
  being wrong about a lockout is far above the fee.

## ⚠⚠ PROVE THE SMS READER IS LIVE BEFORE YOU BELIEVE AN ABSENCE

The single most expensive mistake of this run, and it was OURS, not the portal's.
KDE Connect's SMS sync on the desktop was malfunctioning, and it served a
**partially stale store**. Every conclusion drawn from "the code did not arrive"
was an artefact of a broken reader: the bank's OTPs were on the phone the whole
time. A throttle theory and a wrong-registered-number theory were both built on
that hole, and both were wrong.

**The rule: never conclude anything from an SMS that is NOT there until you have
proven the store is live, with a canary you TRIGGER and then OBSERVE within about
60 seconds.**

⚠ And the refinement that actually bit here, because it defeats a naive canary:
**staleness can be PER-THREAD.** During the failure window a triggered rtionline
OTP appeared in its own thread within ~40 s (three times over), while the bank's
thread stayed frozen on a six-hour-old message. So a canary in thread A does NOT
prove thread B is current. Canary the **same sender family** you are waiting on,
or treat the absence as unknown rather than negative. After a `kdeconnectd`
restart the cache cold-starts and is briefly WORSE - it served months-old threads
and dropped messages it had previously shown - so a restart is not an instant fix
and must itself be re-canaried.

When the reader is in doubt, use the second channel: rtionline sends its OTP to
**e-mail as well as SMS**, and a human watching the handset can relay. (Mail is
read with `ssh <mail-host> 'tb ...'` - one host is designated the mail host; never run a mail client on
the desktop host, it opens a window in the user's face.)

**Card (`paySubmitForCard('CreditCard')`) -> CONFIRM -> button `#Go` ->
`areionsbi.wibmo.com/cardcapture/`** with clean named fields `nameOnCard`,
`pan` (`#cr_no`), `expiryDateYYYY` (`#exp`, MM/YY), `cvv2` (`#cvv-card`).
⛔ **`web fill-card` cannot fill them: `reason: vault_cli_no_card_op`.** This is a
deliberate policy, not a gap — `ychrome-vault card` is metadata-only
(brand/cardholder/expMonth/expYear/last4) because "a PAN printed to a terminal is
durable ... and unlike a password it cannot be rotated on demand"; the number is
meant to reach a page only through ychrome's sidebar injector. So the card is a
**human instrument** here. Note that `web fill-card --help` still advertises
`--field number|expiry|code|holder`, which promises what the credential plane
refuses — do not plan a session around it.

**UPI** needs the phone's PIN. ⇒ With netbanking, card and UPI all closed, **an
agent cannot pay an RTI fee today.** Plan the session around a human at the
payment step, or fix the instrument first.

## The curl -> browser session bridge WORKS (do the application in curl)

Recommended shape: drive the whole application in `curl` (deterministic, no
browser), then hand the session to ychrome only for the gateway.
`curl -c` writes a Netscape jar and `web cookies --import <jar>` reads it
directly. **Strip the server's malformed pseudo-cookie first** — it emits
`Set-Cookie: Path=/tmp/NGINX_cache`, which the jar records as a cookie literally
named `Path`; `grep -v NGINX_cache` is enough. Import then reports
`count: 1, domains: [rtionline.gov.in]` and the browser lands on the staged
payment page fully authenticated. Use a per-agent profile — an unqualified
surface writes the user's own cookie jar.

## card-rail-works-otp-readers-are-the-blocker · PARTIAL
task: 
model: claude-opus-5
date: 2026-07-26
tags: 

Third run (2026-07-26 night). **The card rail is FIXED and PROVEN; the run was
stopped by the OTP READERS, not by the portal and not by the payment instrument.**
Nothing was staged, nothing was submitted, zero rupees moved.

## ✅ SUPERSEDES the previous entry: `web fill-card` WORKS as of yggterm 2.12.16

The previous entry recorded `web fill-card -> vault_cli_no_card_op` as deliberate
policy and concluded "an agent cannot pay an RTI fee today". **The first half was
right about the mechanism and wrong about the verdict.** yggterm 2.12.16 points
`fill-card` at the vault AGENT SOCKET (`op: card-secret`) — the same door the
sidebar injector always used — instead of the CLI, which deliberately has no card
op. The boundary is unchanged (no verb prints a PAN); only the door moved.

Proven live against the deployed build, all four gateway fields:

```
yggterm server app web fill-card --session <s> --item <vault-card-item> \
    --field number|code|holder|expiry|exp-month|exp-year --selector <css>
```

The answer is `{item, field, chars, matched}` — a name and a LENGTH, never a
value — and every fill leaves one line in `~/.yggterm/vault/audit.log` recording
`op: card-secret`, the item, the field list and the host. Character counts came
back consistent with the stored record (holder / number / expiry / code). Needs
only that the vault is UNLOCKED; a locked one refuses `vault_locked` and names
`ychrome-vault unlock`.

⇒ **Do not plan a session around "the card is a human instrument" any more.**
Re-read the payment section of the older entry with that correction applied.

### ⚠ The stored PAN carries SEPARATORS — normalise before submit

The vault record holds the number with embedded spaces, and `fill-card` types
what is stored, verbatim. A gateway field with `maxlength=16` will therefore
TRUNCATE it and the payment fails with a wrong number that looks like a typo.
After filling `#cr_no`, normalise **in place** and never read the value out:

```js
var el=document.querySelector('#cr_no');
el.value=el.value.replace(/\D/g,'');
el.dispatchEvent(new Event('input',{bubbles:true}));
el.dispatchEvent(new Event('change',{bubbles:true}));
```

Verify by asserting a DIGIT COUNT, not by printing: return
`(el.value.match(/[0-9]/g)||[]).length` and check it equals the expected length.

### Smoke-testing a card fill without a live gateway

`file://` navigation is refused on the surface (a `location.href` eval to a local
file silently no-ops and you stay on the current page — check `location.href`
before concluding the fill target is missing). To prove the rail before staging
anything, INJECT throwaway inputs into the current document and wipe them after:

```js
var d=document.createElement('div'); d.id='t';
d.innerHTML='<input id=a><input id=b>'; document.body.appendChild(d);
// ... fill-card into #a / #b, read only lengths ...
d.querySelectorAll('input').forEach(function(i){i.value=''}); d.remove();
```

This costs nothing, touches no gateway, and is the correct way to satisfy the
standing rule *"do not stage an application until you know your payment
instrument works"*.

## ⛔⛔ THE RUN-KILLER: the OTP readers. Read this before planning any filing.

Both agent-facing SMS readers for the handset were unusable, and the mail channel
did not deliver either. This is now the #1 risk to an RTI session — above the
captcha, above the WAF, above the payment gateway.

**1. Termux sshd — the PRIMARY reader — was simply OFF.** Every port refused
(`Connection refused` on the ssh port, and on the obvious alternates). The
critical trap: **the phone was a perfectly healthy tailnet peer at the same
time** — `tailscale ping` returned a pong in well under 100 ms via a direct
path. So a successful tailnet ping proves the NETWORK, and proves NOTHING about
the reader. Probe the actual port, never the peer.

There is also **no way to start it from the desktop side**: the handset exposes
only `device` and `device.conversations` over KDE Connect — no `remotecommands`
plugin — so the run-command route does not exist. Restoring it needs a human at
the phone. Confirm the reader answers BEFORE the session, as its own pre-flight.

**2. KDE Connect: the link was fixable, the STORE was not trustworthy.** The
device showed `paired` with no IP and not reachable; a single D-Bus call restored
it to `paired and reachable` in seconds, with NO daemon restart:

```
gdbus call --session --dest org.kde.kdeconnect --object-path /modules/kdeconnect \
  --method org.kde.kdeconnect.daemon.forceOnNetworkChange
```

That is worth knowing and it worked. But the store it then served was **missing
an entire sender family**:

> 40 threads, spanning 25 days, newest 16 minutes old — and **not one message
> from the portal's OTP sender family**, on a day that family had demonstrably
> delivered four codes (the same codes were sitting in the mailbox).

⇒ **The previous entry's rule is not strong enough.** It said staleness can be
PER-THREAD and told you to canary the same sender family. The failure here is
worse: **the family had no thread at all**, so there was nothing to canary and
nothing to look stale. A missing thread is indistinguishable from a sender that
has never written. **Never treat "the sender is absent from the thread list" as
"no message arrived."** Bank-family threads were fresh to the minute in the same
read, which is exactly what makes this seductive — a canary on the family you
*can* see tells you nothing about the family you cannot.

**3. The mail channel did not deliver in time.** Four codes reached the mailbox
earlier the same day, so the path works in general; the code for a request made
at T0 had still not arrived **35 minutes later**, after a fully completed sync.
Suspect per-day rate limiting on OTP dispatch after several requests, and do not
assume mail is the safe fallback.

### `tb` timing — budget MINUTES, and the flag is `--refresh`

- **The flag is `--refresh`, not `--sync`.** `--sync` is rejected as an unknown
  flag and the command exits non-zero having written nothing.
- A `--refresh` search launches the mail client to sync and takes **4-8 minutes**
  on a cold cache. A 170-second timeout kills it EVERY time, before it writes a
  single byte — which reads exactly like "no results". One completed run needs a
  ~500 s budget. Do not build an OTP poll loop out of short-timeout refreshes;
  it will spin forever producing empty files.
- Reading the mbox tail directly is fast but only sees what the client has
  already synced, so it cannot beat the sync — it is a cheap confirmation, not a
  substitute.

## Recipe points re-confirmed live (unchanged, still correct)

- Hops 1-3 drive cleanly in `curl` with the documented Chrome header set and
  `--http1.1`. The undertaking POST -> the email/mobile page -> the OTP page all
  behaved exactly as recorded.
- **Read the `action="..."` attribute; do not reconstruct the URL.** The
  `pageid` rotates. Extract it with a real parser — a shell regex over the raw
  HTML silently returned empty here and cost a hop.
- The email/mobile form posts `Email`, `cell`, `6_letters_code`, `Submit=Submit`
  to `/request/request_email_check.php` and redirects to `Request_Check_Otp.php`
  with `emailchk` / `cellchk` / `urletoken` query parameters on success.
- Captcha acceptance is unambiguous: a good code redirects, a bad one re-renders
  with `Captcha code does not match` and keeps every other field.
- The OTP page stayed alive and re-fetchable well past the ~20-minute figure
  quoted for a staged payment page, so the OTP gate itself is not tightly timed.

### Captcha segmentation — one concrete fix to the recorded method

Ink-column segmentation is right, but the published split rule under-splits.
Rounding a 32 px run at ~22 px per glyph gives `round(1.45) = 1`, so two touching
glyphs stay merged and the strip reads one character short. Force at least two:

```python
if width > 26:
    n = max(2, round(width / 22))
```

With that fix the strip resolved a genuinely ambiguous glyph on the first read.

## The shape of a session that can actually finish

1. **Pre-flight the OTP reader first, before anything else** — probe the port,
   read a recent message, confirm the specific sender family you will need is
   present in the thread list at all. If the primary reader is down, STOP: get it
   restarted rather than proceeding on a fallback that can drop a whole family.
2. Prove the card rail on injected inputs (above). Cheap, no gateway.
3. Only then stage, and pay inside the same live session.

## filed-end-to-end-transerror-lies-captcha-one-shot · WORKS
task: 
model: claude-opus-5
date: 2026-07-26
tags: 

Fourth run (2026-07-26 night). **Applications were FILED end to end for the first
time — staged, paid by card, 3DS passed, registration numbers received.** The card
rail and the payment leg are no longer the hard part. What now costs a session is
(a) the portal's lying failure page, (b) a one-shot captcha at the staging hop, and
(c) the phone reader dying mid-run.

## ✅ CONFIRMED: the whole chain works, unattended

`curl` for the application, cookie-jar bridge to a ychrome surface for the gateway,
`web fill-card` for the card, SMS for the bank code. Three applications completed
this way in about 35 minutes, three debits, three registration numbers, no human
touched anything. The previous entry's card-rail findings all held.

Timings worth budgeting: bank debit lands **~114 s after the Pay click**, so the
gateway's 5-minute countdown is not the binding constraint it looks like. The
registration e-mail arrives **~3 minutes** after that.

## ⛔⛔ THE ONE THAT WILL MAKE YOU FILE A DUPLICATE: `transerror.php` LIES

After the 3DS the browser sits on *"Please wait while your transaction is being
processed"* for about two minutes and then lands on **`/transerror.php`**, which
says in terms:

> RTI Request filing failed! Sorry, your RTI Request could not be filed!!!

**It was already filed.** The confirmation e-mail carrying the registration number
arrived about three minutes later, with `Transaction Status : Completed
Successfully`. That page also tells you the number will come in 24-48 hours after
reconciliation; in practice it came in three minutes.

⇒ **Never conclude anything from the portal's own page.** The success criteria, in
this order, are: **the bank debit SMS**, then **the confirmation e-mail with the
registration number**. Both are independent of the portal. If the page says failure
and either of those says success, the filing SUCCEEDED. **Never re-file on the
strength of `transerror.php`** — that is how you get a duplicate application and a
second debit. A payment whose outcome you cannot yet determine is **UNKNOWN**, not
failed: wait and re-check.

Reconcile at the end of every session: **count of debits must equal count of
registration numbers.**

## ⛔ The staging captcha is ONE-SHOT PER PORTAL SESSION

This is the most expensive discovery of the run and it is not documented anywhere
above. On the request-form POST (the staging hop), **a single rejected captcha
poisons that portal session permanently** — every subsequent captcha in the same
session is rejected no matter how correct, while the page keeps re-rendering with
all fields retained and the `Captcha code does not match` element present.

It was proven not to be a reading problem and not a payload problem:

- the identical reading method was **8 for 8** on the earlier hops (undertaking,
  e-mail/mobile, OTP) in the same sessions;
- a **negative control** with a deliberately wrong code produced exactly the
  expected error page, so the field is being read by the server;
- the **PHPSESSID is byte-identical** across the captcha fetch and the POST;
- **six consecutive clean six-glyph reads** were all rejected in the wedged
  session, after which a FRESH session accepted its first read immediately.

⇒ Treat the staging captcha as one attempt. On a rejection, **abandon the portal
session and restart from `/guidelines.php?request`** rather than retrying. The
retry is not merely useless, it is what convinces you your captcha reading has
broken when it has not. Cost of a restart is one fresh OTP, so read that one
captcha carefully.

## Captcha reading: the render matters more than the segmentation

The previous entry's advice to segment by ink columns is fine but secondary. The
thing that actually moved accuracy was **how the image is rendered**:

1. **Median filter (radius 3) to kill the speckle, then autocontrast, then upscale
   ~14x LANCZOS.** Read the whole strip. This is dramatically better than a raw
   upscale.
2. **Do NOT hard-threshold.** A `point(v < 140 -> 0 else 255)` binarisation was
   tried and it *thickened strokes into blobs* and made glyphs unreadable. Gentle
   beats aggressive.
3. Per-glyph crops at ~34x are the tie-breaker for a confusable pair, and the
   segmentation prints the runs so you can see when two glyphs merged.

**Codes are not always 6 characters** — a 5-glyph code was observed (a genuine
18 px ink-free gap, verified by column profile, not a faint glyph). Do not force a
6-character reading by splitting a wide run.

Confusable pairs that actually matter here, with the discriminator:
`4` has an open apex and a stem that continues BELOW the crossbar; `A` has a closed
apex and no descender. `B` has a straight left stem, `8` does not. `Z` has flat top
and bottom strokes, `2` has a curved top. `1` carries a top-left flag AND a foot
serif; `7` carries a full horizontal top bar; `I` has neither and is often slanted.
A chevron glyph (`>` or `<`) is a rotated **V** — both rotations occur.

## The OTP channels, corrected AGAIN — and the correction matters

The previous entry called e-mail an unreliable fallback that "did not deliver in
time". **That was wrong, and it was our own timeout.** In this run e-mail was the
*more* reliable channel:

- Portal OTPs arrived by e-mail **within about a minute**, every time.
- The previous run's failure was a short timeout killing `tb ... --refresh`
  mid-sync. **Budget ~500 s for a refresh.** (`--refresh` is the flag; `--sync`
  does not exist.)
- The OTP mail lands in the **outlook** account, not gmail — though one arrived in
  gmail, so search both rather than pinning a folder.

**SMS dispatch for the portal is THROTTLED after a few codes**: three arrived early
in the session, then the portal stopped sending SMS entirely while continuing to
e-mail every code. A missing portal SMS therefore proves nothing.

**The portal reuses the SAME code until it is CONSUMED.** Three separate requests
were served the identical code; once one application consumed it, the next request
got a new one and the old one answered `OTP does not match`. So a cached code is
worth trying once, but expect a fresh one after every successful filing.

⚠⚠ **The split that decides session planning: e-mail covers the PORTAL OTP only.
The bank's 3DS code is SMS-ONLY.** So the phone reader is mandatory for payment
even though it is optional for the application. **Canary the SMS reader immediately
before staging** — an unpaid application evaporates, so staging into a dead SMS
channel burns the application.

The bank's own 3DS code can be **slow** (>100 s) rather than missing; a 100-second
poll window timed out once on a code that then arrived. The 3DS page carries a
**Resend** control which is legitimate — it re-requests the code for the SAME
in-flight transaction and is not a payment retry. Using it recovered that payment.

## Two handset failure modes, and they are DIFFERENT

Both killed a stretch of this run, and they need different responses:

| Signature | Meaning |
|---|---|
| `tailscale ping` **pongs**, port 8022 **refused** | the phone is on the network but Android froze the sshd. Doze. |
| `tailscale ping` **no reply**, port 8022 **times out** | the phone is off the tailnet entirely. |

Refused and timed-out are not interchangeable, and neither is fixable from the
desktop — there is no run-command plugin on the KDE Connect link. Probe the PORT,
never the peer.

**KDE Connect is NOT a usable fallback for this.** Its link was repairable in
seconds with `forceOnNetworkChange`, but the store served was an hour stale and
missing the sender family entirely; requesting a refresh (`requestAllConversationThreads`,
note the name — `requestAllConversations` does not exist) **emptied the cache to
zero threads** and it did not rebuild. Its `watch` mode is also broken for this
device: it drives a `.../sms` object path that does not exist here, so it errors
forever while looking like it is waiting.

## Portal mechanics: three corrections to the recorded flow

- **Parse the RIGHT form.** The pages carry a language form FIRST; a naive
  "first form on the page" parser silently posts the wrong fields. Select by name:
  `FrmStatus` (e-mail/mobile), `FrmFirstAppeal` (OTP), `frmRequest` (the request).
- **A byte-identical re-render means YOUR POST was malformed, not that the server
  rejected your credential.** An OTP POST that omitted the `Submit` key was
  discarded silently, and the response differed from the original page only in the
  captcha nonce. A genuinely wrong OTP renders a visible `OTP does not match`.
  Diff normalised responses before theorising.
- **Never post the `resend` field.** It is a submit button rendered **disabled for
  the first 300 s**, so a browser never sends it; posting it makes the server branch
  to a resend. And **two resends in quick succession returned 403 -> `/forbidden.php`,
  which killed the session outright.** One resend, then wait.

Also: a staged `payment.php` expires in roughly 20 minutes and then answers
`/forbidden.php`; the application evaporates unpaid, at zero cost. That is the safe
failure mode — losing a staged application is free, paying twice is not.

## Cookie bridge and surface hygiene

The `curl -> browser` bridge works exactly as recorded, with one snag: **`curl`
writes an EMPTY expires field for a session cookie and the import rejects the jar**
with `bad_jar: line N: bad expires field ""`. Rewrite that column to `0` before
importing. (Strip the malformed `NGINX_cache` pseudo-cookie as before.)

Two yggterm issues hit this run and both are filed there rather than here:
`web fill-card` and every other verb began refusing with `reason: preempted` on a
surface nobody had touched, and the only cure was a new surface generation; and an
unrevealed surface reports itself **visible** to the web engine, so a gateway page
with a spinner burns real CPU on the GUI host. **Park the surface at `about:blank`
between payments** — never mid-transaction, and never while a 3DS or processing page
is live.

## The shape of a session that finishes

1. Canary the SMS reader. If it refuses, STOP — do not stage.
2. Application hops in `curl`. Read each captcha from a median-filtered, autocontrast,
   14x render.
3. Read the portal OTP from **e-mail** (budget ~500 s for the refresh); do not wait on SMS.
4. Stage — **one captcha attempt only**. On rejection, restart the session.
5. Pay immediately. Card fields, then normalise the PAN in place and assert 16 digits.
6. 3DS code from SMS; use the page's Resend if it is slow.
7. **Ignore whatever the portal page says.** Confirm from the debit SMS and the
   registration e-mail. Reconcile debits against registration numbers before
   declaring the session done.

## t4-t7-filed-handset-identity-and-card-limit-traps · WORKS
task: 
model: claude-fable-5
date: 2026-07-27
tags: 

Fifth run (2026-07-27 evening). **T4-T7 all filed: RBIND/R/E/26/NNNNN,
UGCOM/R/E/26/NNNNN, DARPG/R/E/26/NNNNN, NITDP/R/E/26/NNNNN. Four debits, four
registration numbers, reconciled.** Recipe held end to end. New traps:

1. **HANDSET IDENTITY: a canary that ANSWERS is not a canary on the RIGHT STORE.**
   One of the family handsets receives IDFC debit-card/savings families and
   passes every liveness canary, but has NO RTISMS, NO EJGRTI, NO WOW-card-NNNN
   traffic ever. The true handset is the Pixel 8 (SIM NNNNNNNNNN).
   Discriminator: the round-3 debit SMSes (26-07 21:43:32/22:05:16/22:13:49)
   exist only on the true phone. Tailnet-dead != sshd-dead: it answered on the
   home LAN (192.168.N.NNN:8022, and link-local via a ProxyJump) all along.
2. **Card-limit exhaustion mode:** 3DS OTP arrives, ACS accepts, THEN SBIePay says
   FAILURE; the tell is the bank SMS "exceeds your available limit". Zero debit.
3. **After a failed gateway txn, payment.php -> confirmpayment.php auto-verify ->
   /forbidden.php kills the session.** No re-payment path; re-file fresh.
4. **IDFC netbanking merchant OTP did not deliver** (2 dispatches incl. Resend,
   0 arrivals in 4 min) while card-3DS/portal OTPs hit the same phone in ~60 s.
   Login worked first try via fill-vault. Netbanking: login-provable,
   OTP-unreliable; the card stays the rail.
5. **Portal-OTP e-mail throttles late in a run** (6th dispatch in 30 min never
   e-mailed; SMS copy still delivered over cellular during a wifi Doze gap).
6. **A session held ~29 min at the OTP gate answers Forbidden** — the "well past
   20 minutes" claim has a ceiling. Restart is cheap; the portal re-sends the
   SAME unconsumed OTP code to the new session.
7. Surface note: the payment surface died once ("web surface not live");
   `web ensure` rebuilt it (generation bump), cookies re-imported, run continued.

## invalid-details-is-not-a-captcha-error · PARTIAL
task: 
model: claude-opus-5
date: 2026-08-04
tags: 

Fifth run (2026-08-04). **T5 (UGC) FILED end to end unattended — UGCOM/R/E/26/05932,
Rs 10 by card, 3DS passed.** T6 (DARPG) blocked portal-side with nothing staged and no
money moved. Two corrections to earlier entries, one of them important.

## ⛔⛔ CORRECTION: "Invalid Details.!!" IS NOT A CAPTCHA ERROR

No earlier entry separates these two messages, and conflating them costs a session.
Today one POST came back with the captcha **accepted** (no `Security code does not
match` element anywhere in the response) and the answer `Invalid Details.!!` — using
the identical applicant block that had filed T5 successfully thirty minutes earlier.

| Page says | Meaning | Cost |
|---|---|---|
| `Security code does not match` (form re-renders, all fields retained) | ordinary captcha miss | cheap, retry |
| `Invalid Details.!!` (form NOT re-rendered) | **the portal will not issue an OTP for this identity right now** | run is over until it clears |

⇒ The reading that fits: **an OTP is bound to the e-mail+mobile, not to the session**
(the portal's own banner says *"OTPs do not expire until they are used"*), and while one
is outstanding and unconsumed the portal refuses to start another request. Today's
sequence: T5 consumed OTP #1; T6 reached the OTP page and was issued OTP #2; two captcha
misses at the OTP hop meant #2 was never consumed; every later attempt answered
`Invalid Details.!!` with a good captcha. A per-identity issuance throttle fits equally.

**Do not respond to `Invalid Details.!!` by re-reading the captcha, restarting the
session, or touching the applicant block.** All three were tried; all three are wasted.

## The one-shot captcha poison is NOT limited to the request-form POST

The previous entry documents session poisoning at the staging hop. It happens at the
**earlier hops too**: two clean reads were rejected in a row at the OTP hop, and a
genuinely fresh session (jar deleted, new PHPSESSID) then rejected its first read at
the e-mail/mobile hop. Treat *any* captcha rejection as session-fatal.

## Glyph discriminators added today

- A bare slanted stem **with a foot serif but no top-left flag** is **`L`**, not `1`
  and not `I`. (`XBCZLE` passed the staging hop first time on this call.)
- **`9` vs `g`**: `9` has a straight descender; `g` curls back left under the bowl.
  Both appeared today.
- A **chevron `∧`** is a rotated `V`, same as the `>` / `<` forms already recorded.
- ⚠ **Segmentation invents letters.** A 32 px ink run split at 17 px produced a
  confident-looking `A`+`N` that was rejected. **Read the WHOLE strip first** at ~18x
  to get the glyph count from spacing, and use per-glyph crops only as a tie-breaker
  on a specific confusable pair.

## Working notes for the payment leg (unchanged where not stated)

- `web fill-card` filled all four fields but reported **`matched: false` on every one**
  while every value landed. Its own field is an assumption; verify page-side.
- ⚠ **The CVV landed 2 of 3 characters on the first fill** and needed a clear-and-refill.
  `fill-card` had already reported `chars: 3`. **Assert the CVV length page-side before
  clicking Pay** — a short CVV burns a staged application, which is unrecoverable.
- `#cr_no` has `maxlength=23`, not 16, so stored separators fit; normalise anyway.
- Timings: bank debit SMS **~90 s** after the 3DS submit; registration e-mail ~90 s
  after that. Faster than the previous run's ~114 s + 3 min.
- Portal OTP SMS sender seen as **`BZ-RTISMS-S`** and `BV-RTISMS-S` today; the known
  set was BV/BH/BT. **Match on the body string `Submit Request OTP`, never on a sender
  prefix list.** Bank 3DS came from `VD-IDFCFB-T` (the `-T` suffix distinguishes it from
  same-day `-S` promotional traffic).
- The registration e-mail landed in **gmail** today; the previous run said outlook.
  Search both.
- ✅ The portal's `doubleverification.php` → generic-page ending still tells you
  nothing. Debit SMS then registration e-mail remain the only success test.

---

## ★★★ 2026-08-06 run 5 — the BillDesk Embedded SDK v2 blocker, SOLVED (atlasStore lobe)

Measured against `190da86` (verified deployed: binary mtime 19:08:46, daemon pid 124242 started
19:08:55 — daemon NEWER than binary).

**`ctl console` on the BillDesk bootstrap page returned EMPTY** — no uncaught errors, no
rejections, no failed subresources. That is the finding, not the absence of one: the SDK is not
crashing. One `ctl dom` read then answered it:

```
iframe  id="sdk-iframe"  name="response-frame"
form    action=https://pay.billdesk.com/web/v1_2/sdk  target="response-frame"  method=post
```

⇒ **The SDK builds the iframe AND a correctly-targeted form, and never calls `submit()`.**
⛔ **The id-vs-name hypothesis is FALSIFIED for this page** — the form's `target` already equals
the iframe's `name`. No popup was ever requested, matching the journal's zero `engine.window.create`.
✅ The engine was never at fault. `ychrome engine embed`'s eleven passing cases were right.

### ⚠ THE MEASUREMENT TRAP THAT COST THREE SESSIONS

**`iframe.src` is NOT a navigation indicator.** A form POST into a *named* frame navigates it
while leaving the `src` attribute empty forever. Three sessions polled `src`, saw `""`, and
concluded "the SDK never navigates its iframe".

**The honest test is document access:**
```js
try { window.frames[0].document; "same-origin, still about:blank" }
catch (e) { "SecurityError => a real cross-origin document IS loaded" }
```
After a manual `f.submit()` this flipped to `SecurityError` immediately.

### THE WORKAROUND THAT REACHES THE BANK LIST

```js
const f = [...document.querySelectorAll("form")].find(x => x.target === "response-frame");
f.target = "_self";   // render the payment UI TOP-LEVEL
f.submit();
```
The order token tolerates the second POST (the payload carries `retryCount`). Top-level renders
the full Net Banking bank list, drivable with ordinary `ctl eval`; clicking `IDFC First Bank
Limited` reaches `auth.idfcfirst.bank.in/opt-customer-login`. `ctl fill entry=idfcfirstbank.com`
returns `user-only` on step 1 and `filled` once the password field exists.

### ⚠ TWO ENGINE GAPS THIS EXPOSED (both are why `_self` was necessary)

1. **No cross-origin frame reach.** `ctl eval` is page-scoped, so the in-frame payment UI is
   unreadable. A frame-targeted eval (or exposing subframes in `ctl pages`) would remove the need
   to re-POST top-level.
   ✅ **2026-08-06 — the substrate question is answered and the answer is cheap.** `ychrome engine
   frames` (8/8, mutation-proven) shows a cross-origin child IS reachable with no WebKit
   web-process extension: an `@all-frames` userscript runs inside a bank frame, and a rect
   measured in the child plus the iframe's own rect gives a coordinate a real `GdkEvent` click
   lands on (`isTrusted: true`), with typed text reading back from inside the child. ⚠ The
   `frame=` verb is not built yet, so **`_self` is still the working method today** — but the
   next lane here should check `docs/pending-bugs.md` before re-POSTing, not assume.
2. ✅ **`ctl shot` writes a file now** (`f03b0c2`): `--out FILE` always worked and `out=FILE` now
   does too; `path=`/`file=`/`dest=`/`output=` are refused **by name** instead of being ignored.
   The old note below is why blind-coordinate driving looked like the only alternative — and it
   was still correctly refused. ⛔ A driving surface with neither frame reach nor a screenshot
   must not be used to spend money; that rule stands whatever the tooling does.

### Portal-driver lore (`rti_portal.py`)

⚠ **`identify` prints `ok:true` and THEN renders the next hop's captcha.** A truncated read of its
output looks exactly like a failure. Re-running it burns a live session and a portal OTP. Read the
whole output — the second JSON object is the next captcha, not an error.

### Glyph discriminators (this run)

- A **bare vertical with no serif and no top-left flag is `I`**, not `1`. (`9UFI8R` passed.)
- A stroke that **hooks left at the bottom is `J`**, not `7`; `7` has no bottom hook. (`7KBJDC`
  passed the one-shot staging hop.)

---

## ⭐⭐ PAYMENT INSTRUMENT ORDER — OWNER THUMB RULE (2026-08-06). Read this BEFORE picking a MOPS tile.

**His words:** *"we pay by wow avikalpa credit card NOT netbanking unless all option fails or they
carry extra charge and netbanking or UPI doesn't. Even in those cases we use UPI first and
Netbaking last."*

1. ⭐ **CARD FIRST** — the WOW Avikalpa card → `paySubmitForCard('CreditCard')`.
2. **UPI SECOND** → `paySubmitUPI('UPI')`.
3. **NETBANKING LAST** → `paySubmit('OTHERINB')`.

Fall back ONLY if the card leg fails, or if the card carries a surcharge the others do not.
**Cost moves you down the list; convenience never does.**

⛔ **THE "`paySubmitForCard(...)` ⛔ NEVER" LINE IN THE EARLIER ENTRIES IS SUPERSEDED AND WRONG.**
It was a **capability limit recorded as a prohibition** — an early headless session could not reach
the vault card op, concluded "the card is a human instrument", and the note outlived the limitation
(the `web fill-card` WORKS entry above already corrected the mechanism, but the ⛔ was never pulled
out of the recipe). **On 2026-08-06 that stale line sent a full run down the netbanking rail to
`auth.idfcfirst.bank.in`, where it died on a stale password — a blocker the card would have walked
straight past.**
⇒ **Standing lesson: a capability limit written down as a prohibition will outlive the limit.**
When lore forbids a route, ask whether it is forbidden or merely was once impossible.

⚠ **The card leg lives on a yggterm WEB SURFACE, not a ychrome page.** `ctl fill` (ychrome) does
credentials; `yggterm server app web fill-card --session <s>` does cards. **Check
`yggterm server app clients` first — zero clients means no card rail in this session** (that was
the state at 19:40 on 2026-08-06).

⚠ **Trigger (b) is live on THIS gateway.** The BillDesk SDK payload carries
**`showConvenienceFeeDetails=true`**, i.e. per-instrument fees are displayed. **Read the fee shown
against each instrument before choosing** — if the card is surcharged and UPI is not, the rule
routes to UPI. The rule is only correctly applied by someone who looked.

⛔ Unchanged: **never a card number by any route an agent can read.** `fill-card` answers
`{item, field, chars, matched}` — a length, never a value — and audits `op: card-secret`. Verify by
digit count, never by printing. Normalise the stored separators in place before submit.
