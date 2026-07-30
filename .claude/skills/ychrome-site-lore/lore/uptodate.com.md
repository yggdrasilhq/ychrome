# uptodate.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## institutional-login-and-topic-read · WORKS
task: log in with institutional subscriber creds and read full clinical topics incl. grades and topic version
model: claude-opus-5
date: 2026-07-30
tags: login, two-step, vault-match-trap, medical, text-extraction, tos

Institutional (subscriber) login + full-text topic reads. Worked first try; no captcha challenge fired.

**Login is TWO-STEP** (`https://www.uptodate.com/login`):
1. `#userName` -> click `#btnContinueLogin` (`--role button --label 'Continue'`)
2. password field `#password` appears -> click `#btnLoginSubmit` (`--role button --label 'Sign in'`)
Lands on `/contents/search`. A `g-recaptcha-response` textarea is present but was invisible/passive.

**⚠ THE TRAP: the vault holds TWO logins under `uptodate.com`, and `ychrome-vault match uptodate.com`
resolves to the WRONG (inactive) one.** Never let `web fill --entry uptodate.com` auto-pick.
Always pin the account explicitly:
```
web fill-vault --item uptodate.com --field username --user <active-user> --selector '#userName'
web fill-vault --item uptodate.com --field password --user <active-user> --selector '#password'
```
Which of the two is active is recorded in the versestore campaign (`~/data/versestore/CAMPAIGN.md`);
do not guess and do not trust `match`. Both `fill-vault` calls verified via `#password.value.length`.

**Topics are plain URL slugs — no search dance needed:**
`https://www.uptodate.com/contents/<kebab-case-topic-title>`
e.g. `/contents/cluster-headache-treatment-and-prognosis`,
`/contents/obesity-in-adults-drug-therapy`,
`/contents/approach-to-the-adult-patient-with-fatigue`.
A wrong slug 302s to `/page-not-found` (detect on `document.title`). Recover the real slug via the
search URL below and read the `a[href*="/contents/"]` list.

**Search by URL (returns UpToDate's own NLP ranking — useful to prove a concept is ABSENT):**
`/contents/search?search=<terms>&sp=0&searchType=PLAIN_TEXT&source=USER_INPUT&searchControl=TOP_PULLDOWN`

**Reading a topic cheaply.** The whole topic renders in `document.body.innerText` (30-150 kB) with
nothing collapsed. Do NOT pull it into the agent's context. Dump it to a file on the driving host,
then `grep`/`sed` there:
```
echo 'JSON.stringify({t:document.title,x:document.body.innerText})' \
  | yggterm server app web eval --stdin --session $S > raw.json
python3 -c "import json;d=json.load(open('raw.json'));open('topic.txt','w').write(json.loads(d['data']['value'])['x'])"
```
Provenance lines are deterministic:
- `Literature review current through: <Mon YYYY>.` and `This topic last updated: <Mon DD, YYYY>.` near the top
- `Topic <id> Version <n>.0` as the last line before the footer
Section headings are ALL-CAPS on their own line (`grep -nE '^[A-Z][A-Z /,&()-]{5,}$'`), which makes
`sed -n 'A,Bp'` extraction exact. Recommendation grades appear inline as `(Grade 2C)` etc. —
`grep -oE 'Grade [12][ABC]'` inventories them.

**⚠ ToS, read before caching.** Every page carries: UpToDate content "…prohibit the use, training,
inputting or processing of UpToDate content by or into automated software or tools, including…
artificial intelligence solutions, algorithms, machine learning, and/or large language models."
Driving it with an agent is squarely in tension with that clause. Flag it to the owner; prefer
storing citation + topic-id/version + paraphrase over bulk verbatim in any persistent cache.
