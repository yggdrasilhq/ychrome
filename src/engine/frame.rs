//! `/engine/frame` — the READ-ONLY reach into a cross-origin child, and the
//! `frame=` translation that lets `/engine/input` drive one.
//!
//! `docs/pending-bugs.md` carried "THE ENGINE CANNOT REACH INTO A CROSS-ORIGIN
//! FRAME, AND THAT BLOCKED A PAYMENT" for two days while every mechanism it
//! needed was measured, one at a time, by [`super::frames`]. This module is what
//! those measurements bought. Each of its three parts exists because a
//! measurement said it had to, and the ones that came out the restrictive way
//! are the ones that shaped it most:
//!
//! | measured | consequence, here |
//! |---|---|
//! | an `@all-frames` userscript RUNS inside a cross-origin child | [`BRIDGE`] is how we get code in there at all |
//! | `postMessage` dispatch is **not** world-scoped | a command is page-observable and page-FORGEABLE ⇒ the bridge READS ONLY |
//! | `window.webkit.messageHandlers` **is** world-scoped | the reply goes to the UI process, where a forger cannot see it |
//! | `script-message-received` hands back the content manager, not the frame | a payload's `frame`/`origin` is a CLAIM ⇒ never route on one |
//!
//! ## The shape, in one paragraph
//!
//! A command goes OUT by `postMessage` from the top document, addressed to the
//! frame's own origin, and it carries a selector — so what leaks to whatever
//! occupies that frame is a selector, not a capability. The answer comes BACK on
//! [`CHANNEL`], a script message handler registered in the engine's own world,
//! which the page can neither see nor call. A forged read therefore costs the
//! forger nothing: it cannot hear the answer. And because the bridge has no
//! mutation op at all, there is no forged WRITE to worry about — a page cannot
//! use us to set a value in a document on another origin, which is the
//! same-origin-policy breach we would otherwise have been providing.
//!
//! ## Writing is not a bridge op
//!
//! It is a real `GdkEvent` aimed by a rect the bridge measured. `/engine/input`
//! was never missing the ability to reach a child — WebKit hit-tests a trusted
//! event through the frame tree already. It was missing the COORDINATE, because
//! selector resolution runs in the top document. [`click_target`] is that
//! coordinate: the element's rect inside the child, plus the iframe's own box in
//! the top document.
//!
//! ⛔ **`/engine/eval` must REFUSE `frame=`.** A bridge that evaluated a
//! page-supplied string would let a hostile TOP page make our code run inside
//! the bank's frame, and the token in the command is no defence — whichever
//! document receives the message reads the token straight out of it (measured).
//! The refusal lives in `api::route` and names this verb instead.
//!
//! ## The dispatcher runs in the engine's world, and that is not decoration
//!
//! `iframe.contentWindow`, `getBoundingClientRect` and `postMessage` are all
//! replaceable on the prototype by the document that owns them. Run in the
//! page's world, our own dispatcher could be handed a different window, a lying
//! rect, or a silently swallowed post — by the merchant page, about the bank's
//! frame. Run in [`super::identity::ISOLATED_WORLD`] it reads the real ones: a
//! world shares the DOM, never the intrinsics.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::host::Engine;
use crate::userscript::Userscript;

/// The content→UI-process channel the bridge answers on.
///
/// Registered in [`super::identity::ISOLATED_WORLD`] for every profile identity
/// the engine builds — see `identity::build`, which arms it before any page can
/// load, because `window.webkit.messageHandlers` is populated as a document is
/// created and a channel armed afterwards exists on nothing that document sees.
pub const CHANNEL: &str = "ychromeFrame";

/// How long a frame round trip may take before the verb answers with a timeout.
const DEFAULT_TIMEOUT_MS: u64 = 4000;

/// How often the reply mailbox is checked while the engine loop runs.
const POLL: Duration = Duration::from_millis(20);

/// The most text one read may carry back. A reply is a wire payload, not a
/// document dump; `text_length` is always the true length, so a caller can see
/// that it was cut rather than infer it.
///
/// Substituted into [`BRIDGE`] by [`bridge`], because a number written once in
/// Rust and again in the script is two numbers, and the one that drifts is the
/// one nobody reads.
const TEXT_CAP: usize = 4096;

/// The default frame selector, when a caller addresses a frame by index alone.
///
/// Both tags, because `<frame>` still exists in gateway-era markup and a bank
/// that serves one would otherwise be unaddressable by index.
const DEFAULT_FRAME_SELECTOR: &str = "iframe,frame";

/// The bridge: **read-only, isolated world, every frame, document-start.**
///
/// The placement is declared in the metadata block and nowhere else, so
/// `identity::attach_script` reads it out of the same parser every other
/// userscript goes through. A second placement decision in Rust is how a script
/// ends up in a world its channel was not registered in, silently.
///
/// ⛔ **There is no mutation op and there must never be one.** The commands this
/// listens for are forgeable by whatever document sits in the frame's embedder
/// (`postMessage` dispatch is not world-scoped — measured), so every op exposed
/// here is an op that page can invoke against another origin. A read it cannot
/// hear the answer to is worth nothing to it. A `set_value` would be worth
/// everything.
///
/// ⛔ **No `eval` path.** The probe bridge in [`super::frames`] has one, which is
/// exactly why that script is an instrument and may not be lifted into `src/`.
pub const BRIDGE: &str = r#"// ==UserScript==
// @name         ychrome-frame-bridge
// @version      1.0.0
// @match        *://*/*
// @all-frames   true
// @world        isolated
// @run-at       document-start
// ==/UserScript==
(() => {
  const CHANNEL = "ychromeFrame";
  const CAP = {TEXT_CAP};
  // SUBFRAMES ONLY. The top document already answers /engine/eval and
  // /engine/dom; a bridge that also answered for it would be a second way to
  // ask one question, and the two would drift.
  if (window.top === window) { return; }
  window.addEventListener("message", (event) => {
    const ask = event.data;
    if (!ask || typeof ask !== "object" || ask.ychromeFrameRead !== 1) { return; }
    if (typeof ask.seq !== "number" || typeof ask.nonce !== "string") { return; }
    const reply = { nonce: ask.nonce, seq: ask.seq, ok: false };
    try {
      const all = document.querySelectorAll(String(ask.selector));
      const nth = typeof ask.nth === "number" ? ask.nth : 0;
      reply.ok = true;
      reply.count = all.length;
      reply.exists = all.length > 0;
      reply.nth = nth;
      const el = all[nth];
      if (el) {
        const r = el.getBoundingClientRect();
        const rect = { left: r.left, top: r.top, w: r.width, h: r.height };
        const text = String((el.innerText !== undefined && el.innerText !== null)
          ? el.innerText : (el.textContent || ""));
        reply.tag = String(el.tagName || "").toLowerCase();
        reply.text = text.slice(0, CAP);
        reply.text_length = text.length;
        reply.text_truncated = text.length > CAP;
        // ⛔ A LENGTH, NEVER THE VALUE, for a password field — the same
        // boundary /engine/fill keeps. The limit is named rather than
        // pretended: a card NUMBER is an ordinary text input and is NOT
        // covered by this, because nothing in the DOM marks it as a secret.
        const secret = reply.tag === "input" &&
          String(el.type || "").toLowerCase() === "password";
        reply.value = (typeof el.value === "string" && !secret) ? el.value : null;
        reply.value_length = (typeof el.value === "string") ? el.value.length : null;
        reply.value_withheld = secret ? "password" : null;
        reply.rect = rect;
        reply.viewport = { w: window.innerWidth, h: window.innerHeight };
        reply.visible = rect.w > 0 && rect.h > 0;
        reply.in_viewport = rect.left >= 0 && rect.top >= 0 &&
          (rect.left + rect.w) <= window.innerWidth &&
          (rect.top + rect.h) <= window.innerHeight;
        // ⭐ THE HIT TEST, INSIDE THE CHILD. Without it a caller learns where an
        // element is and nothing about whether a click there reaches it — which
        // is the difference between aiming and landing, and the top document
        // cannot perform this test through a frame border.
        const cx = rect.left + rect.w / 2, cy = rect.top + rect.h / 2;
        const hit = (reply.visible && reply.in_viewport && document.elementFromPoint)
          ? document.elementFromPoint(cx, cy) : null;
        reply.on_target = !!(hit && (hit === el || el.contains(hit)));
        reply.hit_tag = hit ? String(hit.tagName || "").toLowerCase() : null;
        reply.hit_id = (hit && hit.id) ? String(hit.id) : null;
      }
    } catch (error) {
      reply.ok = false;
      reply.error = String(error);
    }
    // ⛔ TO THE UI PROCESS, never back across postMessage. The page can hear a
    // `message`; it cannot see this handler, which is registered in this world
    // and this world only (measured — `ychrome engine worlds`).
    try {
      window.webkit.messageHandlers[CHANNEL].postMessage(reply);
    } catch (ignored) {}
  });
})();
"#;

/// The bridge as a placed userscript, parsed from its own metadata block.
pub fn bridge() -> Userscript {
    crate::userscript::parse(&BRIDGE.replace("{TEXT_CAP}", &TEXT_CAP.to_string()))
}

/// This process's namespace for frame traffic, and the per-call counter.
///
/// ⚠ **The nonce is a NAMESPACE, not a secret, and calling it a token would
/// re-encode a belief that has already been measured false.** Whichever document
/// receives our command reads the nonce straight out of it, because
/// `postMessage` delivery is not world-scoped. What it buys is that a reply
/// cannot be confused with one from another process or another call — the
/// security property is elsewhere, in a reply channel the page cannot call.
fn nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        format!("ycf-{:x}-{:x}", std::process::id(), nanos)
    })
}

fn next_seq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Addressing a frame — by the CALLER's own words, never by the payload's claim
// ---------------------------------------------------------------------------

/// Which frame a caller means, expressed entirely in TOP-DOCUMENT terms.
///
/// ⛔ **A frame is addressed by the caller's selector and index and by nothing
/// else.** `script-message-received` hands back the `UserContentManager` — not
/// the WebView, not the frame — so the `frame` and `origin` in any payload are
/// the sender's own claim about which document it is. Routing on such a claim
/// would be trusting a page's word about its own identity. The `src` attribute
/// this reads instead is a fact of the TOP document, which is the one document
/// we can measure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameTarget {
    pub selector: String,
    pub nth: usize,
}

/// Read `frame` (+ `frame_nth`) out of a request body.
///
/// `frame` is a CSS selector when it is a string and an index into
/// [`DEFAULT_FRAME_SELECTOR`]'s matches when it is a number, because both
/// spellings are things a caller reading the page can see, and refusing the
/// short one would make the common single-frame case need a selector nobody
/// needs.
pub fn parse_target(body: &Value) -> Result<FrameTarget, String> {
    let requested = body.get("frame").unwrap_or(&Value::Null);
    let (selector, index) = match requested {
        Value::String(selector) if !selector.trim().is_empty() => (selector.trim().to_string(), 0),
        Value::Number(number) => match number.as_u64() {
            Some(index) => (DEFAULT_FRAME_SELECTOR.to_string(), index as usize),
            None => {
                return Err(format!(
                    "`frame` as a number is an index into the page's frames, so it must be a \
                     non-negative integer — got {number}"
                ));
            }
        },
        Value::Null => {
            return Err(
                "this needs a `frame`: a CSS selector for the frame element in the TOP document \
                 (\"#pay\"), or its index among the page's frames (0). A frame is addressed by \
                 what the top document can see, never by what a frame says it is"
                    .to_string(),
            );
        }
        other => {
            return Err(format!(
                "`frame` must be a selector string or a frame index — got {other}"
            ));
        }
    };
    let nth = match body.get("frame_nth") {
        None | Some(Value::Null) => index,
        Some(value) => match value.as_u64() {
            Some(nth) => nth as usize,
            None => {
                return Err(format!(
                    "`frame_nth` must be a non-negative integer, got {value}"
                ));
            }
        },
    };
    Ok(FrameTarget { selector, nth })
}

// ---------------------------------------------------------------------------
// The round trip
// ---------------------------------------------------------------------------

/// One resolved read: what the top document said about the frame, and what the
/// frame said about the element.
pub struct FrameRead {
    /// The frame as the TOP document measures it — box, addressed origin, tag.
    pub frame: Value,
    /// The child's own reply, correlation fields stripped.
    pub answer: Value,
    /// The top document's viewport, for the coordinate checks in
    /// [`click_target`].
    pub viewport: (f64, f64),
}

/// Ask a frame about a selector. **Read-only, by construction.**
///
/// Two hops: an isolated-world eval in the top document that measures the frame
/// and posts the command, then a wait on the reply channel. The status code
/// separates them — a 400 is the caller's request (no such frame, bad selector),
/// a 504 is the frame not answering, which on a real gateway usually means the
/// child navigated away from the `src` we addressed.
pub fn read(
    engine: &Engine,
    page: &str,
    target: &FrameTarget,
    selector: &str,
    nth: usize,
    timeout_ms: u64,
) -> Result<FrameRead, (u16, String)> {
    let seq = next_seq();
    let dispatch = engine
        .eval_in_world(
            page,
            &dispatch_js(target, selector, nth, nonce(), seq),
            Some(super::identity::ISOLATED_WORLD),
        )
        .map_err(|error| {
            (
                400,
                format!("the top document could not be measured: {error}"),
            )
        })?;

    if dispatch["ok"].as_bool() != Some(true) {
        return Err((400, dispatch_refusal(target, &dispatch)));
    }
    let viewport = (
        dispatch["viewport"]["w"].as_f64().unwrap_or(0.0),
        dispatch["viewport"]["h"].as_f64().unwrap_or(0.0),
    );

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut answer = loop {
        if let Some(payload) = super::identity::take_delivered(CHANNEL, |payload| {
            payload["nonce"] == json!(nonce()) && payload["seq"] == json!(seq)
        }) {
            break payload;
        }
        if Instant::now() >= deadline {
            return Err((
                504,
                format!(
                    "the frame did not answer within {timeout_ms}ms. The command was addressed to \
                     {origin} — if the child has navigated away from its `src`, the post was \
                     dropped by design and no bridge ever saw it",
                    origin = dispatch["target_origin"]
                ),
            ));
        }
        // The reply lands on the ENGINE thread, so the loop has to run for it to
        // arrive. Sleeping this thread would wait for a message that cannot be
        // delivered while we hold still.
        let _ = engine.settle(POLL);
    };
    if answer["ok"].as_bool() != Some(true) {
        let detail = answer["error"].as_str().unwrap_or("no reason given");
        return Err((400, format!("the frame refused {selector:?}: {detail}")));
    }
    // The correlation fields are ours, not the caller's: echoing them would
    // invite someone to route on a value the payload asserts.
    if let Some(object) = answer.as_object_mut() {
        object.remove("nonce");
        object.remove("seq");
    }

    Ok(FrameRead {
        frame: json!({
            "selector": target.selector,
            "nth": target.nth,
            "tag": dispatch["tag"],
            "box": dispatch["box"],
            "target_origin": dispatch["target_origin"],
            "frames": dispatch["frames"],
        }),
        answer,
        viewport,
    })
}

/// The whole `/engine/frame` reply, including the translated click point.
///
/// `point` is here rather than left to the caller because the arithmetic is the
/// one thing this verb knows that nobody else does, and because `/engine/input`
/// computes it through [`click_target`] — one owner, so a caller who clicks by
/// hand and a caller who passes `frame=` aim at the same pixel.
pub fn read_report(read: &FrameRead) -> Value {
    let mut report = json!({
        "ok": true,
        "frame": read.frame,
    });
    if let (Some(object), Some(answer)) = (report.as_object_mut(), read.answer.as_object()) {
        for (key, value) in answer {
            object.insert(key.clone(), value.clone());
        }
    }
    if let Some((x, y)) = translate(read) {
        report["point"] = json!({ "x": x, "y": y });
    }
    report
}

/// Resolve a selector INSIDE a frame to the viewport point a click must land on.
///
/// The refusal vocabulary is deliberately the top-document resolver's
/// (`target_moved`, `zero-size element`) plus the two conditions only a frame
/// has, so an agent carries one set of words across both paths.
pub fn click_target(
    engine: &Engine,
    page: &str,
    target: &FrameTarget,
    selector: &str,
    nth: usize,
    timeout_ms: u64,
) -> Result<(f64, f64, Value), (u16, String)> {
    let read = read(engine, page, target, selector, nth, timeout_ms)?;
    let answer = &read.answer;

    if answer["exists"].as_bool() != Some(true) {
        return Err((
            400,
            format!(
                "no element matches {selector:?} inside frame {frame:?}",
                frame = target.selector
            ),
        ));
    }
    if answer["nth"].as_u64() != Some(nth as u64) || answer["tag"].is_null() {
        return Err((
            400,
            format!(
                "no element matches {selector:?} at index {nth} inside frame {frame:?} — it has \
                 {count} match(es)",
                frame = target.selector,
                count = answer["count"]
            ),
        ));
    }
    if answer["visible"].as_bool() != Some(true) {
        return Err((400, format!("{selector:?} matched a zero-size element")));
    }
    // ⛔ NO SCROLLING. Scrolling a cross-origin child would be a mutation, and
    // every op this bridge exposes is one a hostile embedder can forge. So an
    // element the frame is not showing is refused BY NAME, with what the caller
    // can do about it, rather than reached for by moving someone else's document.
    if answer["in_viewport"].as_bool() != Some(true) {
        return Err((
            409,
            format!(
                "outside_frame_viewport ({selector:?} is scrolled out of the frame's own viewport, \
                 and the engine does not scroll a cross-origin child — drive the frame itself \
                 first, with a click or a key inside it)"
            ),
        ));
    }
    if answer["on_target"].as_bool() != Some(true) {
        let detail = match answer["hit_tag"].as_str() {
            Some(hit) => format!("`{hit}` is what receives a click there"),
            None => "nothing is painted there".to_string(),
        };
        return Err((
            409,
            format!(
                "target_moved (the resolved point no longer hits {selector:?} inside the frame — \
                 {detail})"
            ),
        ));
    }

    let Some((x, y)) = translate(&read) else {
        return Err((
            400,
            format!("{selector:?} resolved to no rect the frame's box could be added to"),
        ));
    };

    // ⛔ THE POINT MUST BE INSIDE THE FRAME IT WAS COMPUTED FOR. A child that
    // scrolls its own content can put an element's rect outside the iframe's
    // box, and a click dispatched there lands on the EMBEDDER — the merchant
    // page receiving a click meant for the bank. Refusing is the only honest
    // answer; the alternative is a click reported as landed in a frame it never
    // entered.
    let (left, top, w, h) = frame_box(&read.frame);
    if x < left || y < top || x > left + w || y > top + h {
        return Err((
            409,
            format!(
                "point_outside_frame (the resolved point ({x:.0},{y:.0}) is outside the frame's \
                 own box ({left:.0},{top:.0} {w:.0}x{h:.0}) — a click there would land on the \
                 EMBEDDING page, not in the frame)"
            ),
        ));
    }
    let (view_w, view_h) = read.viewport;
    if x < 0.0 || y < 0.0 || (view_w > 0.0 && x > view_w) || (view_h > 0.0 && y > view_h) {
        return Err((
            409,
            format!(
                "point_outside_viewport (the resolved point ({x:.0},{y:.0}) is off the page's own \
                 viewport ({view_w:.0}x{view_h:.0}) — the frame itself needs scrolling into view)"
            ),
        ));
    }

    let mut report = json!({
        "frame": read.frame,
        "selector": selector,
        "count": answer["count"],
        "nth": answer["nth"],
        "tag": answer["tag"],
        "x": x,
        "y": y,
    });
    report["ambiguous"] = json!(answer["count"].as_u64().unwrap_or(0) > 1);
    Ok((x, y, report))
}

/// The translated viewport point: the element's rect inside the child, plus the
/// frame's own content box in the top document.
///
/// ⭐ This addition IS the verb. Selector resolution runs in the top document
/// and stops at the frame border, so the coordinate was the missing half — never
/// the input, which WebKit already hit-tests through the frame tree.
fn translate(read: &FrameRead) -> Option<(f64, f64)> {
    let rect = &read.answer["rect"];
    let (left, top, _, _) = frame_box(&read.frame);
    let x = left + rect["left"].as_f64()? + rect["w"].as_f64()? / 2.0;
    let y = top + rect["top"].as_f64()? + rect["h"].as_f64()? / 2.0;
    Some((x, y))
}

fn frame_box(frame: &Value) -> (f64, f64, f64, f64) {
    let boxed = &frame["box"];
    (
        boxed["left"].as_f64().unwrap_or(0.0),
        boxed["top"].as_f64().unwrap_or(0.0),
        boxed["w"].as_f64().unwrap_or(0.0),
        boxed["h"].as_f64().unwrap_or(0.0),
    )
}

/// Turn the dispatcher's own refusal token into a sentence naming the frame.
///
/// PURE, so every refusal this verb can produce for a bad address is decided in
/// a unit test rather than only on a live page.
pub fn dispatch_refusal(target: &FrameTarget, dispatch: &Value) -> String {
    let selector = &target.selector;
    let nth = target.nth;
    match dispatch["refusal"].as_str().unwrap_or("") {
        "bad_frame_selector" => format!(
            "{selector:?} is not a valid CSS selector ({detail})",
            detail = dispatch["detail"].as_str().unwrap_or("no reason given")
        ),
        "no_such_frame" => format!(
            "no frame matches {selector:?} at index {nth} — the top document has {frames} \
             match(es)",
            frames = dispatch["frames"]
        ),
        "not_a_frame" => format!(
            "{selector:?} matched a <{tag}>, which owns no browsing context — a frame address must \
             resolve to an <iframe> or a <frame>",
            tag = dispatch["tag"].as_str().unwrap_or("?")
        ),
        "frame_has_no_addressable_origin" => format!(
            "the frame at {selector:?} has no `src` origin to address ({src}). A command is posted \
             to the frame's OWN origin, never to \"*\", so a srcdoc or about:blank frame cannot be \
             reached — and does not need to be, since it is same-origin with the page",
            src = dispatch["src"]
        ),
        "post_refused" => format!(
            "the command could not be posted into {selector:?} ({detail})",
            detail = dispatch["detail"].as_str().unwrap_or("no reason given")
        ),
        _ => format!("the frame at {selector:?} could not be addressed ({dispatch})"),
    }
}

/// The isolated-world dispatcher, built for one call.
///
/// Everything a caller supplies is interpolated through `serde_json`, so a
/// selector containing a quote is a string and not a syntax error — the same
/// rule `resolve_selector` follows for the top document.
fn dispatch_js(target: &FrameTarget, selector: &str, nth: usize, nonce: &str, seq: u64) -> String {
    let frame_selector = json!(target.selector);
    let frame_nth = target.nth;
    let selector = json!(selector);
    let nonce = json!(nonce);
    format!(
        r#"(() => {{
  let all;
  try {{ all = document.querySelectorAll({frame_selector}); }}
  catch (e) {{ return {{ ok: false, refusal: "bad_frame_selector", detail: String(e) }}; }}
  if ({frame_nth} >= all.length) {{
    return {{ ok: false, refusal: "no_such_frame", frames: all.length }};
  }}
  const el = all[{frame_nth}];
  const win = el.contentWindow;
  if (!win) {{
    return {{ ok: false, refusal: "not_a_frame",
              tag: String(el.tagName || "").toLowerCase() }};
  }}
  const src = el.getAttribute("src");
  let origin = null;
  if (src) {{ try {{ origin = new URL(src, location.href).origin; }} catch (e) {{ origin = null; }} }}
  if (!origin || origin === "null") {{
    return {{ ok: false, refusal: "frame_has_no_addressable_origin", src: src || null }};
  }}
  const r = el.getBoundingClientRect();
  const box = {{
    left: r.left + el.clientLeft, top: r.top + el.clientTop,
    w: el.clientWidth || r.width, h: el.clientHeight || r.height
  }};
  try {{
    win.postMessage({{ ychromeFrameRead: 1, nonce: {nonce}, seq: {seq},
                       selector: {selector}, nth: {nth} }}, origin);
  }} catch (e) {{
    return {{ ok: false, refusal: "post_refused", detail: String(e) }};
  }}
  return {{ ok: true, frames: all.length, nth: {frame_nth},
            tag: String(el.tagName || "").toLowerCase(), box: box, target_origin: origin,
            viewport: {{ w: window.innerWidth, h: window.innerHeight }} }};
}})()"#
    )
}

/// The timeout a request asked for, clamped to something a caller cannot use to
/// pin an engine thread open.
pub fn timeout_ms(body: &Value) -> u64 {
    body.get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(250, 30_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⭐ THE PLACEMENT IS THE SECURITY PROPERTY, AND IT LIVES IN THE METADATA.
    ///
    /// `@all-frames` is the only reason our code is inside a cross-origin child
    /// at all, and `@world isolated` is the only reason the reply channel is one
    /// the page cannot call. A bridge that silently lost either would still
    /// install and still run — the first in the top frame only (every read times
    /// out), the second in the page's own world (every reply becomes forgeable,
    /// and NOTHING at runtime says so).
    #[test]
    fn the_bridge_is_placed_in_every_frame_and_in_the_engines_own_world() {
        let parsed = bridge();
        assert!(
            parsed.all_frames,
            "without @all-frames the bridge never reaches a child and the verb cannot work"
        );
        assert_eq!(
            parsed.world,
            crate::userscript::ScriptWorld::Isolated,
            "a bridge in the page's world answers on a channel the page can call"
        );
        assert_eq!(parsed.run_at, crate::userscript::RUN_AT_DOCUMENT_START);
        // And the placed body carries the CAP as a number, not as its own
        // placeholder — a substitution that silently failed would ship a script
        // with a syntax error in it, and the symptom would be every read timing
        // out, which reads as a substrate limit.
        assert!(parsed.body.contains(&format!("const CAP = {TEXT_CAP};")));
        assert!(!parsed.body.contains("{TEXT_CAP}"));
    }

    /// ⛔ READ-ONLY IS A PROPERTY OF THE SCRIPT, NOT OF THE DOCUMENTATION.
    ///
    /// The commands this bridge listens for are FORGEABLE by the embedding page
    /// — `postMessage` dispatch is not world-scoped, measured. So every op the
    /// bridge exposes is an op a hostile top document can invoke against another
    /// origin. A read it cannot hear the answer to is worthless to it; a write
    /// would be a same-origin-policy breach we were providing.
    /// ⚠ The check is MECHANICAL rather than a list of forbidden spellings, and
    /// that is the lesson from the world probe's first comparison: a predicate
    /// built on "this substring must never appear" breaks on honestly-made
    /// edits and gets silenced. `.value =` was the first casualty here — it
    /// matches `reply.value =`, which is the bridge filling in its own ANSWER.
    /// So the rule is the real one: the only thing this script assigns to is the
    /// reply it is building. Every other assignment target is a mutation,
    /// whatever it is spelled.
    #[test]
    fn the_bridge_has_no_write_op_and_no_eval_path() {
        for path in ["eval(", "Function(", "document.write", "execCommand"] {
            assert!(
                !BRIDGE.contains(path),
                "the frame bridge must expose no eval path ({path}) — a command that reaches it \
                 is one the embedding page can forge"
            );
        }
        for line in BRIDGE.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            // ` = ` and not `===`/`!==`/`>=`/`<=`/`=>`, none of which contain a
            // space on BOTH sides of a lone `=`.
            let Some((left, _)) = trimmed.split_once(" = ") else {
                continue;
            };
            let left = left.trim();
            if ["const ", "let ", "var "]
                .iter()
                .any(|keyword| left.starts_with(keyword))
            {
                continue;
            }
            assert!(
                left.starts_with("reply."),
                "the frame bridge assigns to {left:?} — the only thing it may write is the reply \
                 it hands the UI process. A mutation op is one a hostile embedder can forge \
                 against another origin, which is a same-origin-policy breach WE would be \
                 providing"
            );
        }
    }

    /// ⛔ THE REPLY MUST NOT TRAVEL BACK ACROSS `postMessage`.
    ///
    /// That is the one line that makes a forged read harmless. If the answer
    /// went back the way the question came, the top page would hear every fact
    /// we learn about another origin, and even a read-only verb would be a
    /// same-origin-policy bypass we shipped.
    #[test]
    fn the_reply_goes_to_the_ui_process_and_nowhere_else() {
        assert!(
            BRIDGE.contains("window.webkit.messageHandlers[CHANNEL].postMessage(reply)"),
            "the answer must leave over the UI-process channel"
        );
        // The child half must never post a reply to another window. `window.top`
        // and `event.source` are the two handles it would use.
        assert!(!BRIDGE.contains("window.top.postMessage"));
        assert!(!BRIDGE.contains("event.source.postMessage"));
        // And the channel name is one string in two languages.
        assert!(BRIDGE.contains(&format!("const CHANNEL = \"{CHANNEL}\";")));
    }

    /// A password is reported as a LENGTH, exactly as `/engine/fill` reports one.
    /// The route a secret can reach must not become a second copy of it.
    #[test]
    fn a_password_field_answers_with_a_length_and_never_the_secret() {
        assert!(BRIDGE.contains("value_withheld"));
        assert!(
            BRIDGE.contains("=== \"password\""),
            "the withholding must key on the field's own type"
        );
        assert!(
            BRIDGE.contains("reply.value_length"),
            "a withheld value still has to be verifiable, or a caller cannot tell a fill that \
             landed from one that did not"
        );
    }

    /// A frame is addressed by what the TOP document can see. Both spellings
    /// resolve to one shape, so nothing downstream has to know which was used.
    #[test]
    fn a_frame_is_addressed_by_selector_or_by_index() {
        assert_eq!(
            parse_target(&json!({ "frame": "#pay" })),
            Ok(FrameTarget {
                selector: "#pay".into(),
                nth: 0
            })
        );
        assert_eq!(
            parse_target(&json!({ "frame": 2 })),
            Ok(FrameTarget {
                selector: DEFAULT_FRAME_SELECTOR.into(),
                nth: 2
            })
        );
        // An explicit nth wins over the index form's own default.
        assert_eq!(
            parse_target(&json!({ "frame": "iframe", "frame_nth": 3 })),
            Ok(FrameTarget {
                selector: "iframe".into(),
                nth: 3
            })
        );
        // And the refusals name what is wrong rather than defaulting to frame 0,
        // which would drive a document the caller never asked for.
        for bad in [json!({}), json!({ "frame": "" }), json!({ "frame": -1 })] {
            assert!(parse_target(&bad).is_err(), "{bad} must be refused");
        }
    }

    /// Every refusal the dispatcher can answer with must become a sentence that
    /// names the frame. A bare token would leave a caller reading the source to
    /// find out what `not_a_frame` meant.
    #[test]
    fn every_dispatch_refusal_is_named_in_the_callers_terms() {
        let target = FrameTarget {
            selector: "#pay".into(),
            nth: 0,
        };
        let cases = [
            ("bad_frame_selector", "not a valid CSS selector"),
            ("no_such_frame", "no frame matches"),
            ("not_a_frame", "owns no browsing context"),
            ("frame_has_no_addressable_origin", "no `src` origin"),
            ("post_refused", "could not be posted"),
        ];
        for (token, expected) in cases {
            let message = dispatch_refusal(&target, &json!({ "refusal": token }));
            assert!(
                message.contains(expected) && message.contains("#pay"),
                "{token} answered {message:?}"
            );
        }
        // Every token the dispatcher can emit must be covered — a new refusal
        // that fell through to the catch-all would print raw JSON at a caller.
        let js = dispatch_js(&target, "#otp", 0, "n", 1);
        for line in js.lines() {
            let Some((_, rest)) = line.split_once("refusal: \"") else {
                continue;
            };
            let token = rest.split('"').next().unwrap_or("");
            assert!(
                cases.iter().any(|(known, _)| *known == token),
                "the dispatcher can answer {token:?} and nothing translates it"
            );
        }
    }

    /// ⭐ THE TRANSLATION IS THE VERB. A point that forgot the frame's own
    /// offset is inside the frame's document coordinates and outside the page's,
    /// which is why the fixture's decoy band exists.
    #[test]
    fn a_point_is_the_childs_rect_plus_the_frames_own_box() {
        let read = FrameRead {
            frame: json!({ "box": { "left": 8.0, "top": 90.0, "w": 600.0, "h": 400.0 } }),
            answer: json!({ "rect": { "left": 10.0, "top": 200.0, "w": 240.0, "h": 34.0 } }),
            viewport: (1280.0, 900.0),
        };
        assert_eq!(translate(&read), Some((8.0 + 10.0 + 120.0, 90.0 + 200.0 + 17.0)));
        // A reply with no rect (the selector matched nothing) has no point, and
        // must not be given one by defaulting the missing numbers to zero.
        let empty = FrameRead {
            frame: json!({ "box": { "left": 8.0, "top": 90.0 } }),
            answer: json!({ "exists": false }),
            viewport: (1280.0, 900.0),
        };
        assert_eq!(translate(&empty), None);
    }

    /// The command carries a SELECTOR and the caller's own correlation, and
    /// nothing else. It is addressed to the frame's own origin — `"*"` would
    /// hand it to whatever occupies the slot.
    #[test]
    fn a_command_is_addressed_to_the_frames_own_origin_never_a_wildcard() {
        let js = dispatch_js(
            &FrameTarget {
                selector: "#pay".into(),
                nth: 0,
            },
            "#otp",
            0,
            "ycf-1-2",
            7,
        );
        assert!(js.contains("}, origin);"), "the post must name the origin");
        assert!(!js.contains(", \"*\")"));
        assert!(js.contains("new URL(src, location.href).origin"));
        // The selector is interpolated as JSON, so a quote in it is data.
        let quoted = dispatch_js(
            &FrameTarget {
                selector: "iframe[title=\"pay\"]".into(),
                nth: 0,
            },
            "input[name=\"otp\"]",
            0,
            "n",
            1,
        );
        assert!(quoted.contains(r#"document.querySelectorAll("iframe[title=\"pay\"]")"#));
        assert!(quoted.contains(r#"selector: "input[name=\"otp\"]""#));
    }

    /// The dispatcher must read the frame's box from the TOP document and post
    /// from a world the page cannot tamper with. The world is chosen at the call
    /// site, so this locks the call site.
    #[test]
    fn the_dispatcher_runs_in_the_engines_own_world() {
        let source = include_str!("frame.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module body precedes its tests");
        assert!(
            source.contains("Some(super::identity::ISOLATED_WORLD)"),
            "a dispatcher in the page's world can be handed a replaced contentWindow, a lying \
             rect, or a swallowed post — by the embedder, about the frame"
        );
    }

    /// A caller cannot pin an engine thread open, and cannot ask for a budget so
    /// short that a healthy frame reads as silent.
    #[test]
    fn the_timeout_is_the_callers_within_bounds() {
        assert_eq!(timeout_ms(&json!({})), DEFAULT_TIMEOUT_MS);
        assert_eq!(timeout_ms(&json!({ "timeout_ms": 900 })), 900);
        assert_eq!(timeout_ms(&json!({ "timeout_ms": 1 })), 250);
        assert_eq!(timeout_ms(&json!({ "timeout_ms": 600_000 })), 30_000);
    }
}
