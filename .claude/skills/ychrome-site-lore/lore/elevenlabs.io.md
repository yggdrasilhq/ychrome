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
