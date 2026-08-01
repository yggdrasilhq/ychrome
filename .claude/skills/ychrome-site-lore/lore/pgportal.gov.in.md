# pgportal.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## export-full-cpgrams-record-curl-login · WORKS
task: Log in to CPGRAMS and export every grievance, appeal, status transition and closure text for a atlasStore case, with faithful PDFs
model: claude-opus-5[1m]
date: 2026-07-26
tags: government, cpgrams, grievance, login, captcha, sha512, curl, pdf, print-view, existence-probe

**The whole logged-in CPGRAMS record is exportable with plain `curl` — no browser, no OTP,
no ychrome surface. Login succeeded on the FIRST attempt, 2026-07-26; 46 grievances,
11 appeals and 61 faithful PDFs pulled in ~20 minutes.**

### Login (5 facts, all load-bearing)

1. `GET https://pgportal.gov.in/Signin` with a cookie jar and a full Chrome header set
   (`sec-ch-ua`, `sec-ch-ua-mobile`, `sec-ch-ua-platform`, `Sec-Fetch-*`, `--http1.1`).
   Sets `__RequestVerificationToken` and `CPGRAMS` cookies.
2. Scrape TWO per-render values out of that HTML: the hidden **form** field
   `__RequestVerificationToken` (a DIFFERENT value from the cookie of the same name), and
   `var randomString = "..."`. **`randomString` is a per-session salt and changes on every
   render**, so it must come from the same jar you POST with.
3. `Password = sha512( sha256(plaintext) + randomString )`, lowercase hex — the page bundles
   js-sha256 / js-sha512 at `/bundles/encription`. Post `TempPassword` as `'*' * len(password)`;
   that is exactly what the page's own `#btnSubmit` handler does before submitting.
4. Captcha `GET /Captcha/GetCaptcha` — fetch it **LAST**, immediately before the POST (each fetch
   regenerates the code in the PHP/ASP session). 150x30 JPEG, 6 alphanumeric chars, serif
   typewriter face, light dotted noise. **Read it by eye off an ~8x LANCZOS upscale** — a plain
   full-image upscale was enough for 5/5 captchas here. `tesseract` is unreliable on this face.
   An audio fallback exists at `/Captcha/GenerateAudioCaptcha` if an ASR path is ever wired.
5. `POST /Signin` (form-urlencoded) with `__RequestVerificationToken`, `Username`
   (mobile / e-mail / username all accepted), `TempPassword`, `Captcha`, `Password`
   → **302 `/Desk`** plus a `CitizenAuthenticationToken` cookie. Session ~30 min;
   `/Signin/ExtendSession` exists. Credentials: vault item `pgportal.gov.in`
   (`ychrome-vault match pgportal.gov.in` resolves the live one).

### Endpoints — where each fact actually lives

| Need | Endpoint |
|---|---|
| complete grievance list + per-row hashes | `/Desk` (also `/Desk/Index/{All,Pending,Closed}`) |
| appeals list | `/Appeal`, `/Appeal/Index/*` |
| status, closure remark, rating, appeal block, officer block, reminders | `/Status/Detail/<64-hex>` |
| **the movement trail (routing evidence!)** | `/NewGrievance/Details/<64-hex>` — "Communication Details" table with Date / Action / From / To. **It is NOT on the status page.** |
| clean printable page — render THIS to PDF | `/Status/PrintDetail/<64-hex>` — ⚠ its hash DIFFERS from the status-page hash; scrape the link off the status page |
| appeal detail | `/Appeal/Detail/<64-hex>` |
| account identity (confirm which login you are on) | `/EditProfile` — shows Username, Name, mobile, e-mail unmasked |
| login history | `/AuditTrail` — **empty in practice**, do not rely on it |

The status page masks mobile/e-mail (`NN xxxxxxx NN`, `fixxxx…@example.com`); `/EditProfile` does not.

### Public tracker as an EXISTENCE PROBE (no login needed)

`POST /Status` with `RegistrationNo`, `EmailOrMobileno`, `Captcha` and optionally
`GrievancePassword` hashed as `sha512(md5(pw) + window.randomString).upper()` (note: **md5** here,
**sha256** on the login page — different recipes on the two forms).
**302 → `/Status/Detail` = the id+contact pair exists. 302 → `/Status` = it does not.**
The on-page error is generic ("Please provide correct Registration Number, Grievance
Password/Email Id/Mobile Number"), so **always run a positive control** with a known-good pair
before reading a failure as "this grievance does not exist". The `RegistrationNo` regex accepts
only 5–7 trailing digits and years `20xx`/`21xx`.

### Faithful PDF of a grievance page

Stage the `/Status/PrintDetail` HTML locally, rewrite the two versioned stylesheet hrefs
(`/Content/styleLayout?v=…` → a local copy, `/Content/Custom/print.css`), serve on a loopback
port, then
`~/.cache/ms-playwright/chromium-1223/chrome-linux64/chrome --headless=new --no-sandbox
--no-pdf-header-footer --print-to-pdf=out.pdf http://127.0.0.1:<port>/<page>.html`.
Verify one output with `pdftoppm -png` and actually look at it. Full site pages (Desk, Appeal
detail) also render this way once `<script>` tags are stripped — images 404 but all text lands.

### Gotchas

- `pgportal.gov.in/Home/Preview/<base64-of-filename>` serves the DARPG Office Memoranda and is
  NOT bot-walled — the most reliable route to the governing instruments (`darpg.gov.in` is
  WAF-blocked; use `static.pib.gov.in` mirrors for the monthly reports).
- Grievance/appeal detail hashes are **per-session**; do not cache them across logins.
- Write paths deliberately untouched here: `/Reminder`, `/Rating` (`/status/Index/<hash>`),
  `/NewGrievance`, `/ChangePassword`, `/AccountDeletion`. Reads are free; anything that lodges,
  reminds, rates or appeals is a state change on a government portal and needs the operator.
- `ychrome-vault get "<name>" <user>` CANNOT disambiguate two vault items sharing a username
  ("matches 2 accounts — name one: <user>, <user>") and `get` does not accept an item id.
  `ychrome-vault match <host>` resolves one item and was the way out. Worth teaching `get` to
  take an id.
