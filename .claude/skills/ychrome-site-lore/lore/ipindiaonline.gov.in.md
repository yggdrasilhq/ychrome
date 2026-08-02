# ipindiaonline.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## tls-jumbled-chain-and-esign-not-dsc · PARTIAL
task: 
model: claude-opus-5
date: 2026-07-31
tags: 

The trademark e-filing portal (Comprehensive e-Filing Services) is reachable ONLY over
curl, and only after a trust-store fix. Both defects below are the site's, not ours.

## 1. The TLS chain is jumbled and kills GnuTLS (browsers/WebKitGTK)

The server sends **7 certificates** containing **two different paths** for the same
intermediate `EM DV TLS CA - G2A-1`:

- a DEAD path issued by Comodo `AAA Certificate Services` — a retired root that Debian
  removed from `ca-certificates` — and it is listed FIRST;
- a GOOD path issued by `emSign Root TLS CA - G1` -> `emSign Root CA - G1`, which Debian
  ships and trusts.

OpenSSL and GnuTLS both match the leaf's issuer against the first candidate in the
presented order, walk into the AAA branch, and fail with a generic error. `curl` says
"failed to verify the legitimacy of the server"; WebKitGTK says `Unacceptable TLS
certificate`. Neither message hints that a perfectly valid path is sitting in the same
handshake.

**Fix for curl/OpenSSL (no new authority granted):** extract the emSign-issued copy of
`EM DV TLS CA - G2A-1` from the handshake and install it as an anchor. It already
verifies against the stock Debian store, is `CA:TRUE, pathlen:0`, and its EKU is limited
to TLS server/client auth, so trusting it adds nothing that was not already implied.

```sh
echo | openssl s_client -connect ipindiaonline.gov.in:443 \
    -servername ipindiaonline.gov.in -showcerts 2>/dev/null > chain.pem
# split, then pick the cert whose subject is "EM DV TLS CA - G2A-1"
# AND whose issuer is "emSign Root TLS CA - G1"  (NOT the AAA-issued twin)
sudo install -m 644 emudhra.pem \
    /usr/local/share/ca-certificates/emudhra-em-dv-tls-ca-g2a-1.crt
sudo update-ca-certificates
```

sha256 of the correct cert:
`3F:13:59:7D:33:55:D3:41:BD:09:32:8A:EE:56:7D:7F:4E:00:2D:99:19:B9:86:F9:96:2D:AF:F1:97:EA:75:D1`

**GnuTLS is NOT fixed by this** and therefore neither is ychrome. Measured: GnuTLS walks
the presented order and will not search for an alternate anchor, so the only thing that
satisfies it is trusting the retired `AAA Certificate Services` root itself. That is a
much broader change — do not do it casually, and prefer curl.

## 2. Drive it in curl

Login page `user/frmloginnew.aspx` is plain ASP.NET ViewState:
`TBUserName`, `TBPassword`, `txtCaptcha`, submit `LnkSubmitLogin`, plus a CAPTCHA image.
Fetch the CAPTCHA over the same cookie jar and read the image directly rather than
reaching for a browser.

## 3. The signing gate is eSign, not a DSC dongle

Every third-party guide on the web says a physical Class-3 DSC token is mandatory for
Form TM-A. **The portal's own FAQ contradicts them** (`Extras/FAQESign.aspx`): eSign is
integrated into the IPO e-filing portal and lets an applicant sign
"without having to obtain a physical digital signature dongle", authenticating by
**Aadhaar e-KYC OTP**. Sole authorised provider today is eMudhra; the portal links an
account-creation URL for it. Certificates live 30 minutes, keys are single-use.

Useful in-portal documents, all readable over curl once the trust fix is in:
`UsefullDownloads/e-usermanual.pdf`, `UsefullDownloads/DSCManual.pdf`,
`UsefullDownloads/eMudhra_eSign_User_Guide.pdf`, `Extras/RegistrationSteps.aspx`,
`user/How-To-Register.aspx`.

## tm-a-drafting-postback-wipes-and-a-dead-search · PARTIAL
task: 
model: claude-opus-5
date: 2026-08-02
tags: 

TM-A drafted end to end on the e-filing portal (v3.0). The MECHANISMS below are
reusable; the DEFECTS are recorded because they cost citizens money and time and
belong in an accountability record, not only a how-to.

## The path

Login (User ID + password + captcha) -> New Form Filing -> File TM-A ->
applicant fee category -> application type -> class -> the form at
`newtmforms/frmTM-A.aspx?appNo=<temp>&form=TM-A&entryno=<type>`.

Reopen a draft ONLY via **Update Application/Forms -> Drafted Applications ->
Edit**. ⛔ Typing the form URL RE-INITIALISES it and shows an empty form that
looks exactly like data loss.

## ⛔ Label order does not match radio order — this one files the WRONG FORM

`rdbentryno_2` is **MULTICLASS CONVENTION APPLICATION** (a foreign-priority
claim). Plain **MULTICLASS is `rdbentryno_3`**. Choosing by visual position gets
you a convention application, and the only place it shows is the fee. Map
`label[for=...]` to ids before selecting anything on this portal.

The goods/services class dropdown is INVERTED: option **value** is the class
heading text, option **text** is the class NUMBER. Select by matching
`o.text.trim() === '9'`, never by value.

## ⛔⛔ EVERY POSTBACK WIPES THE SCALAR FIELDS, AND SAVE CAN COMMIT THE WIPE

A category tick, an applicant edit, a grid Add — each re-renders and empties the
word mark, the use-claim and the grid staging. Fill in this order, then do not
navigate:

1. applicant editor -> state + district -> **Update** (this is what resolves
   jurisdiction)
2. class rows, one Add at a time
3. the scalars LAST (word mark, proposed-to-be-used)

⛔ **"Save & Resume" is NOT trustworthy.** Measured across two cycles on the same
draft: one cold reopen showed the mark and both classes intact, the next showed
an empty form, with no difference in the steps. **A save must be verified by
closing and REOPENING the draft** — and even a passing check is not proof it
will hold. Prefer completing the form in one sitting.

⚠ Jurisdiction (`Branch Name`) resolves ONLY in the render right after the
applicant **Update**, and reverts to the literal string `BranchName` afterwards.
The state/district ARE stored on the applicant (verifiable by reopening the
applicant editor); the label is what is unreliable. A blocking modal, *"Kindly
edit Applicant details and select appropriate state/district to decide
JURISDICTION"*, is the portal's way of saying the label did not resolve.

⚠ Rapid scripted postbacks earn `Error.aspx?aspxerrorpath=...`. The login
survives it, but any reading taken around that error is worthless. Pace it.

⚠ A stale `RegularExpressionValidator` span can keep showing "Invalid Email
Format" after the field is valid. Read `validationexpression` off the validator
and run `Page_ClientValidate()` before believing it — the regex here accepts a
`.top` address, and the visible error was leftover state. This nearly caused a
correct contact address to be changed on a statutory form.

## ⛔ THE AI/ML PUBLIC SEARCH DECLARATION CANNOT BE HONESTLY ANSWERED "YES"

The form asks the applicant to declare *"I have searched the applied trademark
using AI/ML based Public Search"* and links
`tmsearch.ipindia.gov.in/ords/r/tisa/trademark_search1000/dpiit-public-search`
(Oracle APEX). **That search does not execute.** Real-keys fill seats the word
and Enter preserves it, but the result stays "No data found" — and a POSITIVE
CONTROL proves the tool, not the term: **searching "TATA" also returns "No data
found"**. A term with thousands of live marks returning nothing means no query
ran.

⇒ Any "No data found" from this tool is meaningless, and must never be reported
as a clean register. Answer the declaration **No**, or have a human run the
search. **Always run a positive control before trusting a null result from a
search you did not write.**

## Verification section

`chkverify` ("I hereby verify that above mentioned facts are true...") arrives
PRE-TICKED, with the applicant name and "Proprietor" pre-filled. Read it rather
than assuming it is inert: it is the statutory verification.
