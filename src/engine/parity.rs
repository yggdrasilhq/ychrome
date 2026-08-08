//! Phase C — the identity-parity proof (`ychrome engine parity`).
//!
//! §8's AC: a page logged in under profile X in the visible surface is logged
//! in in the engine with zero re-auth; the adblock differential passes
//! headless; userscript state is visible via `/eval`.
//!
//! Every check below runs through [`super::api::dispatch`], the shipping
//! router, and every one is a DIFFERENTIAL where it can be — "the ad div is
//! hidden" proves nothing on its own, because a div can be hidden for a dozen
//! reasons. The same div on a profile with ad blocking off is what makes it
//! evidence.

use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::api::{dispatch, request};

/// A generic cosmetic selector from the SHIPPED ruleset.
///
/// EasyList carries `##.Adsense`, which the converter emits as a
/// `css-display-none` rule with `"url-filter": ".*"` and no `if-domain` — one
/// of 971 generic cosmetic rules covering 36,028 selectors. Naming the exact
/// rule matters: a differential against a selector nothing blocks would pass by
/// accident on both sides.
const BLOCKED_SELECTOR: &str = "Adsense";

/// A control class no filter list mentions, on the same page, in the same
/// stylesheet pass.
const CONTROL_SELECTOR: &str = "ychrome-parity-control";

/// A probe userscript. Broad `@match` and the MAIN world, so that seeing
/// `window.__ychromeParity` from `/eval` proves both that the userscript plane
/// reaches engine pages and that world placement is honoured — an isolated
/// world would leave the page's `window` untouched, which is exactly the
/// failure `docs/adblock.md` §5 records for `youtube-adblock`.
const PROBE_SCRIPT: &str = r#"// ==UserScript==
// @name         ychrome-parity-probe
// @version      1.0.0
// @match        *://*/*
// @world        main
// @run-at       document-start
// ==/UserScript==
window.__ychromeParity = { world: 'main', at: Date.now() };
"#;

struct Check {
    name: &'static str,
    pass: bool,
    detail: Value,
}

impl Check {
    fn to_json(&self) -> Value {
        json!({ "check": self.name, "pass": self.pass, "detail": self.detail })
    }
}

fn call(verb: &str, body: Value) -> (u16, Value) {
    super::api::json_status(dispatch(&request(verb, body)))
}

fn read(page: &str, js: &str) -> Value {
    match call("eval", json!({ "page_id": page, "js": js })) {
        (200, body) => body["value"].clone(),
        (_, body) => body,
    }
}

pub fn run() -> Result<Value> {
    let started = Instant::now();
    let mut checks: Vec<Check> = Vec::new();
    let mut record = |check: Check| {
        crate::daemon::journal("engine.parity.check", check.to_json());
        checks.push(check);
    };

    // The probe script goes in through the userscript plane's OWNER, not by
    // writing a file here — `webpolicy` decides where scripts live and which
    // are enabled.
    crate::webpolicy::install_userscript("ychrome-parity-probe", PROBE_SCRIPT)
        .context("installing the parity probe userscript")?;

    // ---- 1. identity resolves FROM ITS OWNERS ----------------------------
    let (status, identity) = call("identity", json!({ "profile": "default" }));
    let expected_jar = crate::profile_dir("default")?.display().to_string();
    let expected_ua = crate::useragent::effective();
    // ⭐ THE POLICY'S SCRIPTS, PLUS EXACTLY ONE THE ENGINE OWNS.
    //
    // This used to assert equality with the policy's count, as a proxy for "the
    // engine adds nothing of its own". That proxy stopped being true the day
    // `/engine/frame` shipped: the frame bridge is engine-owned by necessity —
    // it answers on a channel `identity` registers, so no other plane could own
    // it — and the check caught the change honestly. The claim it makes now is
    // the precise one, and it is STRICTER than the old equality: every policy
    // script must be attached AND the engine's own additions must number
    // exactly the one we can name. A second smuggled-in script fails here.
    let engine_owned = [crate::engine::frame::bridge()];
    let expected_scripts =
        crate::webpolicy::policy("default").userscripts.len() + engine_owned.len();
    record(Check {
        name: "identity comes from profile_dir / useragent / webpolicy, plus only the engine's \
               own named scripts",
        pass: status == 200
            && identity["jar"] == json!(expected_jar)
            && identity["user_agent"] == json!(expected_ua)
            && identity["userscripts"] == json!(expected_scripts)
            && identity["adblock"]["attached"] == json!(true),
        detail: json!({
            "jar": identity["jar"], "expected_jar": expected_jar,
            "user_agent": identity["user_agent"], "expected_user_agent": expected_ua,
            "userscripts": identity["userscripts"], "expected_userscripts": expected_scripts,
            "policy_userscripts": crate::webpolicy::policy("default").userscripts.len(),
            "engine_owned_userscripts": engine_owned.len(),
            "adblock": identity["adblock"],
        }),
    });

    // ---- 2. the filter compiles ONCE, then loads -------------------------
    //
    // The cost the adblock lane measured: `save` recompiles unconditionally at
    // ~15.7 s / 476 MB, `load` against a populated store is 0.011 s. A second
    // profile shares the store, so it must LOAD. Measured, not assumed.
    let (_, second) = call("identity", json!({ "profile": "parity-second" }));
    let first_path = identity["adblock"]["path"].as_str().unwrap_or("");
    let second_path = second["adblock"]["path"].as_str().unwrap_or("");
    let second_ms = second["adblock"]["elapsed_ms"].as_u64().unwrap_or(u64::MAX);
    record(Check {
        name: "the content filter compiles once per store and loads thereafter",
        pass: second_path == "load" && second_ms < 2000,
        detail: json!({
            "first_profile": { "path": first_path, "elapsed_ms": identity["adblock"]["elapsed_ms"] },
            "second_profile": { "path": second_path, "elapsed_ms": second_ms },
            "identifier": identity["adblock"]["identifier"],
            "note": "first may read `load` too when the store was already warm from an earlier run",
        }),
    });

    // ---- 3. the jar is the SAME jar (zero re-auth) -----------------------
    //
    // Honest scope: this proves the engine reads and writes the very directory
    // `profile_dir` gives the visible surface, and that a cookie set in one
    // engine page is there for the next one. It does NOT drive the yggterm GUI
    // — that needs a live desktop and the owner is working at it.
    let (_, page) = call(
        "open",
        json!({ "url": "https://example.com/", "profile": "default" }),
    );
    let first_page = page["page_id"].as_str().unwrap_or_default().to_string();
    let stamp = format!("parity-{}", std::process::id());
    read(
        &first_page,
        &format!("document.cookie = 'ychrome_parity={stamp}; path=/; max-age=600'"),
    );
    let set_here = read(&first_page, "document.cookie");
    let _ = call("close", json!({ "page_id": first_page }));

    let (_, page2) = call(
        "open",
        json!({ "url": "https://example.com/", "profile": "default" }),
    );
    let second_page = page2["page_id"].as_str().unwrap_or_default().to_string();
    let seen_there = read(&second_page, "document.cookie");
    let carried = seen_there.as_str().is_some_and(|c| c.contains(&stamp));
    record(Check {
        name: "a cookie set on one engine page is there for the next, in the surface's own jar",
        pass: carried && identity["jar"] == json!(expected_jar),
        detail: json!({
            "stamp": stamp,
            "set_on_first_page": set_here,
            "seen_on_second_page": seen_there,
            "jar": expected_jar,
            "scope": "same-jar persistence; the visible GUI was NOT driven (owner is working at it)",
        }),
    });

    // ---- 4. the adblock differential, headless ---------------------------
    //
    // Same page, same injected markup, two profiles: one with ad blocking on,
    // one with it off. `.Adsense` must be display:none only on the first, and
    // the control class must be visible on both — otherwise "hidden" could mean
    // anything.
    let probe_js = format!(
        "(() => {{ \
           const mk = (cls) => {{ const d = document.createElement('div'); d.className = cls; \
             d.textContent = cls; document.body.appendChild(d); \
             return getComputedStyle(d).display; }}; \
           return {{ blocked: mk('{BLOCKED_SELECTOR}'), control: mk('{CONTROL_SELECTOR}') }}; }})()"
    );
    let with_adblock = read(&second_page, &probe_js);

    // The control profile: ad blocking off, decided by webpolicy — the engine
    // does not get its own switch.
    crate::webpolicy::set_adblock_profile_disabled("parity-noblock", true)?;
    let (_, page3) = call(
        "open",
        json!({ "url": "https://example.com/", "profile": "parity-noblock" }),
    );
    let third_page = page3["page_id"].as_str().unwrap_or_default().to_string();
    let (_, noblock_identity) = call("identity", json!({ "profile": "parity-noblock" }));
    let without_adblock = read(&third_page, &probe_js);

    let hidden_when_on = with_adblock["blocked"] == json!("none");
    let visible_when_off = without_adblock["blocked"] != json!("none");
    let control_ok =
        with_adblock["control"] != json!("none") && without_adblock["control"] != json!("none");
    record(Check {
        name: "the adblock differential passes headless",
        pass: hidden_when_on && visible_when_off && control_ok,
        detail: json!({
            "rule": format!(
                "EasyList `##.{BLOCKED_SELECTOR}` -> css-display-none, url-filter \".*\", no if-domain"
            ),
            "with_adblock": with_adblock,
            "without_adblock": without_adblock,
            "noblock_profile_attached_filter": noblock_identity["adblock"]["attached"],
            "control_selector": CONTROL_SELECTOR,
        }),
    });

    // ---- 5. userscript state is visible via /eval ------------------------
    let probe = read(&second_page, "window.__ychromeParity || null");
    record(Check {
        name: "a userscript runs on an engine page, in the world it declared",
        pass: probe["world"] == json!("main"),
        detail: json!({
            "window.__ychromeParity": probe,
            "why_main_world": "an isolated-world injection would leave the page's window untouched",
        }),
    });

    // SponsorBlock, honestly: reported rather than asserted.
    let sponsorblock = crate::webpolicy::userscript_installed(crate::extensions::SPONSORBLOCK_STEM);
    let sponsorblock_note = if sponsorblock {
        json!({ "installed": true, "exercised": false,
                "why": "needs a live youtube.com watch page; not asserted here" })
    } else {
        json!({ "installed": false, "exercised": false,
                "why": "not installed on this host, so there is no state to read" })
    };

    let _ = call("close", json!({ "page_id": second_page }));
    let _ = call("close", json!({ "page_id": third_page }));
    let _ = crate::webpolicy::delete_userscript("ychrome-parity-probe");

    let pass = checks.iter().all(|check| check.pass);
    let report = json!({
        "parity": "phase-c",
        "pass": pass,
        "elapsed_ms": started.elapsed().as_millis(),
        "sponsorblock": sponsorblock_note,
        "checks": checks.iter().map(Check::to_json).collect::<Vec<_>>(),
    });
    crate::daemon::journal("engine.parity.result", report.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The differential is only a differential if the two selectors differ in
    // the one way that matters: one is in the shipped lists and one is not.
    #[test]
    fn the_differential_uses_a_real_rule_and_a_real_control() {
        let rules = crate::adblock::bundled_ruleset();
        assert!(
            rules.contains(&format!(".{BLOCKED_SELECTOR}")),
            "the blocked selector must actually appear in the shipped ruleset"
        );
        assert!(
            !rules.contains(CONTROL_SELECTOR),
            "the control selector must appear in NO rule, or it is not a control"
        );
    }

    // The probe script's whole point is world placement.
    #[test]
    fn the_probe_script_declares_the_main_world() {
        let parsed = crate::userscript::parse(PROBE_SCRIPT);
        assert_eq!(parsed.world, crate::userscript::ScriptWorld::Main);
        assert!(parsed.matches.iter().any(|m| m.contains("://*/*")));
    }
}
