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

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::api::{dispatch, json_status as json_reply, request};
use super::gateway::{document, spawn_origin};

/// The profile every page in this proof opens under. Its own, so the run
/// touches no jar an agent or the user is using.
const PROFILE: &str = "engine-frames-proof";

/// The stem of the bridge userscript, installed through `webpolicy` (its owner)
/// and removed by [`InstalledProbe`].
const BRIDGE_STEM: &str = "ychrome-frame-bridge-probe";

/// The stem of the MUTATION CONTROL: the same world, the same match, and no
/// `@all-frames`. Without this, step 2 proves only "a userscript ran somewhere".
const TOPONLY_STEM: &str = "ychrome-frame-toponly-probe";

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

/// Both probe scripts, removed on EVERY exit path including a panic.
///
/// `webpolicy::install_userscript` writes into the shared userscript directory,
/// which is the user's real one when `HOME` is real. parity.rs sets the
/// precedent; the guard is this module's addition, because this script carries
/// an `eval` and a leftover copy is not the harmless global parity's probe
/// leaves behind.
struct InstalledProbe;

impl Drop for InstalledProbe {
    fn drop(&mut self) {
        let _ = crate::webpolicy::delete_userscript(BRIDGE_STEM);
        let _ = crate::webpolicy::delete_userscript(TOPONLY_STEM);
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

    let mut steps: Vec<Step> = Vec::new();
    let mut seq: u64 = 0;

    let (status, body) = call(
        "open",
        json!({ "url": format!("{merchant_base}/pay"), "profile": PROFILE }),
    );
    let page = body["page_id"].as_str().unwrap_or_default().to_string();
    if status != 200 || page.is_empty() {
        anyhow::bail!("the frames fixture would not open: {status} {body}");
    }

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
}
