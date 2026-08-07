# claude.ai

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## claude-code-cli-account-signin · WORKS
task: Sign fleet Claude Code CLIs into a claude.ai subscription account, headlessly
model: claude-opus-5
date: 2026-08-07
tags: 

Signing a fleet host's Claude Code CLI into a claude.ai subscription account,
end to end, with no human at a browser. Proven 2026-08-07 moving four fleet hosts
from a Pro account to the Max account.

## THE ONE THING THAT BLOCKS EVERYTHING: the UA gate

claude.ai and claude.com REFUSE WebKitGTK's default identity. The tell is not a
Cloudflare page, it is Anthropic's own JSON, served as the document body:

    {"error": {"type": "forbidden", "message": "Request not allowed"}}

`document.readyState` is "complete", the URL is the one you asked for, and the
page is 82 bytes of refusal. It reads like a dead surface and is not.

FIX — one per-site override, exactly what `ychrome/src/useragent.rs`'s own module
docs ("The claude.ai story, re-measured") predicted would be needed:

    ~/.yggterm/ychrome/user-agent.json
    { "preset": "engine",
      "sites": { "claude.ai": "safari", "claude.com": "safari" } }

Verify with `ychrome identity --host claude.ai --json` BEFORE navigating. Both
domains are needed and neither inherits from the other: the login is on
claude.ai, the Claude Code OAuth consent screen is on claude.com. Everything
else on the host keeps the coherent engine identity.

⚠ curl cannot substitute and cannot diagnose this. curl gets Cloudflare's
"Just a moment..." interstitial (403) regardless of UA, because its TLS
fingerprint is nothing like WebKit's. An AUTHENTICATED api call with a
sessionKey cookie DOES work in curl (`/api/bootstrap`, `/api/organizations`) --
that is how you identify which account a profile jar holds without a browser.

## THE LOGIN IS A MAGIC LINK, NOT A CODE

The page renders `input#code` "Enter verification code" -- but for a
custom-domain (non-Google) account the email contains ONLY a sign-in LINK,
expiring in 10 minutes. An agent that waits for six digits waits forever, and
the 6-digit runs a naive regex finds in that email are tracking-pixel noise.

The anchor whose text is "Sign in" is a direct claude.ai href, ~78 chars, NOT a
click-tracking redirect (every other link in the mail goes through
url8792.mail.anthropic.com). Extract that one, `ctl goto` the SAME page to it,
and the session lands. Mail arrives within ~1 s of clicking "Continue with
email"; read it with `tb` on the mail host.

## THE HEADLESS ENGINE IS THE RIGHT LANE

The whole flow runs on `ychrome ctl` -- no yggterm session, no row, no viewport,
nothing on the operator's screen. Confirm the engine is live with a REAL verb
(`ychrome ctl pool --json`); `ctl --help` exits 0 on an old binary too.

    ychrome ctl open url=... profile='claude dada'    # creates the profile
    ychrome ctl eval page_id=pg_N js='...'            # note: js=, not script=
    ychrome ctl input page_id=pg_N events='[{"type":"click","selector":"..."}]'

⚠ `ctl wait` rejects `until=load:finished`; a plain `sleep` after goto is the
working pattern. `ctl open` takes NO user_agent parameter -- the per-site config
file above is the only door.

Address a button by tagging it in an eval first, then clicking the tag:

    ctl eval js='(function(){var b=[...document.querySelectorAll("button")]
      .find(x=>x.innerText.trim()==="Authorize");
      b.setAttribute("data-ygg","auth"); return "tagged";})()'
    ctl input events='[{"type":"click","selector":"button[data-ygg=auth]"}]'

## RELAYING A CLAUDE CODE LOGIN FROM ANOTHER HOST

`claude auth login --claudeai --email <addr>` prints a claude.com authorize URL
carrying `code=true`, so the consent page DISPLAYS a pasteable code
(`<48 chars>#<44 chars>`) instead of only redirecting to its localhost callback.
That display path is what makes a cross-host relay possible: browser on the GUI
host, CLI anywhere.

  1. run `claude auth login` inside a pty (it is a TUI prompt, not a pipe)
  2. scrape the URL, open it in the signed-in profile, click Authorize
  3. regex the code off the page, write it back to the waiting pty

⛔ Send the code and the Enter as TWO separate writes ~1 s apart. One write
carrying text + "\r" is paste-buffered and never submits -- the fleet's standing
TUI trap.

⛔ Set BROWSER=true when running `claude auth login` over ssh. Without it the CLI
runs xdg-open on the target host; on a GUI host that is a window on the
operator's seat (observed live: xdg-open + www-browser spawned).

⚠ `claude auth status --json` is the honest verifier, but its `email`/`orgName`
come from the CACHED `~/.claude.json` `oauthAccount` block while
`subscriptionType` comes from `~/.claude/.credentials.json`. The two can
disagree and it looks like the login went to the wrong account. Cross-check the
credentials file directly.

⚠ If you must log in on a host with LIVE sessions, run the whole flow under an
isolated HOME (`HOME=/tmp/x claude auth login`) and then `os.replace()` the
resulting `.credentials.json` into place -- an atomic rename, so a running
session never reads a torn or absent file. Proven safe against 6 concurrent
sessions. But remember this leaves `~/.claude.json`'s cached oauthAccount stale,
so patch that too or `auth status` will report the previous account forever.

## IDENTIFYING WHICH ACCOUNT A PROFILE JAR HOLDS

The jar is a Netscape text file at `~/.yggterm/web-profiles/<p>/cookies`. The
real `sessionKey` is stored on a `#HttpOnly_.claude.ai` line -- a parser that
skips lines beginning with "#" (the usual comment rule) silently reports every
profile as logged out. `sessionKeyLC` (13 chars) is only a marker; the 131-char
`sessionKey` is the session.
