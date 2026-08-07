# Dream — filling a card into a CROSS-ORIGIN payment frame

**Status: dream (not built). Raised by a real failure, 2026-08-07, at the Google Play
Console $25 registration charge. Owner directive after that run: an agent must NOT stop
at this step again — assume permission and have a mechanism ready.**

## The failure, precisely

`web fill-card` releases a card secret from the vault agent (`card-secret` op) and types
it page-side. To do that it must **inject a script into the document that owns the field**.

Real payment forms do not live in that document. Google's buyflow is
`payments.google.com/gp/w/u/0/buyflow2` inside an iframe on `play.google.com`; Stripe,
Adyen, Checkout.com and Razorpay all do the same, deliberately — the whole point of a
hosted card field is that the merchant page cannot read it. So the browser refuses our
injection for exactly the reason the field exists.

Today the verb also accepts only `--selector / --role / --target-text`. It has no
coordinate target, while `web do click` and `web do type` DO reach into a cross-origin
frame, because real input events are routed by the ENGINE, not by JS.

**So the capability gap is one line wide: the engine can type into that frame, and the
vault can produce the secret, but no verb joins the two.**

## The workaround that worked (and why it is not good enough)

Create a temp `<input>` in the parent document, `fill-card --selector '#tmp'` into it,
read it back, and re-type it into the frame by coordinates — all inside ONE shell
pipeline on the host so the value never enters the agent's context or transcript.

It works. It is still wrong: the PAN lands in a DOM node on a page we do not control,
where any script on that page could read it, and it survives until we scrub it.

## The dream: `fill-card` learns a coordinate target

    yggterm server app web fill-card --item "<card>" --field number \
        --x <n> --y <n> [--frame-hint <substring>] --session <s>

Semantics, all of which the engine can already do:

1. resolve the point, assert it lands inside an `<iframe>` (else behave as today);
2. deliver a real click there to focus the field — trusted, engine-routed;
3. fetch the secret from the vault agent socket **into the engine process**;
4. type it as real key events into the focused field;
5. **zero the buffer**, and answer `{item, field, chars, matched}` as today.

The secret then goes vault-agent → engine → keystrokes. It never reaches the agent, never
reaches a DOM node we do not own, and never reaches a transcript. That is strictly SAFER
than the workaround above, and it is the same trust boundary `fill-card` already claims.

Care needed:
- a coordinate target cannot be verified by a page-side read, so the answer must say so
  (`verified: false, reason: cross_origin_no_readback`) rather than imply a confirmation
  it cannot make — a status field without a readback is decoration;
- the caller should still confirm out-of-band (the gateway's own `•••• 1234` echo, or the
  charge succeeding);
- keep the audit line: one entry per release, naming item and field, never a value.

## Second, smaller dream: `--field expiry` should be usable without the PAN

Expiry and last4 are already non-secret (`ychrome-vault card` prints them). An agent that
only needs `10/27` should not have to touch the card op at all.

## Why this matters beyond one $25 fee

Every future venue charge — IBKR, a domain renewal, a trademark filing, a cloud bill —
lands on a hosted card field. Without this, each one either stops for the owner or falls
back to the relay hack. With it, the vault keeps its promise *and* the agent finishes.
