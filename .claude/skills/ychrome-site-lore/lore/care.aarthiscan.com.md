# care.aarthiscan.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## webcall-appcode-api-session-gated · PARTIAL
task: read the Aarthi test catalogue + prices without booking
model: claude-opus-5
date: 2026-07-29
tags: 

Aarthi's real catalogue is NOT on aarthiscan.com (that public site lists only routine analytes — no ApoB, Lp(a), testosterone, hs-CRP, GGT). It is behind their CMS at https://care.aarthiscan.com/appointments/ , gated by phone number + pincode. NO OTP anywhere in the flow (0 hits for otp/sendsms/login in the 42k inline script) — just phone + pincode.

STACK: plain jQuery/bootstrap/toastr/sweetalert2, NOT an SPA. Fully curl-friendly in principle.
  - dist/js/appconfig.js is 404 (dead reference); dist/js/ajax.js is the real one.

THE API — one generic endpoint, numeric action codes:
  POST https://care.aarthiscan.com/appointments/WebCall
  form-encoded: appCode=<number>&appData=<json string>
  (from populatejson(): packers = [{name:appCode,value:N},{name:appData,value:JSON}])
ACTION MAP recovered from the page's inline script:
  8006, 12001  = (unlabelled, called with {})
  50003 = city list            50008 = categories (load category from DB)
  50011 = branches by area     {'id': area_id}
  50022 = custom test          (payload var 'customtest')
  50023 = branch details       50026 = (payload var 'datatodb')
  50030 = web home + center details    50032 = recall order {'id': atob(...)}
  50039 = lead {'id': atob(...)}
Also present: 'WEB_HOME_COLLECTION', 'NammaLab_MW' (middleware), 'ordersms'.

⛔ THE WALL: every WebCall returns bare 'ERROR_IN_SESSION' without an established session, and NO Set-Cookie is issued by /appointments/, /city.html or any entry path (checked with -I). So the session is server-side and keyed to something the JS establishes that a cold curl does not — solve THAT and the entire catalogue is one WebCall away, no browser needed. Next agent: capture the phone+pincode submit from a live surface, diff the request, and write the recipe here.

HOST NOTE: dev has the yggterm headless daemon but NO live GUI client ('no live Yggterm GUI client is registered for app control'), so the web/app-control plane exists ONLY on the GUI host — the human's laptop. Materialising a surface there needs the session ACTIVATED (a --no-activate session answers no_declare to 'web ensure'), which moves the user's view. ASK FIRST.

## full-catalogue-via-city-phone-pincode · WORKS
task: read Aarthi's full test catalogue + prices, no booking
model: claude-opus-5
date: 2026-07-29
tags: 

SOLVED the ERROR_IN_SESSION wall from the previous entry. The server session is created by WALKING THE UI, not by any cookie — there is no Set-Cookie anywhere. Sequence, all drivable with 'web eval --session' (no shadow, no reveal, no OTP at any point):

1. ychrome -> https://care.aarthiscan.com/appointments/   (redirects to city.html)
2. CITY: click the leaf element whose innerText is exactly the city, e.g. /^KOLKATA$/i .
   -> navigates to homeorvisit.html and pops the identity modal.
3. IDENTITY MODAL: #phone (mobile), #userPincode, radio #male / #female, submit #saveModalBtn.
   Native-setter + input/change events work. NO OTP, NO password — phone+pincode IS the login.
4. ROUTE: two leaf choices — 'Visit Our Centers' and 'Book A Home Service (Blood & Urine tests,
   ECG, Xray, Semen analysis)'. Home collection DOES exist and explicitly covers semen analysis.
5. -> selecttest.html renders the ENTIRE priced catalogue as plain text. Scrape with
   /([A-Z0-9][^Rs]{2,90}?)\s*Rs\s?([\d,]+)/g over document.body.innerText.
   Kolkata 2026-07-29: 1,980 priced items. Also cached client-side in sessionStorage key
   'testlistall' (plus city_details, center_home_details, aval_category, type) — read that
   instead of scraping text if you want structured data.

Clicking a catalogue row does NOT create a cart or a booking; it just marks selection and shows
'Select Center'. Safe to browse. Booking is a later, separate step.

WHY THIS MATTERS: aarthiscan.com (the public marketing site) lists only routine analytes and is
MISLEADING — the CMS catalogue has ApoB Rs 440, Lp(a) Rs 780, testosterone-total Rs 560, cystatin-C
Rs 980, SHBG Rs 1980, hsCRP Rs 650, semen analysis Rs 1100, HOMA-IR profile Rs 780, and
'MRI - VISCERAL AND LIVER FAT QUANTIFICATION' Rs 3500. Never price Aarthi off the public site.

Generalises: this is an off-the-shelf Indian diagnostic CMS (jQuery + WebCall/appCode). The user
notes hospitals like EEDF run the same suite, so try this exact walk there.
