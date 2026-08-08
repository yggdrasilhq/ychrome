//! **Probe 1: can the engine reach INTO a cross-origin frame without a WebKit
//! web-process extension?** (`ychrome engine frames`).
//!
//! `docs/pending-bugs.md` carries "THE ENGINE CANNOT REACH INTO A CROSS-ORIGIN
//! FRAME, AND THAT BLOCKED A PAYMENT". A payment UI that renders inside a
//! bank's `<iframe>` cannot be read or driven: `ctl eval` runs in the TOP
//! document, `window.frames[0].document` throws `SecurityError` by design, and
//! no verb takes a `frame=` argument.
//!
//! The entry names the shape of the fix and then stops at the one thing nobody
//! had measured:
//!
//! > `webkit_web_view_evaluate_javascript` takes a `world_name`, and
//! > `WebKitFrame` exists in the web-process extension API only. Whether it can
//! > be done without a web-process extension is UNPROVEN — establish that first.
//!
//! **That is what this module settles, and it settles it by measurement.** A
//! web-process extension is a much larger change (a second shared object, a
//! second lifecycle, a second thing to deploy across five hosts), so the answer
//! decides how expensive the `frame=` verb is. It must not be guessed.
//!
//! ## The mechanism under test
//!
//! Two UI-process APIs the engine already links, and nothing else:
//!
//! 1. **`UserContentInjectedFrames::AllFrames`** — `identity::attach_script`
//!    already honours a userscript's `@all-frames`, so WebKit will inject our
//!    code into EVERY frame of a page, whatever its origin. That is the half
//!    that matters: a bank's document will never cooperate with us, so the only
//!    question is whether we can get our own code to run inside it.
//! 2. **`postMessage`** — deliberately cross-origin by design. The copy of the
//!    bridge in the child talks to the copy in the top frame, and ordinary
//!    `ctl eval` (which reaches the top frame) reads the result off a global.
//!
//! No `WebKitFrame`. No web-process extension. No loosened origin check: the
//! child's document is never handed to the parent, and the parent never reads
//! it — the child reads its OWN document, in its own frame, and reports a value.
//!
//! ## What is being proven, in order
//!
//! | step | claim |
//! |---|---|
//! | 1 | the child really is cross-origin (`SecurityError`), so nothing here passes by accident |
//! | 2 | an `@all-frames` userscript runs INSIDE that cross-origin child |
//! | 3 | ⭐ the MUTATION CONTROL: the same script WITHOUT `@all-frames` does not |
//! | 4 | the UI process can read the child's own DOM through the bridge |
//! | 5 | the UI process can install a listener inside the child |
//! | 6 | the UI process can measure an element's rect inside the child |
//! | 7 | ⭐ a REAL `GdkEvent` click, aimed by that rect, lands in the child with `isTrusted` |
//! | 8 | typed text arrives in the child's own input |
//!
//! Steps 6-8 are the ones that turn this from a curiosity into the missing verb:
//! `/engine/input` already dispatches real events at viewport coordinates, and
//! WebKit already hit-tests them through the frame tree. What was missing was
//! never the input — it was knowing WHERE, because selector resolution runs in
//! the top document. A rect measured inside the child plus the iframe's own
//! rect is that coordinate, and it needs no new WebKit API at all.
//!
//! ## ⛔ The probe bridge is NOT the shipped design
//!
//! This script is an instrument, not a product. It is installed for the run and
//! removed on every exit path (including a panic, via [`InstalledProbe`]), and
//! it will not act on a command that does not carry a per-run token generated in
//! this process — so a copy left on disk by a killed run is inert to any web
//! page, which could not read the token anyway. A shipped `frame=` verb would
//! carry the token per page rather than per run and would not accept `eval` from
//! the page's own message channel at all. Do not lift this script into
//! `src/`.
//!
//! # Probe 2: is `postMessage` delivery WORLD-SCOPED? (`ychrome engine worlds`)
//!
//! Probe 1 settles that we CAN reach into a cross-origin child. It does not
//! settle what the `frame=` verb may CARRY, which is a security question and
//! has its own answer — see [`run_worlds`], and `docs/pending-bugs.md` for what
//! the answer forces the design to be. In one line: **worlds isolate globals,
//! not event dispatch**, so a `postMessage` bridge is page-observable and
//! page-forgeable in any world, and the shipped verb must therefore be
//! read-only with its replies routed to the UI process rather than back across
//! the page's own message channel.
//!
//! That last clause was a reasonable expectation and nothing more, so
//! [`run_worlds`] measures it too: **does a script message handler registered
//! `..._in_world(ISOLATED_WORLD)` reach the UI process from inside a
//! CROSS-ORIGIN child?** It does — and unlike `postMessage`, the handler IS
//! world-scoped, so the page's own world cannot call it. Two channels in one
//! substrate with opposite answers, which is why neither could be assumed from
//! the other.
//!
//! ⚠ The two probes share ONE fixture ([`open_fixture`]) on purpose. A second
//! wiring of the same two origins is a second thing to keep honest, and the one
//! that drifted would still look green.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::api::{dispatch, json_status as json_reply, request};
use super::gateway::{Hits, document, spawn_origin};

/// The profile every page in this proof opens under. Its own, so the run
/// touches no jar an agent or the user is using.
const PROFILE: &str = "engine-frames-proof";

/// The stem of the bridge userscript, installed through `webpolicy` (its owner)
/// and removed by [`InstalledProbe`].
const BRIDGE_STEM: &str = "ychrome-frame-bridge-probe";

/// The stem of the MUTATION CONTROL: the same world, the same match, and no
/// `@all-frames`. Without this, step 2 proves only "a userscript ran somewhere".
const TOPONLY_STEM: &str = "ychrome-frame-toponly-probe";

/// The two instantiations of [`WORLD_PROBE_TEMPLATE`] — see [`run_worlds`].
/// Their stems carry the label, because the label is also the attribute prefix
/// each one writes, and a mismatch between the two would read as silence.
const WORLD_ISO_STEM: &str = "ychrome-world-iso-probe";
const WORLD_MAIN_STEM: &str = "ychrome-world-main-probe";

/// The label each world probe stamps on everything it writes. One constant per
/// script, used for the stem, the `@world` instantiation and the `data-` prefix
/// at once, so the three can never drift apart.
const ISO_LABEL: &str = "iso";
const MAIN_LABEL: &str = "main";

/// The two content→UI-process channels [`run_worlds`] arms — one registered in
/// the engine's ISOLATED world, one in the page's MAIN world.
///
/// Two, not one, because a silence on the isolated channel has to be
/// distinguishable from "script message handlers do not work here at all". The
/// main-world channel is that control, and a cross-origin child reaching it is
/// the control for the frame variable as well.
const ISO_CHANNEL: &str = "ychromeProbeIso";
const MAIN_CHANNEL: &str = "ychromeProbeMain";

/// How long a bridge round trip may take.
const CALL_TIMEOUT_MS: u64 = 6000;

/// How many times a read is retried while the page settles. Typing is
/// acknowledged one key at a time (see `docs/agent-engine.md`), so the first
/// read after an input can legitimately be one character short.
const READ_TRIES: u32 = 40;
const READ_SETTLE: Duration = Duration::from_millis(150);

struct Step {
    name: &'static str,
    pass: bool,
    detail: Value,
}

impl Step {
    fn to_json(&self) -> Value {
        json!({ "step": self.name, "pass": self.pass, "detail": self.detail })
    }
}

// ---------------------------------------------------------------------------
// The probe userscripts
// ---------------------------------------------------------------------------

/// The bridge, with `{TOKEN}` substituted for this run's token.
///
/// It is ONE script in every frame, and which half it becomes is decided by
/// `window.top === window`. That is deliberate: a shipped version has to work
/// the same way, because WebKit injects one script into all frames and gives it
/// no say in the matter.
const BRIDGE_TEMPLATE: &str = r#"// ==UserScript==
// @name         ychrome-frame-bridge-probe
// @version      1.0.0
// @match        *://*/*
// @all-frames   true
// @world        main
// @run-at       document-start
// ==/UserScript==
(() => {
  const TOKEN = "{TOKEN}";
  const MARK = "__ychromeFrameBridge";
  if (Object.prototype.hasOwnProperty.call(window, MARK)) { return; }
  const hide = (name, value) => Object.defineProperty(window, name, {
    value: value, enumerable: false, configurable: true, writable: false
  });
  hide(MARK, TOKEN.slice(0, 4));

  if (window.top === window) {
    const frames = [];
    const results = Object.create(null);
    hide("__ychromeFrames", frames);
    hide("__ychromeFrameResults", results);
    window.addEventListener("message", (event) => {
      const data = event.data;
      if (!data || typeof data !== "object" || data.token !== TOKEN) { return; }
      if (data.hello) {
        let index = -1;
        for (let i = 0; i < window.frames.length; i++) {
          if (window.frames[i] === event.source) { index = i; break; }
        }
        const seen = frames.some((f) => f.index === index && f.url === data.url);
        if (!seen) { frames.push({ index: index, origin: event.origin, url: data.url }); }
      } else if (data.result) {
        results[data.seq] = { ok: data.ok, json: data.json, error: data.error || null };
      }
    });
    hide("__ychromeFrameCall", (index, js, seq) => {
      const known = frames.filter((f) => f.index === index)[0];
      if (!known) {
        results[seq] = { ok: false, json: null, error: "no frame announced at index " + index };
        return false;
      }
      // The command goes to the frame's OWN origin, never "*": a wildcard would
      // hand the script text to whatever happens to be in that slot.
      window.frames[index].postMessage(
        { token: TOKEN, cmd: 1, seq: seq, js: js }, known.origin);
      return true;
    });
  } else {
    window.addEventListener("message", (event) => {
      const data = event.data;
      if (!data || typeof data !== "object" || data.token !== TOKEN || !data.cmd) { return; }
      let reply;
      try {
        const out = (0, eval)(data.js);
        reply = {
          token: TOKEN, result: 1, seq: data.seq, ok: true,
          json: out === undefined ? "undefined" : JSON.stringify(out)
        };
      } catch (error) {
        reply = { token: TOKEN, result: 1, seq: data.seq, ok: false, json: null,
                  error: String(error) };
      }
      try { window.top.postMessage(reply, "*"); } catch (ignored) {}
    });
    const announce = () => {
      try {
        window.top.postMessage(
          { token: TOKEN, hello: 1, url: String(location.href) }, "*");
      } catch (ignored) {}
    };
    announce();
    document.addEventListener("DOMContentLoaded", announce);
    window.addEventListener("load", announce);
  }
})();
"#;

/// The mutation control. Identical placement — main world, same `@match` — and
/// NO `@all-frames`, so it must reach the top frame and only the top frame.
const TOPONLY_SCRIPT: &str = r#"// ==UserScript==
// @name         ychrome-frame-toponly-probe
// @version      1.0.0
// @match        *://*/*
// @world        main
// @run-at       document-start
// ==/UserScript==
Object.defineProperty(window, "__ychromeTopOnly", {
  value: 1, enumerable: false, configurable: true
});
"#;

/// ⭐ The world-delivery probe, instantiated TWICE from this ONE template — once
/// in the isolated world and once in the main world.
///
/// `{WORLD}` and `{LABEL}` are the only substitutions besides the run token, so
/// any difference in what the two copies record is a difference the **world**
/// made and nothing else. That is what makes this a measurement rather than two
/// scripts that happen to disagree.
///
/// The instrument turns on one asymmetry: **worlds do not share globals, but
/// they do share the DOM.** So each copy records what it heard into a `data-`
/// attribute on `documentElement`, where the other world — and `/engine/eval`,
/// which runs in the main world — can read it out. A global would be invisible
/// from outside its own world, which is exactly the thing being measured and
/// therefore cannot be the thing doing the measuring.
///
/// Posting happens at `load`. The top document reaches `load` only after every
/// subframe has loaded, so the child's `document-start` listeners are already
/// installed when a message is sent — which is what makes a *silence* measured
/// afterwards a real silence rather than a race.
const WORLD_PROBE_TEMPLATE: &str = r#"// ==UserScript==
// @name         ychrome-world-{LABEL}-probe
// @version      1.0.0
// @match        *://*/*
// @all-frames   true
// @world        {WORLD}
// @run-at       document-start
// ==/UserScript==
(() => {
  const LABEL = "{LABEL}";
  const TOKEN = "{TOKEN}";
  const CHANNELS = { iso: "{ISO_CHANNEL}", main: "{MAIN_CHANNEL}" };
  const WHERE = window.top === window ? "top" : "child";
  const mark = (kind, value) => {
    try {
      document.documentElement.setAttribute(
        "data-ychrome-" + LABEL + "-" + kind, value);
    } catch (ignored) {}
  };
  const heard = [];
  Object.defineProperty(window, "__ychromeWorld_" + LABEL, {
    value: 1, enumerable: false, configurable: true, writable: false
  });
  window.addEventListener("message", (event) => {
    const data = event.data;
    if (!data || typeof data !== "object" || data.worldProbe !== TOKEN) { return; }
    heard.push(data.from);
    mark("heard", JSON.stringify(heard));
  });
  mark("ran", WHERE);
  // The channel probe, run TWICE. `window.webkit.messageHandlers` is populated
  // from the content manager's registrations as the document is created, so a
  // handler should be there at document-start — but a probe that only looked
  // then would report "no channel" for one that merely arrived late, and a
  // false negative here sends the design back to a web-process extension.
  const probeChannels = (phase) => {
    const handlers = (window.webkit && window.webkit.messageHandlers) || null;
    for (const key of Object.keys(CHANNELS)) {
      const handler = handlers ? handlers[CHANNELS[key]] : undefined;
      mark("chan-" + key + "-" + phase, typeof handler);
      if (!handler) { continue; }
      try {
        handler.postMessage({
          channelProbe: TOKEN, world: LABEL, frame: WHERE, phase: phase,
          href: String(location.href), origin: String(location.origin)
        });
      } catch (error) {
        mark("chan-" + key + "-" + phase + "-threw", String(error));
      }
    }
  };
  probeChannels("start");
  window.addEventListener("load", () => { probeChannels("load"); });
  if (window.top !== window) { return; }
  window.addEventListener("load", () => {
    try {
      window.postMessage(
        { worldProbe: TOKEN, from: LABEL + "-self" }, location.origin);
    } catch (ignored) {}
    const frame = document.getElementById("pay");
    if (frame && frame.contentWindow) {
      try {
        frame.contentWindow.postMessage(
          { worldProbe: TOKEN, from: LABEL + "-into-child" },
          new URL(frame.getAttribute("src"), location.href).origin);
      } catch (ignored) {}
    }
    mark("posted", "1");
  });
})();
"#;

/// Every probe script this module can install, removed on EVERY exit path
/// including a panic.
///
/// `webpolicy::install_userscript` writes into the shared userscript directory,
/// which is the user's real one when `HOME` is real. parity.rs sets the
/// precedent; the guard is this module's addition, because this script carries
/// an `eval` and a leftover copy is not the harmless global parity's probe
/// leaves behind.
struct InstalledProbe;

impl Drop for InstalledProbe {
    fn drop(&mut self) {
        // Every stem, not only the ones this run installed: a stem left behind
        // by a killed run is exactly the thing the guard exists to clear, and
        // deleting one that was never installed is a no-op error we discard.
        for stem in [BRIDGE_STEM, TOPONLY_STEM, WORLD_ISO_STEM, WORLD_MAIN_STEM] {
            let _ = crate::webpolicy::delete_userscript(stem);
        }
    }
}

/// A per-run token. Not cryptographic and does not need to be: its whole job is
/// that a page which never saw this process cannot forge a command, and a page
/// cannot read the file the token is written into.
fn run_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("yct-{:x}-{:x}", std::process::id(), nanos)
}

// ---------------------------------------------------------------------------
// The fixture: two real origins, and a child that helps us with nothing
// ---------------------------------------------------------------------------

/// The merchant's page: one iframe, on the OTHER origin, and no bridge code of
/// its own. `border:0` so the iframe's content box starts exactly at its border
/// box and the coordinate translation in step 7 has nothing hidden in it.
fn merchant_page(bank: &str) -> String {
    document(
        "MERCHANT frames",
        &format!(
            "<h1 id=\"merchant\">MERCHANT</h1>\
             <iframe id=\"pay\" name=\"payframe\" src=\"{bank}/bank\" \
             style=\"width:600px;height:400px;border:0;display:block\"></iframe>"
        ),
    )
}

/// The bank's page. **Inert HTML on purpose.** No script, no `postMessage`
/// listener, no beacon — a page that helped us would prove nothing about a real
/// gateway, which will never have heard of us.
///
/// ⭐ The `#decoy` and the 180px gap are the CONTROL FOR THE TRANSLATION, and
/// they were added after the first green run: the merchant page and the bank
/// page had accidentally identical geometry (both `85.875` from the top,
/// because each puts one default `<h1>` above the thing being measured), so a
/// click that forgot to add the iframe's own offset would have landed on the
/// right element for the wrong reason. The decoy now occupies exactly the band
/// an untranslated point would hit, and step 7 asserts the click's target was
/// `otp` — not merely that something inside the frame was clicked.
fn bank_page() -> String {
    document(
        "BANK frames",
        "<h1 id=\"who\">BANK OF FIXTURE</h1>\
         <div id=\"decoy\" style=\"height:180px;background:#eee\">decoy band</div>\
         <p><input id=\"otp\" style=\"width:240px;height:34px;font:16px monospace\"></p>\
         <p><button id=\"pay\" style=\"width:120px;height:36px\">Pay</button></p>",
    )
}

/// The two origins, serving, with the merchant page already open.
///
/// ONE copy, because both proofs in this module drive the same two documents
/// and the geometry is load-bearing in the first of them (the decoy band, the
/// `border:0`). A second wiring of the same fixture is a second thing to keep
/// honest, and the one that drifted would still look green.
struct Fixture {
    page: String,
    merchant_base: String,
    bank_base: String,
    merchant_hits: Hits,
    bank_hits: Hits,
}

fn open_fixture() -> Result<Fixture> {
    let merchant_listener = TcpListener::bind("127.0.0.1:0")?;
    let bank_listener = TcpListener::bind("127.0.0.1:0")?;
    let merchant_base = format!(
        "http://127.0.0.1:{}",
        merchant_listener.local_addr()?.port()
    );
    let bank_base = format!("http://127.0.0.1:{}", bank_listener.local_addr()?.port());

    let bank_for_merchant = bank_base.clone();
    let (_, merchant_hits) = spawn_origin(
        merchant_listener,
        Arc::new(move |_method: &str, path: &str| {
            if path == "/" || path.starts_with("/pay") {
                (200u16, Vec::new(), merchant_page(&bank_for_merchant))
            } else {
                (404u16, Vec::new(), "no such merchant route".to_string())
            }
        }),
    )?;
    let (_, bank_hits) = spawn_origin(
        bank_listener,
        Arc::new(|_method: &str, path: &str| {
            if path.starts_with("/bank") {
                (200u16, Vec::new(), bank_page())
            } else {
                (404u16, Vec::new(), "no such bank route".to_string())
            }
        }),
    )?;

    crate::daemon::journal(
        "engine.frames.start",
        json!({ "merchant": merchant_base, "bank": bank_base }),
    );

    let (status, body) = call(
        "open",
        json!({ "url": format!("{merchant_base}/pay"), "profile": PROFILE }),
    );
    let page = body["page_id"].as_str().unwrap_or_default().to_string();
    if status != 200 || page.is_empty() {
        anyhow::bail!("the frames fixture would not open: {status} {body}");
    }

    Ok(Fixture {
        page,
        merchant_base,
        bank_base,
        merchant_hits,
        bank_hits,
    })
}

// ---------------------------------------------------------------------------
// Driving
// ---------------------------------------------------------------------------

fn call(verb: &str, body: Value) -> (u16, Value) {
    json_reply(dispatch(&request(verb, body)))
}

fn read(page: &str, js: &str) -> Value {
    match call("eval", json!({ "page_id": page, "js": js })) {
        (200, body) => body["value"].clone(),
        (_, body) => json!({ "eval_failed": body }),
    }
}

/// One bridge round trip: ask frame `index` to evaluate `js` in its own world.
///
/// Returns the child's reply as `{ok, json, error}` — `json` is the child's
/// `JSON.stringify` of the value, because a `postMessage` payload has to be
/// structured-cloneable and a DOM node is not.
fn frame_call(page: &str, seq: u64, index: usize, js: &str) -> Value {
    let quoted = serde_json::to_string(js).unwrap_or_else(|_| "\"\"".to_string());
    let fired = read(
        page,
        &format!(
            "!!window.__ychromeFrameCall && window.__ychromeFrameCall({index}, {quoted}, {seq})"
        ),
    );
    if fired != json!(true) {
        return json!({ "ok": false, "error": "the top frame has no bridge", "fired": fired });
    }
    let _ = dispatch(&request(
        "wait",
        json!({
            "page_id": page,
            "until": { "js": format!("window.__ychromeFrameResults && window.__ychromeFrameResults[{seq}] !== undefined") },
            "timeout_ms": CALL_TIMEOUT_MS,
        }),
    ));
    let raw = read(
        page,
        &format!("JSON.stringify(window.__ychromeFrameResults[{seq}] || null)"),
    );
    raw.as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(json!({ "ok": false, "error": "no reply landed", "raw": raw }))
}

/// A bridge read that is allowed to settle. Input is acknowledged one event at
/// a time, so the first read after typing can honestly be short — the rule the
/// engine spec states for the top frame holds inside a child too.
fn frame_call_until(
    page: &str,
    seq: &mut u64,
    index: usize,
    js: &str,
    want: &Value,
) -> (bool, Value) {
    let mut last = Value::Null;
    for _ in 0..READ_TRIES {
        *seq += 1;
        let reply = frame_call(page, *seq, index, js);
        let value: Value = reply["json"]
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or(Value::Null);
        if &value == want {
            return (true, reply);
        }
        last = reply;
        std::thread::sleep(READ_SETTLE);
    }
    (false, last)
}

// ---------------------------------------------------------------------------
// The proof
// ---------------------------------------------------------------------------

pub fn run() -> Result<Value> {
    let started = Instant::now();
    let token = run_token();

    // Through the userscript plane's OWNER, exactly as parity.rs does — the
    // engine does not decide where scripts live or which are enabled.
    let _guard = InstalledProbe;
    crate::webpolicy::install_userscript(BRIDGE_STEM, &BRIDGE_TEMPLATE.replace("{TOKEN}", &token))
        .context("installing the frame-bridge probe userscript")?;
    crate::webpolicy::install_userscript(TOPONLY_STEM, TOPONLY_SCRIPT)
        .context("installing the top-frame-only control userscript")?;

    let fixture = open_fixture()?;
    let Fixture {
        page,
        merchant_base,
        bank_base,
        merchant_hits,
        bank_hits,
    } = &fixture;
    let page = page.clone();

    let mut steps: Vec<Step> = Vec::new();
    let mut seq: u64 = 0;

    // The child announcing itself is the only evidence a document really
    // arrived in the frame AND that our code is inside it. An iframe element
    // with a body is a different claim, and `iframe.src` is not a claim at all.
    let _ = dispatch(&request(
        "wait",
        json!({
            "page_id": page,
            "until": { "js": "window.__ychromeFrames && window.__ychromeFrames.length > 0" },
            "timeout_ms": CALL_TIMEOUT_MS,
        }),
    ));

    // ---- 1. the child really is cross-origin -----------------------------
    let reach = read(
        &page,
        "(() => { try { return String(window.frames[0].document.title); } \
         catch (e) { return 'THREW:' + e.name; } })()",
    );
    let frame_count = read(&page, "window.frames.length");
    steps.push(Step {
        name: "the child frame is genuinely cross-origin: the top document CANNOT read it",
        pass: frame_count == json!(1) && reach.as_str().unwrap_or("").starts_with("THREW:"),
        detail: json!({ "frames": frame_count, "top_frame_reach": reach,
                        "merchant_hits": merchant_hits.lock().map(|l| l.len()).unwrap_or(0),
                        "bank_hits": bank_hits.lock().map(|l| l.len()).unwrap_or(0) }),
    });

    // ---- 2. an @all-frames userscript runs INSIDE it ----------------------
    let announced = read(&page, "JSON.stringify(window.__ychromeFrames || null)");
    let seen: Value = announced
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null);
    let child_announced = seen
        .as_array()
        .map(|rows| {
            rows.iter().any(|row| {
                row["index"] == json!(0) && row["origin"].as_str() == Some(bank_base.as_str())
            })
        })
        .unwrap_or(false);
    steps.push(Step {
        name: "an @all-frames userscript RUNS INSIDE the cross-origin child",
        pass: child_announced,
        detail: json!({ "announced": seen, "expected_origin": bank_base }),
    });

    // ---- 3. THE MUTATION CONTROL -----------------------------------------
    // Same world, same @match, no @all-frames. If this global is visible in the
    // child too, then something OTHER than the flag put our code there and
    // step 2 proves nothing.
    seq += 1;
    let child_toponly = frame_call(&page, seq, 0, "typeof window.__ychromeTopOnly");
    let top_toponly = read(&page, "typeof window.__ychromeTopOnly");
    let control_ok =
        top_toponly == json!("number") && child_toponly["json"] == json!("\"undefined\"");
    steps.push(Step {
        name: "MUTATION CONTROL: the same script WITHOUT @all-frames does not reach the child",
        pass: control_ok,
        detail: json!({ "in_top": top_toponly, "in_child": child_toponly }),
    });

    // ---- 4. read the child's own DOM --------------------------------------
    seq += 1;
    let who = frame_call(&page, seq, 0, "document.getElementById('who').textContent");
    steps.push(Step {
        name: "the UI process can READ the cross-origin child's DOM, with no web-process extension",
        pass: who["json"] == json!("\"BANK OF FIXTURE\""),
        detail: who.clone(),
    });

    // ---- 5. install a listener inside the child ---------------------------
    // The bank page is inert HTML; this listener is OURS, placed from outside,
    // which is what makes step 7's `isTrusted` reading possible without the
    // fixture cooperating.
    seq += 1;
    let armed = frame_call(
        &page,
        seq,
        0,
        "(() => { window.__probeClick = null; \
         document.addEventListener('click', (e) => { window.__probeClick = \
         { trusted: e.isTrusted, target: e.target && e.target.id }; }, true); \
         return 'armed'; })()",
    );
    steps.push(Step {
        name: "the UI process can INSTALL a listener inside the child",
        pass: armed["json"] == json!("\"armed\""),
        detail: armed.clone(),
    });

    // ---- 6. measure an element's rect inside the child --------------------
    seq += 1;
    let rect = frame_call(
        &page,
        seq,
        0,
        "(() => { const r = document.getElementById('otp').getBoundingClientRect(); \
         return { left: r.left, top: r.top, w: r.width, h: r.height }; })()",
    );
    let child_rect: Value = rect["json"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null);
    let frame_box: Value = read(
        &page,
        "(() => { const f = document.getElementById('pay'); const r = f.getBoundingClientRect(); \
         return JSON.stringify({ left: r.left + f.clientLeft, top: r.top + f.clientTop, \
         w: r.width, h: r.height }); })()",
    )
    .as_str()
    .and_then(|text| serde_json::from_str(text).ok())
    .unwrap_or(Value::Null);
    let measured = child_rect["w"].as_f64().unwrap_or(0.0) > 0.0
        && frame_box["w"].as_f64().unwrap_or(0.0) > 0.0;
    steps.push(Step {
        name: "the UI process can MEASURE an element inside the child",
        pass: measured,
        detail: json!({ "child_rect": child_rect, "iframe_box": frame_box }),
    });

    // ---- 7. a REAL click, aimed by that rect, lands in the child -----------
    //
    // ⭐ This is the step that makes the whole thing a payment-driving
    // capability rather than a reading trick. `/engine/input` was never the
    // missing piece — WebKit already hit-tests a real `GdkEvent` through the
    // frame tree. What was missing is the COORDINATE, because selector
    // resolution runs in the top document and cannot see into the child. A rect
    // measured inside the child plus the iframe's own rect is that coordinate.
    let point_x = frame_box["left"].as_f64().unwrap_or(0.0)
        + child_rect["left"].as_f64().unwrap_or(0.0)
        + child_rect["w"].as_f64().unwrap_or(0.0) / 2.0;
    let point_y = frame_box["top"].as_f64().unwrap_or(0.0)
        + child_rect["top"].as_f64().unwrap_or(0.0)
        + child_rect["h"].as_f64().unwrap_or(0.0) / 2.0;
    let (click_status, click_body) = call(
        "input",
        json!({ "page_id": page,
                "events": [{ "type": "click", "x": point_x, "y": point_y }] }),
    );
    // The target is asserted, not just the trustedness: an untranslated point
    // would still be inside the frame and would still raise a trusted click —
    // on `#decoy`, which is there to catch exactly that.
    let (landed_ok, landed) = frame_call_until(
        &page,
        &mut seq,
        0,
        "window.__probeClick",
        &json!({ "trusted": true, "target": "otp" }),
    );
    seq += 1;
    let focused = frame_call(
        &page,
        seq,
        0,
        "document.activeElement && document.activeElement.id",
    );
    steps.push(Step {
        name: "a REAL GdkEvent click, aimed by the child's own rect, lands in the child (isTrusted)",
        pass: click_status == 200 && landed_ok && focused["json"] == json!("\"otp\""),
        detail: json!({ "point": { "x": point_x, "y": point_y }, "input": click_body,
                        "child_click": landed, "child_activeElement": focused }),
    });

    // ---- 8. typed text arrives in the child's own input --------------------
    let (type_status, type_body) = call(
        "input",
        json!({ "page_id": page, "events": [{ "type": "type", "text": "424242" }] }),
    );
    let (typed_ok, typed) = frame_call_until(
        &page,
        &mut seq,
        0,
        "document.getElementById('otp').value",
        &json!("424242"),
    );
    steps.push(Step {
        name: "typed text arrives in the CHILD's input, read back from inside the child",
        pass: type_status == 200 && typed_ok,
        detail: json!({ "input": type_body, "child_value": typed }),
    });

    let _ = dispatch(&request("close", json!({ "page_id": page })));

    let pass = steps.iter().all(|step| step.pass);
    for step in &steps {
        crate::daemon::journal("engine.frames.step", step.to_json());
    }
    let report = json!({
        "pass": pass,
        "mechanism": "UserContentInjectedFrames::AllFrames userscript + cross-origin postMessage",
        "web_process_extension_required": !pass,
        "merchant": merchant_base,
        "bank": bank_base,
        "steps": steps.iter().map(Step::to_json).collect::<Vec<_>>(),
        "elapsed_ms": started.elapsed().as_millis(),
    });
    crate::daemon::journal("engine.frames.report", report.clone());
    Ok(report)
}

// ---------------------------------------------------------------------------
// Probe 2: is `postMessage` delivery WORLD-SCOPED? (`ychrome engine worlds`)
// ---------------------------------------------------------------------------
//
// The frames proof above settled that we CAN reach into a cross-origin child.
// This one settles what the `frame=` verb is ALLOWED to carry, which is a
// different question and has a security answer rather than a capability one:
//
// > Does a `message` event posted between frames reach listeners in an ISOLATED
// > world, and can the page's own MAIN world observe the same event?
//
// Worlds isolate *globals*; they are not documented to isolate *event
// dispatch*, and in Chromium `postMessage` is explicitly not world-scoped —
// which is why extension content scripts need a separate messaging API. If
// WebKitGTK agrees, then a bridge in the engine's isolated world is still
// speaking on a channel the page can hear and forge, and:
//
// - a token in the script body is NOT a secret, because whichever document
//   receives our message reads the token straight out of it; and
// - a bridge that evaluates a page-supplied string is a cross-origin
//   escalation WE would be providing — a hostile top page could make our code
//   run inside the bank's frame.
//
// ⇒ the verb must then expose a CLOSED set (resolve a selector to text / value
// / rect, set a value, focus) and no `eval` path at all.

/// One instantiation of [`WORLD_PROBE_TEMPLATE`].
fn world_probe(label: &str, world: &str, token: &str) -> String {
    WORLD_PROBE_TEMPLATE
        .replace("{LABEL}", label)
        .replace("{WORLD}", world)
        .replace("{TOKEN}", token)
        .replace("{ISO_CHANNEL}", ISO_CHANNEL)
        .replace("{MAIN_CHANNEL}", MAIN_CHANNEL)
}

/// The `from` tags a recorder heard, decoded from the attribute text it wrote.
///
/// Takes the attribute's own text (a JSON array), NOT the wire wrapper — the
/// caller unwraps the child's `JSON.stringify` first, because a value that came
/// back through the bridge is encoded once more than one read locally.
fn heard_tags(attribute_text: &Value) -> Vec<String> {
    attribute_text
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .as_ref()
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The value a bridge reply carries, unwrapped from the child's
/// `JSON.stringify`.
fn child_value(reply: &Value) -> Value {
    reply["json"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null)
}

/// Read a probe's `data-` attribute out of the TOP document. `/engine/eval`
/// runs in the main world, and the DOM is shared, so this reaches the isolated
/// probe's marker as readily as the main one's.
fn top_attr(page: &str, label: &str, kind: &str) -> Value {
    read(
        page,
        &format!("document.documentElement.getAttribute('data-ychrome-{label}-{kind}')"),
    )
}

/// The same attribute, read from INSIDE the cross-origin child through the
/// bridge proved by [`run`]. The top document cannot read the child's DOM —
/// that is step 1 of the other proof — so this is the only way to see what the
/// child's two recorders heard.
fn child_attr(page: &str, seq: &mut u64, label: &str, kind: &str) -> Value {
    *seq += 1;
    let reply = frame_call(
        page,
        *seq,
        0,
        &format!("document.documentElement.getAttribute('data-ychrome-{label}-{kind}')"),
    );
    child_value(&reply)
}

pub fn run_worlds() -> Result<Value> {
    let started = Instant::now();
    let token = run_token();

    // ⛔ BEFORE the fixture, and that is not a style preference. A profile's
    // identity is built on its first page and cached forever after, and
    // `window.webkit.messageHandlers` is populated from the manager's
    // registrations as a document is created — so a channel armed after
    // `open_fixture` would exist on nothing the fixture can see, and every
    // reading below would be a false negative about the substrate.
    super::identity::arm_message_channel(ISO_CHANNEL, crate::userscript::ScriptWorld::Isolated);
    super::identity::arm_message_channel(MAIN_CHANNEL, crate::userscript::ScriptWorld::Main);

    let _guard = InstalledProbe;
    // The bridge is the READ INSTRUMENT here, not the thing under test: the
    // child's markers live in the child's DOM, which the top document cannot
    // read. It is main-world on purpose — a bridge in the isolated world would
    // be unreadable by `/engine/eval` for precisely the reason being measured,
    // so the instrument would inherit the unknown it exists to resolve.
    crate::webpolicy::install_userscript(BRIDGE_STEM, &BRIDGE_TEMPLATE.replace("{TOKEN}", &token))
        .context("installing the frame-bridge read instrument")?;
    crate::webpolicy::install_userscript(
        WORLD_ISO_STEM,
        &world_probe(ISO_LABEL, "isolated", &token),
    )
    .context("installing the isolated-world probe userscript")?;
    crate::webpolicy::install_userscript(
        WORLD_MAIN_STEM,
        &world_probe(MAIN_LABEL, "main", &token),
    )
    .context("installing the main-world probe userscript")?;

    let fixture = open_fixture()?;
    let page = fixture.page.clone();

    let mut steps: Vec<Step> = Vec::new();
    let mut seq: u64 = 0;

    // Both probes post at `load` and stamp `posted`; the bridge's child half
    // announces itself. Waiting on all three is what makes a silence below a
    // measurement rather than a race.
    let _ = dispatch(&request(
        "wait",
        json!({
            "page_id": page,
            "until": { "js": format!(
                "window.__ychromeFrames && window.__ychromeFrames.length > 0 && \
                 document.documentElement.getAttribute('data-ychrome-{ISO_LABEL}-posted') === '1' && \
                 document.documentElement.getAttribute('data-ychrome-{MAIN_LABEL}-posted') === '1'") },
            "timeout_ms": CALL_TIMEOUT_MS,
        }),
    ));
    // Delivery of a `message` is a task, not a synchronous call, so a settle is
    // owed after the posts land — and a silence is only honest once it has had
    // the same chance to speak that a heard event had.
    std::thread::sleep(Duration::from_millis(1200));

    // ---- W1. the two worlds really ARE separate ---------------------------
    // Without this, every reading below could be one world talking to itself.
    // `/engine/eval` runs in the main world, so the main probe's global must be
    // visible to it and the isolated probe's must not.
    let main_global = read(&page, &format!("typeof window.__ychromeWorld_{MAIN_LABEL}"));
    let iso_global = read(&page, &format!("typeof window.__ychromeWorld_{ISO_LABEL}"));
    let worlds_separate = main_global == json!("number") && iso_global == json!("undefined");
    steps.push(Step {
        name: "the two probes really are in DIFFERENT worlds: globals do not cross",
        pass: worlds_separate,
        detail: json!({ "main_world_global_from_eval": main_global,
                        "isolated_world_global_from_eval": iso_global }),
    });

    // ---- W2. both probes ran, in both frames ------------------------------
    let iso_ran_top = top_attr(&page, ISO_LABEL, "ran");
    let main_ran_top = top_attr(&page, MAIN_LABEL, "ran");
    let iso_ran_child = child_attr(&page, &mut seq, ISO_LABEL, "ran");
    let main_ran_child = child_attr(&page, &mut seq, MAIN_LABEL, "ran");
    let both_present = iso_ran_top == json!("top")
        && main_ran_top == json!("top")
        && iso_ran_child == json!("child")
        && main_ran_child == json!("child");
    steps.push(Step {
        name: "both probes ran in BOTH frames, so a silence below is a silence and not an absence",
        pass: both_present,
        detail: json!({ "iso": { "top": iso_ran_top, "child": iso_ran_child },
                        "main": { "top": main_ran_top, "child": main_ran_child } }),
    });

    // ---- W3. the instrument can hear anything at all ----------------------
    // The main probe posted to its own window and to the child. If its own
    // main-world listeners heard neither, nothing below distinguishes "worlds
    // are partitioned" from "this fixture never delivered a message".
    let main_heard_top = heard_tags(&top_attr(&page, MAIN_LABEL, "heard"));
    let main_heard_child = heard_tags(&child_attr(&page, &mut seq, MAIN_LABEL, "heard"));
    let instrument_live = main_heard_top.iter().any(|tag| tag == "main-self")
        && main_heard_child.iter().any(|tag| tag == "main-into-child");
    steps.push(Step {
        name: "CONTROL: a message DOES arrive — same-world, same-document AND cross-frame",
        pass: instrument_live,
        detail: json!({ "top_main_heard": main_heard_top, "child_main_heard": main_heard_child }),
    });

    // ---- W4. the answer -----------------------------------------------------
    let iso_heard_top = heard_tags(&top_attr(&page, ISO_LABEL, "heard"));
    let iso_heard_child = heard_tags(&child_attr(&page, &mut seq, ISO_LABEL, "heard"));

    // Can our own bridge work at all from the isolated world?
    let isolated_receives_cross_frame = iso_heard_child.iter().any(|tag| tag == "iso-into-child");
    // ⛔ Can the PAGE hear what we send it? This is the one that decides the verb.
    let main_observes_our_post = main_heard_child.iter().any(|tag| tag == "iso-into-child")
        || main_heard_top.iter().any(|tag| tag == "iso-self");
    // ⛔ Can the PAGE drive our bridge? The forgery half of the same coin.
    let isolated_hears_page_post = iso_heard_child.iter().any(|tag| tag == "main-into-child")
        || iso_heard_top.iter().any(|tag| tag == "main-self");

    let world_scoped = !main_observes_our_post && !isolated_hears_page_post;
    let verb_shape = if world_scoped && isolated_receives_cross_frame {
        "eval-permitted: delivery is world-partitioned, so a per-page token is a real secret"
    } else {
        "closed-verb-set: postMessage is page-observable and page-forgeable, so no eval path"
    };
    steps.push(Step {
        name: "MEASURED: whether `message` delivery is world-scoped (reported, not asserted)",
        pass: true,
        detail: json!({
            "isolated_world_receives_cross_frame": isolated_receives_cross_frame,
            "page_main_world_observes_our_post": main_observes_our_post,
            "isolated_world_hears_a_page_post": isolated_hears_page_post,
            "top_iso_heard": iso_heard_top,
            "child_iso_heard": iso_heard_child,
        }),
    });

    // ---- W5-W7. the private reply channel ----------------------------------
    //
    // Probe 2's answer above forbids an `eval` path and forces the bridge to be
    // read-only. It does NOT settle where a read's ANSWER travels, and the
    // whole design rests on that: if the reply has to come back across
    // `postMessage`, the top page hears every answer about the child's origin
    // and even a read verb is a same-origin-policy bypass we would be
    // providing. The claim being checked is that a frame can speak to the UI
    // PROCESS instead, over a handler registered in the engine's own world.
    let registrations = super::identity::channel_registrations();
    let registered = |name: &str| {
        registrations
            .iter()
            .any(|row| row["name"] == json!(name) && row["registered"] == json!(true))
    };
    steps.push(Step {
        name: "INSTRUMENT: both message channels were REGISTERED before the page loaded",
        pass: registered(ISO_CHANNEL) && registered(MAIN_CHANNEL),
        detail: json!({ "registrations": registrations,
                        "isolated_world_channel": ISO_CHANNEL,
                        "main_world_channel": MAIN_CHANNEL }),
    });

    // What each world SAW, in each frame, at each phase — the typeof of the
    // handler object. `undefined` here and no delivery below are one fact; a
    // handler that was visible and still delivered nothing is a different one.
    let mut visibility = json!({});
    for label in [ISO_LABEL, MAIN_LABEL] {
        for key in ["iso", "main"] {
            for phase in ["start", "load"] {
                let kind = format!("chan-{key}-{phase}");
                visibility[format!("{label}.top.{kind}")] = top_attr(&page, label, &kind);
                visibility[format!("{label}.child.{kind}")] =
                    child_attr(&page, &mut seq, label, &kind);
            }
        }
    }

    let delivered = super::identity::delivered_messages();
    let arrived = |channel: &str, world: &str, frame: &str| {
        delivered.iter().any(|row| {
            row["channel"] == json!(channel)
                && row["payload"]["world"] == json!(world)
                && row["payload"]["frame"] == json!(frame)
        })
    };

    // The control: a script message handler works AT ALL in this process, from
    // the world it was registered in. Without it, silence on the isolated
    // channel would not distinguish "the isolated world cannot" from "message
    // handlers are not wired here".
    let main_channel_from_top = arrived(MAIN_CHANNEL, MAIN_LABEL, "top");
    // The second control, and it separates the FRAME variable from the WORLD
    // one: a cross-origin child reaching the UI process on a main-world handler.
    let main_channel_from_child = arrived(MAIN_CHANNEL, MAIN_LABEL, "child");
    steps.push(Step {
        name: "CONTROL: a script message handler DOES reach the UI process (main world, top frame)",
        pass: main_channel_from_top,
        detail: json!({ "main_channel_from_top": main_channel_from_top,
                        "main_channel_from_cross_origin_child": main_channel_from_child }),
    });

    // ⭐ THE ANSWER.
    let iso_channel_from_child = arrived(ISO_CHANNEL, ISO_LABEL, "child");
    let iso_channel_from_top = arrived(ISO_CHANNEL, ISO_LABEL, "top");
    // ⛔ The security half: can the PAGE's own world call the handler we
    // registered in ours? If it can, the reply path is not private and a forged
    // read gets its answer after all.
    let page_reaches_isolated_channel =
        arrived(ISO_CHANNEL, MAIN_LABEL, "top") || arrived(ISO_CHANNEL, MAIN_LABEL, "child");
    let private_reply_channel = iso_channel_from_child && !page_reaches_isolated_channel;
    let reply_path = if private_reply_channel {
        "ui-process: a cross-origin child can answer on a handler the page cannot see"
    } else if iso_channel_from_child {
        "NOT PRIVATE: the channel reaches, but the page's own world can call it too"
    } else {
        "NO UI-PROCESS REPLY PATH from a cross-origin child — the design must change"
    };
    steps.push(Step {
        name: "MEASURED: whether an ISOLATED-world message handler reaches the UI process \
               from a CROSS-ORIGIN child (reported, not asserted)",
        pass: true,
        detail: json!({
            "isolated_channel_from_cross_origin_child": iso_channel_from_child,
            "isolated_channel_from_top": iso_channel_from_top,
            "page_main_world_reaches_the_isolated_channel": page_reaches_isolated_channel,
            "main_channel_from_top": main_channel_from_top,
            "main_channel_from_cross_origin_child": main_channel_from_child,
            "handler_visibility_by_world_frame_phase": visibility,
            "delivered": delivered,
            // ⚠ Not a caveat, a limit: `script-message-received` hands back the
            // UserContentManager — not the WebView and not the frame — so every
            // `frame`/`origin` above is the SENDER'S OWN CLAIM. A verb may read
            // such a claim; one that ROUTED on it would be trusting a page's
            // word about which document it is.
            "frame_identity_is_self_claimed": true,
        }),
    });

    let _ = dispatch(&request("close", json!({ "page_id": page })));

    // ⭐ The gate asserts the INSTRUMENT, never the answer. Both answers are
    // real findings; a fixture that stopped delivering messages is not.
    let pass = steps.iter().all(|step| step.pass);
    for step in &steps {
        crate::daemon::journal("engine.worlds.step", step.to_json());
    }
    let report = json!({
        "pass": pass,
        "question": "does a `message` posted between frames reach an ISOLATED-world \
                     listener, and can the page's own main world observe it?",
        "postmessage_delivery_is_world_scoped": world_scoped,
        "isolated_world_receives_cross_frame": isolated_receives_cross_frame,
        "page_main_world_observes_our_post": main_observes_our_post,
        "isolated_world_hears_a_page_post": isolated_hears_page_post,
        "verb_shape": verb_shape,
        "isolated_channel_from_cross_origin_child": iso_channel_from_child,
        "page_main_world_reaches_the_isolated_channel": page_reaches_isolated_channel,
        "private_reply_channel": private_reply_channel,
        "reply_path": reply_path,
        "merchant": fixture.merchant_base,
        "bank": fixture.bank_base,
        "steps": steps.iter().map(Step::to_json).collect::<Vec<_>>(),
        "elapsed_ms": started.elapsed().as_millis(),
    });
    crate::daemon::journal("engine.worlds.report", report.clone());
    Ok(report)
}

// ---------------------------------------------------------------------------
// Probe 3: the SHIPPED verb (`ychrome engine frame-verb`)
// ---------------------------------------------------------------------------

/// Drive `/engine/frame` and `/engine/input frame=` against the same fixture the
/// two probes above measured the substrate with.
///
/// ⭐ **This installs NO probe script.** That is the point: the bridge under test
/// is the one `identity::build` attaches to every engine profile, and the reply
/// channel is the one it arms. If either were missing, every read below would
/// time out — so a green run is evidence about the SHIPPED path and not about an
/// instrument set up beside it.
///
/// The same fixture, deliberately. Its geometry is load-bearing twice over: the
/// `border:0` iframe makes the coordinate translation exact, and the `#decoy`
/// band occupies precisely where an UNTRANSLATED point would land — so step 4
/// can state, from measured rects rather than from assumed layout, that
/// forgetting the frame's own offset would click the wrong element.
pub fn run_verb() -> Result<Value> {
    let started = Instant::now();

    // A probe script left behind by a killed run is an `@all-frames` bridge with
    // an `eval` in it, and it would be live in the very frames this drives. The
    // guard's whole job is to clear every stem it knows; running it up front
    // means this proof starts from a page carrying our SHIPPED code and nothing
    // else.
    drop(InstalledProbe);

    let fixture = open_fixture()?;
    let page = fixture.page.clone();
    let mut steps: Vec<Step> = Vec::new();

    let frame_read = |selector: &str| -> (u16, Value) {
        call(
            "frame",
            json!({ "page_id": page, "frame": "#pay", "selector": selector }),
        )
    };

    // ---- 1. INSTRUMENT: the child really is cross-origin -------------------
    let reach = read(
        &page,
        "(() => { try { return String(window.frames[0].document.title); } \
         catch (e) { return 'THREW:' + e.name; } })()",
    );
    steps.push(Step {
        name: "INSTRUMENT: the child frame is genuinely cross-origin, so every read below is one \
               the top document could not have made",
        pass: reach.as_str().unwrap_or("").starts_with("THREW:"),
        detail: json!({ "top_frame_reach": reach }),
    });

    // ---- 2. INSTRUMENT: the page cannot see the reply channel --------------
    //
    // ⛔ THE SECURITY PROPERTY, MEASURED ON THE SHIPPED CHANNEL RATHER THAN ON
    // A PROBE'S. `/engine/eval` runs in the page's own world; if the handler
    // were visible there, a forged read would get its answer and the whole
    // read-only design would be decoration.
    let page_sees_channel = read(
        &page,
        &format!(
            "(() => {{ const h = window.webkit && window.webkit.messageHandlers; \
             return typeof (h && h.{channel}); }})()",
            channel = super::frame::CHANNEL
        ),
    );
    steps.push(Step {
        name: "INSTRUMENT: the PAGE's own world cannot see the reply channel",
        pass: page_sees_channel == json!("undefined"),
        detail: json!({ "typeof_from_page_world": page_sees_channel,
                        "channel": super::frame::CHANNEL }),
    });

    // ---- 3. the verb READS the child's own DOM ----------------------------
    let (who_status, who) = frame_read("#who");
    steps.push(Step {
        name: "/engine/frame READS the cross-origin child's own DOM",
        pass: who_status == 200 && who["text"] == json!("BANK OF FIXTURE"),
        detail: who.clone(),
    });

    // ---- 4. the verb MEASURES, and the DECOY control ----------------------
    let (otp_status, otp) = frame_read("#otp");
    let (_, decoy) = frame_read("#decoy");
    let frame_top = otp["frame"]["box"]["top"].as_f64().unwrap_or(0.0);
    let frame_left = otp["frame"]["box"]["left"].as_f64().unwrap_or(0.0);
    let otp_centre_x = otp["rect"]["left"].as_f64().unwrap_or(0.0)
        + otp["rect"]["w"].as_f64().unwrap_or(0.0) / 2.0;
    let otp_centre_y =
        otp["rect"]["top"].as_f64().unwrap_or(0.0) + otp["rect"]["h"].as_f64().unwrap_or(0.0) / 2.0;
    let translated = otp["point"]["x"].as_f64().unwrap_or(-1.0) == frame_left + otp_centre_x
        && otp["point"]["y"].as_f64().unwrap_or(-1.0) == frame_top + otp_centre_y;
    // ⭐ Where an UNTRANSLATED point would land, expressed in the CHILD's own
    // coordinates: a top-document click at `otp_centre_y` corresponds to child
    // `otp_centre_y - frame_top`. The decoy band is there to catch it.
    let untranslated_in_child = otp_centre_y - frame_top;
    let decoy_top = decoy["rect"]["top"].as_f64().unwrap_or(0.0);
    let decoy_bottom = decoy_top + decoy["rect"]["h"].as_f64().unwrap_or(0.0);
    let decoy_catches = untranslated_in_child >= decoy_top && untranslated_in_child <= decoy_bottom;
    steps.push(Step {
        name: "the point is the child's rect PLUS the frame's own box — and an untranslated one \
               would land on the decoy",
        pass: otp_status == 200 && otp["on_target"] == json!(true) && translated && decoy_catches,
        detail: json!({ "otp": otp, "decoy_band": { "top": decoy_top, "bottom": decoy_bottom },
                        "untranslated_point_in_child_coords": untranslated_in_child,
                        "decoy_catches_an_untranslated_click": decoy_catches }),
    });

    // ---- 5. /engine/eval REFUSES frame=, by name --------------------------
    let (eval_status, eval_body) = call(
        "eval",
        json!({ "page_id": page, "frame": "#pay", "js": "document.title" }),
    );
    let refusal = eval_body["error"].as_str().unwrap_or("");
    steps.push(Step {
        name: "/engine/eval REFUSES frame= and names the read verb to use instead",
        pass: eval_status == 400 && refusal.contains("/engine/frame"),
        detail: eval_body.clone(),
    });

    // ---- 6. a frame nobody can address is refused by name ------------------
    let (missing_status, missing) = call(
        "frame",
        json!({ "page_id": page, "frame": "#no-such-frame", "selector": "#otp" }),
    );
    steps.push(Step {
        name: "a frame address that matches nothing is refused by name, not defaulted to frame 0",
        pass: missing_status == 400
            && missing["error"]
                .as_str()
                .unwrap_or("")
                .contains("no frame matches"),
        detail: missing.clone(),
    });

    // ---- 7. frame= on /engine/input lands a REAL click inside the child ----
    //
    // `#otp:focus` is the readback, and it is a selector rather than an
    // `activeElement` read on purpose: the frame verb resolves selectors, so
    // asking for one that only matches when focused proves the click landed
    // using the verb's own vocabulary — no second instrument to keep honest.
    let (click_status, click_body) = call(
        "input",
        json!({ "page_id": page,
                "events": [{ "type": "click", "frame": "#pay", "selector": "#otp" }] }),
    );
    let (_, focused) = frame_read("#otp:focus");
    steps.push(Step {
        name: "frame= on /engine/input lands a REAL click on the child's own field",
        pass: click_status == 200 && focused["exists"] == json!(true),
        detail: json!({ "input": click_body, "otp_is_focused": focused }),
    });

    // ---- 8. typed text arrives in the child's input ------------------------
    let (type_status, type_body) = call(
        "input",
        json!({ "page_id": page, "events": [{ "type": "type", "text": "424242" }] }),
    );
    let mut typed = json!(null);
    let mut typed_ok = false;
    for _ in 0..READ_TRIES {
        let (_, value) = frame_read("#otp");
        typed = value;
        if typed["value"] == json!("424242") {
            typed_ok = true;
            break;
        }
        std::thread::sleep(READ_SETTLE);
    }
    steps.push(Step {
        name: "typed text reads back FROM INSIDE the child, through the shipped verb",
        pass: type_status == 200 && typed_ok,
        detail: json!({ "input": type_body, "child_value": typed }),
    });

    let _ = dispatch(&request("close", json!({ "page_id": page })));

    let pass = steps.iter().all(|step| step.pass);
    for step in &steps {
        crate::daemon::journal("engine.frameverb.step", step.to_json());
    }
    let report = json!({
        "pass": pass,
        "verb": "/engine/frame + /engine/input frame=",
        "channel": super::frame::CHANNEL,
        "merchant": fixture.merchant_base,
        "bank": fixture.bank_base,
        "steps": steps.iter().map(Step::to_json).collect::<Vec<_>>(),
        "elapsed_ms": started.elapsed().as_millis(),
    });
    crate::daemon::journal("engine.frameverb.report", report.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge must declare the two placements the whole finding rests on.
    /// A bridge that silently lost `@all-frames` would still install, still run
    /// in the top frame, and report "the child never announced" — which reads
    /// as a substrate limit rather than as a typo.
    #[test]
    fn the_bridge_declares_all_frames_in_the_main_world() {
        let parsed = crate::userscript::parse(&BRIDGE_TEMPLATE.replace("{TOKEN}", "t"));
        assert!(
            parsed.all_frames,
            "the bridge must be injected into ALL frames"
        );
        assert_eq!(parsed.world, crate::userscript::ScriptWorld::Main);
    }

    /// The control must differ from the bridge in EXACTLY the flag under test.
    /// If it drifted into the isolated world, step 3 would pass for the wrong
    /// reason and the mutation proof would be worthless.
    #[test]
    fn the_control_differs_from_the_bridge_in_exactly_one_flag() {
        let control = crate::userscript::parse(TOPONLY_SCRIPT);
        let bridge = crate::userscript::parse(&BRIDGE_TEMPLATE.replace("{TOKEN}", "t"));
        assert!(!control.all_frames, "the control must be top-frame only");
        assert_eq!(
            control.world, bridge.world,
            "same world, or it is not a control"
        );
        assert_eq!(
            control.matches, bridge.matches,
            "same @match, or it is not a control"
        );
    }

    /// A command without this run's token must be ignored. A copy of the script
    /// left behind by a killed run is then inert to any web page, which cannot
    /// read the file the token lives in.
    #[test]
    fn the_bridge_refuses_a_command_that_does_not_carry_the_run_token() {
        let body = BRIDGE_TEMPLATE.replace("{TOKEN}", "t");
        assert!(
            body.contains("data.token !== TOKEN"),
            "both halves of the bridge must gate on the run token"
        );
        assert_eq!(
            body.matches("data.token !== TOKEN").count(),
            2,
            "the top half and the child half each need the gate"
        );
    }

    /// A command carries script text, so it must go to the frame's known origin.
    /// `postMessage(..., "*")` would hand it to whatever occupies the slot.
    #[test]
    fn a_command_is_addressed_to_the_frames_own_origin_never_a_wildcard() {
        let body = BRIDGE_TEMPLATE.replace("{TOKEN}", "t");
        assert!(body.contains("js: js }, known.origin)"));
    }

    /// Two tokens from one process must differ, or a second run could act on a
    /// first run's leftover listener.
    #[test]
    fn a_run_token_is_not_a_constant() {
        assert_ne!(run_token(), run_token());
    }

    /// ⭐ The world probe's whole claim is "same script, different world". If
    /// the two instantiations differ anywhere else, a difference in what they
    /// hear is not attributable to the world and the measurement is void.
    #[test]
    fn the_two_world_probes_differ_in_exactly_the_world() {
        let iso = crate::userscript::parse(&world_probe(ISO_LABEL, "isolated", "t"));
        let main = crate::userscript::parse(&world_probe(MAIN_LABEL, "main", "t"));
        assert_eq!(iso.world, crate::userscript::ScriptWorld::Isolated);
        assert_eq!(main.world, crate::userscript::ScriptWorld::Main);
        assert_eq!(iso.matches, main.matches, "same @match, or it is not a pair");
        assert!(
            iso.all_frames && main.all_frames,
            "both must reach the child, or only one of them could ever hear a cross-frame post"
        );
        // Under ONE neutral label, the two bodies must be byte-identical apart
        // from the `@world` metadata line — which is where `{WORLD}` lives, and
        // the count below holds it to being the ONLY place it lives.
        //
        // This used to compare the two bodies after replacing each world's own
        // WORD with a placeholder, and that broke the moment the script had a
        // legitimate reason to contain the text "main" (the channel table). A
        // comparison whose mechanism depends on a word never appearing in the
        // body under test will keep breaking honestly-made edits, so it now
        // cuts the one line instead of rewriting every match of a word.
        let without_world_line = |body: &str| {
            body.lines()
                .filter(|line| !line.trim_start().starts_with("// @world"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(
            without_world_line(&world_probe("L", "isolated", "t")),
            without_world_line(&world_probe("L", "main", "t")),
            "with the label held equal the two probes must differ in the @world \
             line and nowhere else"
        );
        // And the world must be substituted in exactly one place, or "the same
        // script in two worlds" is a claim about more than one line.
        assert_eq!(WORLD_PROBE_TEMPLATE.matches("{WORLD}").count(), 1);
        // The cut line must be the one that carries the world, or the
        // comparison above is deleting something else and hiding a difference.
        assert!(
            world_probe("L", "isolated", "t")
                .lines()
                .any(|line| line.trim_start().starts_with("// @world")
                    && line.contains("isolated"))
        );
    }

    /// ⭐ A CHANNEL MUST BE ASKED FOR TWICE, OR A LATE HANDLER READS AS NO
    /// HANDLER.
    ///
    /// `window.webkit.messageHandlers` is expected to be populated as the
    /// document is created, but "expected" is what this entry keeps getting
    /// wrong. A probe that looked only at `document-start` would report a
    /// missing handler for one that merely arrived late — and a false negative
    /// here does not fail quietly: it sends the design back to a web-process
    /// extension for the reply path.
    #[test]
    fn the_channel_probe_looks_at_document_start_AND_at_load() {
        let body = world_probe(ISO_LABEL, "isolated", "t");
        assert!(body.contains("probeChannels(\"start\")"));
        assert!(body.contains("probeChannels(\"load\")"));
        // And the phase must reach the marker, or two looks are recorded as one.
        assert!(body.contains("mark(\"chan-\" + key + \"-\" + phase, typeof handler)"));
    }

    /// ⭐ THE TWO CHANNELS ARE THE CONTROL PAIR, AND THEY MUST BE IN DIFFERENT
    /// WORLDS — armed BEFORE the fixture opens.
    ///
    /// The measurement is "does an ISOLATED-world handler reach the UI process
    /// from a cross-origin child". If both channels were armed in the same
    /// world, the main-world one would stop being a control: a silence would no
    /// longer separate "the isolated world cannot" from "handlers do not work
    /// here". And a channel armed after `open_fixture` is registered on nothing
    /// the fixture can see, so every reading would be a false negative.
    #[test]
    fn the_probe_arms_one_isolated_channel_and_one_main_channel() {
        assert_ne!(ISO_CHANNEL, MAIN_CHANNEL);
        let body = include_str!("frames.rs")
            .split("pub fn run_worlds()")
            .nth(1)
            .expect("run_worlds must exist");
        let opens_at = body
            .find("open_fixture()")
            .expect("run_worlds opens the fixture");
        let arming = &body[..opens_at];
        assert!(
            arming.contains(
                "arm_message_channel(ISO_CHANNEL, crate::userscript::ScriptWorld::Isolated)"
            ),
            "the measured channel must be armed in the ISOLATED world, before the fixture"
        );
        assert!(
            arming.contains(
                "arm_message_channel(MAIN_CHANNEL, crate::userscript::ScriptWorld::Main)"
            ),
            "the control channel must be armed in the MAIN world, before the fixture"
        );
    }

    /// The recorder must write to the DOM, which both worlds share. A global
    /// would be invisible from outside its own world — which is the very thing
    /// under measurement, so it cannot also be the measuring instrument.
    #[test]
    fn a_world_probe_records_into_the_shared_dom_not_a_global() {
        let body = world_probe(ISO_LABEL, "isolated", "t");
        assert!(
            body.contains("document.documentElement.setAttribute"),
            "the recorder must land in the DOM"
        );
        assert!(
            body.contains("data-ychrome-\" + LABEL + \"-\" + kind"),
            "the marker must carry the probe's own label, or the two probes collide"
        );
    }

    /// The stem, the label and the attribute prefix are one identity. If a stem
    /// stopped naming its label, the run would install fine, record fine, and
    /// be read back under a name nothing ever wrote.
    #[test]
    fn each_world_probe_stem_names_its_own_label() {
        assert!(WORLD_ISO_STEM.contains(ISO_LABEL));
        assert!(WORLD_MAIN_STEM.contains(MAIN_LABEL));
        assert_ne!(ISO_LABEL, MAIN_LABEL);
    }

    /// The guard must clear every stem this module can install. A leftover
    /// `@all-frames` script with an `eval` in it is the one artefact here that
    /// must never outlive the run.
    #[test]
    fn the_guard_clears_every_stem_this_module_installs() {
        let source = include_str!("frames.rs");
        let guard = source
            .split("impl Drop for InstalledProbe")
            .nth(1)
            .expect("the guard must exist");
        for stem in ["BRIDGE_STEM", "TOPONLY_STEM", "WORLD_ISO_STEM", "WORLD_MAIN_STEM"] {
            assert!(
                guard[..guard.find("\n}").unwrap_or(guard.len())].contains(stem),
                "{stem} is installed by this module but not removed by the guard"
            );
        }
    }
}
