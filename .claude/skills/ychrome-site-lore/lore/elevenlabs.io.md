# elevenlabs.io

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## create-api-key · WORKING
task: provision a scoped Speech-to-Text key for a transcription pipeline
model: claude-opus-5
date: 2026-08-04
tags: api-key, react, hit-testing

⛔⛔ **"Speech to Text" DEFAULTS TO `No Access` IN THE CREATE-KEY MODAL.** A key created by
filling only the name and submitting is **silently useless for STT** — the call later fails on
permissions, far from the cause. Set the endpoint toggle BEFORE submitting. The modal has one
row per endpoint (Text to Speech, Speech to Speech, Speech to Text, …), each a two-button
`No Access | Access` group with `data-state` telling you which is live. Least privilege is also
just correct here: grant the one endpoint, leave the rest.

**Finding the endpoint row:** the rows have no stable selectors. Walk text nodes for the exact
label, then climb parents until one holds >=2 buttons:
```js
var w=document.createTreeWalker(dlg,NodeFilter.SHOW_TEXT),n,t;
while(n=w.nextNode()){ if(/^\s*Speech to Text\s*$/.test(n.nodeValue||'')){t=n;break;} }
var p=t.parentElement; while(p && p.querySelectorAll('button').length<2) p=p.parentElement;
```

⚠ **`web do click --selector button --target-text 'Create Key'` REFUSES with `no_hittable_match`**
even though the button is on-screen with a real rect: `document.elementFromPoint()` at its centre
returns `HTML`. Same class as the e-Jagriti nav. **And a plain JS `.click()` does NOT submit it
either** (it is `type=submit` with no `form`; React ignores the untrusted click).
✅ **What works: measure the rect, then `web do click --x <cx> --y <cy>`** — real trusted events
at coordinates. Measure and click in quick succession; the modal scrolls and stale coordinates
land elsewhere.
- The name field takes `web do fill --mechanism real-keys` normally.
- Pressing Enter in the name field does NOT submit.

★ **THE KEY IS SHOWN EXACTLY ONCE, in an `<input>`, not in body text** — a `document.body.innerText`
regex will not find it. Read `input.value` matching `/^sk_/`. **Capture it before dismissing
anything.** Keys are `sk_` + ~48 chars.
⛔ **Never let the key transit an agent transcript.** Write the eval result to a file ON the
browser host, pipe that file straight into `ychrome-vault add`, echo only a length, then `shred`
the scratch.


## google-signup + onboarding gauntlet · WORKING
task: create a second ElevenLabs account (own free credit pool) via Google login
model: claude-opus-5
date: 2026-08-04
tags: oauth, onboarding, signup

**Google signup works on a profile that already holds the Google session** — the account chooser
lists it, click the address, then `Continue` on the consent screen. No password, no OTP.

⚠⚠ **THE OAUTH REDIRECT KILLS THE SURFACE, AND THAT LOOKS LIKE FAILURE WHEN IT IS SUCCESS.**
After `Continue`, `web eval` starts answering `web surface not live` and `web ensure` sits at
`reload_pending` without recovering. **The account was created anyway** — OAuth completed
server-side. ⇒ **Do not retry the signup.** Relaunch `ychrome --profile <p>
https://elevenlabs.io/app/home` and read the page; you will be logged in.
⚠ The relaunch opens a **tab in the ORIGINAL session** (`opened … as a new tab in the running
[<profile>] session`), so `web eval` the FIRST session's path, not the new terminal's.

**A NEW ACCOUNT LANDS IN AN ONBOARDING GAUNTLET, not the app.** `/app/api/api-keys` silently
redirects to `/app/onboarding` until it is cleared. Four steps:
1. **Choose your platform** — only *ElevenCreative* and *ElevenAgents* are offered; **there is no
   ElevenAPI card**. Pick ElevenCreative (Speech to Text is listed under it) → `Continue`.
   API keys are reachable afterwards regardless.
2. **"Help us personalize"** — carries an **age-of-18 legal attestation**. ⚠ Two checkbox-ish nodes
   match: a real `<input type=checkbox>` **and** a `[role=checkbox]`. **The `role=checkbox` is the
   load-bearing one** — ticking only the input leaves `aria-checked="false"` and `Next` silently
   does nothing. Verify with `getAttribute('aria-checked')`, not `.checked`.
3. **"Which one describes you"** / **"What would you like to do"** — a `Skip` button exists on both.
4. **Plan picker** — skip it; the free tier is what you came for.

⛔⛔ **THE TRAP THAT COST THE MOST TURNS: COORDINATES GO STALE BETWEEN MEASURE AND CLICK.**
`Next` measured at y=770, then moved to y=746 as the step re-laid-out. Clicking the old point hits
nothing, reports `delivered:true`, and the page simply does not advance — indistinguishable from a
validation failure, so you go hunting for a missing field that is not missing.
⇒ **Re-measure immediately before every click on this site**, and if a click "does nothing", suspect
a moved target before suspecting the form.

**Then the API key** — same as the `create-api-key` entry above: Speech to Text defaults to
`No Access`, `web do click --selector` refuses with `no_hittable_match`, coordinates work, and the
`sk_` key appears once in an `<input>` (never in body text).

★ **Each account is a separate 10,000-credit / ~2.6 h monthly pool. Pools do NOT merge** — every
API call bills the key's own account, so the consumer must be told which account to use
(`cg_scribe.py --account …`) and must stamp it on each spend row, or a budget report sums three
independent pools into one imaginary one.
