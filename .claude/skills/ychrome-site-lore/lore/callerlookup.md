# callerlookup

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## phone-otp-login · PARTIAL
task: reverse phone lookup for orbitstore
model: claude-opus-4-8
date: 2026-07-24
tags: 

CallerLookup reverse lookup requires LOGIN — unauth /search/in/<10digit> shows "Sign in to unlock
caller name", no name to anonymous.

LOGIN = phone-number OTP at /auth/sign-in:
- 3 tel inputs exist; only the one with offsetParent!==null is real. Country <select> defaults India (+91).
- The submit button stays disabled until React sees the value. Plain value-set does NOT enable it;
  use the _valueTracker reset trick:
    set.call(inp,""); inp._valueTracker.setValue("x"); set.call(inp,"<10digit>");
    inp.dispatchEvent(new InputEvent("input",{bubbles:true,data:...,inputType:"insertText"}));
  Then the "Sign in" submit button enables; click it → OTP sent; page shows a maxLength=6 text input.
- ⚠ OTP CHANNEL: CallerLookup sends "an OTP notification" to the CallerLookup APP on the registered
  phone, NOT an SMS. It did NOT surface via KDE Connect notifications on the paired phone (CallerLookup
  app appears to auto-consume / not mirror it). So the invisible KDE-Connect-OTP path FAILS here —
  needs operator to read the code from the CallerLookup app (co-pilot), OR a phone with mirror-able
  CallerLookup notifications. Open problem.

Drive fully headless via `web ensure --session <path>` + `web eval` — NO app open (invisible).
