//! The embedded-SDK proof (`ychrome engine embed`).
//!
//! A payment SDK does not hand off by navigating the page. It ships an **ES
//! module** that defines a **custom element**, and that element builds an
//! `<iframe>` at runtime and puts the bank's document inside it. BillDesk's
//! "Embedded SDK v2" is this shape, and on webkitgtk-headless it got as far as
//! *constructing the element* and no further: the iframe was in the document,
//! visible, 1280x900, and its `src` stayed empty forever.
//!
//! **`src` staying EMPTY is the whole clue, and it narrows the field hard.**
//! `iframe.src = url` writes the attribute whether or not the load succeeds, so
//! an empty `src` means the SDK never took that road. The roads that leave `src`
//! empty are exactly these, and this proof walks every one of them:
//!
//! | mechanism | why `src` stays empty |
//! |---|---|
//! | `srcdoc` | a different attribute entirely |
//! | `contentDocument.write` | no navigation at all, just a document |
//! | `contentWindow.location = …` | navigating a frame does not write its parent's attribute |
//! | a form whose `target` names the frame | the frame is navigated by the submit |
//! | a `postMessage` handshake gating any of the above | the road is never taken |
//!
//! Each case is its own page, driven through [`super::api::dispatch`] like every
//! other proof, and each reports through the SAME channel a real SDK uses: the
//! child posts a message to its parent. A case that "worked" because the parent
//! could reach into a same-origin child would prove nothing about a bank iframe,
//! so the child is always on the OTHER origin except where the mechanism is
//! defined to inherit the parent's (`srcdoc`, `document.write`).

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::api::{dispatch, json_status as json_reply, request};
use super::gateway::{Route, document, spawn_origin};

/// How long a case may take to report. Generous: the point is to tell "this
/// mechanism does not work here" from "this mechanism is slow", and a stingy
/// budget cannot.
const CASE_TIMEOUT_MS: u64 = 8000;

/// Every mechanism that can populate a dynamically created iframe while leaving
/// its `src` attribute empty, plus the two that do write `src` — those two are
/// the CONTROL. If they fail too, the finding is "dynamic iframes are broken"
/// and not "this mechanism is".
const CASES: [(&str, &str); 13] = [
    // Controls: these write `src`, and the live symptom says the SDK does not.
    ("src-after-append", "control: append, then set src"),
    ("src-before-append", "control: set src, then append"),
    // The candidates named by the live investigation.
    ("srcdoc", "srcdoc, which never writes src"),
    (
        "docwrite",
        "document.write into the initial about:blank child",
    ),
    ("location-assign", "contentWindow.location = url"),
    ("location-replace", "contentWindow.location.replace(url)"),
    ("form-target-name", "a form whose target NAMES the frame"),
    // EXPECTED to be refused, and it is the only row here that is. A form whose
    // target names no existing frame asks for a new window, and WebKit blocks a
    // window nobody clicked for. Kept as the control that says so out loud: an
    // `id` is not a `name`, and an SDK that confuses them gets silence.
    (
        "form-target-id-only",
        "EXPECTED REFUSAL: target names no frame, no gesture",
    ),
    (
        "postmessage-handshake",
        "child announces, parent answers, child confirms",
    ),
    // The two shapes an SDK is actually delivered in.
    (
        "shadow-dom",
        "the iframe lives in a custom element's shadow root",
    ),
    (
        "module-toplevel",
        "built at module top level, not in a ready handler",
    ),
    // ⭐ THE TWO THAT REPRODUCE THE LIVE SYMPTOM EXACTLY. Neither is a substrate
    // gap: the frame is built and the navigation is simply never reached,
    // because the bootstrap died in between. Every DOM probe an agent can run
    // reports "constructed, never navigated" — which is what a whole live
    // investigation had to stop at. These pass when `/engine/console` NAMES the
    // reason; the frame is expected to stay empty.
    (
        "throws-before-navigate",
        "builds the frame, then throws — console must name it",
    ),
    (
        "rejects-before-navigate",
        "async bootstrap rejects — console must name it",
    ),
];

/// The cases whose frame is SUPPOSED to stay empty, and which pass on the
/// engine reporting why rather than on the frame filling.
const EXPLAINED_BY_CONSOLE: [&str; 2] = ["throws-before-navigate", "rejects-before-navigate"];

struct Step {
    name: String,
    pass: bool,
    detail: Value,
}

impl Step {
    fn to_json(&self) -> Value {
        json!({ "step": self.name, "pass": self.pass, "detail": self.detail })
    }
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// The parent page for one case: nothing but the module, exactly as a bootstrap
/// page is nothing but the SDK.
fn case_page(case: &str) -> String {
    document(
        &format!("EMBED {case}"),
        &format!(
            "<div id=\"sdk-root\"></div>\
             <script type=\"module\" src=\"/sdk.js?case={case}\"></script>"
        ),
    )
}

/// The SDK: one real ES module defining one real custom element.
///
/// It is deliberately not a bundle. Every case goes through the same
/// `connectedCallback`, so a difference between two cases is a difference
/// between the two MECHANISMS and not between two code paths.
fn sdk_module(case: &str, child: &str) -> String {
    format!(
        r#"const CASE = {case:?};
const CHILD = {child:?};

window.__probe = {{ case: CASE, reported: null, frames_before: 0, frames_after: 0,
                   src_attr: null, error: null, created: false }};

// The SAME channel a real SDK uses. A parent reaching into a same-origin child
// would prove nothing about a bank's iframe.
window.addEventListener("message", (event) => {{
  if (event.data && event.data.__embed) {{
    window.__probe.reported = event.data;
    if (event.data.stage === "announce") {{
      event.source.postMessage({{ __embed: true, stage: "answer" }}, "*");
    }}
  }}
}});

function makeFrame(doc) {{
  const frame = doc.createElement("iframe");
  frame.id = "sdk-iframe";
  frame.style.cssText = "display:block;width:640px;height:400px;border:0";
  return frame;
}}

function post(where, body) {{
  const form = document.createElement("form");
  form.method = "POST";
  form.action = CHILD + "/frame";
  form.target = where;
  const field = document.createElement("input");
  field.type = "hidden"; field.name = "bdOrderId"; field.value = "OID123";
  form.appendChild(field);
  document.body.appendChild(form);
  form.submit();
}}

class SdkHost extends HTMLElement {{
  connectedCallback() {{
    const probe = window.__probe;
    probe.frames_before = document.querySelectorAll("iframe").length;
    let mount = this;
    if (CASE === "shadow-dom") {{ mount = this.attachShadow({{ mode: "open" }}); }}
    const frame = makeFrame(document);
    try {{
      if (CASE === "src-before-append") {{
        frame.src = CHILD + "/frame";
        mount.appendChild(frame);
      }} else if (CASE === "form-target-name" || CASE === "form-target-id-only") {{
        // A form can only target a frame that is already in the document.
        if (CASE === "form-target-name") {{ frame.name = "sdkframe"; }}
        mount.appendChild(frame);
        post(CASE === "form-target-name" ? "sdkframe" : "sdk-iframe");
      }} else {{
        mount.appendChild(frame);
        if (CASE === "src-after-append" || CASE === "shadow-dom"
            || CASE === "module-toplevel" || CASE === "postmessage-handshake") {{
          frame.src = CHILD + "/frame";
        }} else if (CASE === "srcdoc") {{
          frame.srcdoc = "<!doctype html><script>parent.postMessage("
            + "{{__embed:true,stage:'done',how:'srcdoc'}},'*')<\/script>";
        }} else if (CASE === "docwrite") {{
          const inner = frame.contentDocument || frame.contentWindow.document;
          inner.open();
          inner.write("<!doctype html><script>parent.postMessage("
            + "{{__embed:true,stage:'done',how:'docwrite'}},'*')<\/script>");
          inner.close();
        }} else if (CASE === "location-assign") {{
          frame.contentWindow.location = CHILD + "/frame";
        }} else if (CASE === "location-replace") {{
          frame.contentWindow.location.replace(CHILD + "/frame");
        }} else if (CASE === "throws-before-navigate") {{
          // OUTSIDE the try, or the catch below would swallow it and there
          // would be nothing for an uncaught-error listener to hear — which is
          // exactly how a real SDK's failure stays invisible.
          setTimeout(() => {{ window.__sdkConfig.launch(); }}, 0);
        }} else if (CASE === "rejects-before-navigate") {{
          (async () => {{
            await Promise.resolve();
            throw new TypeError("sdk: could not read authToken from the order");
          }})();
        }}
      }}
      probe.created = true;
    }} catch (error) {{
      probe.error = String(error);
    }}
    probe.frames_after = document.querySelectorAll("iframe").length;
    // Read the ATTRIBUTE, the way the live investigation read it.
    probe.src_attr = frame.getAttribute("src") || "";
  }}
}}
customElements.define("sdk-host", SdkHost);

const host = document.createElement("sdk-host");
if (CASE === "module-toplevel") {{
  // Module scripts are deferred, so the body exists by now. This is the shape
  // an SDK that does NOT wrap itself in a ready handler ships as.
  document.getElementById("sdk-root").appendChild(host);
}} else if (document.readyState === "loading") {{
  // Exactly one of these, ever. Appending the same host twice MOVES it, which
  // fires connectedCallback again and builds a second frame — measured, and it
  // is how a fixture starts reporting the wrong frame's state.
  document.addEventListener("DOMContentLoaded", () => {{
    document.getElementById("sdk-root").appendChild(host);
  }});
}} else {{
  document.getElementById("sdk-root").appendChild(host);
}}
"#
    )
}

/// The bank's own document. It reports the only way a cross-origin child can.
const CHILD_FRAME: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<title>BANK FRAME</title></head><body><p id="bank">the bank frame rendered</p>
<script>
  parent.postMessage({__embed: true, stage: "announce", how: "navigated"}, "*");
  window.addEventListener("message", function (event) {
    if (event.data && event.data.stage === "answer") {
      parent.postMessage({__embed: true, stage: "done", how: "handshake"}, "*");
    }
  });
  parent.postMessage({__embed: true, stage: "done", how: "navigated"}, "*");
</script></body></html>"#;

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

pub fn run() -> Result<Value> {
    let started = Instant::now();

    let parent_listener = TcpListener::bind("127.0.0.1:0").context("binding the parent origin")?;
    let child_listener = TcpListener::bind("127.0.0.1:0").context("binding the child origin")?;
    let parent_base = format!("http://127.0.0.1:{}", parent_listener.local_addr()?.port());
    let child_base = format!("http://127.0.0.1:{}", child_listener.local_addr()?.port());

    let child_for_module = child_base.clone();
    let (_, _parent_hits) = spawn_origin(
        parent_listener,
        Arc::new(move |_method: &str, path: &str| {
            // `path` still carries the query, which is how the module learns
            // its case — one module, one code path, eleven mechanisms.
            let (route, query) = path.split_once('?').unwrap_or((path, ""));
            let case = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("case="))
                .unwrap_or("")
                .to_string();
            if route == "/sdk.js" {
                return (
                    200u16,
                    vec!["Content-Type: text/javascript; charset=utf-8".to_string()],
                    sdk_module(&case, &child_for_module),
                );
            }
            if let Some(case) = route.strip_prefix("/case/") {
                return (200u16, Vec::new(), case_page(case));
            }
            (404u16, Vec::new(), "no such parent route".to_string())
        }),
    )?;
    let (_, child_hits) = spawn_origin(
        child_listener,
        Arc::new(|_method: &str, path: &str| {
            if path.starts_with("/frame") {
                (200u16, Vec::new(), CHILD_FRAME.to_string())
            } else {
                (404u16, Vec::new(), "no such child route".to_string())
            }
        }),
    )?;

    crate::daemon::journal(
        "engine.embed.start",
        json!({ "parent": parent_base, "child": child_base }),
    );
    let mut steps: Vec<Step> = Vec::new();

    for (case, what) in CASES {
        let (status, body) = json_reply(dispatch(&request(
            "open",
            json!({ "url": format!("{parent_base}/case/{case}"), "profile": "engine-embed-proof" }),
        )));
        let page = body["page_id"].as_str().unwrap_or_default().to_string();
        if status != 200 || page.is_empty() {
            steps.push(Step {
                name: format!("{case} — {what}"),
                pass: false,
                detail: json!({ "open_status": status, "body": body }),
            });
            continue;
        }

        // Wait for the CHILD to report, which is the only evidence that a
        // document really arrived in the frame. A frame element with a body is
        // not the same claim.
        let _ = dispatch(&request(
            "wait",
            json!({
                "page_id": page,
                "until": { "js": "window.__probe && window.__probe.reported !== null" },
                "timeout_ms": CASE_TIMEOUT_MS,
            }),
        ));
        let probe = json_reply(dispatch(&request(
            "eval",
            json!({ "page_id": page, "js": "JSON.stringify(window.__probe)" }),
        )))
        .1["value"]
            .clone();
        let seen: Value = probe
            .as_str()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or(Value::Null);
        let reported = seen["reported"]["stage"] == json!("done");
        // One case is expected to be refused; passing it would mean a
        // gesture-less popup got through, which is its own bug.
        let expected_refusal = case == "form-target-id-only";
        let reported = reported != expected_refusal;

        // Every case reads the console, because a case that failed for a reason
        // the page already stated should never be reported as a mystery.
        let console = json_reply(dispatch(&request(
            "console",
            json!({ "page_id": page, "clear": true }),
        )))
        .1;
        let entries = console["entries"].as_array().cloned().unwrap_or_default();
        let named: Vec<&Value> = entries
            .iter()
            .filter(|row| row["kind"] == json!("error") || row["kind"] == json!("rejection"))
            .collect();

        let pass = if EXPLAINED_BY_CONSOLE.contains(&case) {
            // The frame is SUPPOSED to stay empty here. What must hold is that
            // the engine can say why.
            !named.is_empty() && seen["src_attr"] == json!("")
        } else {
            reported
        };
        let step = Step {
            name: format!("{case} — {what}"),
            pass,
            detail: json!({
                "probe": seen,
                "child_saw": child_hits.lock().map(|log| log.len()).unwrap_or(0),
                "console_installed": console["installed"],
                "named": named,
            }),
        };
        crate::daemon::journal("engine.embed.step", step.to_json());
        steps.push(step);
        let _ = dispatch(&request("close", json!({ "page_id": page })));
    }

    // This proof REPORTS; it does not gate. Which mechanisms a substrate
    // supports is a measurement, and a run that went red on a mechanism nobody
    // uses would teach an agent to ignore it. The caller reads `broken`.
    let broken: Vec<&str> = steps
        .iter()
        .filter(|step| !step.pass)
        .map(|step| step.name.split(' ').next().unwrap_or(""))
        .collect();
    let report = json!({
        "flow": "embedded-sdk-iframe",
        "pass": steps.iter().all(|step| step.pass),
        "broken": broken,
        "parent": parent_base,
        "child": child_base,
        "child_saw": child_hits.lock().map(|log| log.clone()).unwrap_or_default(),
        "elapsed_ms": started.elapsed().as_millis(),
        "steps": steps.iter().map(Step::to_json).collect::<Vec<_>>(),
    });
    crate::daemon::journal("engine.embed.result", report.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_is_reachable_in_the_one_module() {
        for (case, _) in CASES {
            let module = sdk_module(case, "http://child.invalid");
            assert!(
                module.contains(&format!("{case:?}")),
                "case {case} must name itself to the module"
            );
        }
    }

    /// The controls are what make a red row mean something. Without a mechanism
    /// that DOES write `src`, "src stayed empty" could just be "iframes do not
    /// work", and the whole proof would be one undifferentiated failure.
    #[test]
    fn the_controls_are_the_two_that_write_src() {
        assert!(CASES.iter().any(|(name, _)| *name == "src-after-append"));
        assert!(CASES.iter().any(|(name, _)| *name == "src-before-append"));
        let module = sdk_module("src-after-append", "http://child.invalid");
        assert!(
            module.contains("frame.src = CHILD"),
            "the control must set src, or it is not a control"
        );
    }

    /// A case that passed because the PARENT read a same-origin child would say
    /// nothing about a bank's cross-origin frame.
    #[test]
    fn the_child_reports_through_postmessage_not_through_the_parent() {
        assert!(
            CHILD_FRAME.contains("parent.postMessage"),
            "the child must report over the one channel that crosses an origin"
        );
        let module = sdk_module("src-after-append", "http://child.invalid");
        assert!(
            !module.contains("contentDocument.body"),
            "the parent must never read the child's DOM to decide a case passed"
        );
    }
}
