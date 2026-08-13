# cybercrime.gov.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## ncrp-complaint-headless-end-to-end · WORKS
task: File an NCRP Other Cyber Crime complaint end to end, headless, with file uploads
model: claude-opus-5
date: 2026-08-14
tags: 

Driven headless through the `ychrome ctl` engine end to end: login, a full four-step
complaint wizard, two file uploads, and a verified acknowledgement number. No GUI host, no
operator screen touched.

## ⛔⛔ THE ONE THAT COSTS AN HOUR: a DataTransfer file is silently DROPPED by the UpdatePanel

Every upload on this portal sits inside an ASP.NET AJAX `UpdatePanel`. Assigning a file with
`DataTransfer` WORKS at the DOM level (`input.files` really does hold a File in this WebKit
build), and clicking the ADD button reports nothing wrong — but a partial postback **does not
transmit files**, so the server receives none. The grid stays empty, no validator fires, and the
wizard just refuses to advance with no visible reason.

**The fix — force a genuine full-page multipart POST, injecting the file in the SAME eval:**

```js
// ... build `u` (Uint8Array) from atob(base64) ...
var dt=new DataTransfer();
dt.items.add(new File([u],'exhibit.pdf',{type:'application/pdf'}));
document.getElementById('ContentPlaceHolder1_fu_info').files=dt.files;
document.getElementById('__EVENTTARGET').value='ctl00$ContentPlaceHolder1$btnAdd';
document.getElementById('__EVENTARGUMENT').value='';
HTMLFormElement.prototype.submit.call(document.forms[0]);   // NOT form.submit()
```

`HTMLFormElement.prototype.submit.call(form)` is the load-bearing part: `PageRequestManager`
overrides `__doPostBack` and the form's submit handler, but not the prototype method. The form is
already `enctype="multipart/form-data"`.

⚠ **The file MUST be injected in the same eval as the submit.** Any intervening postback empties
the input. A run that injected, then checked state, then submitted read `files_at_submit: 0` and
uploaded nothing while looking fine.

✅ Proof it worked: the server answers with its own renamed file in the grid —
`Evidence20260814...pdf`, `NationalId20260814...jpg`. Read that, never the click's return.

⛔ **Base64 in `js=` has a ceiling.** ~46 KB of file (≈62 KB base64) passes; a 159 KB PNG dies with
`/usr/bin/timeout: Argument list too long` before the browser is even reached. Re-encode large
images down (a 843x275 ID card stayed fully legible as JPEG quality 68, 46 KB).

## ⛔ Do not force the full submit for NAVIGATION

The same `HTMLFormElement.prototype.submit` trick applied to the wizard's Next link returned
`error.aspx?aspxerrorpath=...` ("Oops!! Something went wrong"). Use it ONLY for the buttons that
consume a file; navigate with an ordinary `.click()`.

## The wizard's own traps

- **Each step has its OWN next button id.** `ContentPlaceHolder1_lnknext` on Incident Details,
  `ContentPlaceHolder1_btntab3next` on Suspect Details. Clicking a stale id silently does nothing
  and reads as a validation failure.
- ⛔ **"Save as Draft" saves the INCIDENT step only.** After a server error the draft was
  recoverable from Draft Complaint → Modify Record with the narrative and evidence intact, but all
  13 suspects were gone and had to be re-entered. Budget for that; do not assume a draft holds the
  later steps.
- ⚠ **The resumed draft loses the incident DATE** while keeping everything else. Re-set it.
- **Suspect entry is a three-postback dance and the order is not guessable:** set `ddl_Id` to the
  ID type → wait → click the matching RADIO (`rbtnlistMobileType_1` = Landline No) → wait →
  confirm `txt_IdNo`'s placeholder actually reads "Enter landline No" → only then fill and ADD.
  After every ADD the section rebuilds into a dropdown mode where the field is a generic "ID
  Number" and adding silently does nothing. **Guard on the placeholder**, or the loop reports
  success while registering nothing.
- **Useful ID types for phone abuse:** `Landline Call`, `Mobile Number`, `International Call`,
  `WhatsApp Call`.
- **The preview renders the narrative inside a `TEXTAREA`** (`lblinfoincidnt`), so `innerText`
  scraping shows it as EMPTY. Read `.value` before concluding the complaint lost its body.
- Login is mobile + SMS OTP + captcha; the OTP arrives from the portal's own DLT sender, so filter the SMS store by sender family rather than by body text. Session is 60 minutes and refreshes
  on navigation.
- ⛔ **Verify the filing from Check Status, never from the submit.** The submit leaves you on the
  preview with no acknowledgement shown; the acknowledgement number only appears under Check
  Status.

## The free instruments worth knowing

- **Suspect Repository** (`suspect_search_repository.aspx`) — public, captcha-gated, answers
  whether a number/email/account has been reported by other citizens. ⚠ **It prints the heading
  "Found" in BOTH outcomes**; the verdict is `has been reported` vs `There are no records found`.
  Keying on "Found" marks every clean number a suspect.
- **Report Suspect to I4C** (`cyber_suspect.aspx`) — no login, takes phone numbers and an evidence
  file. ⛔ **It issues NO acknowledgement, NO reference and NO error; the form just resets.** A
  submission through it cannot be proven to have landed, so treat it as feeding a database, never
  as a filing.
