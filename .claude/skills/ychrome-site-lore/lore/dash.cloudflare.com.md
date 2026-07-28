# dash.cloudflare.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## headless-eval-bridge-down · BLOCKED
task: mint a Cloudflare API token headlessly to fix an expiring wildcard cert
model: claude-opus-5
date: 2026-07-28
tags: 

Headless drive of dash.cloudflare.com was blocked not by the site but by the
surface eval bridge on the GUI host, which is currently returning an error for EVERY
script on EVERY page.

Reproducer (fails identically on a trivial page, so it is not site-specific):

    S=$(yggterm server app terminal new --kind shell --no-activate | jq -r .data.session_path)
    sleep 3
    printf '\x15~/.local/bin/ychrome --profile agent-test https://example.com/\r' \
      | yggterm server app terminal send $S --stdin
    sleep 8
    yggterm server app open $S; sleep 12
    yggterm server app web ensure --session $S --ttl 600
    yggterm server app web eval --session $S 'document.title'

Observed:
  - `web eval` -> accepted:false, detail: "the script returned a Promise or a
    non-serializable object; return a JSON value, or use `web await`"
    ...for `document.title`, which is a plain string. The message is a
    MISDIAGNOSIS and cost real time: it points at the script, not the channel.
  - `web read --as forms` -> "js_result_unsupported: Unsupported result type"
  - `web screenshot --session` -> "There was an error creating the snapshot"
  - `web ensure` repeatedly -> accepted:true but
    "page was unresponsive; a rebuild is queued"; generation increments on each
    call (6,7,8,9) with generation_before == generation_after and healed:false,
    i.e. the queued rebuild never converges.

Ruled out:
  - NOT the stale-agent trap: ychrome --daemon (pid 945569, started 23:06:34)
    and yggterm (946994, 23:07:14) both match their binary mtimes
    (23:06:34 / 23:06:50), so both run current code.
  - NOT site-specific: example.com fails the same way as the Cloudflare SPA.
  - NOT a missing declare: after a long enough reveal the declare lands and
    `ensure` finds the surface; it is the page that never becomes responsive.

Timing trap found on the way (worth fixing in the recipe regardless): sending
the ychrome line immediately after `terminal new` RACES the shell's startup and
is silently swallowed - the send reports accepted:true, bytes:70, but no
process appears and the later `ensure` reports "the daemon has no web-surface
declare ... (a plain shell, or the app already closed its surface)". Sleep ~3s
after `terminal new` before the send, and allow ~15-20s of reveal (not the ~5s
in the skill) before `ensure`, because the declare cannot land until ychrome has
actually started.

Impact: the whole headless co-browse lane is down on this host. Passkey work is
unaffected in principle - ychrome/yggterm/ychrome-vault all carry the fido2
symbols and the ceremony is implemented in src/passkey.rs - but there is no way
to drive a page to the point of triggering the ceremony while eval is dead.

Also stale and actively misleading: ychrome/.claude/skills/ychrome/SKILL.md
still lists Passkeys under "Still open" ("needs a navigator.credentials
userscript shim ... The agent may never auto-consent"), while src/passkey.rs is
859 lines of implemented ceremony and the shipped binaries carry the symbols.
An agent that trusts that section concludes passkeys are impossible. It should
be moved out of "Still open" and describe the built flow.
