//! `ychrome engine hit` — the lock on selector-addressed clicks.
//!
//! **What it exists to stop.** `/engine/input` once resolved
//! `{"type":"click","selector":"button[type=submit]"}` to a hidden duplicate and
//! answered `{"dispatched":3,"ok":true}` while the real control's handler never
//! fired. On IBKR's login page that reported success cost a reporting agent
//! three wrong conclusions in one session, one of which blamed the operator's
//! vault for a 2FA rejection that had never been submitted.
//!
//! So the bar this run holds the engine to is a single sentence: **a selector
//! click either lands on the element a human would have clicked, or it refuses
//! with a named reason. It never reports a dispatch that hit nothing.**
//!
//! Every step goes through [`super::api::dispatch`] — the same router the daemon
//! socket calls — so a green run is a statement about the shipping code path.
//! And every step is falsifiable: each refusal is checked to have left its
//! target's own state untouched, so a step cannot pass because the engine
//! refused everything.

use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::api::{dispatch, json_status as json_reply, request};

/// The fixture. Every element earns its place by making one refusal or one
/// choice falsifiable:
///
/// - `.go` — **the reported bug, verbatim**: a `visibility:hidden` duplicate
///   AHEAD of the real control. It measures a perfectly good rect, so nothing
///   about its geometry says "do not click me"; only the browser's own hit test
///   does. `#real` sets `document.title`, so a click that misses is silent and a
///   click that lands is unmistakable.
/// - `.only` — `display:none`, so the ONLY match measures `0x0`.
/// - `.veiled` — a real button with a real rect under a local overlay, the
///   COVERED case a rect check can never catch.
/// - `.parked` — a real button parked where no scroll can reach it.
/// - `.twin` — two genuinely hittable matches, so ambiguity can be tested
///   against a page where it is real rather than manufactured by hidden nodes.
/// - `#open` / `#item` — `#item` does not exist until `#open` is clicked, which
///   is how a batch proves it resolves each event against the page as it is when
///   that event is dispatched.
/// - `.ghost` — replaces itself the moment it is scrolled to, so the pinned node
///   is detached by the time the rect is re-measured.
const FIXTURE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>engine hit fixture</title></head>
<body style="margin:0;font:16px sans-serif">

<button class="go" style="visibility:hidden">decoy sized but invisible</button>
<button class="go" id="real" onclick="document.title='CLICKED'">real</button>

<button class="only" style="display:none">zero size</button>

<div style="position:relative;width:160px;height:34px">
  <button class="veiled" style="position:absolute;left:0;top:0;width:160px;height:34px">under the veil</button>
  <div id="veil" style="position:absolute;left:0;top:0;width:160px;height:34px;z-index:9"></div>
</div>

<div id="deaf" style="width:160px;height:34px">
  <button class="numb" style="pointer-events:none;width:160px;height:34px">pointer-events none</button>
</div>

<button class="parked" style="position:absolute;left:-9999px;top:0;width:120px;height:30px">parked</button>

<button class="twin" style="width:90px;height:30px">twin one</button>
<button class="twin" style="width:90px;height:30px">twin two</button>

<button id="open" style="width:90px;height:30px">open</button>
<div id="menu"></div>

<div style="height:4000px"></div>
<button class="ghost" id="ghost" style="width:120px;height:30px">ghost</button>

<script>
  document.querySelector('.veiled').addEventListener('click', function () { window.__veiled = true; });
  document.querySelector('.numb').addEventListener('click', function () { window.__numb = true; });
  document.querySelector('.parked').addEventListener('click', function () { window.__parked = true; });
  var twins = document.querySelectorAll('.twin');
  twins[0].addEventListener('click', function () { window.__twin = 'first'; });
  twins[1].addEventListener('click', function () { window.__twin = 'second'; });
  document.getElementById('open').addEventListener('click', function () {
    if (document.getElementById('item')) { return; }
    var b = document.createElement('button');
    b.id = 'item'; b.textContent = 'item';
    b.style.width = '90px'; b.style.height = '30px';
    b.addEventListener('click', function () { window.__item = true; });
    document.getElementById('menu').appendChild(b);
  });
  // The ghost swaps itself for a clone the first time it is scrolled near the
  // viewport. The engine pins a node, scrolls to it, and re-measures 120ms
  // later — so the node it pinned is gone by the time it looks again, which is
  // what a React re-render does to an agent mid-click.
  var ghostSwapped = false;
  window.addEventListener('scroll', function () {
    if (ghostSwapped) { return; }
    var g = document.getElementById('ghost');
    if (!g) { return; }
    if (g.getBoundingClientRect().top > window.innerHeight) { return; }
    ghostSwapped = true;
    g.replaceWith(g.cloneNode(true));
  });
</script></body></html>"#;

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

/// Send one click and hand back `(status, body)`.
fn click(page: &str, event: Value) -> (u16, Value) {
    json_reply(dispatch(&request(
        "input",
        json!({ "page_id": page, "events": [event] }),
    )))
}

/// Did this refusal name the reason we meant, and did it dispatch nothing?
///
/// Both halves matter. "It returned an error" is not the property under test —
/// an engine that refused every click for the wrong reason would pass that.
fn refused(status: u16, body: &Value, token: &str) -> bool {
    status == 409
        && body["ok"] == json!(false)
        && body["dispatched"] == json!(0)
        && body["error"].as_str().is_some_and(|e| e.contains(token))
}

pub fn run() -> Result<Value> {
    let started = Instant::now();
    let dir = dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("ychrome")
        .join("engine-gate");
    std::fs::create_dir_all(&dir)?;
    let fixture = dir.join("hit-fixture.html");
    std::fs::write(&fixture, FIXTURE)?;
    let url = format!("file://{}", fixture.display());

    crate::daemon::journal("engine.hit.start", json!({ "fixture": url }));
    let mut steps: Vec<Step> = Vec::new();
    let mut record = |step: Step| {
        crate::daemon::journal("engine.hit.step", step.to_json());
        steps.push(step);
    };

    let (status, body) = json_reply(dispatch(&request("open", json!({ "url": url }))));
    let page = body["page_id"].as_str().unwrap_or_default().to_string();
    record(Step {
        name: "open the fixture",
        pass: status == 200 && !page.is_empty(),
        detail: body.clone(),
    });

    // ---- THE REPORTED BUG -------------------------------------------------
    //
    // `.go` matches a `visibility:hidden` decoy first and the real button
    // second. The decoy's rect is real (159x27 measured), so every geometric
    // test passes on it; only `elementFromPoint` says it is not there. The old
    // resolver asked `hit === e || e.contains(hit) || hit.contains(e)`, the
    // decoy's centre hit `<body>`, `<body>` contains everything, and the click
    // went to (87.875, 21.5) where nothing listens.
    let (status, body) = click(&page, json!({ "type": "click", "selector": ".go" }));
    let title = read(&page, "document.title");
    let report = body["resolved"][0].clone();
    record(Step {
        name: "a hidden duplicate ahead of the real control does not eat the click",
        pass: status == 200
            && title == json!("CLICKED")
            && body["dispatched"] == json!(3)
            && report["matches"] == json!(2)
            && report["hidden"] == json!(1)
            && report["hittable"] == json!(1)
            && report["nth"] == json!(0)
            && report["ambiguous"] == json!(false),
        detail: json!({ "title": title, "dispatched": body["dispatched"], "resolved": report }),
    });

    // ---- the refusals ------------------------------------------------------
    let (status, body) = click(&page, json!({ "type": "click", "selector": ".only" }));
    record(Step {
        name: "a display:none only-match refuses as zero_size_element",
        pass: refused(status, &body, "zero_size_element"),
        detail: body.clone(),
    });

    let (status, body) = click(&page, json!({ "type": "click", "selector": ".nowhere" }));
    record(Step {
        name: "a selector that matches nothing says so",
        pass: refused(status, &body, "no element matches"),
        detail: body.clone(),
    });

    let (status, body) = click(&page, json!({ "type": "click", "selector": ".veiled" }));
    let veiled = read(&page, "String(window.__veiled)");
    record(Step {
        name: "a covered button refuses as target_moved and its handler never fires",
        pass: refused(status, &body, "target_moved")
            && body["error"]
                .as_str()
                .is_some_and(|e| e.contains("div#veil"))
            && veiled == json!("undefined"),
        detail: json!({ "error": body["error"], "handler_ran": veiled }),
    });

    // ⛔ THE ANCESTOR-HIT CASE, and the one that isolates the wrong clause.
    //
    // `.numb` is `pointer-events:none`, so `elementFromPoint` at its centre
    // returns its PARENT. A hittability test that accepts `hit.contains(el)`
    // calls that on-target — an ancestor "contains" the node, after all — and
    // dispatches a click that reaches the parent while the button's own handler
    // never runs. That is the same wrong reasoning that let a `visibility:hidden`
    // decoy pass because `<body>` contains everything on the page.
    let (status, body) = click(&page, json!({ "type": "click", "selector": ".numb" }));
    let numb = read(&page, "String(window.__numb)");
    record(Step {
        name: "an ancestor hit is NOT a hit: pointer-events:none refuses as target_moved",
        pass: refused(status, &body, "target_moved")
            && body["error"]
                .as_str()
                .is_some_and(|e| e.contains("div#deaf"))
            && numb == json!("undefined"),
        detail: json!({ "error": body["error"], "handler_ran": numb }),
    });

    let (status, body) = click(&page, json!({ "type": "click", "selector": ".parked" }));
    let parked = read(&page, "String(window.__parked)");
    record(Step {
        name: "a button no scroll can reach refuses, naming the viewport",
        pass: refused(status, &body, "target_moved")
            && body["error"]
                .as_str()
                .is_some_and(|e| e.contains("outside the viewport"))
            && parked == json!("undefined"),
        detail: json!({ "error": body["error"], "handler_ran": parked }),
    });

    // ---- the semantics: ambiguity is counted over HITTABLE matches ---------
    let (status, body) = click(&page, json!({ "type": "click", "selector": ".twin" }));
    let twin = read(&page, "String(window.__twin)");
    let report = body["resolved"][0].clone();
    record(Step {
        name: "two hittable twins: the default takes the first and REPORTS the ambiguity",
        pass: status == 200
            && twin == json!("first")
            && report["hittable"] == json!(2)
            && report["ambiguous"] == json!(true)
            && report["nth"] == json!(0),
        detail: json!({ "clicked": twin, "resolved": report }),
    });

    let (status, body) = click(
        &page,
        json!({ "type": "click", "selector": ".twin", "require_unique": true }),
    );
    record(Step {
        name: "require_unique refuses REAL ambiguity before the click, not after",
        pass: refused(status, &body, "ambiguous_selector"),
        detail: body.clone(),
    });

    // The same opt-in must NOT refuse the reported bug's page: one live control
    // behind five corpses is a question with exactly one answer, and refusing it
    // would be refusing to answer.
    let (status, body) = click(
        &page,
        json!({ "type": "click", "selector": ".go", "require_unique": true }),
    );
    record(Step {
        name: "require_unique does NOT call hidden duplicates ambiguous",
        pass: status == 200 && body["resolved"][0]["hittable"] == json!(1),
        detail: body["resolved"].clone(),
    });

    let (status, body) = click(
        &page,
        json!({ "type": "click", "selector": ".twin", "nth": 1 }),
    );
    let twin = read(&page, "String(window.__twin)");
    record(Step {
        name: "nth picks among the HITTABLE matches",
        pass: status == 200 && twin == json!("second") && body["resolved"][0]["nth"] == json!(1),
        detail: json!({ "clicked": twin, "resolved": body["resolved"] }),
    });

    let (status, body) = click(
        &page,
        json!({ "type": "click", "selector": ".twin", "nth": 5 }),
    );
    record(Step {
        name: "an nth past the end refuses instead of falling back to the first",
        pass: refused(status, &body, "at hittable index 5"),
        detail: body.clone(),
    });

    // ---- the batch resolves each event against the page it will act on -----
    //
    // `#item` does not exist when the batch arrives. Resolving the whole batch
    // up front would refuse it (or, worse on a page where the node merely
    // MOVES, click where it used to be); resolving each event as it is
    // dispatched is what makes a two-step interaction expressible at all.
    let (status, body) = json_reply(dispatch(&request(
        "input",
        json!({ "page_id": page, "events": [
            { "type": "click", "selector": "#open" },
            { "type": "click", "selector": "#item" }
        ] }),
    )));
    let item = read(&page, "String(window.__item)");
    record(Step {
        name: "a batch resolves event 2 against the page event 1 created",
        pass: status == 200
            && item == json!("true")
            && body["dispatched"] == json!(6)
            && body["resolved"]
                .as_array()
                .is_some_and(|list| list.len() == 2),
        detail: json!({ "item_clicked": item, "dispatched": body["dispatched"] }),
    });

    // ---- the node that dies mid-resolve -----------------------------------
    let (status, body) = click(&page, json!({ "type": "click", "selector": ".ghost" }));
    record(Step {
        name: "a node replaced between the scroll and the re-measure refuses as detached_node",
        pass: refused(status, &body, "detached_node"),
        detail: body.clone(),
    });

    // ---- the empty batch ---------------------------------------------------
    //
    // Same bug family, different door: a request whose shape does not produce
    // an `events` array arrived as zero events and answered `ok:true`.
    let (status, body) = json_reply(dispatch(&request(
        "input",
        json!({ "page_id": page, "events": [] }),
    )));
    record(Step {
        name: "an empty batch is a caller error, not a no-op success",
        pass: status == 400 && body["ok"] == json!(false),
        detail: body.clone(),
    });

    let _ = dispatch(&request("close", json!({ "page_id": page })));

    let pass = steps.iter().all(|step| step.pass);
    let report = json!({
        "proof": "selector-click-hittability",
        "pass": pass,
        "fixture": url,
        "elapsed_ms": started.elapsed().as_millis(),
        "steps": steps.iter().map(Step::to_json).collect::<Vec<_>>(),
    });
    crate::daemon::journal("engine.hit.result", report.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The fixture's whole value is that its assertions cannot pass by accident.
    // The decoy must be `visibility:hidden` and NOT `display:none`: a
    // `display:none` decoy measures `0x0` and is caught by a plain rect check,
    // which is the case that ALREADY worked. Only a decoy with a real rect
    // exercises the hit test that was wrong.
    #[test]
    fn the_decoy_has_a_real_rect_and_comes_first() {
        let decoy = FIXTURE
            .split(r#"<button class="go""#)
            .nth(1)
            .expect("a .go decoy exists");
        assert!(
            decoy.contains("visibility:hidden"),
            "the FIRST .go must be visibility:hidden — a display:none decoy has no rect and \
             tests nothing: {decoy}"
        );
        let decoy_at = FIXTURE
            .find("decoy sized but invisible")
            .expect("the decoy");
        let real_at = FIXTURE.find(r#"id="real""#).expect("the real button");
        assert!(
            decoy_at < real_at,
            "the decoy must come FIRST in document order, or querySelector would have \
             returned the real button anyway and the proof is hollow"
        );
        assert!(
            FIXTURE.contains("document.title='CLICKED'"),
            "the real button must leave evidence a missed click cannot fake"
        );
    }

    // The covered and offscreen cases need REAL geometry, or they collapse into
    // the zero-size case that was never broken.
    #[test]
    fn the_covered_and_parked_buttons_have_real_boxes() {
        for marker in ["under the veil", "parked"] {
            let fragment = FIXTURE.split(marker).next().unwrap_or_default();
            assert!(
                fragment.contains("width:160px;height:34px") || fragment.contains("width:120px"),
                "{marker} must have a real width and height"
            );
        }
        assert!(
            FIXTURE.contains("z-index:9"),
            "the veil must actually paint over the button, or elementFromPoint returns the \
             button and there is nothing to refuse"
        );
        assert!(
            FIXTURE.contains("left:-9999px"),
            "the parked button must be somewhere no scroll can reach"
        );
    }

    // `.twin` is the only place ambiguity is REAL. If both twins were not
    // hittable, `require_unique` would pass for the wrong reason.
    #[test]
    fn the_twins_are_both_hittable_and_distinguishable() {
        assert_eq!(
            FIXTURE.matches(r#"class="twin""#).count(),
            2,
            "exactly two twins, both plainly visible"
        );
        assert!(
            FIXTURE.contains("__twin = 'first'") && FIXTURE.contains("__twin = 'second'"),
            "the twins must write DIFFERENT evidence, or nth proves nothing"
        );
    }

    // `.numb` is the ONLY case in the fixture where the element that receives a
    // click at the target's centre is the target's own ANCESTOR. It is what
    // isolates `hit.contains(el)` — with the liveness filter in place, the
    // `visibility:hidden` decoy never reaches the hit test, so `.go` alone would
    // let that clause creep back in.
    #[test]
    fn the_numb_button_is_the_ancestor_hit_case() {
        let numb = FIXTURE
            .split(r#"class="numb""#)
            .nth(1)
            .expect("a .numb button exists");
        assert!(
            numb.contains("pointer-events:none"),
            "the numb button must refuse the pointer itself, so elementFromPoint returns its \
             PARENT rather than a sibling: {numb}"
        );
        assert!(
            FIXTURE.contains(r#"<div id="deaf""#),
            "the parent must be identifiable in the refusal message"
        );
        assert!(
            FIXTURE.contains("__numb = true"),
            "the numb button must leave evidence, or 'the handler never ran' is unfalsifiable"
        );
    }

    // The ghost must replace itself, not merely hide: `detached_node` is about a
    // node leaving the document, and a hidden node is a different refusal.
    #[test]
    fn the_ghost_detaches_itself_rather_than_hiding() {
        assert!(
            FIXTURE.contains("g.replaceWith(g.cloneNode(true))"),
            "the ghost must leave the document, or this tests `hidden` instead of `detached_node`"
        );
        assert!(
            FIXTURE.contains("height:4000px"),
            "the ghost must be far enough down that reaching it requires a real scroll"
        );
    }
}
