# dash.cloudflare.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## mint-scoped-api-token · WORKS
task: Mint a scoped DNS-01 API token for lego wildcard renewal
model: claude-opus-5
date: 2026-07-28
tags: 

Minting a scoped API token end-to-end on an agent surface. Worked 2026-07-28.

## The short version

Login works unattended. 2FA does NOT go through the security key — use the
backup codes in the vault item's `notes`. Once in, do NOT drive the token
wizard: call the dashboard's own API same-origin.

## Login

- **Turnstile auto-solves on an agent surface.** `cf_challenge_response` is
  populated (~709 chars) by the time the page settles, and the Sign in button is
  never `disabled`. An older note claiming Turnstile gates the button is
  superseded — do not plan a workaround for it.
- `web fill --entry dash.cloudflare.com` fills both fields correctly
  (`filled: "user+password"`). Verify with an eval: `#email` value and
  `#password` length.
- Plain `el.click()` drives the Sign in button (React). No full gesture needed.
- Wrap click scripts in an IIFE with an explicit `return` — a bare
  `if (b) {…} else {…}` has no completion value and `eval` reports `null`, which
  looks like the click failed when it did not.

## 2FA — the wall, and the way through

`/two-factor` defaults to **security key** and renders "Sorry, your browser does
not support security key." That is OURS, not WebKit's: an agent-created surface
gets no passkey shim and no `yggterm-appctl://` signer bridge. See
`yggterm/docs/agent-passkey-gap-2026-07-28.md`. A passkey IS registered on this
account, so do not conclude it is missing.

Options offered: security key / **authenticator app** / recovery.

- The vault item has **no** authenticator secret.
- It **does** carry **8 × 9-character backup codes, one per line, in `notes`**.
  Click "Use authenticator app", then put one code into `#twofactor_token` using
  the native-setter injection (plain `el.value=` leaves React's model stale):

  ```js
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;
  set.call(el, code);
  el.dispatchEvent(new Event('input',{bubbles:true}));
  el.dispatchEvent(new Event('change',{bubbles:true}));
  el.dispatchEvent(new Event('blur',{bubbles:true}));
  ```

  then click **Verify** (`type=submit`).
- ⚠ **Codes are SINGLE-USE. #1 was consumed 2026-07-28.** Take the next unused
  line, and tell the operator when the stock runs low so they can regenerate.

## Minting a token — use the API, not the wizard

After login the dashboard session cookie authorises same-origin calls, and
**no CSRF header was required**:

```js
await fetch('/api/v4/zones?name=<zone>',                {credentials:'include'})
await fetch('/api/v4/user/tokens/permission_groups',    {credentials:'include'})
await fetch('/api/v4/user/tokens', {method:'POST', credentials:'include',
  headers:{'Content-Type':'application/json'},
  body: JSON.stringify({
    name: '<name>', status: 'active',
    policies: [{ effect:'allow',
      resources: { ['com.cloudflare.api.account.zone.' + ZONE]: '*' },
      permission_groups: [{id: DNS_WRITE}, {id: ZONE_READ}] }]})})
```

The token value comes back once, in `result.value`. Pipe the verb's stdout
through a filter that writes it mode-0600 and prints only its length — never let
it reach a transcript or `argv`.

Permission-group names to look up (ids are account-stable but re-read them):
`DNS Write`, `Zone Read`.

## Traps

- **The 600 s surface lease is silent.** Mid-flow a verb answered
  `web surface not live`; re-ensuring returned `healed: false, leased: true` with
  the page reset to `about:blank` — the logged-in session was discarded. It
  recovers because the cookie jar is per-profile: just re-navigate to
  `https://dash.cloudflare.com/` and you land authenticated. Budget for this on
  any flow longer than ten minutes.
- **Stale `_acme-challenge` TXT records break DNS-01 renewal silently.** Two
  records from 2026-04-08 were still in the zone; lego's propagation check read
  them from `1.1.1.1` and never saw its own new values, failing with
  `time limit exceeded ... did not return the expected TXT record`. Earlier runs
  could not clean up because their token was already invalid, so the garbage
  accrued. **Check and clear `_acme-challenge.<zone>` before re-running lego**,
  and remember the recursive resolver caches the RRset for its TTL (600 s), so
  wait that out before retrying.
