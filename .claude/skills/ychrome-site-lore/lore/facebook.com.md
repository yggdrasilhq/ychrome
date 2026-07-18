# facebook.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## login-vault · BLOCKED
task: log in with saved creds
model: claude-fable-5
date: 2026-07-18
tags: login, vault

Blocked: vault locked on the live host; user must run: read -rs PW; echo "$PW" | ychrome-vault unlock. Then #email + #pass via app web eval + input event.

## login-form-fill · PARTIAL
task: fill the Facebook login form via co-browse (creds pending vault unlock)
model: claude-fable-5
date: 2026-07-18
tags: login, react, fill, meta

Facebook /login (2026-07 layout). Verified live on the desktop host, meta profile.

Selectors (by NAME, not id — the old #email/#pass are gone):
- email/mobile: input[name=email]  (type text)
- password:     input[name=pass]   (type password)
- submit:       input[type=submit] / [data-testid=royal_login_button] / the 'Log in' button

FILL that commits to React state (a bare .value= does NOT; the button stays
disabled and the value is wiped on re-render). Use the prototype value setter +
an input event, per field:
  var set=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;
  var e=document.querySelector('input[name=email]');
  set.call(e,'<user>'); e.dispatchEvent(new Event('input',{bubbles:true}));
  e.dispatchEvent(new Event('change',{bubbles:true}));
Proven: set+input readback held the value across a tick (React kept it). Drive it
with: yggterm server app web eval '(function(){ ... })()' (web eval evaluates RAW,
so wrap in an IIFE — unlike app dom-eval which wants a bare return).

Credentials: ychrome-vault get facebook.com --field username|password once the
user unlocks the vault. Inject the password the same way; never a literal.

BLOCKED tail: (1) vault is the user's to unlock (stdin master password). (2) Meta
challenges automated logins — expect a checkpoint / 'save browser?' interstitial /
2FA. 2FA OTP could be auto-pulled from the SMS data-fabric store (see the dream
doc) rather than asking the user. Do NOT submit until the operator is lined up for
a possible checkpoint.
