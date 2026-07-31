//! Phase B — the verb-coverage proof (`ychrome engine flow`).
//!
//! The Phase B AC asks for a script that opens pages, waits, screenshots and
//! "clicks through one flow". Phase A's gate proved the hard half (a trusted
//! click a synthetic one cannot fake) and `engine bench` proved concurrency;
//! this proves the REST of the verb surface — `/nav`, `/wait`, `/dom` and the
//! four input events that were refused by name until now.
//!
//! Every step goes through [`super::api::dispatch`], the same router the daemon
//! socket calls. Nothing here reaches past it into the engine except to write
//! the fixture file, so a passing flow is a statement about the shipping code
//! path and not about a test harness.
//!
//! The fixture is a local `file://` page rather than a live site. That is
//! deliberate: a DuckDuckGo run would prove the same verbs while failing on a
//! layout change or a captcha, and a proof that goes red for reasons unrelated
//! to the thing it tests stops being read.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::api::{dispatch, request};

/// The fixture. Every element it carries exists to make one verb falsifiable:
///
/// - `#name` / `#go` — a form, for selector-addressed click, `type` and `key`;
/// - `#hoverable` — flips text on `mouseenter`, so a real `move` is the only
///   thing that can change it (a synthetic click never enters an element);
/// - `#trust` — records `isTrusted` for every event, so the flow re-checks the
///   Phase A property across ALL the new event kinds, not just the click;
/// - `#late` — appears on a timer, so `/wait {selector}` has something that is
///   genuinely absent when the wait begins;
/// - a tall spacer, so `scrollY` can actually move.
const FIXTURE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>engine flow fixture</title></head>
<body style="margin:0;font:16px sans-serif">
<div id="hoverable" style="width:300px;height:60px;background:#eee">not-hovered</div>
<form id="form" onsubmit="document.getElementById('out').textContent='submitted:'+document.getElementById('name').value; return false;">
  <input id="name" type="text" style="width:300px;height:40px" placeholder="your name">
  <input id="pw" type="password" value="hunter2">
  <button id="go" type="submit" style="width:120px;height:40px">Go</button>
</form>
<div id="out">nothing</div>
<div id="trust">none</div>
<div id="scrolled">0</div>
<div id="slot"></div>
<div style="height:3000px"></div>
<script>
  var trusted = [];
  function note(e) { trusted.push(e.type + '=' + e.isTrusted); document.getElementById('trust').textContent = trusted.join(','); }
  document.getElementById('hoverable').addEventListener('mouseenter', function (e) {
    if (e.isTrusted) { this.textContent = 'hovered'; } note(e);
  });
  document.getElementById('name').addEventListener('keydown', note);
  document.getElementById('go').addEventListener('click', note);
  window.addEventListener('scroll', function () {
    document.getElementById('scrolled').textContent = String(Math.round(window.scrollY));
  });
  setTimeout(function () {
    var d = document.createElement('div');
    d.id = 'late'; d.textContent = 'arrived';
    document.getElementById('slot').appendChild(d);
  }, 700);
</script></body></html>"#;

struct Step {
    name: &'static str,
    verb: &'static str,
    pass: bool,
    detail: Value,
}

impl Step {
    fn to_json(&self) -> Value {
        json!({ "step": self.name, "verb": self.verb, "pass": self.pass, "detail": self.detail })
    }
}

/// One owner of "reply -> (status, body)", shared with the parity run and the
/// bench. A local copy here would have to learn every new `Reply` variant
/// separately, and the NDJSON one proved that: three copies, three compile
/// errors, and one of them could have been "silently drop the stream".
use super::api::json_status as json_reply;

/// Read one value out of the page through `/engine/eval`.
fn read(page: &str, js: &str) -> Value {
    match json_reply(dispatch(&request(
        "eval",
        json!({ "page_id": page, "js": js }),
    ))) {
        (200, body) => body["value"].clone(),
        (_, body) => body,
    }
}

pub fn run() -> Result<Value> {
    let started = Instant::now();
    let dir = dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("ychrome")
        .join("engine-gate");
    std::fs::create_dir_all(&dir)?;
    let fixture = dir.join("flow-fixture.html");
    std::fs::write(&fixture, FIXTURE)?;
    let url = format!("file://{}", fixture.display());

    crate::daemon::journal("engine.flow.start", json!({ "fixture": url }));
    let mut steps: Vec<Step> = Vec::new();
    let mut record = |step: Step| {
        crate::daemon::journal("engine.flow.step", step.to_json());
        steps.push(step);
    };

    // ---- open ------------------------------------------------------------
    let (status, body) = json_reply(dispatch(&request("open", json!({ "url": url }))));
    let page = body["page_id"].as_str().unwrap_or_default().to_string();
    record(Step {
        name: "open the fixture",
        verb: "/engine/open",
        pass: status == 200 && !page.is_empty() && body["loading"] == json!(false),
        detail: body.clone(),
    });

    // ---- /dom snapshot ---------------------------------------------------
    //
    // The trust property first: every selector the extractor emits must have
    // resolved, in the page, to exactly the element it describes. Then the
    // password redaction, which is the one thing a structured read must never
    // hand back.
    let (status, body) = json_reply(dispatch(&request("dom", json!({ "page_id": page }))));
    let dom = &body["dom"];
    let nodes = dom["nodes"].as_array().cloned().unwrap_or_default();
    let password = nodes.iter().find(|node| node["selector"] == json!("#pw"));
    let go = nodes.iter().find(|node| node["selector"] == json!("#go"));
    record(Step {
        name: "snapshot resolves every selector and redacts the password",
        verb: "/engine/dom",
        pass: status == 200
            && dom["unresolved_selectors"] == json!(0)
            && nodes.len() == 3
            && password
                .is_some_and(|node| node["value"].is_null() && node["redacted"] == json!(true))
            && go
                .is_some_and(|node| node["role"] == json!("button") && node["text"] == json!("Go")),
        detail: json!({
            // Three, and exactly three: #name, #pw and #go. `#hoverable` is a
            // plain div, and v1 has no way to see a listener added with
            // `addEventListener` — only inline `[onclick]`. Asserting the exact
            // count is what makes that limit visible if it ever changes.
            "node_count": dom["node_count"],
            "selectors": nodes.iter().map(|node| node["selector"].clone()).collect::<Vec<_>>(),
            "unresolved_selectors": dom["unresolved_selectors"],
            "truncated": dom["truncated"],
            "password_node": password,
            "button_node": go,
        }),
    });

    // ---- /input move (real hover) ----------------------------------------
    //
    // `mouseenter` does not fire for a synthetic click, and the fixture only
    // flips its text when the event is trusted. If this passes, the pointer
    // really moved.
    let (status, body) = json_reply(dispatch(&request(
        "input",
        json!({ "page_id": page, "events": [{ "type": "move", "x": 150, "y": 30 }] }),
    )));
    std::thread::sleep(Duration::from_millis(250));
    let hovered = read(&page, "document.getElementById('hoverable').textContent");
    record(Step {
        name: "a real pointer move fires mouseenter with isTrusted",
        verb: "/engine/input move",
        pass: status == 200 && hovered == json!("hovered"),
        detail: json!({ "dispatched": body["dispatched"], "hoverable_text": hovered }),
    });

    // ---- /input click BY SELECTOR + type + key ---------------------------
    let (status, click) = json_reply(dispatch(&request(
        "input",
        json!({ "page_id": page, "events": [{ "type": "click", "selector": "#name" }] }),
    )));
    let focused = read(&page, "document.activeElement && document.activeElement.id");
    record(Step {
        name: "a selector-addressed click focuses the field it names",
        verb: "/engine/input click{selector}",
        pass: status == 200 && focused == json!("name"),
        detail: json!({ "dispatched": click["dispatched"], "active_element": focused }),
    });

    let (status, typed) = json_reply(dispatch(&request(
        "input",
        json!({ "page_id": page, "events": [{ "type": "type", "text": "ada lovelace" }] }),
    )));
    // WAIT for the text rather than reading straight after the batch. Not a
    // weakened assertion — it still demands the exact string — but an honest
    // one: WebKitGTK queues key events in the UI process and only sends the
    // next after the previous is acked, so a read issued immediately can
    // overtake the final keystroke and see "ada lovelac". `/engine/wait` is
    // precisely the primitive for "do not believe the page until it says so",
    // and a proof that races is a proof that will lie one run in six.
    let (_, settled) = json_reply(dispatch(&request(
        "wait",
        json!({ "page_id": page,
                "until": { "js": "document.getElementById('name').value === 'ada lovelace'" },
                "timeout_ms": 4000 }),
    )));
    let value = read(&page, "document.getElementById('name').value");
    record(Step {
        name: "trusted key events actually produce text",
        verb: "/engine/input type",
        pass: status == 200 && value == json!("ada lovelace") && settled["met"] == json!(true),
        detail: json!({
            "dispatched": typed["dispatched"],
            "field_value": value,
            "wait_elapsed_ms": settled["elapsed_ms"],
        }),
    });

    let (status, key) = json_reply(dispatch(&request(
        "input",
        json!({ "page_id": page, "events": [{ "type": "key", "key": "Return" }] }),
    )));
    std::thread::sleep(Duration::from_millis(300));
    let out = read(&page, "document.getElementById('out').textContent");
    let trust = read(&page, "document.getElementById('trust').textContent");
    record(Step {
        name: "Enter submits the form, and every event was trusted",
        verb: "/engine/input key",
        pass: status == 200
            && out == json!("submitted:ada lovelace")
            && trust
                .as_str()
                .is_some_and(|log| !log.contains("=false") && log.contains("=true")),
        detail: json!({ "dispatched": key["dispatched"], "form_output": out, "is_trusted_log": trust }),
    });

    // ---- /input scroll ---------------------------------------------------
    let (status, scroll) = json_reply(dispatch(&request(
        "input",
        json!({ "page_id": page, "events": [{ "type": "scroll", "x": 200, "y": 300, "dy": 600 }] }),
    )));
    std::thread::sleep(Duration::from_millis(400));
    let scroll_y = read(&page, "Math.round(window.scrollY)");
    record(Step {
        name: "a smooth scroll event moves the page",
        verb: "/engine/input scroll",
        pass: status == 200 && scroll_y.as_f64().unwrap_or(0.0) > 0.0,
        detail: json!({ "dispatched": scroll["dispatched"], "scroll_y": scroll_y }),
    });

    // ---- /wait -----------------------------------------------------------
    //
    // `#late` is added on a 700ms timer and the page reloads first, so the
    // element is genuinely absent when the wait begins — a wait that returned
    // met:true instantly would be proving nothing.
    let (status, navigated) = json_reply(dispatch(&request(
        "nav",
        json!({ "page_id": page, "action": "reload" }),
    )));
    let after_reload = read(&page, "document.getElementById('name').value");
    record(Step {
        name: "reload really reloads (typed state is gone)",
        verb: "/engine/nav reload",
        pass: status == 200 && after_reload == json!(""),
        detail: json!({ "url": navigated["url"], "field_after_reload": after_reload }),
    });

    let absent = read(&page, "document.getElementById('late') === null");
    let (status, waited) = json_reply(dispatch(&request(
        "wait",
        json!({ "page_id": page, "until": { "selector": "#late", "state": "visible" }, "timeout_ms": 5000 }),
    )));
    record(Step {
        name: "wait{selector,visible} blocks until the element really arrives",
        verb: "/engine/wait selector",
        pass: status == 200
            && absent == json!(true)
            && waited["met"] == json!(true)
            && waited["elapsed_ms"].as_u64().unwrap_or(0) >= 100,
        detail: json!({ "absent_at_start": absent, "wait": waited }),
    });

    let (status, idle) = json_reply(dispatch(&request(
        "wait",
        json!({ "page_id": page, "until": { "idle_ms": 400 }, "timeout_ms": 8000 }),
    )));
    record(Step {
        name: "wait{idle_ms} settles once layout and network are quiet",
        verb: "/engine/wait idle_ms",
        pass: status == 200 && idle["met"] == json!(true),
        detail: idle.clone(),
    });

    // A wait that cannot be met must say so, not raise. A script branches on
    // `met:false`; an error would make it crash instead.
    let (status, never) = json_reply(dispatch(&request(
        "wait",
        json!({ "page_id": page, "until": { "js": "false" }, "timeout_ms": 300 }),
    )));
    record(Step {
        name: "an unmeetable wait reports met:false rather than failing",
        verb: "/engine/wait js",
        pass: status == 200 && never["met"] == json!(false) && never["reason"].is_string(),
        detail: never.clone(),
    });

    let _ = dispatch(&request("close", json!({ "page_id": page })));

    let pass = steps.iter().all(|step| step.pass);
    let report = json!({
        "flow": "phase-b-verbs",
        "pass": pass,
        "fixture": url,
        "elapsed_ms": started.elapsed().as_millis(),
        "steps": steps.iter().map(Step::to_json).collect::<Vec<_>>(),
    });
    crate::daemon::journal("engine.flow.result", report.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The fixture's whole value is that its assertions cannot pass by accident.
    // If someone drops the isTrusted guard on the hover handler, `move` would
    // "pass" against a synthetic event and the proof would be hollow.
    #[test]
    fn the_fixture_gates_hover_on_is_trusted_and_carries_a_password() {
        let hover = FIXTURE
            .split("mouseenter")
            .nth(1)
            .expect("the hover handler exists");
        assert!(
            hover.contains("if (e.isTrusted)"),
            "the hover text may only change for a trusted event"
        );
        assert!(
            FIXTURE.contains(r#"id="pw" type="password""#),
            "the fixture needs a password field, or the redaction check tests nothing"
        );
        assert!(
            FIXTURE.contains("setTimeout"),
            "#late must arrive on a timer, or wait{{selector}} proves nothing"
        );
        assert!(
            FIXTURE.contains("height:3000px"),
            "the page must be scrollable, or the scroll step proves nothing"
        );
    }
}
