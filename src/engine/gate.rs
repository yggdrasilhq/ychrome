//! Phase A — the gate (`docs/agent-engine.md` §8).
//!
//! Five proofs, each journaled with the number or the pixel that decided it:
//!
//! 1. a headless display and one web view on it;
//! 2. example.com loads;
//! 3. a PNG readback whose PIXELS carry the "Example Domain" text;
//! 4. `eval` returns `document.title`;
//! 5. a trusted input event mutates DOM state that an untrusted synthetic
//!    click provably does not.
//!
//! This is a committed, re-runnable subcommand rather than a spike that gets
//! deleted: the gate is the regression test for the substrate, and it has to
//! be runnable again on another host (the GUI host) and after any WebKit bump.
//!
//! Nothing here touches profiles, jars, egress, adblock, userscripts, UA or
//! the vault. Phase A does not need identity and must not pre-wire it — those
//! modules have one owner each and Phase C reuses them as they stand.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::host::{Engine, Shot};

/// Viewport for the gate. Wide enough that example.com's centred content sits
/// clear of the edges, so the blank control region really is blank.
const VIEWPORT_W: i32 = 1024;
const VIEWPORT_H: i32 = 768;

/// A pixel counts as ink below this luma. example.com's backgrounds are
/// #f0f0f2 and #fdfdff; its body text is #000-ish. Nothing in between.
const INK_LUMA: u32 = 128;

/// Below this many dark pixels inside the heading's own rect, we do not
/// believe the words painted. "Example Domain" at the default 32px h1 inks
/// several thousand; a stray antialiasing artefact inks a handful.
const MIN_HEADING_INK: u32 = 300;

// Compile-time, not test-time: an ink floor near zero would let a blank canvas
// pass proof 3, and a threshold outside the luma range would make every pixel
// (or none) count as ink. Neither may ever build.
const _: () = assert!(
    MIN_HEADING_INK >= 100,
    "an ink floor near zero proves nothing"
);
const _: () = assert!(
    INK_LUMA > 0 && INK_LUMA < 255,
    "the ink threshold must discriminate"
);

/// One proof's outcome.
struct Proof {
    number: u32,
    name: &'static str,
    pass: bool,
    detail: Value,
}

impl Proof {
    fn to_json(&self) -> Value {
        json!({
            "proof": self.number,
            "name": self.name,
            "pass": self.pass,
            "detail": self.detail,
        })
    }
}

/// Where the gate leaves its PNGs. Beside the journal, because they are the
/// same evidence trail.
fn artifact_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("ychrome")
        .join("engine-gate");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The trusted-input differential page.
///
/// The handler mutates `#guard` ONLY when the event is trusted. That is what
/// makes this a differential rather than a pair of observations: after the
/// synthetic click the guarded state must still read `untouched`, and only a
/// real seat event may move it. A page that merely recorded `isTrusted` would
/// pass even if the trusted path did nothing.
const TRUST_PAGE: &str = r#"<!doctype html><html><body style="margin:0;background:#fff">
<div id="target" style="position:absolute;left:0;top:0;width:400px;height:200px;background:#0a0"></div>
<div id="guard">untouched</div>
<script>
window.__seen = [];
document.getElementById('target').addEventListener('click', function (event) {
  window.__seen.push(event.isTrusted);
  if (event.isTrusted) {
    document.getElementById('guard').textContent = 'mutated-by-trusted-input';
  }
});
window.__synthetic = function () {
  document.getElementById('target').dispatchEvent(new MouseEvent('click', {
    bubbles: true, cancelable: true, clientX: 50, clientY: 50
  }));
  return true;
};
</script></body></html>"#;

/// Run the gate. Returns the full report; the caller decides how to print it.
pub fn run() -> Result<Value> {
    let started = Instant::now();
    let dir = artifact_dir()?;
    let mut proofs: Vec<Proof> = Vec::new();

    crate::daemon::journal(
        "engine.gate.start",
        json!({
            "viewport": {"w": VIEWPORT_W, "h": VIEWPORT_H},
            "substrates": super::substrate::probe_all()
                .iter().map(|p| p.to_json()).collect::<Vec<_>>(),
        }),
    );

    // ---- Proof 1: a headless display and one view on it ------------------
    let engine = Engine::start(VIEWPORT_W, VIEWPORT_H)?;
    let open_started = Instant::now();
    engine.open("gate-main", VIEWPORT_W, VIEWPORT_H)?;
    let ids = engine.page_ids()?;
    record(
        &mut proofs,
        Proof {
            number: 1,
            name: "headless display + one web view",
            pass: ids == vec!["gate-main".to_string()],
            detail: json!({
                "substrate": engine.substrate().id(),
                "display": engine.display_name(),
                "display_size": {"w": engine.display_size().0, "h": engine.display_size().1},
                "pages": ids,
                "elapsed_ms": open_started.elapsed().as_millis(),
                "substrate_probes": engine.probes().iter().map(|p| p.to_json()).collect::<Vec<_>>(),
            }),
        },
    );

    // ---- Proof 2: example.com loads --------------------------------------
    let load_started = Instant::now();
    let committed = engine.goto("gate-main", "https://example.com/", Duration::from_secs(45))?;
    let ready: Value = engine.eval("gate-main", "document.readyState")?;
    record(
        &mut proofs,
        Proof {
            number: 2,
            name: "load example.com",
            pass: committed.starts_with("https://example.com") && ready == json!("complete"),
            detail: json!({
                "committed_url": committed,
                "ready_state": ready,
                "elapsed_ms": load_started.elapsed().as_millis(),
            }),
        },
    );

    // ---- Proof 4 (read before the pixel, so the rect is trustworthy) -----
    // `document.title` comes back through the same /eval path Phase B exposes.
    let title = engine.eval("gate-main", "document.title")?;
    record(
        &mut proofs,
        Proof {
            number: 4,
            name: "eval returns document.title",
            pass: title == json!("Example Domain"),
            detail: json!({ "title": title, "expected": "Example Domain" }),
        },
    );

    // ---- Proof 3: the PNG's PIXELS carry the heading ---------------------
    //
    // Not "a PNG came back" — a blank PNG comes back from a broken view too.
    // We ask the DOM where the <h1> is, then count ink INSIDE that rect and
    // inside a rect the layout says is empty. Text present + control empty is
    // the only pair that can distinguish a painted page from both a blank
    // canvas and a uniformly dark one.
    let heading: Value = engine.eval(
        "gate-main",
        "(() => { const h = document.querySelector('h1'); const r = h.getBoundingClientRect();
          return { text: h.textContent.trim(), x: Math.round(r.x), y: Math.round(r.y),
                   w: Math.round(r.width), h: Math.round(r.height) }; })()",
    )?;
    let shot_started = Instant::now();
    let shot: Shot = engine.shot("gate-main")?;
    let shot_ms = shot_started.elapsed().as_millis();
    let png_path = dir.join("proof3-example-com.png");
    std::fs::write(&png_path, &shot.png)?;

    let rect = |key: &str| heading[key].as_i64().unwrap_or(0) as i32;
    let heading_ink = shot.dark_pixels(rect("x"), rect("y"), rect("w"), rect("h"), INK_LUMA);
    // A region the layout guarantees is empty: bottom-left of the viewport,
    // clear of example.com's centred content box.
    let control_ink = shot.dark_pixels(0, VIEWPORT_H - 80, 200, 60, INK_LUMA);

    let pixel_pass = heading["text"] == json!("Example Domain")
        && shot.width == VIEWPORT_W
        && shot.height == VIEWPORT_H
        && heading_ink >= MIN_HEADING_INK
        && control_ink == 0;
    record(
        &mut proofs,
        Proof {
            number: 3,
            name: "PNG readback carries the \"Example Domain\" pixels",
            pass: pixel_pass,
            detail: json!({
                "png": png_path.display().to_string(),
                "png_bytes": shot.png.len(),
                "size": {"w": shot.width, "h": shot.height},
                "heading": heading,
                "heading_ink_px": heading_ink,
                "heading_ink_min": MIN_HEADING_INK,
                "blank_control_ink_px": control_ink,
                "shot_ms": shot_ms,
            }),
        },
    );

    // ---- Proof 5: the isTrusted differential ------------------------------
    //
    // A SECOND live page, concurrently with the first: the gate doubles as the
    // smallest possible down payment on Phase B's "10 concurrent live pages".
    engine.open("gate-trust", VIEWPORT_W, VIEWPORT_H)?;
    engine.load_html(
        "gate-trust",
        TRUST_PAGE,
        "https://gate.invalid/trust",
        Duration::from_secs(20),
    )?;

    engine.eval("gate-trust", "window.__synthetic()")?;
    engine.settle(Duration::from_millis(400))?;
    let after_synthetic: Value = engine.eval(
        "gate-trust",
        "({ guard: document.getElementById('guard').textContent, seen: window.__seen })",
    )?;

    let dispatched = engine.click_trusted("gate-trust", 50.0, 50.0)?;
    engine.settle(Duration::from_millis(400))?;
    let after_trusted: Value = engine.eval(
        "gate-trust",
        "({ guard: document.getElementById('guard').textContent, seen: window.__seen })",
    )?;

    let trust_png = dir.join("proof5-trusted-input.png");
    std::fs::write(&trust_png, &engine.shot("gate-trust")?.png)?;

    let synthetic_seen = after_synthetic["seen"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let trusted_seen = after_trusted["seen"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let differential = after_synthetic["guard"] == json!("untouched")
        && synthetic_seen == vec![json!(false)]
        && after_trusted["guard"] == json!("mutated-by-trusted-input")
        && trusted_seen == vec![json!(false), json!(true)]
        // Three, not two. A click is a pointer ARRIVING and then pressing:
        // motion, press, release. This read `== 2` while `click_trusted` sent
        // only the button pair, and Phase B's `/input` corrected that — without
        // the motion, WebKit's last-known pointer position is stale, so
        // `:hover` never applies and a hover-opened menu is not open when the
        // press lands. The count is asserted exactly, rather than loosened to
        // `>= 2`, so that losing the motion again shows up here.
        && dispatched == 3;
    record(
        &mut proofs,
        Proof {
            number: 5,
            name: "trusted input mutates state a synthetic click cannot",
            pass: differential,
            detail: json!({
                "after_synthetic": after_synthetic,
                "after_trusted": after_trusted,
                "events_dispatched": dispatched,
                "concurrent_live_pages": engine.page_ids()?,
                "png": trust_png.display().to_string(),
            }),
        },
    );

    // Teardown is part of the evidence: `close` is a Phase B verb and a page
    // that will not close is a leak the pool would inherit.
    engine.close("gate-trust")?;
    engine.close("gate-main")?;
    let pages_after_close = engine.page_ids()?;

    proofs.sort_by_key(|proof| proof.number);
    let pass = proofs.iter().all(|proof| proof.pass);
    let report = json!({
        "gate": "phase-a",
        "pass": pass,
        "substrate": engine.substrate().id(),
        "substrate_is_spec_default": false,
        "display": engine.display_name(),
        "host": std::env::var("HOSTNAME").ok(),
        "elapsed_ms": started.elapsed().as_millis(),
        "artifacts": dir.display().to_string(),
        "pages_after_close": pages_after_close,
        "proofs": proofs.iter().map(Proof::to_json).collect::<Vec<_>>(),
    });
    crate::daemon::journal("engine.gate.result", report.clone());
    Ok(report)
}

/// Journal a proof the moment it is decided, then keep it for the report. The
/// journal line is written BEFORE the next proof runs, so a gate that dies
/// halfway still leaves the evidence for everything it did prove.
fn record(proofs: &mut Vec<Proof>, proof: Proof) {
    crate::daemon::journal("engine.gate.proof", proof.to_json());
    proofs.push(proof);
}

#[cfg(test)]
mod tests {
    use super::*;

    // The differential only means anything if the page refuses to move its
    // guarded state on an untrusted event. If someone "simplifies" the fixture
    // into recording isTrusted without gating the mutation, proof 5 passes on
    // a substrate with no trusted input at all.
    #[test]
    fn the_trust_fixture_gates_its_mutation_on_is_trusted() {
        assert!(
            TRUST_PAGE.contains("if (event.isTrusted)"),
            "the guarded mutation must be conditional on isTrusted"
        );
        let guarded = TRUST_PAGE
            .split("if (event.isTrusted)")
            .nth(1)
            .expect("the guarded branch exists");
        assert!(
            guarded.contains("mutated-by-trusted-input"),
            "the guarded branch must be the ONLY writer of the mutated state"
        );
        assert_eq!(
            TRUST_PAGE.matches("mutated-by-trusted-input").count(),
            1,
            "no second writer of the guarded state may exist in the fixture"
        );
        assert!(
            TRUST_PAGE.contains("dispatchEvent(new MouseEvent"),
            "the fixture must offer the untrusted half of the differential"
        );
    }
}
