# broker-b.example

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## backoffice-plain-login-stale-credential · BLOCKED
task: 
model: claude-opus-5
date: 2026-08-20
tags: 

**Reached: the form drives perfectly, the STORED CREDENTIAL IS REJECTED.** A plain
username/password back office — no OTP, no PIN, no captcha — so the automation side is trivial and
was never the problem.

```sh
p=$(ychrome ctl open url=https://<backoffice-host>/ profile=agent-fin-broker | jq -r .page_id)
# → /Account/Login ; inputs #UserName #Password ; submit #submit-btn (an <input type=submit>)
ychrome ctl fill page_id=$p entry='<vault item>' user='<the client code>'
ychrome ctl eval page_id=$p js='JSON.stringify({user:document.querySelector("#UserName").value,
                                                pwlen:document.querySelector("#Password").value.length})'
ychrome ctl input page_id=$p events='[{"type":"click","selector":"#submit-btn"}]'
```

⚠ `#submit-btn` is an `input[type=submit]`, so it does NOT appear in a
`document.querySelectorAll("button")` enumeration — a button sweep finds one unrelated element and
an agent concludes the form has no submit. Enumerate `button, input[type=submit]`.

## ⭐⭐ THE VAULT REFUSED AN AMBIGUOUS MATCH, AND THAT IS THE SAFEGUARD A BANK LANE ASKED FOR

Two accounts are stored under this host — the owner's and a family member's. `ctl fill` did not
pick one:

```
{"error":"vault: \"<host>\" matches 2 accounts — name one: <code-A>, <code-B>","ok":false}
```

⇒ **It refuses rather than guessing, and it NAMES both candidates.** Another lane's lore records the
opposite hazard (a strict host match silently resolving a banking identity to a different person).
**Pass `user=<client code>` explicitly on any shared or family vault, and read the username field
back page-side before submitting** — the username is not a secret, so reading it costs nothing and
tells you which identity you are about to spend an attempt on.

## ⛔ THE STORED PASSWORD IS STALE — AND STOP AFTER ONE ATTEMPT

The host-matched vault item holds a 4-character secret (below any plausible minimum). The
back-office-named item holds a 12-character one; that is the informed choice, and the portal
answered **"Invalid User Name or Password"**.

Five vault entries exist across this broker's hosts and **all five secrets are distinct**
(compared by fingerprint, never printed). ⇒ Further attempts are a lottery.

⛔ **DO NOT SPRAY THE REMAINING CANDIDATES.** A broking back office locks, and a lockout costs the
owner real access for a report that is not urgent. **One informed attempt, then stop and escalate.**
⚠ Note the other hosts are DIFFERENT SYSTEMS, not aliases — a trading login and a back-office login
do not share a credential just because they share a brand.

⇒ The fix is a vault refresh, not code. Everything mechanical here already works.

## Boundary

Reads only. "Forgot Password?" is a credential RESET and a WRITE; a read grant does not cover it.
