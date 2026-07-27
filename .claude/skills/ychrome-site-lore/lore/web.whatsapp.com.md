# web.whatsapp.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## qr-login-cobrowse-surface · WORKS
task: reach the WhatsApp Web QR link screen headless in a dedicated profile and make it human-co-browsable for account linking
model: claude-fable-5
date: 2026-07-27
tags: login, qr, whatsapp, cobrowse, profile, persistence, user-agent

**WhatsApp Web serves its REAL login page (QR) to ychrome with no UA work at all** —
ychrome's default Safari-on-macOS preset passes the browser gate because the engine
really is WebKit. Verified live 2026-07-27: `title: WhatsApp`, "Scan to log in" card,
one `<canvas>` QR, and a "Download WhatsApp for Mac" banner (it believes the Safari
claim). Do NOT switch to the Chrome preset for this site; the matching-engine UA is
the whole trick.

### The rigged surface (headless QR + human co-browse), end to end

1. Row on a remote machine's daemon, GUI view untouched:
   `yggterm server app terminal new --machine-key <host> --kind shell --no-activate --title "WhatsApp Web" --purpose "..."`
2. `terminal send <session> --stdin` → `ychrome --profile <dedicated-profile> https://web.whatsapp.com`
   (dedicated profile, always — the linked account must never share a jar with other personas).
3. `web ensure --session <session> --ttl 3600` materializes the surface headless
   (`rebuilt_from_daemon_declare: true` on 2.12.10+, no reveal ever needed).
4. QR presence probe (cheap, no pixels): `web eval` →
   `document.querySelectorAll('canvas').length === 1 && document.body.innerText.includes('Scan the QR code')`.
   Pixel proof: `web screenshot --session <session> out.png` (webkit snapshot is faithful for a web surface).
5. Mark the row keep-alive or it dies with the GUI window: `server app terminal keep <session>`
   (plain shells are NOT born keep-alive; agent-CLI rows are).
6. The human co-browses by clicking the row in their GUI; they scan with
   phone → WhatsApp → Settings → Linked devices → Link a device.

### QR lifecycle (matters for timing)

- The QR self-rotates every ~30-60 s without any interaction.
- After a few idle minutes an overlay appears: a `[role=button]` whose text/aria is
  "Select to reload QR code". It can CLEAR ON ITS OWN when the page's socket re-syncs —
  observed live: present in one probe, gone 3 minutes later with a fresh canvas and no
  input from anyone. So a stale QR is never fatal: the co-browsing human clicks the
  overlay, or agent-side `web reload --session` fetches a fresh page+QR (harmless
  pre-link).
- Alternative path exists on the login card: "Log in with phone number" (code entry
  instead of QR) — untested.

### Storage / persistence (the part that is NOT obvious)

- The profile jar materializes on the GUI HOST that renders the surface —
  `~/.yggterm/web-profiles/<profile>/` (cookies, localstorage, IndexedDB `databases/`,
  serviceworkers, CacheStorage). The remote host in `--machine-key` owns the PTY and
  the ychrome process; the BROWSER STATE lives with the rendering GUI.
- WhatsApp's linked-device state lives in IndexedDB, not cookies — `web cookies
  --export` does NOT carry the login; the jar DIRECTORY is the unit of persistence.
- A reclaimed/rebuilt surface (lease expiry, tab reclaim, GUI restart) comes back
  through lazy-create on row click and restores the login from the jar. If the ychrome
  process itself exits (`declare_stale`), relaunch the SAME `--profile` and the login
  survives.

### Driving the surface headless (verified pre-link)

- `web eval` / `web read` work on the never-revealed surface.
- `web do click` runs its full pipeline here too — no seat-input refusal on an
  unrevealed surface (2.12.18). Two honest refusals seen live, both correct:
  `no element matches text~:...` (the overlay had vanished between probe and click)
  and `target_moved (the resolved point no longer hits css:canvas)` — the QR canvas
  sits UNDER an overlaying element, so hit-testing refuses the mis-click. Address the
  overlay/button, not the canvas.

### Phase-2 doors (message send — investigated, NOT exercised)

- Post-link composer is a `contenteditable` div (React/Lexical) in the chat footer —
  synthetic `.value=`/native-setter cannot commit into it. The seat-grade path:
  `web do click` the composer → `web do type --text ...` (real keys) →
  `web do key --key Enter` (or click `span[data-icon="send"]`); `web batch` for the
  whole sequence behind one gate.
- PRIVACY RULE for this site: once linked, the page shows real conversations. No
  full-page screenshots into logs/lore, no message content or contact identifiers in
  any committed artifact. Targeted probes only.

### Trap

- ychrome may print "the running daemon is serving OLD CODE ... N live surface(s)
  attached" at launch — a warning only; the new surface opens fine. Never
  `ychrome daemon restart` while another live surface is attached to it.


## message-send-lexical-composer · WORKS
task: send messages into a chat on the linked account, headless, with per-message verification
model: claude-fable-5
date: 2026-07-27
tags: send, composer, lexical, react, keyboard, deep-link, preempted, surface-not-mapped

**`web do type` and a `do click` on the Send button are both ACCEPTED but neither
commits on this React/Lexical composer** — `accepted:true` means the synthetic event
was delivered, not that the app took it. The working recipe is page-side:

1. **Open the chat by deep link**, not by driving the search UI:
   `web eval "location.href='https://web.whatsapp.com/send?phone=<E164>'; 'nav'"` —
   full SPA reload (~10-15 s), login restores from the jar. Composer probe:
   `footer div[contenteditable=true]` count = 1; also check body text does NOT
   contain "invalid" (bad-number modal).
2. **Insert text with `web await`** (async body): focus the composer, then
   `document.execCommand('insertText', false, TEXT)` with TEXT **JSON-encoded into
   the script file** (never hand-escape through ssh quoting). The commit is ASYNC —
   read `innerText.length` only after ~500 ms or you'll read the empty state.
3. **Send with a synthetic Enter**, not the button: dispatch
   `keydown/keypress/keyup` KeyboardEvents (`key:'Enter', keyCode:13, bubbles:true`)
   on the composer. Composer length back to ≤1 ⇒ sent. The Send button today is
   `aria-label "Send"` / `data-icon "wds-ic-send-filled"` (the old
   `span[data-icon=send]` selector is dead), and a synthetic click on it does NOT
   fire its handler.
4. **Verify without reading the chat**: prefix-match your own sent text against
   `span.selectable-text` (booleans only), and `[data-icon="msg-time"]` count 0 =
   nothing stuck pending. Keep every probe boolean/count-shaped — the page is a
   real inbox now.

### Traps that cost a round each

- **Composer accumulation**: a failed send leaves the text in place and every retry
  APPENDS. Read composer length before inserting; clear with
  `execCommand selectAll` + `delete` (also async — re-read after ~300 ms).
- **Reveal-then-leave kills the do plane twice over**: after the human co-browses
  the surface (QR scan) and navigates away, `do` verbs refuse `preempted` (the
  shared batch id is locked out), and `--new-batch` then hits `surface_not_mapped`
  (the webview is fully hidden, not soft-stashed). Recovery that KEEPS the login:
  `web close --session` → `web ensure --session` (rebuilds mapped-headless from the
  daemon declare; `forget()` clears the preempt lane on recreate; the jar carries
  the login). `web ensure` alone will not remap a live-but-hidden page.
- The `web` verb plane lives in the **GUI binary** (`yggterm server app web …`);
  `yggterm-headless` answers "unsupported app control command: web".
