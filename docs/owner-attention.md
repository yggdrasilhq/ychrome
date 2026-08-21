# ychrome — decisions that are the owner's, not the lane's

Each entry: **the question · the options · THE RECOMMENDATION · what was done meanwhile ·
how to reverse it.** A lane does not stop on these; it routes around them and carries on.
An entry with no recommendation is unfinished.

Read at the end of a campaign, not mid-relay. When one is settled, it is deleted in the
same commit as the work it unblocked — same law as `pending-bugs.md`.

---

## 1. The YouTube segfault's likeliest remedy is a `glibc` upgrade on the live machine

**Filed 2026-08-21.**

**The question.** The engine segfaults within seconds of a YouTube watch page, reproduced
5/5. The most likely single remedy is moving WebKitGTK from the installed **2.52.5-1** to
**2.52.6-1** — a crash in a library at `n-1` is worth retrying at `n`, and symbolised
debugging is impossible until then because the only `-dbgsym` package published is 2.52.6
and symbols must match the binary exactly.

⛔ **But it is not one library.** 2.52.6 links `GLIBC_2.43`; this host runs `2.42-17`. So
the upgrade pulls **`libc6` 2.42-17 → 2.43-3 and its i386 multiarch twin**, under a running
desktop, on a machine with live sessions and other agents on it. Every GTK web application
on the host links the same WebKit, including the terminal's own GUI.

**The options.**

| | what | cost |
|---|---|---|
| **A** | Upgrade `libwebkit2gtk-4.1-0`, accepting the `libc6` upgrade, at a quiet moment | a C-library upgrade under a live desktop; 2.52.5-1 is **no longer downloadable**, so rollback means snapshot archaeology |
| **B** | Test 2.52.6 inside a `bwrap` namespace first, bind-mounting the newer libraries over `/usr/lib/x86_64-linux-gnu` | ~an hour of setup; changes nothing on the host; answers "does it even fix it" before anything is risked |
| **C** | Leave it | the agent-facing engine cannot drive any video page, indefinitely |

⭐ **RECOMMENDATION: B, then A only if B says the upgrade actually fixes it.** The whole
weakness of A today is that it is a risky change with an **unmeasured payoff** — nobody has
shown 2.52.6 fixes this. B converts it into a measured one for no risk at all, and if 2.52.6
does not fix the crash it saves the upgrade entirely. `bwrap` is already installed.

⚠ The cheap version of B does **not** work and should not be re-tried: extracting 2.52.6 into
a private prefix and pointing `LD_LIBRARY_PATH` at it fails, because the loader refuses on
`GLIBC_2.43` and because the library carries **no `WEBKIT_EXEC_PATH`** — the `WebKitWebProcess`
helper path is compiled in, so the UI process would be 2.52.6 while the web process stayed
2.52.5. It has to be a namespace or a container.

**What was done meanwhile.** The crash was narrowed rather than left. Seven hypotheses are now
eliminated with controls — DMA-BUF, GL, resolution, compositing, VA-API hardware decode, MSE,
EME — and the standing suspect (**MSE / adaptive streaming**) is exonerated: locally generated
H.264, VP9 and AV1 all play, and fragmented H.264 through `MediaSource` plays to completion,
none of them crashing. ychrome's own injected userscripts are ruled out too. See
`pending-bugs.md`.

**How to reverse.** Nothing to reverse — no system package was touched.

⚠ **And it may not be his crash at all.** Every measurement is on the **headless** substrate
(Xvfb, no DRI3). The owner's reports describe degraded picture, not a browser that dies, and
his daemon's journal shows no crash cluster outside the lane's own testing. The crash is
certain for **agent-driven** browsing of video pages; that it affects what he watches is
**not established**, and should not be repeated as though it were.

---

## 2. Four tests are red on the trunk and they belong to another lane

**Filed 2026-08-21.**

**The question.** `cargo test --bin ychrome` fails 4 of 389, deterministically, on a clean
tree. Commit `0c6b80e` added a fourth vault item type (`Identity`) and did not update the four
`sidebar::tests` that enumerate the item types. Should this lane edit them?

**RECOMMENDATION: no, and it did not.** Editing another lane's assertions until they match
their code is how a genuine regression gets laundered — those assertions are the only thing
currently checking that feature. The author should say which side is right. Filed at ⛔ in
`pending-bugs.md` with the four names, because until it is settled nobody can use the suite as
a green gate, and every session must either know the four names or suspect its own change.

**What was done meanwhile.** Worked around it: this lane ran targeted tests and reports its own
counts against the same 4 pre-existing failures (382→389 passing, same 4 failing).
