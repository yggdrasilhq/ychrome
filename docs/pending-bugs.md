# ychrome — pending bugs

Entries are removed in the same commit as their verified fix. Newest first.

> **This file is the ONE answer to "what is open" for ychrome.** Open items
> only; an entry is deleted in the same commit as its verified fix and git
> remembers it. The law, the owner table for every other question, and how to
> search the archive are in `yggterm/docs/docs-ssot.md`.

## ⛔ `cargo test --bin ychrome` IS RED ON THE TRUNK — 4 sidebar tests, and it is not a flake

**Status:** OPEN. Measured 2026-08-21. `382 passed; 4 failed`, deterministic, on a clean tree.

```
sidebar::tests::a_field_spec_round_trips_including_an_awkward_name
sidebar::tests::the_add_tab_can_make_a_note_and_a_card_not_only_a_login
sidebar::tests::the_edit_form_can_change_a_card_and_never_shows_one_back
sidebar::tests::the_edit_form_puts_the_mask_the_eye_and_the_copy_on_the_field
```

The visible one: the Add tab's type picker now offers **four** item types and the test asserts
three.

```
left:  ["Login", "Note", "Card", "Identity"]
right: ["Login", "Note", "Card"]
```

**Cause:** commit `0c6b80e` introduced `CIPHER_TYPE_IDENTITY` and the Identity form sections
without updating the four tests that enumerate the item types. Confirmed by
`git log -S CIPHER_TYPE_IDENTITY`, and reproduced on a second checkout with no local changes,
so it is the trunk and not a working tree.

⛔ **Deliberately NOT fixed here.** These are another lane's in-flight feature and the failing
assertions are the only thing standing between that feature and nobody checking it. Editing a
test until it matches the code is exactly how a real regression gets laundered — the author
should say which side is right.

⚠ **What it costs everyone until then:** the suite cannot be used as a green gate, so every
session must either know these four names or conclude its own change broke something. That is
the whole reason this is filed at ⛔ rather than left for whoever notices.

**Falsifier:** `cargo test --bin ychrome` prints `0 failed`.

---

## ⛔⛔ YOUTUBE VIDEO PLAYBACK SEGFAULTS THE ENGINE (the crash is open; the silence around it is fixed)

**Status:** OPEN. Measured 2026-08-21 on a current release build, reproduced **4/4**.

Open a YouTube watch page, let it play, and the whole daemon dies in **1–5 seconds**.
Timed from one run: the `open` verb returned after 3.9 s, the daemon was gone 1.1 s later.

**The exit is a `SIGSEGV`**, caught by running the daemon under a supervisor instead of
letting it be spawned:

```
supervise.sh: line 11: <pid> Segmentation fault    ychrome --daemon
```

⛔⛔ **Nothing in the product records this, by construction, and that is the reason it
has never been diagnosed.** Three separate channels each lose it:

1. **`spawn_daemon()` sets `.stderr(Stdio::null())`** (`daemon.rs`). The crash message,
   the WebKit internal errors and the EGL warnings are all written to a discarded fd.
   A daemon the app spawns can never say why it died.
2. **The journal has no record.** Every earlier daemon transition in `journal.jsonl` is a
   `daemon_stop` / `daemon_start` **pair**; the crash transitions are `daemon_start`
   **alone**. The engine's own log therefore cannot distinguish a crash from a restart.
3. **`engine.display.reaped` is emitted by the SUCCESSOR at startup**, not by the dying
   daemon — so the one line that appears near a death is written by a different process
   about a display it inherited, and reads as routine housekeeping.

### What it is NOT — three controls, all negative

| control | result |
|---|---|
| plain page (`example.com`) | daemon alive indefinitely (30 s+) |
| YouTube **search results** page, no playback | daemon alive, 15 s+ |
| **local 4K60 H.264 progressive clip**, 258 % CPU, 16 s | **no crash**, 3/3 |

⇒ It is **not the site**, and it is **not decode load** — a locally generated 4K60 clip
pushes the same decoder harder for longer and survives.

### ⛔ MSE IS EXONERATED, AND SO ARE THE CODECS AND EME — four more controls, 2026-08-21

The sentence that used to end the paragraph above named **MSE / adaptive streaming** as
"what is left". It was the reasonable next guess and it is **wrong**. Measured on an idle
host (load average 3.9 on 32 cores), against locally generated clips and a local server, in
an isolated `$HOME` so the operator's daemon was never touched:

| control | result |
|---|---|
| progressive **H.264** 720p60, `file://` | plays to completion, **no crash** |
| progressive **VP9** 720p60 | **no crash** |
| progressive **AV1** 720p60 | **no crash** |
| **MSE**: fragmented H.264 fed through `MediaSource.appendBuffer` | plays to completion, **no crash** |
| **EME**: `requestMediaKeySystemAccess` for widevine / clearkey / playready / fps | all four rejected with `TypeError`, **no crash** |
| VA-API hardware decoders demoted to rank 0, then YouTube | **still crashes** |
| ychrome's own userscripts neutered (`scriptlets`, `cosmetic-filters`), then YouTube | **still crashes** |

⇒ The engine decodes all three codecs, drives MSE correctly, and has no EME at all. **The
crash needs something a YouTube watch page does that none of these reproduce.** It is also
not ychrome's own injected scripts, and not the hardware-decode path.

⚠ **One measurement in the earlier version of this file was an artefact of its own
harness and is withdrawn:** progressive `<video src>` appeared to stall at the first frame
(`currentTime` 0.07, `readyState` 2) over `http://`, while the same body played fully over
`file://`. The cause was the throwaway `python3 -m http.server` used to serve it — it does
not implement **Range** requests. ⇒ Never measure a media path through that server; it makes
the engine look broken.

⚠ Alongside it in the discarded stderr, repeatedly:
`WebLoaderStrategy::internallyFailedLoadTimerFired()` — "WebKit encountered an internal
error" — and `ctl console` shows two `failed to load` entries against the watch URL
itself. Whether the failed loads cause the segfault or share a cause with it is not settled.

### ⛔ FOUR WORKAROUNDS TRIED, ALL NEGATIVE — do not spend the session re-trying these

| tried | result |
|---|---|
| `WEBKIT_DISABLE_DMABUF_RENDERER=1` | still crashes |
| `+ GST_GL_DISABLED=1 + LIBGL_ALWAYS_SOFTWARE=1` | still crashes |
| a **320x240** viewport, so the site picks a low resolution | still crashes |
| `WEBKIT_DISABLE_COMPOSITING_MODE=1` | already set by the substrate, and insufficient |
| **VA-API decoders demoted** (`GST_PLUGIN_FEATURE_RANK=vah264dec:0,vavp9dec:0,vah265dec:0,vavp8dec:0,vaav1dec:0`) | still crashes, in 4.9 s |

⇒ It is **not resolution-dependent** and it is **not the GL/DMA-BUF path**, which is where the
EGL warnings invite you to look first.

**Where it actually faults** (gdb, release build so the frames are unsymbolised):

```
Thread "ychrome-engine" received signal SIGSEGV
#0  libwebkit2gtk-4.1.so.0        <- fault here
#1-#9  libwebkit2gtk-4.1.so.0
#10-#12 libjavascriptcoregtk-4.1.so.0
#13-#15 libglib-2.0.so.0 (g_main_loop_run)
#16  gtk_main()
```

⇒ The crash is in the **UI process**, on the engine thread, dispatched from the GTK main
loop — **not in the web content process**. That distinction is the load-bearing one: a web
process crash is survivable and WebKit reports it, which is why the engine has no handler that
could have caught this. Reaching a cause below this needs debug symbols for
`libwebkit2gtk-4.1`, which are not installed and have no repo configured here.

⛔⛔ **THE RECOMMENDED UPGRADE IS NOT ONE LIBRARY — IT IS A `glibc` UPGRADE ON THE LIVE
HOST. Measured 2026-08-21, and it changes what this entry is asking for.**

`libwebkit2gtk-4.1-0` **2.52.6-1** is linked against **`GLIBC_2.43`**. This host runs
**2.42-17**. So the one-line `apt-get install libwebkit2gtk-4.1-0` below does not do what it
looks like:

```
$ apt-get install --simulate libwebkit2gtk-4.1-0
The following additional packages will be installed:
  gir1.2-javascriptcoregtk-4.1 gir1.2-webkit2-4.1 libc-bin libc-dev-bin
  libc-gconv-modules-extra libc-l10n libc6 libc6:i386 libc6-dev libc6-i386
  libjavascriptcoregtk-4.1-0 libjavascriptcoregtk-4.1-dev libwebkit2gtk-4.1-dev locales
```

⇒ **`libc6` 2.42-17 → 2.43-3, plus its i386 multiarch twin, on a machine with live sessions
on it.** The previous framing — "shared system infrastructure that every GTK web app links" —
understated it by a whole layer. This is not a browser-lane call and it is not a small
sysadmin call either; it is a C-library upgrade under a running desktop.

⚠ It also blocks the cheap version of this experiment. Extracting 2.52.6 into a private prefix
and running the engine against it with `LD_LIBRARY_PATH` **does not work**: the loader refuses
(`version 'GLIBC_2.43' not found`), and swapping the loader too does not help because
`libwebkit2gtk` has **no `WEBKIT_EXEC_PATH`** in this build — the `WebKitWebProcess` helper
path is compiled in, so the UI process would be 2.52.6 while the web process stayed 2.52.5.
(Verified: `strings` on the library lists `WEBKIT_INJECTED_BUNDLE_PATH` and no exec-path
variable.) A container or a `bwrap` bind-mount over `/usr/lib/x86_64-linux-gnu` is the
remaining way to test it without touching the host, and `bwrap` is installed.

⭐⭐ **THE MOST LIKELY REMEDY, AND IT NEEDS AN OWNER DECISION: UPGRADE WebKitGTK.**
Installed **2.52.5-1**; the distribution offers **2.52.6-1**.

⚠ **This entry first said 2.52.5 was the newest available. That was wrong, and the instrument
lied in this lane's favourite way — it was STALE.** `apt-cache policy` answers from cached
package lists, so it reported "no newer version" while a newer one had been published. One
`apt-get update` changed the answer. ⇒ Before concluding a version is the ceiling, refresh the
lists; a cache reports the last time you looked, not the world.

⇒ **Not done here, deliberately.** `libwebkit2gtk` is shared system infrastructure that every
GTK web app on the host links, including other live sessions, and **2.52.5-1 is no longer
downloadable** — not in the apt cache and gone from the archive — so a rollback means
snapshot archaeology plus dependency juggling rather than one command. That is a
system-administration call for the host's owner, not a browser-lane one.

⇒ **Two things converge on this single action**, which is what makes it the recommendation:
1. a crash in a library at `n-1` is worth retrying at `n` before any deeper work;
2. **symbolised debugging is blocked without it** — the only `-dbgsym` package published is
   **2.52.6**, and symbols must match the binary exactly, so there is no way to symbolise a
   2.52.5 crash at all. Upgrading either fixes the crash or makes it debuggable.

```sh
sudo apt-get update && sudo apt-get install libwebkit2gtk-4.1-0     # then re-run the repro
# if it still crashes, add the debug source and get a named frame:
echo 'deb http://debug.mirrors.debian.org/debian-debug/ sid-debug main' \
  | sudo tee /etc/apt/sources.list.d/debug.list
sudo apt-get update && sudo apt-get install libwebkit2gtk-4.1-0-dbgsym
```

⚠⚠ **NOT MEASURED, AND IT CHANGES WHAT THIS ENTRY MEANS: does the GUI substrate crash too?**
Every measurement here is on the **headless** substrate (Xvfb, no DRI3). The owner's reports
describe degraded picture, **not** a browser that dies — so the crash may well be
headless-only, and the thing he actually experiences may be the dead-counter entry above and
nothing else. ⇒ **Do not report this as "his browser crashes" until it is measured there.**

⛔ **And do NOT measure it by opening a window on the operator's display.** `substrate.rs`
records what that costs: GTK ignored the engine's Xvfb, connected to the operator's compositor,
and put real ychrome toplevels on the human's desktop — including a filled-in brokerage login
over the video he was watching. Use a dedicated display or the remote-desktop plane.

### Reproduce

```sh
ychrome ctl open url='https://www.youtube.com/watch?v=<any-video>'   # a 4K60 one dies fastest
# then watch from OUTSIDE — the engine cannot report its own death:
while :; do pgrep -x ychrome >/dev/null || echo DEAD; sleep 0.3; done
```

### ✅ THE RECORDING HALF IS FIXED THIS COMMIT — the crash itself is still open

`spawn_daemon()` no longer sends stderr to `/dev/null`. It goes to
`<state-dir>/daemon.stderr.log` (0600, rolled once past 4 MB), so the diagnostics that used
to require running the daemon under a supervisor by hand are now kept by default.
**Live-proven:** after the change, one ordinary reproduction left this on disk with no
special tooling —

```
libEGL warning: DRI3 error: Could not get DRI3 device
(WebKitWebProcess:…): GStreamer-Audio-WARNING **: Invalid channel positions
ERROR: WebKit encountered an internal error. This is a WebKit bug.
Source/WebKit/WebProcess/Network/WebLoaderStrategy.cpp(640) : …internallyFailedLoadTimerFired()
```

The reaping thread also journals `daemon_exit` with the signal when it observes one.
⚠ **That line will usually NOT appear, and the code says so**: the waiter lives in whichever
process spawned the daemon, and a CLI invocation exits long before the daemon dies. The kept
stderr log is the channel that does not depend on who is still watching. ⇒ A `daemon_exit`
line is a bonus, never the thing to look for. Measured after the change: 0 such lines across
several real crashes, exactly as predicted.

⛔ **The segfault is NOT fixed** — only made visible. It still reproduces on every attempt.

### What is still wanted

A `daemon_exit` written by something that outlives the daemon, so the journal can answer for
daemon lifetime on its own rather than by the absence of a `daemon_stop`.

---

## ⛔ `requestVideoFrameCallback` IS ADVERTISED AND NEVER FIRES (the ~30x under-report was the HOST'S LOAD, and is withdrawn)

**Status:** OPEN. Measured 2026-08-21 with locally generated clips — no network, no site.

⚠⚠ **THIS ENTRY WAS FILED WRONG EARLIER THE SAME DAY AND IS CORRECTED HERE.** It claimed
`droppedVideoFrames` was *"a constant 0"* and that **every** in-page instrument was dead. Both
overstated. The counter does move, and one of the four instruments is fine. Kept visible rather
than quietly rewritten, because the wrong version was pushed and someone may have read it.

### What is solid

⛔ **`requestVideoFrameCallback` is advertised and never fires.** `v.requestVideoFrameCallback`
is present, a self-rearming callback was registered, and after 10 s of playback it had fired
**0 times** — while `requestAnimationFrame`, registered in the same eval as a control, fired
232 times. Verified twice, the second time on a clip playing at very near its full rate, so
"nothing was playing" does not explain it. A player that paces on rVFC gets nothing at all.

⛔ **`droppedVideoFrames` under-reports by around thirty to one.** On a 60 fps clip over 10.2 s:
465 frames delivered against ~612 expected — **~147 missing — and `droppedVideoFrames` reported
5.** At 720p60 over 16.7 s: 327 missing, **4 reported**. The counter is not dead; it is
unusable for the one question anyone asks of it. ⇒ A page cannot tell healthy playback from
playback missing a quarter of its frames, which is exactly why *"nerdview reports no dropped
frames"* is not evidence of health.

### ⛔⛔ What is NOT established — the confound I did not check before publishing

**Every one of these measurements was taken on a host at load average 42–49, on 32 cores.**
Dozens of unrelated agent sessions were running their own WebKit processes throughout. So:

- ⛔ **The absolute frame rates here are NOT clean engine numbers** and must not be quoted as
  "the engine caps at 43 fps". Repeat runs of the *same* clip disagreed sharply — 360p60
  measured 37.0 fps once and 59.3 fps on a careful rerun — which is the signature of
  contention, not of a stable engine ceiling.
- ⛔ **The earlier claim that the engine cannot sustain 60 fps is FALSE.** 360p60 delivered
  **59.3 fps**. It sustains 60 fps when it has the machine to do it in.
- ⚠ The shortfall does grow with resolution (720p60 and 4K60 both fell short while 360p60 did
  not), which is what a software decode/paint path under contention looks like. Whether any
  shortfall survives on an idle host is **unmeasured**.

⇒ **The instrument findings above do not depend on load** — a counter reporting 5 of 147, and a
callback firing 0 of anything, are wrong at any load. Those are the findings to build on. The
frame-rate table is not.

### The measurement this wants next

Re-run the ladder on the GUI substrate rather than the headless one.

### ✅ THE IDLE-HOST RUN IS DONE, AND IT CLEARS THE COUNTERS — 2026-08-21

The confound above said the absolute numbers were unusable because the host sat at load
average 42–49. It has now been re-run at **load average 3.9 on 32 cores**, on a locally
generated 720p60 clip over `file://` (no network, no site, no server in the path):

| sampled at | `currentTime` | `totalVideoFrames` | expected at 60 fps | `droppedVideoFrames` |
|---|---|---|---|---|
| mid-playback | 3.33 s | **200** | 200 | **0** |

⇒ **`totalVideoFrames` tracks exactly, and nothing is dropped.** On an idle host the engine
delivers 720p60 perfectly and its counter says so. The "~30x under-report" was **the host's
load, not the engine's counter** — the instrument was reading a machine with dozens of other
WebKit processes on it. ⇒ Do not quote the 30:1 figure; it does not survive an idle host.

⚠ **`requestVideoFrameCallback` is untouched by this** — it is a separate defect, it was never
about frame rate, and it has not been retested here.

⚠ **A counter read AFTER playback ends reports `0`,** which is how a "dead counter" reading is
manufactured: sample mid-playback or the number means nothing.

⇒ The honest statement to the owner is now: *on an idle host the headless engine's frame
counters are accurate and it holds 60 fps; what the GUI substrate does is still unmeasured.*

**Context that bounds it:** the headless substrate gets no GPU —
`libEGL warning: DRI3 error: Could not get DRI3 device` — so decode and composite are entirely
software here. A render node exists on the host; Xvfb does not expose DRI3 to reach it.

---

## ⚠ A COMPANION SCRIPT STAYS MISSING WHEN THE INSTALLED BINARY PREDATES ITS REPAIR

**Status:** ⇒ **CONFIRMED AND CLEARED on one host 2026-08-21** by deploying a current
binary; the underlying gap stays OPEN because nothing reports it. ⛔ **The falsifier this
entry gave is broken — see the correction at the end before you use it.**

`scriptlets.js` was absent from a host's userscript directory with **no tombstone**, so the
2026-08-20 companion repair should have reinstated it on the next launch: the ruleset is
present, the catalog carries the stem, and nothing recorded a deletion. It did not run, and
`provision --json` did not list the asset at all.

The reconciler's code is correct. The **installed binary predates it** — four days older than
the fix, and the marker is absent from it:

```sh
strings -a "$(command -v ychrome)" | grep -c deleted_userscripts   # ⛔ ALWAYS 0 — see below
```

⇒ 3,341 scriptlet invocations over 5,338 domains were silently not running, on a host whose
ad blocking otherwise looked healthy. `unblock-select.js` was absent for the same reason.

⚠ **This is the stale-binary trap wearing a new coat.** The repo already knows it for the vault
agent (`agent_stale`) and for the ychrome daemon (`exe_stamp`), and both of those report
themselves. **Bundled-asset provisioning has no such stamp**: a fix to the reconciler is
invisible until someone happens to rebuild, and the symptom is an asset that looks deliberately
absent. The provisioner should report the binary it is running from, so "the repair is not
deployed here" is distinguishable from "there is nothing to repair".

### ⛔⛔ CORRECTION 2026-08-21 — THE FALSIFIER ABOVE FIRES ON EVERY BINARY, REPAIRED OR NOT

`strings -a "$(command -v ychrome)" | grep -c deleted_userscripts` returns **0 for a binary
freshly built from a tree that contains the repair.** Verified both ways: the source has
`webpolicy::deleted_userscripts()` and `provision.rs` calls it, and a release build of that
same tree still counts 0.

**`deleted_userscripts` is a Rust function name, not a string literal, and the release profile
sets `strip = true`.** The symbol is gone from the binary by design, so the grep can only ever
return 0. It cannot distinguish a repaired binary from an unrepaired one — the one thing it
was written to do.

⚠ The conclusion it was offered in support of happened to be **true** (that binary was four
days older than the fix), which is exactly why this survived: a test that always passes agrees
with you whenever you are right, and the next reader inherits it as proof.

⇒ **The real falsifier is behavioural.** Install the binary under test, run provisioning, and
look at what it did:

```sh
ychrome provision --json | grep -A2 '"id": "scriptlets"'
#   a repaired binary:  verdict "absent" -> wrote true, and says
#     ychrome: installed scriptlets (was missing on this host)
#   an unrepaired one:  the asset is not listed at all
```

**Live result on one host, 2026-08-21:** binary rebuilt from HEAD and installed ⇒ `scriptlets`
went `absent` → `wrote: true`, 579 KB written, and the scriptlet plane came back. The repair
works; only the deploy was missing, as this entry said.

⚠ `unblock-select.js` is still **not** provisioned. It is in the catalog and in the bundled
assets, but it is an **opt-in** extension, and provisioning only refreshes extensions already
installed — so it is absent by design, not by this fault. Do not chase it here.

---

## ⛔ A THIRD-PARTY FORM CAN RESHAPE A VALUE UNTIL EVERY LITERAL IN A SCANNER'S TERM LIST BREAKS

**Status:** OPEN on the host side only. Detected, purged and requested 2026-08-14; the
local half is finished and the remaining wait is on the hosting provider.

A value typed into a third-party signup form was **reformatted by that form into another
country's convention** before it was ever read back: the digits were regrouped, punctuation
was inserted, and **the tail was truncated**. What got written down afterwards was that
reformatted string, in good faith, as an illustrative example.

⇒ **Every literal in the pre-push scanner's term list therefore failed to match**, and the
scan reported no private data found. A sanitisation pass by eye missed it for the same
reason: the string no longer looks like the thing it came from.

**Two properties made it survive, and only the second is obvious.**

1. Punctuation and grouping differ ⇒ a literal comparison cannot fire.
2. ⛔ **The tail was truncated.** So the natural fix — normalise both sides to digits and
   compare the *end* of the value — **still does not match**, and it fails in the reassuring
   direction: it looks like a stronger check while being blind to exactly this case. The
   scanner has to normalise and then search **any interior window**, which is what it now
   does. Verified against number-dense real content with no false positives.

⚠ **The transferable half is not about phone numbers.** Any value a remote form is free to
re-render — an address, a date, an account identifier, a name with diacritics — arrives back
in a shape the term list never described. **A scanner built on literals is only as good as
the assumption that nobody reformatted the value in between**, and a form is entitled to.

⇒ **The standing rule stands and is the cheaper defence: invent every example.** The slip
here was not a rule that was unknown, it was a value that no longer *looked* real, so the
rule did not feel like it applied. ⭐ **A reformatted secret reads as an invented one** —
that resemblance is the whole trap, and it is why the guard and not the author has to be the
thing that catches it.

**Decision, already taken:** the history was rewritten and force-pushed, and a removal
request was raised with the hosting provider, because **a force-push reduces discoverability
and revokes nothing** — unreferenced objects stay retrievable by identifier until the host
collects them. ⛔ **Every identifier for this incident is deliberately kept out of this
repository**, since naming them here would be a finding aid for the objects being removed.

## ⛔⛔ `ychrome-vault match <host>` RESOLVES TO AN ITEM WHOSE USERNAME IS CORRUPTED BINARY

**Status:** OPEN. Measured 2026-08-11 while driving a Meta login.

`match` is documented as "resolve a page host to the ONE entry an auto-fill may use (strict
rule)" — so it is the verb an auto-fill trusts. For a host with several stored entries it
returned, with `ok` and no warning:

```
$ ychrome-vault match <host>
{"id":"…","name":"<host>","password":"<40 chars, correct>",
 "username":"!Ñ'P_°S\fÎ k/ÏTæjDGW0Õ…"}
```

The password decrypts correctly; **the username field is binary garbage.** The same corrupted
string is echoed verbatim into `list` output and into the disambiguation error
(`vault: "<host>" matches 6 accounts — name one: !Ñ'P…, <real>, <real>…`), so it is a stored or
decode-layer corruption, not a display bug in one verb.

**Cost.** An agent that trusts `match` (which is exactly what "strict rule, for auto-fill" invites)
types binary into a login field and spends an attempt. On a site that checkpoints or locks after
repeated failures, that is expensive, and the diagnosis points at the site rather than the vault.
It also makes the "name one" error unreadable and un-copy-pasteable for the caller trying to
recover.

⇒ **Two things wanted, and they are separable.** (1) `diagnose` should count and name ciphers whose
*fields* fail to decode, not only whole ciphers — this item decrypts "successfully" today. (2)
`match` should refuse, or at minimum flag, an entry whose username is not valid UTF-8, rather than
returning it as the one entry an auto-fill may use.

⚠ Sitting next to the known `/fill` decoy defect below, these compound: one picks the wrong field,
the other supplies a wrong value, and both report success.

## ⚠ `lore.py scan`'s PRIVATE-DATA GATE FIRES ON URL PATHS, BURYING A REAL LEAK IN FALSE POSITIVES

**Status:** OPEN. Measured 2026-08-11 on a clean checkout, before any local edit.

`scan` reports 5 hits across the committed lore. **Four are false positives and one is real:**

| file | hit | verdict |
|---|---|---|
| `pay.google.com.md` | `/home/activity`, `/home/paymentmethods`, `/home/reauthprompt`, `/home/settings` | ⛔ FALSE. These are Google Pay **URL routes**, not home directories |
| `rtionline.gov.in.md` | `guihost` | ✅ REAL. A fleet hostname in a public repo |

The `/home/<word>` pattern cannot tell a filesystem path from a URL path, and any site with a
`/home/...` route trips it.

**Cost, and it is the specific failure a gate like this dies of.** A scan that is 80% noise trains
the reader to skim it, and the one genuine hostname leak is the row that gets skimmed past. Worse,
it pushes agents toward habitual `--allow-private`, which is precisely the reflex the write-time
gate exists to prevent. The tool is currently arguing against its own purpose.

⇒ Anchor the path pattern so it requires a path start or a `~`/quote boundary rather than matching
mid-URL, or exempt a `/home/` that is preceded by a scheme+host. Then fix the `guihost` leak, which is
a one-word edit nobody has made because it is item five of five.

## ⚠ `ctl input`'s `nth` INDEXES THE HITTABLE POOL, NOT DOM ORDER — and nothing says so

**Status:** OPEN. Measured 2026-08-11 driving a React dialog.

`docs/agent-engine.md` documents `{"type":"click","selector":"css","nth":k}` without saying what
`k` counts, and the natural reading (DOM order, matching `querySelectorAll`) is wrong. `nth` is an
index into the **hittable** subset. With a modal open, `[role=button]` had **4 DOM matches and 3
hittable**, so the control at DOM index 3 was at hittable index 2, and `nth:3` was refused:

```
no element matches "[role=button]" at hittable index 3 — it has 3 hittable match(es) of 4
```

**Cost.** The refusal is honest and recoverable, which is the good case. The bad case is silent: if
the non-hittable elements sit *after* the target rather than before it, `nth` computed from the DOM
resolves to a **different, real, hittable control** and clicks the wrong thing with `ok:true`. That
is unfalsifiable from the response alone.

⇒ Say it in `--help` and in the `/input` contract, and consider echoing `dom_nth` alongside `nth`
in `resolved[]` so a caller can see the two disagree. The `resolved[]` block already reports
`matches` and `hittable` separately, so the information is there and simply is not connected.

⭐ Working mitigation, now in `facebook.com` site-lore as `tag-and-click-nested-react`: tag the
element by exact visible text with a `data-agent-target` attribute in an `eval`, then click that
unique selector. It needs no index arithmetic and survives re-render.

## ⚠ `ctl fill` TAKES AN UNDOCUMENTED `user=`, WITHOUT WHICH A MULTI-ACCOUNT VAULT ENTRY IS UNUSABLE

**Status:** OPEN (documentation). Measured 2026-08-11.

`ctl fill page_id=… entry=…` fails on any vault entry holding more than one account
(`vault: "<name>" matches 6 accounts`), and neither `ctl --help` nor the error names the way out.
It is `user=<account>`, which works and is in no usage string:

```
ychrome ctl fill page_id=$P entry="<entry>" user="<account>"
```

**Cost.** Low per incident, but it lands exactly when an agent is mid-login and reaching for the
plaintext fallback — which is the moment the credential ends up in argv, the failure the ranked
feature ask under the `/fill` decoy item is already about. The error message is the right place to
say it: it already enumerates the candidate accounts, so it should name the flag that selects one.

⚠ Same shape, same file: the engine's `wait` grammar is `until=load`, while the surface plane's is
`--until load:finished`. Passing the surface form to the engine answers
`wait needs an 'until': {load}, {idle_ms}, {selector,state} or {js}`, which is a good refusal, but
the two planes using different grammars for the same concept is a trap worth one line of docs.

## ⛔⛔ `ctl fill` WRITES THE SECRET INTO A HIDDEN DECOY INPUT AND THEN VERIFIES THE DECOY

Measured on a live Indian bank login (`netbank.examplebank-a.example`), which sandwiches the real password box
between two `display:none` `autocomplete="new-password"` honeypots. `/fill` takes the first
`input[type=password]`, writes there, and answers `{"filled":"filled","ok":true}` with its own
length readback passing (`want:14, got:14`) — while the field the form will submit is **empty**.
The 2026-08-07 `unverified` hardening cannot catch it: it reads back *the field it wrote*.

**Cost: a submit in that state spends a bank login attempt with an empty password**, on a bank that
locks after repeated failures, and points the diagnosis at the credential instead of the tool.
Plus **D2** (`type`'s `landed` is a false negative on every masked input) and **D3** (`lore.py`'s
private-data lock caught a phone number and passed a bank customer ID into a public lore file).

⇒ Full report with exact commands, exact responses, suggested fixes and one ranked feature ask
(`/fill` needs a target selector — its absence forces the plaintext into the agent's argv):
**`docs/agent-cobrowse-gaps-2026-08-10.md`** (a peer campaign row, 2026-08-10).

## ⛔⛔ THE ENGINE SERVES FROM A DELETED BINARY FOR DAYS AND 404s VERBS THE CLI ADVERTISES — and nothing says which build is answering

**Status:** OPEN

**Reported by the `practice` campaign row 2026-08-09** as *"`ctl frame` is
advertised by the CLI's own help and the engine 404s it — either implement it or
drop it from the usage line."*

⛔ **DO NOT DROP IT. `ctl frame` IS IMPLEMENTED**, at `src/engine/api.rs:355`,
shipped in `36cc027` *"a cross-origin child answers a selector, and a real click
lands in it"* — which is also the answer to item 6 in the table below. Deleting
it from the usage line would have removed a working feature on the strength of a
404.

**What is actually true**, measured on `dev` the same day:

    $ readlink /proc/2542173/exe
    $REPO/target/release/ychrome (deleted)
    engine started   Sat Aug  8 01:16:32   (43 h earlier)
    frame commit     Sat Aug  8 13:42      (12 h AFTER the engine started)
    installed CLI    Sat Aug  8 14:52

⇒ The help text comes from the **CLI binary**, which has the verb. The 404 comes
from the **daemon**, a 43-hour-old process still serving from an inode that was
replaced on disk. They are not two components disagreeing about what exists —
they are **two builds**, and nothing in the protocol says so.
[[finding-the-gui-daemon-is-not-your-cli-daemon]],
[[finding-identity-by-reference-decays]].

⭐ **The report's cost is the point.** The reporter *"spent a round believing
frame traversal was supported and only my arguments were wrong"*, then filed a
remedy that would have deleted the feature. A 404 that cannot distinguish *"this
verb does not exist"* from *"this build predates it"* sends the reader to the
wrong system every time.

⚠ **AND IT IS NOT ONE ENGINE — IT IS A FLEET.** Reported by the `practice` row
and re-audited independently on `dev` 2026-08-09 (their count and mine differ, so
take the wider one):

    11 ychrome-family processes; 10 running a binary that no longer exists
    $HOME/.local/bin/ychrome (deleted)        x 7   oldest Jul 27 09:27  — 13 DAYS
    $REPO/target/release/ychrome (deleted)    x 1   Aug  8 01:16         — the 404 above
    $REPO/target/debug/ychrome-vault (deleted) x 1  Aug  2
    /usr/local/bin/ychrome-vault                    the ONLY live inode
    588 MB resident across the deleted-exe set

⚠ **Measure start time with `ps -o lstart=`, never `stat -c %y /proc/<pid>`.**
The directory's mtime tracks kernel bookkeeping, not exec, and it fails in the
most misleading direction available: for four of these it returned the SAME
value to the second (`2026-07-30 11:15:31`) for processes that actually started
Jul 26 13:53, Jul 27 09:27, Jul 27 20:17 and Jul 28 00:04 — while agreeing with
`ps` for every process started after that date. So it is correct on recent
processes and silently collapses old ones onto one timestamp, which is exactly
the population you point it at when hunting stale daemons. An obviously-broken
instrument costs one round; one that returns plausible, self-consistent,
CLUSTERED values costs a wrong conclusion. Verified on both rows independently.

⇒ **Any of these can answer a `ctl` call from a build nobody can name**, and the
oldest predates the installed CLI by a wide margin, so `frame` is unlikely to be
the only verb that 404s. A fix that restarts "the engine" fixes one of eleven.

⛔ **What this is NOT, checked because the obvious next thought is wrong.** This
is not the 2026-07-20 zombie fork-bomb re-arming
([[finding-ychrome-zombie-fork-bomb-dev]], whose SIGCHLD reaper is still owed).
Measured at the same moment: **0 zombies on the whole host**, 0 children on every
one of the long-lived instances, 398 processes against a `pid_max` of 4,194,304.
These are idle, not breeding. The cost today is memory and a wrong answer, not a
host outage — say so plainly rather than borrowing the older incident's urgency.

**The fix, and it is two things:**

1. **A version handshake.** Every `ctl` reply carries the engine's build
   identity, and an unknown verb whose CLI is NEWER than the engine says so:
   `unknown engine verb "frame" — this engine is build X (started <t>), your CLI
   is build Y; restart the engine`. ⛔ Not `--version`: that reads the binary on
   disk, which is exactly the file this engine is no longer running.
2. **Stale-binary self-retire**, the same shape yggterm's daemon already has:
   poll `/proc/self/exe` for the `(deleted)` marker and retire once no page is
   mid-navigation. ⚠ It must PRESERVE live pages or it is worse than the bug —
   this engine was holding two live exam sessions when it was measured.
   ⚠ And it must **sweep a set, not a singleton**: eleven instances, started on
   at least five different days between Jul 26 and Aug 8. A retire that assumes
   "the engine" leaves ten behind.
   ⛔ **CORRECTION to this entry's own first version:** it said several of them
   "share a start second and so look like one supervised group". They do not —
   that came from `stat -c %y /proc/<pid>`, which reads the DIRECTORY's mtime and
   collapsed four processes started across Jul 26–27 onto one fake timestamp
   (`2026-07-30 11:15:31`, identical to the second). There is no supervised
   group; they are eleven independent strays. Use `ps -o lstart=` — see the
   ⚠ note under the audit above.

**Falsifier:** rebuild ychrome without restarting the engine, then run any verb.
The reply names both builds and tells you which one is stale.

## ⛔⛔ `ctl eval` IS SYNCHRONOUS-ONLY, SO EVERY rAF-DRIVEN CANVAS SILENTLY RECORDS A DEGENERATE RESULT AND REPORTS SUCCESS

**Status:** OPEN

**Reported by the `practice` campaign row 2026-08-09**, driving GMAC's exam
whiteboard (literallycanvas, inside an iframe). ⛔ **Filed separately from the
`ctl input` entry below on the reporter's explicit instruction — it is a
different failure**, and merging them would hide it.

**What happens.** A full drag dispatched inside ONE `ctl eval` — mousedown, 3×
mousemove, mouseup, correct `buttons`, the frame's own `MouseEvent` constructor
and `view` — returns success, and the board's shape count goes up by one. The
shape is `x1=y1=x2=y2`: a zero-length line.

**Cause, and it is general.** `lc.pointerMove()` defers through
`requestAnimationFrame`. A synchronous event sequence never yields to a frame,
so every queued move runs **after** the mouseup that already committed the
shape. This is not literallycanvas-specific: it is every canvas or drawing
library that batches on rAF, and most modern drag implementations.

⭐ **Why this outranks an ergonomics gap: the failure is silent AND positively
misleading.** Nothing errors, the count increments, and only reading the shape's
own coordinates back reveals it. The reporter found a pre-existing degenerate
`Line(100,100,100,100)` on that same board left by an EARLIER agent that hit
this and evidently believed its drag had worked. ⇒ this defect has already
written a false result into a real workspace and been believed at least once.

**Either fix is accepted; the reporter would use (2) immediately:**
1. `ctl eval` supports `await` / a returned Promise, so a script can
   `await new Promise(r => requestAnimationFrame(r))` between events; or
2. **a `ctl input drag` verb taking a point list**, dispatching
   mousedown/move…/mouseup with a real frame boundary between each — which also
   subsumes item 1 of the entry below.

**Working around it costs 5 round-trips instead of 1:** one event per `ctl eval`
call, because each round-trip happens to cross several frames.

**Falsifier:** a drag driven in one call across an rAF-batched canvas produces a
shape whose end coordinates differ from its start.

## ⛔ A BATCHED `ctl input` LOSES AND REORDERS EVENTS, AND REPORTS SUCCESS

**Reported by the `practice` campaign row 2026-08-13**, driving a vendor exam
console's on-screen calculator. **Same family as the rAF drag entry above, but a
different verb and a different surface** — this one needs no canvas and no
drawing library.

**What happens.** Thirteen key-clicks dispatched in ONE `ctl input` batch **lost
three and reordered the rest**. The call reports success. Paced one per call at
roughly 350 ms, the identical sequence is exact.

⭐ **Why it outranks a throughput complaint: the output is plausible.** A
calculator fed a mangled key sequence returns *a number* — not an error, not a
blank, a wrong answer that looks like a right one. Anything reading that result
downstream inherits a fabricated measurement with nothing marking it.

**Falsifier:** dispatch a known key sequence in one batch and read the field back
character by character; a batch that is correct at n=13 has not been tested, so
increase the count until it diverges.

**Workaround, and it is expensive:** one event per call, ~350 ms apart — 13
round-trips instead of 1.

## ⭐ `ctl console` IS THE CAUSE-NAMING VERB AND IT READS LIKE A CAPTURE UTILITY

**Reported 2026-08-13 by a row that wrote three wrong causal stories with this
command one call away, across a long session of diagnosing broken pages, and
never reached for it once.** ⛔ Filed as a **discoverability** defect, not a
documentation nicety: every other `ctl` verb answers *what is the state*;
`console` is the only one that answers *what went wrong*. Sitting in a verb list
that reads as capture/inspection, it is passed over exactly when it is most
needed — while an agent is forming a causal story from indirect evidence.

⇒ **Fix: name it as the diagnostic entry point in `ctl` help and in the agent
docs** — "start here when a page misbehaves" — rather than listing it beside the
capture verbs. **Falsifier:** ask an agent unfamiliar with `ctl` how it would
find out why a page is failing, and see whether it names `console` unprompted.

## ⭐ THE `ctl` SURFACE MAKES AGENTS HAND-ASSEMBLE CHORES FROM PRIMITIVES — seven deficiencies, measured while driving live exam consoles

**Status:** OPEN

**Reported by the `practice` campaign row 2026-08-09**, from three successive
sessions driving the PREPEXAM/CFA exam consoles. Filed here because ychrome is owned
by the yggterm campaign row; the reporter touched no repo of ours.

⭐ **The test that selected these is the fleet's own dream test — *did an agent
hand-assemble this chore from primitives and get it wrong?*** Every item below is
something three different rows rebuilt by hand. That is the argument for making
each one a verb: **an agent's discipline resets every session, a verb's does
not.** ⛔ So the fix for the starred items is NOT better documentation.

| # | deficiency | the verb it wants |
|---|---|---|
| 1 ⭐⭐ | **`ctl input` has no `mousedown`/`mouseup`, so EVERY drag is hand-assembled.** Types are `click\|move\|scroll\|type\|key`. A jQuery-UI drag needs `mousedown` on the handle, then `mousemove`/`mouseup` **dispatched on `document`** (not the handle), `buttons:1` on all but the mouseup. Any one detail wrong ⇒ the drag silently does nothing. | `ctl input --type drag --from X,Y --to X,Y`, plus a `--selector` form. **Highest value item here.** |
| 2 ⚠ | ⛔ **RETRACTED AS WRITTEN — `dispatched: 3` IS ONE LOGICAL CLICK** (`mousedown` / `mouseup` / `click`), not three clicks, and the old wording invited the wrong diagnosis in at least two sessions. **Disproof, driven repeatedly on a real keypad: a click on a calculator's `7` reads `7.`; three clicks would read `777.`** The real defect this row was born from is recorded in `src/engine/hit.rs`'s own header — a selector resolving to a **hidden duplicate** answered `{"dispatched":3,"ok":true}` while the true control's handler never fired, which cost a reporting agent three wrong conclusions in one session. **The count was never the fault; a dispatch onto the wrong element reporting success was.** | nothing to coalesce. **Say `press/release/click` in the field, or drop the number** — a bare `3` reads as a multiplicity hazard and sends the caller after a confound that does not exist |
| 2b ⭐ | **BATCHED `ctl input` EVENTS ARE DROPPED AND REORDERED, AND THE RESULT IS A BELIEVABLE NUMBER.** 13 key-clicks in one batch lost three and reordered the rest; **paced one-per-call (~350 ms) the identical sequence is exact.** ⇒ For any target that consumes input on a timer, a batch is **not** equivalent to the same events paced. ⛔ The failure surfaces as plausible wrong data rather than an error, so a caller has no reason to suspect it. | pace internally, or refuse a batch above N; **at minimum document it** — this one is worth a line even if no code changes |
| 2c ⛔⛔ | **`window.open` IS DECLINED SILENTLY, AND IT COSTS A WHOLE DRIVEN SURFACE.** A commercial exam-delivery client launches itself with `testWindow = window.open(…)` and branches on `if (testWindow === null)`. In the headless view the call yields **no page, no error, and nothing that trips that branch** — so from inside the page the launch looks successful and from outside the click looks missed. ⚠ A probe reading `typeof testWindow === "object"` cannot tell the states apart, because that is also what `null` answers. ⛔⛔ **THE RESTART-LOOP EVIDENCE IS WITHDRAWN BY ITS OWN REPORTER (2026-08-13).** This entry previously read that opening the launch URL in a plain tab put the client into a *restart loop — six launch tokens in 37 s* — *because* it checks for the window it believes it opened. **That causal link was never tested**: the reporter read the launcher's source, saw the popup checks, observed a loop, and joined the two. The console shows a different error repeating every cycle (`TypeError: vFrame.$ is not a function`), **and that is not offered as the cause either** — the experiment that would establish it never ran, because the client re-navigates the top window and wipes the instrumentation. ⇒ **The loop's cause is openly unestablished, and this entry must not be sized on it.** ⭐ **What survives is the whole of the defect and it needs no loop:** a call that yields no page, no error, and nothing that trips the site's own `=== null` branch. | make `window.open` produce a real page with the opener relationship and `window.name` intact — **or, at minimum, return `null`** so the site's own popup-blocker branch fires. ⭐ **A silent decline costs the caller a wrong diagnosis; a loud one costs them a retry** |
| 3 ⛔ | **`ctl eval` shares ONE global scope across calls.** A second call declaring `const h` dies with a duplicate-variable `SyntaxError` **that reads like a page defect** — so the agent starts debugging the wrong system. | auto-wrap each eval body in an IIFE, or run it in a fresh realm |
| 4 | **`ctl eval` takes `page_id=` while sibling verbs take `page=`.** | accept both, or normalise |
| 5 ⚠ | **`ctl wait`'s error names a shape that does not work, which costs MORE rounds than naming nothing.** The true form is `until='{"load":"finished"}'` — `until` takes an OBJECT (verified live 2026-08-09: `met:true, elapsed_ms:0`). The engine's error prints ``wait needs an `until`: {load}, {idle_ms}, {selector,state} or {js}``, whose `{load}` is JSON-shape notation and reads as the literal token `load`; `until=load` then 400s. This row's own earlier text (`load=finished`) was a third abbreviation of the same JSON. **Three sources disagreed because two were abbreviating a JSON shape in prose.** | print a form that can be pasted: ``until='{"load":"finished"}'`` |
| 6 ⛔ | **Coordinate clicks do not reach inside iframes.** Top-level footer buttons fire correctly; nothing reaches into `ElementDisplayFrame` or `wbFrame`. The only working route is the frame's own event constructors plus an explicit `view`. | `ctl input --frame <selector\|url-substring>` |
| 8 | **`ctl eval` cannot take its JS from a file.** Args are strictly `key=value`, so `--js-file` is rejected outright (`arguments are key=value pairs`), and the only route is the whole program through argv — multi-KB of newline-stripped JavaScript on the command line for any real `getBoundingClientRect`/`getComputedStyle` sweep. Reported by the `practice` row 2026-08-09; confirmed by grep — no `js_file`, no `js=@`, anywhere in `src/`. | `js_file=PATH` (stays inside the key=value grammar), or `js=@PATH` |
| 7 | **Engine-opened windows are never surfaced as pages.** A `window.open`-style control opens a window the engine never lists; the working recipe is to read the anchor's `url` attribute and `goto` it by hand. | surface engine-opened windows as pages, or `ctl click --follow-target` |

⚠ **Items 3 and 5 are the same shape as this repo's own instrument findings: the
error names what the driver HELD, not what was WRONG.** A `SyntaxError` from a
shared eval realm is indistinguishable, at the agent's eye, from a broken page —
which is the most expensive kind of wrong answer an instrument can give.

**Falsifier, per row:** the chore is one `ctl` invocation with no `eval` in it.

## ⛔ OWNER-REPORTED: AN OTP LOGIN VERIFIES INTO NOTHING — bakingo.com, works in Firefox and Chromium

**Status:** OPEN

⭐ **Owner-reported 2026-08-08:** *"Our browser might be missing some std. web
feature: consider this site https://www.bakingo.com/ I logined it using
mobile/otp but after clicking verify on correct otp nothing happens in ychrome.
I tried in firefox/chromium based browsers the same flow logged in the user or
[asked] for more details in my case of registering."*

**Why this one is filed high.** It is a SILENT dead end on a checkout flow, with
a working control in two other engines, on a correct OTP. Whatever is missing
does not throw where the user can see it — the button simply does nothing — so
every site that uses the same capability fails the same invisible way and we
would never hear about most of them. The value at risk is not one bakery: it is
"can this browser be the one he actually uses".

⚠ **NOT ROOT-CAUSED. Do not guess a cause from the engine's reputation.** The
repo already carries three separate findings that WebKitGTK lacks things Chromium
has, and the temptation is to write "WebKitGTK gap" on this and close it. That
would be a story, not a measurement — the same failure could be our own missing
handler, which is exactly what the dead camera turned out to be
(`[[finding-webkitgtk-no-webusb-serial-hid]]`: *"a dead camera was OUR missing
`permission-request` handler"*). ⛔ The engine is the LAST hypothesis, not the
first.

**The measurement that settles it, in order — the console is the instrument:**
1. Open the JS console on the failing page and read it at the moment of the
   click. An uncaught `TypeError: x is not a function` names the missing API
   outright and ends the investigation in one step.
2. If the console is silent, the click may not be reaching the handler at all
   (an overlay, a form submit we cancel, a popup we never opened). ⚠ Check
   whether the verify step opens a window — we have no `close` signal for
   `window.close()` (`[[finding-webkitgtk-popup-close-and-related-view-ucm]]`).
3. Watch the network for the verify POST. **A request that never leaves is a
   different bug from one that returns 200 and is ignored**, and only this step
   separates them.
4. ⚠ **The eval bridge itself can be dead on every page**
   (`[[bug-class-web-eval-bridge-dead-all-pages]]`). Prove the console answers
   at all before reading a silence as evidence of anything.

⛔ **Do NOT drive the owner's real OTP flow to reproduce.** It costs a live SMS
to his number and one attempt per code. Reproduce against the site's own
pre-verify state first, and if a real OTP is genuinely required, that is an owner
gate — park it rather than burning his codes.

**Falsifier:** the same flow in ychrome with the console open. A named JS error
identifies the gap; a clean console with no outbound verify request means the
click never reached the handler and the engine is not implicated.

## ⛔ [11.2] MEDIA QUALITY (owner item 5): DIAGNOSED — the counters are dead and the engine crashes

**Status:** OPEN, but no longer undiagnosed. Worked 2026-08-21 from outside the engine.

The two owner reports and what the measurements say about each:

1. **Front-end video starts high and falls back to prehistoric quality.** Consistent with a
   player that measures frames itself and reacts to a real shortfall. ⚠ **Not confirmed** —
   against a front-end instance or on an idle host.
2. **YouTube shows frame overlaps while nerdview reports no dropped frames.** ⇒ **Partly
   explained.** `droppedVideoFrames` under-reports by roughly thirty to one, so nerdview
   reporting nothing is consistent with a large shortfall and is not evidence of health.
   ⛔ It does **not** follow that the counter is dead — it moves; see the corrected entry
   *THE FRAME-HEALTH COUNTERS UNDER-REPORT…* above, including the load confound that makes
   the absolute frame rates unusable.

⇒ Both entries above carry the measurements. This entry stays open for the part not done:
**the same measurements on the GUI substrate**, which is the one the owner watches through
and the only place the frame shortfall may differ (the headless substrate has no GPU at all).

⛔⛔ **THE TRAP HELD — but it also caught me, in the other direction.** *An instrument running
on the thing it measures reads zero* is what sent me looking, and it found a real defect:
`requestVideoFrameCallback` never fires, and `droppedVideoFrames` under-reports ~30:1. ⚠ **It
also made an overstatement feel like a confirmation.** The first version of these entries said
every instrument was dead and the engine could not reach 60 fps. Neither was true, and the
check that would have caught both — repeat the measurement, and look at the host's load — is
the cheap one I skipped because the result already agreed with the trap I had been handed.
⇒ **A trap you are told to expect is itself a prior.** Measure it as sceptically as anything
else, and repeat a measurement before it becomes a queue entry.

⚠⚠ **CORRECTION TO THIS ENTRY AS PREVIOUSLY FILED.** It said: *"Wire `ytrace` (the probe bus
already in the workspace, pinned in the top-level `Cargo.toml`)"*. **That is false and cost a
detour.** There is no `ytrace` dependency in the workspace manifest and no reference to it
anywhere in the tree. `ytrace` is a **fleet CLI** that queries a probe bus, and the bus
carries no ychrome producer — only the terminal's. It is not something ychrome can be
"wired" to by adding a crate.

⭐ **What actually works as an outside instrument, and needs no new code:** sample
`/proc/<pid>/stat` and `/proc/<pid>/status` for the WebKit processes on a timer from a
separate script. It keeps reporting after the engine dies, which is the whole property that
was wanted, and it is what caught the segfault and the RSS ramp.

## ⚠ [11.2] EXTENSIONS (owner item 4): MODALS ARE A HOST CHANGE; THE ADBLOCK FAILURE WAS A DEPLOY GAP

**Status:** PARTLY CLOSED 2026-08-21. Both halves measured; one is fixed and live-proven.

### ✅ SPONSORBLOCK CUSTOM SITE ACCESS — delivered (unchanged), but see the deploy trap below
`sponsorblock::sites()` / `add_site` / `remove_site`, a settings control, and
`match_patterns()` as the ONE owner of where SponsorBlock runs. ⛔ No host ships, anywhere.

### ✅ THE YOUTUBE ADBLOCK FAILURE — ROOT-CAUSED, AND IT WAS NEVER `extensions.rs`
⭐ **Two previous rounds edited the code because the code is what a code-reading session can
see. The assets could not reach the machine.** Measured state before any change:

| asset | verdict | on disk |
|---|---|---|
| `rules.json` (the network ruleset) | `forked:1.20260731` | **3 weeks stale** |
| `cosmetic-filters.js` | `forked:1.20260731` | **3 weeks stale** |
| `scriptlets.js` | not listed at all | **absent** |
| `sponsorblock.js` | `forked:2.0.0` | **pre-`5aa909f`** |

⇒ The ad blocker was running a three-week-old ruleset with its scriptlet companion missing
entirely. **The fix was to deploy, not to edit.** After installing a current binary and
clearing the stuck copies, all six assets read `current` and the plane works:

- `static.doubleclick.net/instream/ad_status.js` → **`failed to load`** (blocked by the ruleset)
- no ad DOM on the watch page: `.ad-showing` / `.video-ads` / `.ytp-ad-overlay-container` all absent
- SponsorBlock bound and answering: `data-ysb` reports `bound:true`, a real segment returned
- screenshot of the player, clean, no pre-roll

⛔ **AND THE TRAP THAT CAUSED IT CAUGHT THIS REPO'S OWN WORK.** `sponsorblock.js` was
`forked:2.0.0` — same `@version`, 1,886 bytes different, because commit `5aa909f` changed the
asset without changing its version. **The custom-site feature filed above as "delivered" could
not reach any host**, and provisioning reported that as the user's own edit. See the
`@version` entry at the top of this file: it is not a hypothetical, it has now bitten twice.

⚠ **An opt-in extension is doubly stuck**: provisioning only refreshes extensions that are
already installed, so a `forked` opt-in asset cannot be repaired by provisioning at all — the
only routes are the pane's install action or removing the file.

⛔ **STILL OPEN — `cosmetic-filters.js` REPORTS ITS STATE INTO A WORLD NOTHING CAN READ.**

⚠ **Corrected the same session it was written.** This entry first claimed that
`youtube-adblock.js` and `cosmetic-filters.js` "publish nothing at all". **That is false**,
and testing it took one `ctl eval`. What is actually true, measured on a live YouTube page:

| script | `@world` | what it publishes | readable by `ctl eval`? |
|---|---|---|---|
| `youtube-adblock.js` | `main` | `__yga_state` — pruned/skipped/forwarded + per-hook counts | ✅ **yes** |
| `scriptlets.js` | `main` | `__yggScriptlets` | ✅ **yes** |
| `sponsorblock.js` | `isolated` | `data-ysb` **on the DOM** | ✅ **yes** |
| `cosmetic-filters.js` | `isolated` | `__yggCosmetic` / `__yggCosmeticState` | ⛔ **NO — always `undefined`** |

⇒ Three of the four are already diagnosable, and `youtube-adblock.js` — the one this mandate
is about — carries the richest state of any of them and has all along.

⛔ **The one real gap is `cosmetic-filters.js`, and its failure mode is worse than silence.**
It runs in the ISOLATED world and reports itself through `window` globals, which a page-world
`eval` can never see. So the variables exist, they are the obvious thing to reach for, and they
read `undefined` on a perfectly healthy script — **which is exactly the misread `docs/adblock.md`
already warns about for `window.__ysb`, and the reason SponsorBlock publishes to the DOM
instead.** The lesson was learned once and never applied to the script beside it.

⇒ **Fix:** the generated cosmetic script should set a `data-*` attribute on
`document.documentElement`, in the same shape as `data-ysb`. It is generated, so the change
belongs in the generator, not in the asset. ⚠ And document the four names together — an agent
currently has to read four scripts to learn four different conventions.

### ⛔ STILL OPEN — PER-EXTENSION MODALS ARE NOT A YCHROME JOB FIRST
**Measured, as the mandate asked, before designing anything: a contributed pane CANNOT raise
a modal.** The surface protocol is explicit that this is the dividing line:

> an app "informs and cannot ask; anything needing an answer is a modal the shell owns
> (the `fido2` dialog is the worked example of that split)"

Every modal that exists — the FIDO2 presence dialog, the media-capture prompt — is a native
shell construct, added by changing the shell. There is no modal widget kind in the contract.

⇒ **This is a Tier C change: one new declarative widget kind, and the vocabulary is the
shell's, not ychrome's.** Admission needs both of the spec's rules to hold:

1. **at least two apps want it** — one app's need is a feature request, two is a vocabulary gap;
2. **it is declarative** — data in, events out, never an imperative drawing API.

⚠ A modal kind plausibly clears rule 2 and **has not been shown to clear rule 1** — SponsorBlock
is one app wanting it. ⇒ The honest next step is to find the second caller or to accept that the
settings pane stays the place, **not** to open a native surface for it: jumping A→B to get one
widget "serves one app and charges every app the native-surface tax forever".

⇒ **Routing:** this belongs to the shell's queue, not this one. It is recorded here because
this is where the mandate landed, and it should be raised with the shell's owner rather than
built here.

## ⭐ OWNER-REPORTED: TABS CANNOT BE SHIFT-SELECTED, SO THEY MUST BE FILED ONE AT A TIME

**Status:** OPEN

⭐ **Owner-reported 2026-08-08:** *"I cannot shift select multiple entries in
ychrome to drag them into a folder."*

The sidebar tree's whole promise is the one in its own onboarding copy — *"Tabs
move out of the page into a sidebar tree with folders you can [organise]"*
(`src/sidebar.rs`). Filing a session's worth of tabs one drag at a time is the
work that promise exists to remove, so this is a gap in the feature's reason for
existing, not a nicety.

⚠ **NOT ROOT-CAUSED — do not treat the following as the diagnosis.** A scout for
the usual spellings (`selected_ids`, `selection`, `shift_key`, `multi_select`)
finds nothing in `src/sidebar.rs`, and the only `drag` hits are unrelated. That
is consistent with selection being SINGLE-VALUED by construction (one active
row) rather than with a shift handler that is present but broken — but it is a
grep, not a measurement, and the next agent owes a real one before writing code.

**Two things to settle before any patch, because they are spec calls:**
1. **What is a selection?** Today a row is "selected" to mean ACTIVE (the tab
   being shown). A multi-selection is a second, different thing, and collapsing
   them is how one concept ends up with two owners. Decide whether the tree
   grows a selection SET beside the active row, or whether active becomes a
   member of it.
2. **What does a drag carry?** Every drop target currently receives one row.
   Widening the payload to N rows touches the drop handlers, not just the
   selection, and a half-applied change would move some of a selection and
   silently leave the rest.

**Falsifier:** shift-click two tabs in the sidebar tree and read back whatever
the sidebar exposes as its selection. One id means this entry stands.

## ⚠ VAULT PANE vs THE BITWARDEN EXTENSION — the parity gap, itemised

**Status:** OPEN. Owner directive 2026-08-08: *"Make our vault GUI and implementation on par
with Bitwarden's extension."* Tonight closed the defects that broke FUNCTION or that he could
see; this entry is the honest remainder, so nobody reads the green ticks as parity.

**Closed 2026-08-08** (do not re-open): passkeys were invisible to every ceremony
(credential-id spelling); `icon:copy`/`icon:eye`/`icon:dice` printed as literal text over the
value; row icons too small and un-grounded; a stored passkey shown nowhere on the edit form or
in the list.

**Still missing, roughly in the order a user meets them:**

1. **No passkey autofill, and no picker.** THE big one. A passkey is only usable when the SITE
   starts a ceremony; the Fill tab lists logins only, so there is no "sign in with this
   passkey" affordance and no way to CHOOSE among several for one site. Bitwarden offers it
   from the item. ⛔ Blocked behind the presence-request bug above — a picker is pointless
   while no grant can be delivered.
2. **No passkey removal.** An item's passkey can be read but not deleted from the pane.
3. **A created passkey confirms nothing.** `fido2-create` mints and stores correctly, but the
   sidebar says nothing afterwards, so the user trusts a silent success — the same class the
   owner's own razor names: *a status field without a readback is decoration*.
4. **`clear_notes` / `clear_totp` are standing toggles**, not per-field actions. They read as
   two duplicated delete switches while scrolling and were reported as such. Bitwarden removes
   a value where the value lives.
5. **Cards and identities have no real editor.** `item_type` 3 and 4 render, and a card fills,
   but neither can be edited — and 130 of ~1125 items in this vault are not logins. ⚠ CREATE is
   no longer part of this gap: the Add tab makes logins, secure notes and cards as of
   2026-08-08 (`add_tab_widgets`). The EDIT form is still login-shaped.
7. **An IDENTITY (`item_type` 4) can be neither created nor read.** The create path refuses it
   deliberately — nothing in this client decrypts an identity, so an item written from a form
   would store fields the user could never see again, and the save would report success. The
   two halves must land together: a reader (`Vault::identity`, the twin of `Vault::card`) and
   then the form. Until then the pane offers Login / Note / Card and says so by offering
   nothing else.
6. **The GUI's modal state is unobservable.** `server app state` exposes no `pending_fido2`, so
   neither an agent nor a test can tell whether a ceremony is actually in front of the user or
   was dropped. That is what made the presence-request bug take a full session to corner.

⚠ **Sequencing:** items 1–3 all sit behind the presence-request fix. Do that first or the work
cannot be verified end to end — which is exactly the trap this session fell into.

## ⛔⛔ A PASSKEY CAN NEVER BE APPROVED: the presence request is written to the DAEMON'S `/dev/null` stdout

**Status:** OPEN — this makes `navigator.credentials.get()` unusable on every daemon-served
surface, which is all of them.

Measured on guihost 2026-08-08, driving a real Google sign-in end to end.

`passkey::emit_fido2_request` publishes the ceremony as an OSC on **stdout**:

```rust
let mut stdout = std::io::stdout().lock();
let _ = write!(stdout, "\u{1b}]7717;fido2;request;{encoded}\u{7}");
```

That is correct for a ychrome launched **inside a yggterm row**, where stdout IS the row's PTY
and yggterm parses the sequence into `PendingFido2Dialog`. It is wrong for the architecture we
actually run:

```
ychrome --daemon (pid 1191332)   /proc/PID/fd/1 -> /dev/null      <-- serves the surfaces
foreground ychrome (pid 1145528) /proc/PID/fd/1 -> /dev/pts/12
```

**The daemon serves the pages, and the daemon's stdout is `/dev/null`.** So the request is
written into nothing, no modal is ever raised, and the ceremony parks on the `Signer` condvar
for the full `CEREMONY_TIMEOUT` (120 s) waiting for a `/fido2/grant` that nobody was ever asked
to send. The page then shows Google's "Something went wrong".

**Three independent confirmations that nothing downstream is at fault:**

1. The shim IS installed on the page (`navigator.credentials.get` stringifies to our JS).
2. The vault RESOLVES the passkey — after the 2026-08-08 credential-id fix the call returns no
   error at all, where it previously answered `no passkey in this vault answers that request`.
3. `~/.yggterm/vault/audit.log` contains **zero** `fido2` lines, ever. `fido2-assert` is only
   reached after a grant, so its absence proves the grant never arrives — the failure is upstream
   of the vault, not in it.

**What the fix has to do:** route the presence request over a channel that survives the daemon,
the same way everything else the GUI needs already does — the per-session control endpoint the
surface already declares (the `sidebar` declaration `/fido2/grant` is POSTed back to). The OSC
must be emitted on the OWNING SESSION'S stream, not on whatever stdout the emitting process
happens to hold. `emit_fido2_request` already takes a `session` argument and currently uses it
for diagnostics only ("the GUI routes the OSC by the STREAM it arrived on, not this field") —
under a daemon that comment is precisely the bug.

**Do not confuse this with daemon orphaning** (that one is fixed — `daemon list`/`daemon reap`,
`docs/host-daemon.md` §6.2). A `ychrome daemon restart` ALSO leaves live
surfaces pointing at the retired daemon's control port (observed: `connect 127.0.0.1:41459:
Connection refused`, GUI toast "Web policy unavailable … Its surfaces open unprotected", which
additionally strips the userscripts and therefore the shim). That is a second, separate defect
on the same path; fixing either one alone still leaves passkeys broken.

## ★ THE VAULT PANE STILL WAITS FOR YOU TO PRESS SYNC

**Status:** OPEN — only (c), the sync SCHEDULE, survives. Everything else in
this entry shipped and is live-proven on the GUI host.

✅ **2026-08-04 — the DEPLOY half is closed.** Both binaries are installed on the
GUI host, the vault agent was handed over (unlock preserved, 1122 items, fresh
`last_sync_unix`) and the ychrome daemon restarted, so the pane the user is
looking at is the current one. The row's `✎` is gone: a row OPENS the entry now,
into a Bitwarden-shaped View Login (masked secrets throughout, password history
expanded, `Edit` leading an action bar with archive and delete). Archive was
exercised against the live server and reversed, leaving the vault as found.

✅ **2026-08-04 — and the reason it could not be exercised before is fixed too.**
A contributed pane could be OPENED from the control plane and not PRESSED; the
new `server app pane <pane-id> <action> [value]` verb (yggterm 3.0.24) routes
through the same owner a click uses. Without it this pane's affordances are
pointer-only, on a desktop where absolute pointer injection does not map to
screen pixels.

User-tested the sidebar against the real Bitwarden extension side by side,
2026-08-02, with screenshots. Three gaps; two are closed, one remains.

### 1. Edit could not SHOW a stored value — ✅ CLOSED 2026-08-02, twice, live-proven

The user's words: *"I cannot edit the existing fields or see them to manually
copy paste."* The pane was a REPLACE form, not an EDIT form, so you could not
check what was stored or copy it by hand — the fallback every other client gives
you when autofill misses.

**The first fix** put a separate "Stored values" section under the form: one row
per value the entry HOLDS, each with a `👁` and a `⧉`.

**The user reported again the same evening**, side by side with Bitwarden:
*"I cannot see passwords in edit mode and our vault UX looks so lifeless with
dullness everywhere."* A list of rows beneath a column of blank boxes is not what
edit mode looks like in any client — the mask, the eye and the copy belong ON the
field.

**What landed (final).** The form is Bitwarden's Edit Login: **Item details ·
Login credentials · Autofill options · Additional options**, each a card. The
password box shows mask dots at rest with `👁`, `⧉` and a `⟳` (arms a roll for
the next save; it replaced the "Roll a new password instead" toggle, so the flag
has one owner) inside its trailing edge. Save is pinned in the pane footer,
wearing the accent. Custom fields stay rows — their values have no in-place write
path, and a typeable box that does nothing on save would lie about the
affordance.

⛔ The empty-box rule was a real invariant, not laziness, and it is intact — and
now held by CONSTRUCTION rather than by care. The mask dots are a **placeholder**
(`stored: true` on the widget), so they cannot be submitted or read back; and
yggterm keeps a `stored` field's declared value OUT of the form draft, so a
revealed password cannot be re-sent on the next action. A revealed value is still
a **parameter**, not state — `PaneState` cannot hold one, `GET /pane/vault`
builds through the no-reveal owner, and the next render is built without it.
**A secret is never in a schema AT REST or in a LISTING.** Locked by
`the_schema_route_cannot_carry_a_revealed_value`,
`a_revealed_value_lives_in_exactly_one_render`,
`the_edit_form_declares_every_secret_box_empty` and
`the_edit_form_puts_the_mask_the_eye_and_the_copy_on_the_field`.

⚠ **Needs the yggterm side.** The mask, the inline verbs, the cards and the
pinned footer are yggterm schema fields (`stored`, `actions`, `card`, footer
`primary`) shipped on `lane/dev/youthful-inputs`. On an older GUI the schema
degrades to plain boxes — the form still works, it just draws flat.

### 2. No copy actions on a row — ✅ CLOSED 2026-08-02, live-proven

Bitwarden's row overflow offers **Copy username · Copy password · Copy
verification code**. Ours offered fill ⧉, totp ⏱ and edit ✎ — every affordance
"put it in the page", none "give it to me".

**What landed.** Those three are in the row's right-click `menu` (yggterm's own
row vocabulary, so a menu item can say what it does in words where a fourth glyph
in a 300px rail could not), offered only for what the listing says exists. The
value is handed to the GUI as an `eval` calling `navigator.clipboard.writeText`
— the clipboard belongs to the GUI's host, so it takes the injector road a fill
takes. There is no OSC 52 spelling: that would put the secret in the scrollback
ring.

✅ **THE CLIPBOARD RUNG IS MEASURED, NOT ASSUMED.** The open question was whether
WebKitGTK grants `writeText` to a GUI-injected eval at all (user gesture, focus).
It does: on a live surface at `https://example.com/`, "Copy password" from a
row's menu left the page saying *"Copied the password to the clipboard."* and
`navigator.clipboard.readText()` read back **20 characters** — the scratch item's
generated password, by length, never printed. Same for the notes (**18**).

⚠ **One honest limit remains.** `navigator.clipboard` exists only in a **secure
context**, so on a plain http page there is nothing to write with; the page
notice names that and points at the eye. That branch has not been exercised on a
real http page. A hidden-textarea `execCommand('copy')` fallback was refused on
purpose: it puts the secret in the page's own DOM, where the page can read it,
and a user copying a password on some unrelated site did not consent to that.

⛔ A card still has neither verb. Number and CVV reach a page only through
`card-fill` — a PAN in a transcript is durable and cannot be rotated (settled
2026-07-26).

### 3. The sync line is a warning where a fact belongs — and the cause is ours

The pane shows: *"This host's vault agent does not report when it last synced,
so nothing here can tell you whether it is current. Install the current
ychrome-vault and hand the agent over."* The user: *"our sync now option should
have last synced time and then sync now button … A warning text is
non-confidence inspiring."*

**Root cause, measured 2026-08-02 — and the advice in that warning does not
work.** `status_json` (the ONE status builder, `agent.rs:1772`) always sets
`last_sync_unix`. The RUNNING agent's status has no such key at all — proven by
asking `~/.yggterm/vault/agent.sock` directly, not the CLI. So the agent
predates the field. But:

- `agent_stale` reads **false**, because it compares the agent against the
  INSTALLED binary and cannot see a missing FIELD;
- ⛔ **`ychrome-vault handover` REFUSES**: *"is the binary this agent is ALREADY
  running — nothing to hand over"*. It compares the PATH, and an in-place binary
  replace leaves the path identical while the code differs. **So the one
  documented zero-cost remedy is unavailable exactly when it is needed**, and
  the pane's own advice sends the user in a circle.

**Fixes owed:** ~~(a) `handover` must compare the installed binary's IDENTITY~~
✅ **(a) DONE 2026-08-02** (`ce0a7ec`): `exe_stamp` now captures the running
binary's stamp ONCE, pinned at `serve_on`, instead of re-reading the path's
mtime on every call — which after an in-place install described the SUCCESSOR
and made the identity check blind to the change it exists to detect.
⚠ **Honesty about this one: the reported refusal did NOT reproduce.** On Linux a
replaced binary readlinks as `<path> (deleted)`, so the stale agent stamped
itself `(deleted)@0`, `agent_stale` read **true**, and `ychrome-vault handover`
went through **with the unlock preserved** (verified live on guihost: 1120 items,
no master password re-entry). The fix removes the dependency on that procfs
detail, which does not hold if an installer overwrites in place and keeps the
inode.
✅ **The agent-side half of (b) is also cleared**: the post-handover agent
reports `last_sync_unix` (measured `1785682003`).
✅ **(b) PANE HALF DONE 2026-08-02**: a current copy reads **"Last synced 1
minute ago"** as a muted fact with the Sync now button under it, and carries no
`⚠` at all. The warning survives for the two cases that earn one — a copy over 30
minutes old, and an agent that cannot report the fact — and the second now offers
the one-click **"Hand the agent over (keeps it unlocked)"** button instead of
prose telling the user to run a command that used to refuse.
**(c) still owed:** sync on a schedule/staleness rule rather than making the
user press it. Today the only automatic pull is on pane OPEN, once the copy is
over 30 minutes old (`refresh_if_stale`), so a pane left open goes stale in
silence.

✅ **PIXEL-PROVEN 2026-08-02, ON THE LIVE GUI HOST, WITHOUT TOUCHING ANYTHING
OF HIS.** The proof rig is the one this repo already had for exactly this:
`provision_a_scratch_vault_for_a_live_proof` against a throwaway vaultwarden in
docker, a scratch `HOME` (its own ychrome daemon, its own vault agent, adblock
off so the GUI never recompiles a ruleset), a probe row created with
`--no-activate`, and a SHADOW client. Observed, in that order:

1. the pane paints **"Last synced 1 minute ago"** as a muted fact with `Sync now`
   under it and no `⚠` anywhere — part 3, on screen;
2. `✎` opens the Edit tab with a **Stored values** section: Password ·
   Verification code · Authenticator key · Notes, plus the custom field below;
3. `👁` on Notes paints the stored value with *"Shown once. The pane keeps
   nothing"* under it;
4. a plain `GET /pane/vault` refetch immediately after shows the row **bare
   again** — the one-render invariant, in pixels rather than in an assertion;
5. right-clicking a row opens **Copy username · Copy password · Copy verification
   code · Edit, and read what is stored**;
6. clicking Copy password toasts *"Copying git.example.org's password — the page
   confirms the clipboard, or names the refusal."* and the clipboard really holds
   it (see above).

⚠ **WHAT IS STILL OWED: THE USER'S OWN DAEMONS.** The pane is served by the
ychrome DAEMON, and both of this fleet's daemons were holding live surfaces
(dev: 5, including the linked WhatsApp Web session; the GUI host: 3, two of them
mid-task). Retiring one is the operator's call, not an agent's, so
the binary is installed on both hosts and **the running daemons still serve the
old pane**. One `ychrome daemon restart` per host adopts it; every session on
that host re-registers in ~4s and its page reloads.

⚠ **And on dev the vault AGENT still predates `last_sync_unix`**, so the pane
there will show the warning branch until it is handed over. That is now a
one-click button in the pane itself ("Hand the agent over (keeps it unlocked)"),
which is deliberately where it was left: a handover that fails costs a master
password, and it is his to spend.

⚠ Same family as three other findings today: a version-gated hot-restart that
cannot swap a same-version binary, `ctl --help` exiting 0 on a build without the
verb, and `agent_stale`. **An identity check that cannot see the change it
exists to detect.**


## ★★ A CLIENT-SIDE VIEW SWAP STALLS: the URL changes, the screen does not

**Status:** OPEN

Found 2026-08-02 driving the Google sign-in end to end. After each step Google
navigates its own URL (`/v3/signin/challenge/pwd`, then the 2SV screens) and the
**visible view never swaps**: the body still renders the PREVIOUS screen with
Google's own "Loading" prefix, while the next screen's inputs are present in the
DOM at **`0x0` with `offsetParent: null`**.

**This is layout, not paint** — the measurements come from `web eval`, so the
element genuinely has no box; a stale-pixel explanation is falsified. Google's
client-side swap did not complete.

`web wait --until load:finished` answers `met: true, elapsed_ms: 1`, which is
correct and unhelpful: the document load DID finish. Waiting does not help
(measured: still `0x0` after 60 s of polling).

**`location.reload()` renders the real screen every time**, so the flow is
drivable — at the cost of one reload per step, which is why it reads as "the
Google auth flow cannot be completed" to anyone who does not know the trick.

⚠ Downstream, this makes an HONEST refusal look like a selector bug:
`fill-vault` answers `no_hittable_match (… matched 1 element(s) and NONE could
receive a click)`. That is the tool correctly refusing to dispatch into a 0x0
target — do not "fix" it by loosening hittability.

**Worth knowing before diagnosing:** the passkey shim is NOT the cause. It was
the first hypothesis (Google's identifier field carries
`autocomplete="username webauthn"`, so a conditional-mediation call into a shim
that ignores `options.mediation` would block on the ceremony timeout) and it is
FALSIFIED: `performance.getEntriesByType("resource")` shows **zero** `/fido2/*`
and zero control-endpoint requests on the stalled page. The shim was never
called. ⚠ The `mediation` gap is real anyway —
`isConditionalMediationAvailable()` answers `false` while `shim.get()` ignores
`options.mediation` entirely, so a site that asks regardless would hang on a
ceremony no agent surface can approve. Worth closing on its own merits.


## ★★★ THE PASSKEY SHIM PATCHES `navigator.credentials` ON EVERY PAGE, INCLUDING A CHALLENGE PAGE

**Status:** OPEN
**Found 2026-07-31 while investigating why a Cloudflare challenge on a
brilliant.org login would not clear.** Not the cause of that report (the UA and
the engine's cookie jar were, both fixed) but a real, measured incoherence that
a bot check can read.

`sidebar.rs`'s `/policy` prepends `passkey::shim_userscript()` to EVERY
surface's userscripts, main world, document-start, unconditionally. On a page it
touches:

- `navigator.credentials` becomes a plain `Object.create(native || {})`, whose
  methods stringify to JS source rather than `[native code]`;
- `window.PublicKeyCredential` is DEFINED, and
  `isUserVerifyingPlatformAuthenticatorAvailable()` answers `true`.

**Measured on the engine plane (which does not install the shim), WebKitGTK
2.52.5:** `typeof window.PublicKeyCredential === "undefined"` and
`navigator.credentials === undefined`. WebKitGTK has no WebAuthn at all. So on
the visible surface we claim a platform authenticator that this engine cannot
have — an anomaly no real GNOME Web ever shows.

**Why top-frame-only does not save it.** The shim is already `all_frames:
false`, so the `challenges.cloudflare.com` iframe is clean. But an interstitial
managed challenge is served as the TOP-FRAME document at the site's own URL, and
its collector runs in exactly the environment the shim has already patched.

**Why there is no cheap fix, stated so it is not re-attempted.** Since the
engine genuinely lacks WebAuthn, ANY presence of these APIs is the anomaly —
making the shim "look native" cannot work, and a URL-pattern exclusion cannot
help because the challenge is served at the normal page URL. The only correct
shape is **per-origin installation**: build the shim's `Userscript::matches`
from the set of rpIds the vault actually holds passkeys for, so a page for a
site you have no passkey for sees a pristine `navigator`.

### IT REACHED THE USER, 2026-08-01 — measured, and the cause is NOT what it looked like

**Report:** a Forgejo 2FA page answered *"Could not read your security key. Your
browser does not currently support WebAuthn."* — *"How am I supposed to use the
passkey?"*

**Two independent faults, both measured on the GUI host. Neither was the documented
"the vault holds no passkey for this host" case:** `ychrome-vault passkeys
<host>` returns a real credential for it.

1. **The running agent does not know `passkey-hosts`.** Asked directly on
   `~/.yggterm/vault/agent.sock` it answers `unknown op "passkey-hosts"`, in
   0.2 ms, five times out of five. That is the documented deploy-ordering
   hazard below, and it happened: the browser shipped ahead of its agent.

   ⚠ **The stderr announce was not evidence either way, and reasoning from its
   absence sent a first diagnosis in the wrong direction.** ychrome's stderr is
   the PTY it was launched in — it is not in `~/.yggterm`, not in the GUI's
   `app-launch-logs`, and not anywhere anyone greps. **FIXED:** the vault pane
   now renders the state (`sidebar::passkey_shim_widgets`) with a one-click
   `handover`, so the browser says it where the user already is when a passkey
   login fails.

   ⚠ **`agent_stale` is structurally incapable of catching this**, which is why
   the pane was silent: it compares the agent against the INSTALLED
   `ychrome-vault`, and both were the same six-day-old binary. `status` read
   `unlocked`, `agent_stale: false`, 1116 items — a perfectly healthy vault —
   on the same socket that refused the op. Only ASKING for the op finds it.
   The remedy is also ordered: `handover` execs the *installed* binary, so a
   stale installed binary must be replaced FIRST or the handover is a no-op.

2. **The policy stamp was blind to the shim, which made it permanent.**
   `webpolicy::policy_version` stamped adblock, the UA, SponsorBlock and
   userscript FILES — nothing about the vault — while `/policy` prepends a shim
   whose scope comes from the vault. The GUI refetches only when that stamp
   moves, and yggterm applies userscripts at surface CREATION. So a surface kept
   whatever shim decision was true when it opened, for life.

   Measured, one unchanged `policy_version` (`ebc219f7d40ddc53`):
   `sidebar_contribution/policy` recorded `userscripts: 6` at 14:53 and
   `userscripts: 5` from 16:07 onward, across the deploy. **FIXED:**
   `passkey_shim_stamp()` folds `agent.pid` (rewritten by `serve_on` on every
   start AND every handover, since an `execve` keeps the pid) and the installed
   binary's stamp into `policy_version`, stat-only so it stays off the socket.

   ⚠ **Still open:** a plain lock → unlock touches no file, so a surface opened
   over a locked vault still needs REOPENING. The pane says so in words. A
   vault-published scope stamp would close it properly.

### ⚠ ENROLMENT ON A SITE WITH NO PASSKEY — THE ARM IS BUILT, THE PROOF IS OWED

The shim's match patterns come only from rpIds the vault ALREADY holds
credentials for, so a site you have no passkey for sees a pristine `navigator`
— exactly right for the fingerprinting fix, and it also meant
`navigator.credentials.create()` could never be called there. Every passkey in
this vault was enrolled in some other browser. The user hit this on a Google
sign-in: *"I cannot enter the passkey when anyone requests me … there is no
clicking to give passkey or save a passkey."*

⛔ The fix is NOT to widen the scope back, and it was not: **the user arms one
host from the vault pane** ("Enrol a passkey here"), the anomaly exists only on
a page a human deliberately armed, and arming is per-process so a browser
restart forgets it. Built 2026-08-02, unit-locked and mutation-proven (an armed
host must reach the shim's real match patterns; a wildcard or empty host is
refused at the door).

**Owed:** the end-to-end proof on a real page — arm a site, reopen the tab,
and watch `navigator.credentials.create()` succeed. ⚠ The pane says the tab must
be REOPENED, because the shim is installed when a surface is built; if that
turns out to be wrong on this path, the notice is what needs correcting.

### IMPLEMENTED 2026-08-01, NOT YET PROVEN ON A REAL PAGE

All four steps are done and unit-locked: the `passkey-hosts` agent op (metadata
only — rpIds, no credential ids, no keys), the `sidebar.rs` call through
`ychrome-vault-proto`, no-shim-on-a-locked-vault, and the probe kept off the
`policy_version` heartbeat. Match patterns are `*://<rp>/*` PLUS `*://*.<rp>/*`,
because WebKit's `*://*.example.com/*` does not admit the bare host while
WebAuthn scopes a credential to the rpId and its subdomains. The user-presence
invariant is untouched and locked: scoping decides only WHERE
`navigator.credentials` is patched.

**What is NOT yet demonstrated, and why this entry stays open:**

1. **No end-to-end proof on a real page.** Nobody has loaded a site the vault
   holds a passkey for and confirmed the shim is present, then loaded one it
   does not and confirmed `navigator.credentials` is pristine. That is the
   observation that would actually close this.
2. **The running vault agent predates the op**, so on dev today the browser
   installs the shim NOWHERE. Verified live: the agent answers `unknown op
   "passkey-hosts"`. The code says so loudly on stderr, once, rather than
   letting it look like the healthy case — but until the agent is handed over
   (`ychrome-vault handover`) passkeys are off.
3. ⚠ **DEPLOY ORDERING IS PART OF THIS FIX.** The vault agent must be handed
   over before, or with, the browser. Shipping the browser alone turns passkey
   logins off everywhere.

---

## ★★ (yggterm) A PROFILE WHOSE WRITE-LOCK IS HELD ELSEWHERE OPENS WITH NO JAR

**Status:** OPEN
**Not in this repo — filed here because it is the other half of the cookie-jar
failure ychrome fixed on its own engine plane, and an ychrome user meets it as
"the login will not stick".**

`yggterm-shell/src/shell.rs` commented that a surface whose profile write-lock
is held elsewhere "opens READ-ONLY (ephemeral, no jar)". Ephemeral is not
read-only: an ephemeral `WebContext` reads NOTHING from the jar and writes
nothing back. A second surface on a profile another surface already holds
therefore starts logged out and cannot keep a cookie — including a bot-check
clearance cookie, which is why a challenged login can loop forever.

Two things were owed. **One is done** (yggterm `lane/dev/ychrome-bugs-docs`):
the silence. `WebSurfaceJarMode` now owns the decision, the spelling and the
words — `persistent` / `ephemeral_by_request` / `no_jar_lock_held_elsewhere` —
the mode is on the `profile_write_lock` trace line, and a degraded profile
raises ONE notice (per profile, not per tab) that talks about being logged out
and about the bot-check cookie rather than about write locks. The misleading
"READ-ONLY" comment is gone. A `debug_assert` pins the mode to the jar it
describes.

⚠ **The other half is a design call, not a mechanical fix, and that is why it
is still here.** WebKitGTK has no read-only jar mode, so "genuinely read-only"
means giving the loser a private COPY of the profile's cookies (a Netscape text
file — mechanically easy) and its local storage. Editorially it is not easy:
**every shadow surface an agent opens on a profile the user holds would then
duplicate that profile's live session cookies to a second place on disk.** In a
browser that carries the operator's brokerage sessions, spreading cookie jars is
his decision to make. Options, so the next reader does not start from scratch:

1. copy-on-open into a scratch dir, wiped at teardown, with a startup sweep for
   crash leftovers — full fidelity, maximum secret spread;
2. copy the `cookies` file ONLY, which fixes the reported symptom (the login
   sticks for the surface's life) at a fraction of the exposure;
3. decline, and keep the notice as the whole answer.

⚠ **Not live-proven.** The notice needs two live clients contending for one
profile on the GUI host, which needs a yggterm GUI + daemon deploy — and
`yggterm/docs/pending-bugs.md` still carries "REMOTE ROWS WEDGE IN
`RemoteBootstrap` AFTER A DAEMON VERSION HANDOVER", which wedged 15 rows on the
last bump. The rule and the words are unit-locked and mutation-proven; the
pixel is owed.

## ★★ THE `dream-control-surfaces` ITEMS ARE BUGS, NOT ASPIRATIONS

**Status:** OPEN
`docs/dream-control-surfaces.md` is filed as a dream document. The operator's
ruling, 2026-07-31: **these are bugs.** Framing a missing capability as a dream
puts it outside every process that would fix it — it is not in a bug list, it has
no reproduction, and nobody triages it. Several of its items have now cost real
sessions:

- **§2 Headless surface-create — the OSC must not depend on window focus.** This
  is precisely the gap that sent an agent toward revealing surfaces on the
  operator's screen; it is the subject of `dream-detached-agent-surfaces.md` and
  of the detached-by-default rule in `yggterm/docs/agent-surface-attachment.md`.
- **§1 Unlock request** and **§6 Autofill-from-vault.** The vault is the standing
  friction: it is LOCKED on a headless host and dev (as root it answers `not_configured`) and
  resolves only on the GUI host, and `fill-vault` needs a mapped surface so it is
  unavailable on exactly the detached surfaces agents are told to prefer. Every
  run pays this toll by hand.
- **§3 OTP from the data-fabric** and **§5 Extract surface** — the same shape.

**Ask:** promote each numbered section into this file with a reproduction and an
acceptance test, or state explicitly which are declined and why. The `dream-*`
documents should hold *design*, never *the only record that something is
missing*. Same applies to `dream-detached-agent-surfaces.md`, whose §5 control
and §7 table are already bug-shaped.

## ⚠ FLAKY TEST — `daemon_staleness` fails on a busy host, and it is NOT the code

**Status:** OPEN
**Characterised 2026-08-01 on dev, after it cost a session real time twice.**
Two tests in `tests/daemon_staleness.rs` fail intermittently:

```
a_pre_gate_client_is_named_as_such_by_the_refusal_and_by_status
a_gated_route_survives_a_daemon_handover_on_what_the_client_re_declares
  → panicked at tests/daemon_staleness.rs:326: a control response:
    Os { code: 11, kind: WouldBlock }        ← a 5s read timeout, not a bug
```

`control_get` gives a spawned real binary **5 s** to answer over TCP. dev is a
32-core LXC that regularly sits at load 45-112 (the yggterm test binaries alone
were measured at 1208% and 1144% CPU), and under that the budget is simply not
enough.

**Proven to be load, not a regression, by a controlled A/B under the same load:**
at load 112 the *baseline* (HEAD with the change stashed) failed **3/3**; at
load ~44 the same tree passed **8/8 in 8.2 s**. Anything that changes the
daemon's startup path will look like it caused this. It did not.

**Want:** make the budget generous (or adaptive) rather than a wall clock a
loaded CI host cannot meet — a timing test that fails on a busy machine teaches
agents to ignore red, which is worse than the flake.

---

## `ychrome --help` does not mention `ctl` — the agent surface is undiscoverable (2026-08-20)

**Seen:** `ychrome ctl --help` works and is the entire agent-facing control surface, but
top-level `ychrome --help` lists only the viewport options and never names the `ctl`
subcommand. An agent that does not already know `ctl` exists cannot discover it from the
binary itself — it has to arrive via a skill document or word of mouth, which is exactly the
kind of prose dependency the help output exists to remove.

**Want:** top-level help enumerates every subcommand, `ctl` included, with a one-line
description pointing at `ychrome ctl --help`.

---
