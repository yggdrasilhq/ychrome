# e-jagriti.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## consumer-complaint-filing · PARTIAL
task: 
model: claude-opus-4-8
date: 2026-07-25
tags: 

## edaakhil.nic.in is DEFUNCT -> use e-jagriti.gov.in
edaakhil.nic.in returns NXDOMAIN (authoritative nic.in). The Daakhil/OCMS/CMS/Confonet
apps were SUBSUMED into the e-Jagriti platform (https://e-jagriti.gov.in), live since
2025-01-01. This is the portal for online consumer-complaint filing (District/State/National
commissions), Dept of Consumer Affairs, GoI. Court fee NIL for total claim up to Rs5 lakh.
Note: NIC portals are geo-fenced to India IPs (the GUI host/pi egress Kolkata = fine).

## Stack / driving notes
- Heavy React SPA: single ~20MB main.<hash>.js bundle; slow network (~190KB/s) => 60-90s to
  bootstrap. Deep-linking to /register renders EMPTY; reach it via in-app: load /login, wait
  for form, click the REGISTER anchor (React Router). Keep the bundle in memory (avoid full
  reloads); a full reload re-fetches 20MB AND drops the in-memory auth (no bearer token in
  localStorage; only i18nextLng + persist:root), effectively logging you out.
- Fill controlled inputs with eval native-setter + input/change/blur events (MUI + RHF
  register it; the MUI label shrinks = confirmation). `web do` real-input FAILS on an
  invisible/unmapped surface (reason surface_not_mapped) -> eval only.
- Surface suspends during long idle (>~60s) -> keep it touched with periodic evals or it
  needs a reveal to revive (which can reset it to about:blank).

## Registration (WORKS)
/login -> REGISTER -> fields: #mobilenumber, #emailId, #username(full name), #password,
#confirmpassword, #roleId (MUI Select: Consumer=1/Advocate=2/Company=19), #captcha. CONTINUE
-> OTP screen (6 .input-otp boxes + a NEW #captcha) -> SUBMIT -> "User registered
successfully" -> /complete-profile.

## OTP (critical)
- A 6-digit OTP is sent to BOTH the mobile (SMS) and the email on the FIRST CONTINUE.
- SMS: sender "BH-EJGRTI-G", body "Your one-time password is NNNNNN on e-Jagriti Portal.
  This OTP will be only valid for 15 minutes." Capture via KDE Connect on the phone.
- Email: only the FIRST CONTINUE emails (sender helpdesk-ejagriti@nic.in). RESEND does NOT
  email (SMS-only) -> for a fresh email OTP you must restart registration. So prefer the SMS
  channel. (Betterbird headless/tb --sync is flaky here; the phone SMS path is reliable.)
- Captcha: short TTL (~2-3 min) and single-use. Read it (screenshot+crop+4x zoom of the
  captcha box; glyphs are stylized), fill via eval, and SUBMIT within seconds. A stale/reused
  captcha => "Either Captcha is invalid or It is expired kindly re-generate."

## BLOCKER for full filing via eval-only
Profile completion (required before filing) Address step has MUI async Autocompletes:
#PinCode auto-fills #State + #PostOffice (PIN lookup API works), but #AddressType and
#District load options from a slow master-data API and show "No options" / empty options
prop in the React fiber -> could not populate/select them via synthetic events on the
invisible surface. The complaint wizard has many more such Autocompletes (commission/state/
district/OP-address) + 10+ document uploads + a final OTP+captcha submit. Full irreversible
filing is not reliably completable via eval-only; would need a mapped surface for real input.

## Creds
e-jagriti account for this owner exists (registered Consumer). Reference the vault by domain
e-jagriti.gov.in. (Vault WRITE returned server 401 during this run; account creds were saved
to a 600 file for the owner to vault.)

## consumer-complaint-login-blocked · BLOCKED
task: 
model: claude-opus-5
date: 2026-07-25
tags: 

Invisible-co-browse TEST run (2026-07-25 ~11:00-11:35 IST) driving the wmart consumer
complaint. Login flow fully mapped; filing BLOCKED AT LOGIN, not at the wizard.

## Login is OTP-only-capable — no password needed to reach the account
/login shows three tabs: "Login With Password" | "Login With OTP" | "Forgot Password".
Clicking the OTP tab via eval `.click()` WORKS (subtitle flips to "...and otp").
Fill #username (mobile) with native-setter + input/change/blur -> CONTINUE enables ->
"OTP generated successfully" -> 6x input.input-otp + a NEW #captcha + SUBMIT, with an
on-page "TIME REMAINING 3 MIN" resend timer (the SMS itself says 15 min validity).
API sequence (visible in performance.getEntriesByType("resource")):
checkUserBlock -> verifyUserAndGetSecretNumber -> generateCaptcha -> otp_generated_for_login
-> (submit) login.

## ⛔ THE ACCOUNT IS PASSWORD-EXPIRED AND ALREADY-SESSIONED
POST services/user/auth/v2/login answered 200 with:
{"message":"User is already logged-in in other device.","status":200,
 "data":{"securedUser":false,"alreadyActiveUser":true,"userId":<REDACTED>,
 "passwordExpired":true,"passwordExpiryDate":"2026-07-25T11:24:09"},"error":"false"}
and the SPA opened a CHANGE YOUR PASSWORD modal requiring mobile/email + CURRENT password +
new + confirm + captcha. CORRECTION 2026-07-25 (orchestrator): the password IS
persisted -- vault item `e-jagriti.gov.in` (yggterm ROUND 16's vault-write fix filed the
plaintext fallback INTO the vault and shredded the file; a missing fallback after a
vault-repair round means CHECK THE VAULT, not "lost"). Forgot Password NOT needed: feed the
CHANGE-PASSWORD modal the current password from the vault. The change itself is an account
write -> operator confirm / attended session. VAULT any new password and read the write back.
(Superseded conclusion, kept for history: "never persisted; 600-file fallback absent =>
Forgot Password required" -- that was written before the vault item was found.)

## ⚠ FAILURES ARE SILENT — tap the XHR
A failed login shows NO error toast. It just re-calls verifyUserAndGetSecretNumber +
generateCaptcha (a fresh captcha). The only honest signal is the login response body, so
install an XHR/fetch tap (override XMLHttpRequest.prototype.open/send, push
{url,status,responseText} onto window.__log) BEFORE clicking SUBMIT. Without it you cannot
tell "wrong captcha" from "already logged in" from "expired password".

## ⛔⛔ THE 6-BOX OTP COMPONENT CANNOT BE DRIVEN BY eval
input.input-otp is a segmented auto-advancing component. Per-box native-setter + input/change
sets both the DOM value and the React fiber prop (both read back CORRECT), yet the component's
internal state keeps stale digits: writing 292244 over a previous 278347 produced 278344 (a
merge). A ClipboardEvent('paste') carrying the whole code (the fiber does expose onPaste)
changed nothing. => This widget needs REAL trusted input. Same class as the MUI async
Autocompletes already logged here.

## ⛔⛔⛔ yggterm blocker: `web do` is SINGLE-SHOT per surface
Reproduced twice on virgin surfaces: do click #1 -> accepted:true, delivered:true; do click
#2 and every later do verb -> accepted:false, reason:"preempted", detail:"the user took this
surface". The agent's own injected event is counted as human seat input, cancelling its own
batch, with no agent-reachable reset (--agent <newid> does not help). So today the ONLY way to
drive this portal invisibly is eval — which cannot do the OTP boxes or the Autocompletes.
Recorded in the yggterm campaign memory as the next engineering target.

## Captcha reading (works)
`web screenshot --session <s>` (faithful, webkit_full_document_snapshot), then crop the img
whose alt contains "characters" at its getBoundingClientRect (was 200x50 at 95,374 on a
1280x800 surface) and `convert -resize 400%`. Glyphs are stylized mixed-case+digits
(e.g. 56gZuQ) but legible at 4x. NOTE: PIL is NOT installed on the host; use ImageMagick
`convert`. Re-screenshot per attempt — the captcha regenerates on every failure.

## Surface lifecycle notes for this portal
- terminal new --no-activate + web ensure --session is fully invisible; no reveal needed
  (rebuilt_from_daemon_declare:true). The user's active_session_path never moved.
- A GUI hot-restart DESTROYS the surface and its web_surfaces entry ("session has no web
  surface") and, because the auth bearer is JS-memory-only, logs you out. Do not run this
  flow while anything is deploying; re-`ensure` after any swap.
- The shadow view client is NOT usable here: ensure+eval work but `web do` ->
  surface_not_mapped, and `app open --client` is refused by the viewport/PTY-grid contract.
- The reaper does NOT collect a never-revealed headless surface at all (idle 45s with an
  expired lease survived), so surface loss here is GUI-restart, not reaping.

## consumer-complaint-login-otp-window · PARTIAL
task: 
model: claude-opus-5
date: 2026-07-25
tags: 

Second invisible-co-browse run (2026-07-25 ~18:35-19:05 IST), same wmart consumer
complaint. **Login still not completed, but for entirely different reasons than run 1:
the two yggterm blockers run 1 recorded are FIXED, and what stopped this run was the
portal's own OTP/captcha economics plus a concurrent yggterm deploy.**

## ✅ SUPERSEDED — `web do` is no longer single-shot, and `do fill` drives the OTP boxes
Both run-1 blockers are gone on yggterm 2.12.12 (fixes `417910e` + `063c603`):
- THREE consecutive `do click --selector` verbs on one surface: all
  `accepted:true, delivered:true, is_trusted:true`, and the page's own counter read
  `n=3` with all three `isTrusted`. Not telemetry — page-side truth.
- `do fill --selector '#x' --text ...` REPLACES (proved: "OLDVALUE" → "NEWVAL-123456").
- `do fill --selector-set` DID drive the 6-box `input.input-otp` correctly **when the
  boxes were empty** (`649243` read back exactly).
⇒ The old note "eval is the only usable driver, and eval cannot do the OTP boxes" is
FALSIFIED. Delete that plan; drive this portal with `do`.

## ⚠ BUT: `--selector-set` is NOT reliable over a set that ALREADY holds digits
Re-filling the six boxes that held a previous code landed only **3 of 6** digits
("Please enter 6 digit number"). Reliable recipe: **one `do fill` per box**, six verbs
(multi-verb works now), each `--selector '#agtOtpN' --text '<one digit>'`. Read back
`.map(e=>e.value).join("").length === 6` before submitting. (Reported to the yggterm
campaign as a real `--selector-set` clear-order bug.)

## ⛔ THE REAL PORTAL CEILING: the OTP dies at the ON-PAGE 3-minute timer, not the SMS's 15
The SMS says "valid for 15 minutes". It is not. A submit ~4 min after issue was answered
`400 {"message":"OTP not found, please try again to generate."}` — i.e. the server
invalidates at the on-page "TIME REMAINING: 3 MIN" mark. **Budget 3 minutes from the SMS
landing to the SUBMIT click**, and note that the ~30s captcha-OCR round trip eats a fifth
of it.

## ⛔ The captcha is SINGLE-USE and is NOT auto-regenerated after a failure
Run 1's note "a failed login re-calls generateCaptcha (a fresh captcha)" is WRONG for a
failure whose cause is the OTP: after `OTP not found`, the captcha image was
byte-identical (same md5) and reusing it earned
`400 {"message":"Either Captcha is invalid or It is expired kindly re-generate."}`.
There IS an explicit refresh: `button.MuiIconButton-root` whose innerHTML contains
`data-icon="arrows-ro…"` (the second icon button is the audio/`volume-hi` one). Click it,
then re-grab. **Always burn one refresh and OCR the SECOND captcha** so a stale one cannot
silently eat the OTP window.

## ★ Read the captcha WITHOUT a screenshot — it is a blob you can fetch
Much better than run 1's screenshot-crop-upscale: `generateCaptcha` returns
`{secretKey, base64Image}` in the XHR, and the `<img alt*="characters">` `src` is a
`blob:` URL. Two-step eval (eval cannot return a Promise — "js: Unsupported result type"):
```js
// 1. kick off, stash on window
fetch(img.src).then(r=>r.blob()).then(b=>{const fr=new FileReader();
  fr.onload=()=>{window.__cap=fr.result}; fr.readAsDataURL(b)});
// 2. second eval: window.__cap  → data:image/png;base64,…
```
then `base64 -d` locally + `convert -resize 500% -sharpen 0x1`. 200x50 PNG, ~5KB, glyphs
are stylized mixed-case+digits (seen: `4UDtsg`, `rEMG89`) and legible at 5x.

## ⚠ RESEND OTP does nothing while the timer is running
The RESEND button accepts a `do click` (`delivered:true`) but fires **no**
`otp_generated_for_login` until the on-page timer reaches 0:00 — and by then the code is
already dead. So there is no "retry cheaply" path: a missed window costs a **full restart
of the login** (reload `/login` → OTP tab → mobile → CONTINUE).

## ⚠ SUBMIT can accept a click and fire nothing
One submit reported `delivered:true` with an EMPTY XHR tap — no `login` call at all. React
re-renders drop agent-injected `id`s, so **re-tag immediately before clicking** and verify
by the presence of a `login` entry in the tap, never by the verb's own `delivered`.

## Login flow, confirmed sequence (unchanged, still accurate)
`/login` → `do click` the "Login with OTP" MUI ToggleButton → `do fill #username` (mobile)
→ CONTINUE enables → `checkUserBlock` → `verifyUserAndGetSecretNumber` →
`generateCaptcha` → `otp_generated_for_login` → OTP screen (6 `input.input-otp` + a NEW
`#captcha` + SUBMIT) → `login`. SMS sender was **BT-EJGRTI-G** this run (run 1 saw
BH-EJGRTI-G) — match on the body text "e-Jagriti", not the sender id.

## Credentials — the vault is READ-ONLY right now (do not plan a password change)
The account password IS in vault item `e-jagriti.gov.in` but only **dev's** agent has that
cipher; the GUI host's cached set (1112) does not include it and cannot pull it. **Every host 401s
on any vault WRITE or `sync`** — an unauthenticated `GET /api/sync` returns the identical
HTML 401, so the agents' access token is simply expired and no refresh path exists without
the master password. ⇒ the change-password modal must NOT be satisfied by an agent today:
a new password could not be recorded. This needs the owner to `ychrome-vault unlock`.

## ⛔ A CONCURRENT yggterm DEPLOY WILL EAT THIS FLOW — check before you start
Two surfaces died mid-run. The trace named the cause: repeated
`hot_update_handoff_prepared expected_version:"2.12.13"` + `daemon_self_retire_handoff_ok`
— the yggterm agent was shipping while I drove. Symptoms, in order of appearance:
1. **Wedged webview**: every `eval` → `js: Unsupported result type` (even
   `"x"+location.href`) and `web screenshot` → "There was an error creating the snapshot",
   while `web do` still answered with the OLD `generation`.
2. **`web ensure` CANNOT heal it**: it only rebuilds when the desired-state tab list is
   EMPTY, and a dead-webview entry is non-empty ⇒ `tabs:1,
   rebuilt_from_daemon_declare:false` and the same dead handle. Restarting ychrome in the
   PTY does not help either — the generation never advances.
3. Recovery today = `session remove` the whole work session and build a new lane.
4. Later the successor surface reported `web surface not live (session backgrounded or not
   yet revealed)`, and the session had been resized 120x36 → 168x63 (the new GUI adopted
   it).
**Before any run: `server status` for the daemon version and `app clients` for the GUI pid,
and re-check both after every milestone. Abort rather than race a deploy.**

## ⚠ `file://` cannot be materialized headless
A `file:///tmp/x.html` fixture gets `ychrome: web surface open` and a daemon-ingested
heartbeat, but `web ensure` refuses it: the declare-rebuild path only accepts `http://` /
`https://` (`web_surface_url_scheme_allowed`). Serve the fixture over
`python3 -m http.server --bind 127.0.0.1` instead — an http URL rebuilds fine.

## Surface lifecycle (additions)
- `terminal new --no-activate` + `web ensure` is still fully invisible and needed **no
  reveal**: `rebuilt_from_daemon_declare:true`. The owner's `active_session_path` never
  moved to any surface of mine across the whole run.
- The first `web ensure` right after launching ychrome can legitimately race the declare —
  poll `ensure` every ~6s until `tabs>0` rather than sleeping once.
- `kdeconnect-sms.py` puts `--device` / `--json` **before** the subcommand
  (`kdeconnect-sms.py --device a386d568 --json watch --match … -t 120`). The data-fabric
  skill's example has them after `watch`, which exits 2.
