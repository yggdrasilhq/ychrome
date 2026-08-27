//! Phase B — the control API, mounted at `/engine/*`.
//!
//! Per the §3 amendment the engine has no socket of its own: these routes ride
//! the ychrome daemon's `~/.yggterm/ychrome/daemon.sock`, and there is no
//! bearer token because the socket's `0600` already answers "who may call
//! this" (AGENTS.md, and §4 as corrected).
//!
//! [`dispatch`] is the ONE router. The daemon's socket handler calls it, and so
//! does `ychrome engine bench` — deliberately, so the thing proven on a live
//! host is the real router and not a parallel test-only path that could drift
//! from it.

use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::frame;
use super::host::{CookieSpec, Engine, InputEvent, NavAction, PixelRect, ShotRegion};
use super::js;
use super::pool::{self, pool};
use crate::sidebar::ParsedRequest;

/// Default page viewport. The spec's `page` shape carries a per-page viewport;
/// v1 gives every page the engine's own size unless the caller says otherwise.
pub(super) const DEFAULT_W: i32 = 1280;
pub(super) const DEFAULT_H: i32 = 900;

/// How long a navigation may take before `/engine/goto` gives up.
const GOTO_TIMEOUT: Duration = Duration::from_secs(45);

/// `/engine/wait`'s default budget, and its poll cadence (§4).
const WAIT_TIMEOUT_MS: u64 = 15_000;
const POLL_MS: u64 = 100;

/// What a route hands back. Most replies are JSON; `/engine/shot` is PNG bytes
/// with its own content type, as §4 specifies — a base64 blob inside JSON
/// would have been a second encoding of an image the HTTP layer can carry
/// natively.
/// Writes an NDJSON stream, one JSON object per line, flushing as it goes.
pub type NdjsonBody = Box<dyn FnOnce(&mut dyn std::io::Write) + Send>;

pub enum Reply {
    Json(u16, Value),
    /// PNG bytes plus the ACCOUNT of them, which rides in the
    /// `X-Ychrome-Shot` response header rather than in the body.
    ///
    /// A capture that answers only with pixels cannot say what it captured, and
    /// for a CROPPED capture that is not a nicety: the difference between "the
    /// element" and "the wrong element at the same size" is invisible in the
    /// image. The header carries the region, the measured CSS→device scale, the
    /// document geometry the crop was computed against, and the selector
    /// account — the same `{matches, hittable, nth}` shape `/engine/input`
    /// reports. Body stays pure image bytes, so `--out` still writes a file
    /// nothing has to strip a wrapper off.
    Png {
        png: Vec<u8>,
        meta: Value,
    },
    /// A streaming NDJSON body. The closure writes one JSON object per line as
    /// each result lands, so a caller sees page 1 while page 300 is still
    /// loading. §4 specifies this for `/engine/batch`; Phase D shipped a JSON
    /// array as an honest placeholder and this closes it.
    Ndjson(NdjsonBody),
}

impl Reply {
    fn bad(status: u16, message: impl Into<String>) -> Reply {
        Reply::Json(status, json!({ "ok": false, "error": message.into() }))
    }
}

/// The process-wide engine, started on the FIRST `/engine/*` call and never
/// before: a daemon that no agent has asked to browse must be byte-for-byte
/// the daemon it was, with no GTK, no display and no WebKit in it.
static ENGINE: OnceLock<Mutex<Option<Arc<Engine>>>> = OnceLock::new();

/// Get the engine, starting it if this is the first call. The mutex is held
/// across the start so two concurrent first-callers cannot race two displays
/// into existence.
fn engine() -> Result<Arc<Engine>> {
    let slot = ENGINE.get_or_init(|| Mutex::new(None));
    let mut guard = slot
        .lock()
        .map_err(|_| anyhow::anyhow!("engine lock poisoned"))?;
    if let Some(engine) = guard.as_ref() {
        return Ok(Arc::clone(engine));
    }
    let started = Instant::now();
    let engine = Arc::new(Engine::start(DEFAULT_W, DEFAULT_H)?);
    crate::daemon::journal(
        "engine.start",
        json!({
            "substrate": engine.substrate().id(),
            "display": engine.display_name(),
            "elapsed_ms": started.elapsed().as_millis(),
        }),
    );
    *guard = Some(Arc::clone(&engine));
    // §5's governor tick. Started with the engine, not before: a daemon with
    // no engine has nothing to govern.
    pool::start_governor(Arc::downgrade(&engine));
    Ok(engine)
}

/// Stop the engine if one was ever started, taking its display down with it.
///
/// Rust never drops a `static`, and the engine CLI deliberately ends with
/// `_exit`, which skips even the atexit chain — so `Engine::drop` would never
/// run for the registry's engine and its Xvfb would outlive the process.
/// Measured before this existed: one orphaned Xvfb per `ychrome engine bench`,
/// while `engine gate` (whose Engine is a local) cleaned up fine. The lesson is
/// that owning your own exit means owning your own teardown too.
pub fn shutdown() {
    let Some(slot) = ENGINE.get() else {
        return;
    };
    let taken = slot.lock().ok().and_then(|mut guard| guard.take());
    if let Some(engine) = taken {
        crate::daemon::journal("engine.stop", json!({ "display": engine.display_name() }));
        // An Arc: this reaps the display only if no request thread still holds
        // a handle, which is the correct order — the last caller out turns the
        // lights off.
        drop(engine);
    }
}

/// Does this path belong to the engine? The daemon asks before parsing
/// anything else, so `/engine/*` and the legacy verbs can never collide.
pub fn owns(path: &str) -> bool {
    path == "/engine" || path.starts_with("/engine/")
}

/// Route one `/engine/*` request.
///
/// Every verb is journaled with its latency whether it succeeded or not —
/// §6's "no silent driving" rule. An agent's whole session through the engine
/// is replayable in reading order from `journal.jsonl`.
/// The ctl plane's control state — the signer whose token the shim bakes into
/// ctl-opened pages, and the registry the `fido2` verbs resolve against. Set
/// once by the daemon at startup; absent in tests and in session servers
/// (which answer `/fido2/*` through their own sidebar route).
pub fn set_ctl_control(control: std::sync::Arc<crate::sidebar::ControlState>) {
    *CTL_CONTROL
        .lock()
        .unwrap() = Some(control);
}
fn ctl_control() -> Option<std::sync::Arc<crate::sidebar::ControlState>> {
    CTL_CONTROL.lock().unwrap().clone()
}
static CTL_CONTROL: std::sync::Mutex<Option<std::sync::Arc<crate::sidebar::ControlState>>> =
    std::sync::Mutex::new(None);

/// Does `url` satisfy one userscript match pattern? The patterns the passkey
/// shim carries are GM-style (`*://www.npmjs.com/*`); the check is deliberately
/// shallow - scheme wildcard, host wildcard-or-exact, any path.
fn url_matches_pattern(url: &str, pattern: &str) -> bool {
    let Some((scheme_part, rest)) = pattern.split_once("://") else {
        return url.contains(pattern.trim_start_matches('*'));
    };
    let (pattern_host, pattern_path) = match rest.split_once('/') {
        Some((host, path)) => (host, Some(path)),
        None => (rest, None),
    };
    let Some((url_scheme, url_rest)) = url.split_once("://") else {
        return false;
    };
    let url_host_path = url_rest.split('/').next().unwrap_or(url_rest);
    let (url_host, url_path) = match url_host_path.split_once('/') {
        Some((host, path)) => (host, Some(path)),
        None => (url_host_path, None),
    };
    let scheme_ok = scheme_part == "*" || scheme_part == url_scheme;
    let host_ok = pattern_host == "*"
        || pattern_host
            .strip_prefix("*.")
            .map(|suffix| url_host.ends_with(suffix))
            .unwrap_or_else(|| pattern_host == url_host);
    let path_ok = pattern_path.is_none_or(|wanted| {
        wanted == "*" || url_path.is_some_and(|actual| actual.starts_with(wanted))
    });
    scheme_ok && host_ok && path_ok
}

/// The passkey shim scripts for the ctl control's signer, with the vault
/// rp_id scoping already applied. `None` when the ctl control state is absent
/// (tests, session servers) — callers simply skip the injection.
/// The ctl signer, for the engine's `yggterm-appctl://` scheme handler.
pub fn ctl_signer() -> Option<std::sync::Arc<crate::passkey::Signer>> {
    let control = ctl_control()?;
    Some(std::sync::Arc::clone(&control.signer))
}

pub fn ctl_passkey_shim_scripts() -> Vec<crate::userscript::Userscript> {
    match ctl_control() {
        Some(control) => crate::sidebar::passkey_shim_scripts(&control),
        None => Vec::new(),
    }
}

pub fn dispatch(request: &ParsedRequest) -> Reply {
    let started = Instant::now();
    let verb = request
        .path
        .trim_start_matches("/engine")
        .trim_matches('/')
        .to_string();
    let reply = route(&verb, request);
    let (status, error) = match &reply {
        Reply::Json(status, body) => (*status, body.get("error").and_then(Value::as_str)),
        Reply::Png { .. } | Reply::Ndjson(_) => (200, None),
    };
    crate::daemon::journal(
        "engine.verb",
        json!({
            "verb": verb,
            "method": request.method,
            "status": status,
            "error": error,
            "page_id": request.body.get("page_id"),
            "elapsed_ms": started.elapsed().as_millis(),
        }),
    );
    reply
}

fn route(verb: &str, request: &ParsedRequest) -> Reply {
    // Read the page id once, here, so no verb invents its own spelling of it.
    let page_id = request
        .body
        .get("page_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let engine = match engine() {
        Ok(engine) => engine,
        Err(error) => return Reply::bad(503, format!("engine unavailable: {error}")),
    };

    // Any verb that drives a named page needs it LIVE, and needs the LRU clock
    // stamped — or the governor will park the very page a script is working.
    // A parked page resumes here, transparently, which is what makes §5's
    // "hundreds of logical pages" usable rather than something a caller has to
    // manage by hand.
    if let Some(id) = &page_id
        && DRIVES_A_PAGE.contains(&verb)
        && let Err(error) = pool::ensure_live(&engine, id)
    {
        return saturated_or_bad(&error);
    }

    match verb {
        "open" => {
            let id = page_id.unwrap_or_else(new_page_id);
            let width = request.body["viewport"]["w"]
                .as_i64()
                .unwrap_or(DEFAULT_W as i64) as i32;
            let height = request.body["viewport"]["h"]
                .as_i64()
                .unwrap_or(DEFAULT_H as i64) as i32;
            let tags = request.body["tags"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let url = request
                .body
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("about:blank");
            // The profile is the identity the page browses under (§4). Default
            // is `default`, the same jar the visible browser opens with.
            let profile = request
                .body
                .get("profile")
                .and_then(Value::as_str)
                .unwrap_or("default");
            // The pool makes room FIRST (§5): parking LRU views until this one
            // fits the budget, or refusing with the pressure numbers.
            if let Err(error) = pool::open(&engine, &id, url, profile, tags, (width, height)) {
                return saturated_or_bad(&error);
            }
            // The page is PINNED until this first load settles, so the
            // governor cannot park a view out from under a navigation that has
            // not happened yet. Unpinned on both paths — a failed load must
            // still release the page, or it would hold a live slot forever.
            let loaded = if url == "about:blank" {
                Ok(String::new())
            } else {
                // Identity BEFORE the navigation — the UA is a request header,
                // so applying it after the load would identify the browser
                // correctly only from the second load on. Zoom is per SITE too
                // but it is a rendering fact, so it rides after.
                let _ = engine.apply_identity(&id, url);
                let done = engine.goto(&id, url, GOTO_TIMEOUT);
                let _ = engine.apply_zoom(&id, url);
                journal_main_frame(&engine, &id, "engine.load.open");
                done
            };
            pool().unpin(&id);
            if let Err(error) = loaded {
                return Reply::bad(502, error.to_string());
            }
            // ⛔ THE PASSKEY SHIM, FOR PAGES NO SESSION SURFACE OPENED. Session
            // surfaces get the scoped shim from their `/policy` fetch; a
            // ctl-opened page never makes that request, so `navigator.credentials`
            // stayed undefined on it and EVERY site's WebAuthn died silently —
            // the "passkey UX is broken" report. Same scoping (vault rp_ids),
            // same signer token, evaluated here because the page has just
            // loaded; the `goto` arm re-applies it across in-page navigations.
            if let Some(control) = ctl_control() {
                for script in crate::sidebar::passkey_shim_scripts(&control) {
                    if script.matches.is_empty()
                        || script.matches.iter().any(|pattern| url_matches_pattern(url, pattern))
                    {
                        let _ = engine.eval(&id, &script.body);
                    }
                }
            }
            Reply::Json(200, page_status(&engine, &id))
        }
        "close" => match page_id {
            None => Reply::bad(400, "close needs a page_id"),
            Some(id) => {
                let pooled = pool().remove(&id);
                // A parked page owns no view, so closing it is just forgetting
                // it — not an error the caller should have to special-case.
                let parked = pooled
                    .as_ref()
                    .is_some_and(|page| page.state != pool::PageState::Live);
                match engine.close(&id) {
                    Ok(()) => Reply::Json(200, json!({ "ok": true, "closed": id })),
                    Err(_) if parked => {
                        Reply::Json(200, json!({ "ok": true, "closed": id, "was_parked": true }))
                    }
                    Err(error) => Reply::bad(404, error.to_string()),
                }
            }
        },
        "pages" => {
            // The live views first: a listing reports where a page IS, not
            // where its caller last sent it. Without this, an in-page
            // navigation — a form POST handing off to a payment gateway — is
            // invisible to the one verb an agent can safely poll mid-flight.
            pool::sync_live_urls(&engine);
            // LOGICAL pages, live and parked alike, filtered as §4 allows. A
            // listing that showed only live views would hide most of the pool.
            let want_state = crate::sidebar::query_value(&request.query, "state");
            let want_tag = crate::sidebar::query_value(&request.query, "tag");
            let pages: Vec<Value> = pool()
                .all()
                .into_iter()
                .filter(|page| want_state.as_deref().is_none_or(|s| page.state.id() == s))
                .filter(|page| {
                    want_tag
                        .as_deref()
                        .is_none_or(|t| page.tags.iter().any(|tag| tag == t))
                })
                .map(|page| page.to_json())
                .collect();
            Reply::Json(200, json!({ "ok": true, "pages": pages }))
        }
        "goto" => {
            let (Some(id), Some(url)) = (page_id, request.body.get("url").and_then(Value::as_str))
            else {
                return Reply::bad(400, "goto needs a page_id and a url");
            };
            // Same order as `open`: identity first (it is a request header),
            // zoom after (it is a rendering fact), then the trace.
            let _ = engine.apply_identity(&id, url);
            match engine.goto(&id, url, GOTO_TIMEOUT) {
                Ok(_) => {
                    let _ = engine.apply_zoom(&id, url);
                    journal_main_frame(&engine, &id, "engine.load.goto");
                    Reply::Json(200, page_status(&engine, &id))
                }
                Err(error) => Reply::bad(502, error.to_string()),
            }
        }
        "eval" => {
            // ⛔ `frame=` IS REFUSED HERE, BY NAME, AND THAT IS THE DESIGN
            // RATHER THAN A GAP.
            //
            // Honouring it would mean a bridge inside the child that evaluates a
            // string it was handed. Commands reach that bridge by cross-frame
            // `postMessage`, whose delivery is NOT world-scoped (measured —
            // `ychrome engine worlds`), so a hostile TOP page can post a
            // well-formed one. That is a page making our code run inside another
            // origin's document: a same-origin-policy breach WE would be
            // providing, and no token defends against it — whichever document
            // receives the message reads the token straight out of it.
            //
            // The read verb exists so the legitimate need is still served, and
            // the refusal names it rather than leaving a caller to guess.
            if request
                .body
                .get("frame")
                .is_some_and(|value| !value.is_null())
            {
                return Reply::bad(
                    400,
                    "/engine/eval refuses `frame=` and always will: evaluating a caller's script \
                     inside a cross-origin child means a bridge that runs page-supplied strings, \
                     and the embedding page can forge the command that carries one. Use \
                     /engine/frame instead — it resolves a selector INSIDE the frame and answers \
                     exists/count/text/value/rect/point, read-only. To DRIVE the frame, pass the \
                     same `frame=` to /engine/input, which aims a real event by that rect.",
                );
            }
            let (Some(id), Some(js)) = (page_id, request.body.get("js").and_then(Value::as_str))
            else {
                return Reply::bad(400, "eval needs a page_id and js");
            };
            match engine.eval(&id, js) {
                Ok(value) => Reply::Json(200, json!({ "ok": true, "value": value })),
                Err(error) => Reply::bad(400, error.to_string()),
            }
        }
        // Read a selector INSIDE a cross-origin child frame — the one thing
        // `/engine/eval` cannot do and must not be taught to (see its arm).
        //
        // The reply is the child's own answer plus `point`: the viewport
        // coordinate a click would have to land on, which is the frame's box
        // plus the element's rect inside it. `point` is here rather than left to
        // the caller because `/engine/input`'s `frame=` computes it through the
        // SAME function — one owner, so a caller who clicks by hand and a caller
        // who passes `frame=` aim at the same pixel.
        "frame" => {
            let Some(id) = page_id else {
                return Reply::bad(400, "frame needs a page_id");
            };
            let Some(selector) = request.body.get("selector").and_then(Value::as_str) else {
                return Reply::bad(
                    400,
                    "frame needs a `selector` to resolve inside the frame (and a `frame` naming \
                     which one)",
                );
            };
            let target = match frame::parse_target(&request.body) {
                Ok(target) => target,
                Err(message) => return Reply::bad(400, message),
            };
            let nth = match request.body.get("nth") {
                None | Some(Value::Null) => 0,
                Some(value) => match value.as_u64() {
                    Some(nth) => nth as usize,
                    None => {
                        return Reply::bad(
                            400,
                            format!("`nth` must be a non-negative integer, got {value}"),
                        );
                    }
                },
            };
            let started = Instant::now();
            match frame::read(
                &engine,
                &id,
                &target,
                selector,
                nth,
                frame::timeout_ms(&request.body),
            ) {
                Ok(read) => {
                    let mut report = frame::read_report(&read);
                    report["selector"] = json!(selector);
                    report["elapsed_ms"] = json!(started.elapsed().as_millis());
                    Reply::Json(200, report)
                }
                Err((status, message)) => Reply::bad(status, message),
            }
        }
        // Autofill a vault item into the page's login form.
        //
        // The reply names the FIELDS that were filled and never a value, which
        // is the same contract the sidebar's fill action keeps. `filled` is the
        // script's own verdict — "filled", "user-only" or "no-fields" — so a
        // caller can tell "the vault answered but the page had no password
        // field" from "the vault refused", instead of both arriving as a bare
        // success the way they would if this returned only `ok`.
        //
        // ⛔ AND A FOURTH WORD, `unverified`, WHICH IS THE POINT OF THE REPLY.
        // The first three describe what the script SET OUT to do. Only a
        // readback of what the page is holding afterwards can separate a fill
        // that landed from one that was truncated, reformatted, re-rendered
        // away, or written into one of two fields — and until 2026-08-07 all
        // four of those answered `filled`. `fields` carries the per-field
        // readback (a LENGTH, never a value) and `confirm` names what happened
        // to a second secret field. See `sidebar::SET_FIELD`.
        "fido2" => {
            // THE AGENTIC PASSKEY DOOR. A headless daemon parks a WebAuthn
            // ceremony whose OSC emission has no GUI stream to arrive on -
            // without this verb the login timed out every time. `list` shows
            // every parked ceremony with the accounts that match; `grant` and
            // `deny` resolve one through the SAME path the GUI dialog's HTTP
            // grant takes. The ctl control's signer is answered first (it is
            // the one whose token ctl pages carry); the signer registry walk
            // covers session signers too.
            let action = request
                .body
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("list");
            let resolve = |signer: &crate::passkey::Signer| {
                if action == "grant" {
                    signer.ctl_grant(&request.body)
                } else {
                    signer.ctl_deny(&request.body)
                }
            };
            match action {
                "list" => {
                    let mut ceremonies: Vec<Value> = Vec::new();
                    if let Some(control) = ctl_control() {
                        ceremonies.extend(control.signer.pending_summary());
                    }
                    crate::passkey::for_each_live_signer(|signer| {
                        ceremonies.extend(signer.pending_summary());
                    });
                    Reply::Json(200, json!({ "ok": true, "ceremonies": ceremonies }))
                }
                "grant" | "deny" => {
                    let mut resolved = false;
                    if let Some(control) = ctl_control() {
                        if resolve(&control.signer).0 == 200 {
                            resolved = true;
                        }
                    }
                    if !resolved {
                        crate::passkey::for_each_live_signer(|signer| {
                            if !resolved && resolve(signer).0 == 200 {
                                resolved = true;
                            }
                        });
                    }
                    Reply::Json(
                        200,
                        json!({ "ok": true, "resolved": resolved, "already": !resolved }),
                    )
                }
                "shim" => {
                    // The BUILT shim for the ctl signer, so an agent can inject
                    // it by eval on pages whose WebKit allowlist matching is in
                    // doubt. Token included: the caller already is the host
                    // operator (this is the same trust the open/goto injection
                    // runs with).
                    match ctl_control() {
                        Some(control) => {
                            Reply::Json(200, json!({ "ok": true, "shim": control.signer.shim_userscript() }))
                        }
                        None => Reply::bad(503, "no ctl control state"),
                    }
                }
                other => Reply::bad(
                    400,
                    format!("fido2 action must be list|grant|deny|shim, got {other:?}"),
                ),
            }
        }
        "fill" => {
            let (Some(id), Some(entry)) =
                (page_id, request.body.get("entry").and_then(Value::as_str))
            else {
                return Reply::bad(400, "fill needs a page_id and entry");
            };
            let user = request.body.get("user").and_then(Value::as_str);
            match crate::sidebar::vault_fill_script(entry, user) {
                // A locked or absent agent is the vault's answer, not the
                // page's, so it is a 502 and says so — an agent that reads
                // "fill failed" as "wrong selector" will hunt the DOM for an
                // hour over a vault nobody unlocked.
                Err(error) => Reply::bad(502, format!("vault: {error}")),
                Ok(script) => match engine.eval(&id, &script) {
                    Ok(value) => Reply::Json(
                        200,
                        json!({
                            "ok": true,
                            "entry": entry,
                            // `filled` stays a STRING: it is the documented
                            // field and the one every existing caller reads.
                            "filled": value.get("filled").unwrap_or(&value),
                            "fields": value.get("fields"),
                            "confirm": value.get("confirm"),
                            "secret_field_count": value.get("secret_field_count"),
                        }),
                    ),
                    Err(error) => Reply::bad(400, error.to_string()),
                },
            }
        }
        // Type a stored CARD into the page's payment form.
        //
        // ⛔ THE VERB THE ENGINE'S OWN PREMISE NEEDED AND DID NOT HAVE. `ctl`
        // exists so agent browsing never touches the operator's machine
        // (`agent-engine.md` §4), and the co-browse doctrine prefers dev for
        // exactly that reason — but `server app web fill-card` needs a
        // registered GUI client, which dev has none of. So the one class of
        // task with the strongest reason to stay off his laptop, entering
        // payment credentials, was the one class only his laptop could finish.
        // It cost two runs in 24 h: an RTI fee that fell back to netbanking and
        // died on a stale bank password, and an India Post booking driven end
        // to end here and then handed to a second agent to pay Rs 23.
        //
        // The PAN never exists in this route. It is read by the vault agent,
        // embedded in the script by `sidebar::vault_card_fill_script`, and
        // dropped — the same road the pane's card button takes, and the reason
        // `the_fill_card_route_never_names_the_pan` guards the shape rather
        // than the runtime.
        //
        // The reply names FIELDS in four buckets and never a value or a length:
        // for a card the length IS the value's shape (a 16-digit `got` over a
        // `cc-number` says which network issued it), so this is the one fill
        // whose report stops at the label.
        "fill-card" => {
            let (Some(id), Some(item)) =
                (page_id, request.body.get("item").and_then(Value::as_str))
            else {
                return Reply::bad(400, "fill-card needs a page_id and item");
            };
            let user = request.body.get("user").and_then(Value::as_str);
            // WHERE the PAN is going, read off the page rather than off the
            // caller's request: the audit line's whole job is to say that, and
            // a caller-supplied host would let it name somewhere the value did
            // not go. Best-effort — an unreadable location leaves the line
            // hostless rather than refusing a fill over a log field.
            let host = engine
                .eval(&id, "location.host")
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .filter(|host| !host.is_empty());
            match crate::sidebar::vault_card_fill_script(
                item,
                user,
                host.as_deref(),
                "ychrome engine",
            ) {
                // A locked or absent agent is the VAULT's answer, not the
                // page's — same 502 as `fill`, for the same reason: an agent
                // that reads this as "wrong selector" will hunt the DOM for an
                // hour over a vault nobody unlocked.
                Err(error) => Reply::bad(502, format!("vault: {error}")),
                Ok(script) => match engine.eval(&id, &script) {
                    Ok(value) => Reply::Json(
                        200,
                        json!({
                            "ok": true,
                            "item": item,
                            "host": host,
                            // The pane's one-line summary, so the two surfaces
                            // report a card fill in the same words.
                            "kept": value.get("kept"),
                            "filled": value.get("filled"),
                            "mismatched": value.get("mismatched"),
                            "absent": value.get("absent"),
                            "no_value": value.get("no_value"),
                        }),
                    ),
                    Err(error) => Reply::bad(400, error.to_string()),
                },
            }
        }
        // Put cookies INTO a page's profile store — the missing half of the
        // jar being shared text on disk (the network process reads the jar
        // once at startup, so a file-level handoff from curl never lands).
        // Accepts an inline `cookies` array or a Netscape-format `jar` path,
        // filtered to the origin's host. The reply's `visible_for_origin` is
        // the store's own readback, not the adds' success; values never
        // appear in the reply.
        "cookie-import" => {
            let Some(id) = page_id else {
                return Reply::bad(400, "cookie-import needs a page_id");
            };
            let Some(origin) = request.body.get("origin").and_then(Value::as_str) else {
                return Reply::bad(400, "cookie-import needs an origin url for the readback");
            };
            let specs = match cookie_specs(&request.body, origin) {
                Ok(specs) => specs,
                Err(error) => return Reply::bad(400, error),
            };
            if specs.is_empty() {
                // An empty import is a caller error worth naming: the jar had
                // nothing for this origin, which otherwise surfaces three
                // steps later as an unauthenticated page.
                return Reply::bad(400, "no cookies matched the origin (empty jar or filtered)");
            }
            match engine.cookie_import(&id, specs, origin.to_string()) {
                Ok(report) => Reply::Json(200, report),
                Err(error) => Reply::bad(502, error.to_string()),
            }
        }
        "shot" => match page_id {
            None => Reply::bad(400, "shot needs a page_id"),
            Some(id) => match capture(&engine, &id, &request.body) {
                Ok((shot, meta)) => Reply::Png {
                    png: shot.png,
                    meta,
                },
                Err((status, message)) => Reply::bad(status, message),
            },
        },
        "input" => {
            let Some(id) = page_id else {
                return Reply::bad(400, "input needs a page_id");
            };
            let raw = request.body["events"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            // ⛔ AN EMPTY BATCH IS A CALLER ERROR, NOT A SUCCESS. `raw` comes
            // from `event["events"].as_array().unwrap_or_default()`, so any
            // request whose shape does not produce that array arrives here as
            // zero events — and used to answer `{"dispatched":0,"ok":true}`.
            // That is a click reported as landed when nothing was even
            // resolved, let alone dispatched, and it cost a reporting agent
            // three wrong conclusions in one session, one of which blamed the
            // operator. `dispatched: 0` must never be `ok: true`.
            if raw.is_empty() {
                return Reply::bad(
                    400,
                    "/input needs a non-empty `events` array — nothing was dispatched, \
                     and an empty batch is a caller error rather than a no-op success"
                        .to_string(),
                );
            }
            // PASS 1 — shape. Pure, no page access, so a malformed batch is
            // refused with nothing dispatched, which is what the old
            // "parse the whole batch first" rule was protecting.
            let mut pending = Vec::with_capacity(raw.len());
            for event in &raw {
                match parse_input(event) {
                    Ok(parsed) => pending.push(parsed),
                    Err(error) => return Reply::bad(400, error.to_string()),
                }
            }
            // PASS 2 — resolve each selector against the page AS IT IS when its
            // own event is dispatched, then dispatch it. Resolving the batch up
            // front measured event 2 before event 1 had moved anything, so a
            // click that opened a menu was followed by a click at where the menu
            // item used to be — the exact stale-rect click `target_moved`
            // exists to refuse. A mid-batch refusal is a fact about the PAGE,
            // and it is answered with the count actually dispatched and the
            // index that stopped it, so the resulting state has a name.
            //
            // ⛔ AND A `type` EVENT IS MEASURED AFTERWARDS, because there is
            // nothing to resolve BEFORE it: text goes wherever the focus is
            // when the keys arrive. `typed_landing` is the only thing in this
            // reply derived from the page rather than from the request.
            let mut dispatched = 0u32;
            let mut resolved: Vec<Value> = Vec::new();
            for (index, item) in pending.into_iter().enumerate() {
                // Held only when this event is a `type`: the readback is two
                // extra round trips and a settle, and a click already reports
                // the node it resolved to.
                let typing = match &item {
                    PendingInput::Ready(InputEvent::Text { text }) => {
                        Some((text.chars().count(), focus_readback(&engine, &id)))
                    }
                    _ => None,
                };
                // ⛔ TEXT WITH NOWHERE TO GO IS REFUSED BEFORE IT IS TYPED.
                // This is the measured failure: an earlier event in the batch
                // re-rendered the form, focus fell back to `<body>`, and the
                // remaining `type` events were swallowed by the page while the
                // reply said `dispatched:3, ok:true`. Refusing here — rather
                // than reporting afterwards — is what keeps a secret out of a
                // page that was never going to hold it, and stops the batch
                // before the NEXT event compounds the mistake.
                //
                // The opt-out is named because the case is real: a page that
                // listens for a bare keypress ("press / to search") has no
                // focused field by design. `"require_target": false` says so.
                if let Some((_, before)) = typing.as_ref() {
                    let required = raw
                        .get(index)
                        .and_then(|event| event.get("require_target"))
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    // ⛔ A focused FRAME is exempt: the target is inside it and
                    // this readback stops at the frame's border. Refusing there
                    // is refusing the cross-origin payment case outright — see
                    // `focus_is_a_frame`. The landing is reported as
                    // unverifiable afterwards instead of being asserted here.
                    if required
                        && !accepts_text(before.as_ref())
                        && !focus_is_a_frame(before.as_ref())
                    {
                        let landed_on = before
                            .as_ref()
                            .and_then(|value| value["tag"].as_str())
                            .unwrap_or("nothing readable");
                        return Reply::Json(
                            409,
                            json!({
                                "ok": false,
                                "error": format!(
                                    "event {index} would type into <{landed_on}>, which holds no \
                                     text — the page has no focused editable field, so the text \
                                     would be discarded and reported as dispatched. Click the \
                                     field first (a selector click resolves at dispatch time), or \
                                     pass \"require_target\": false if this page reads bare \
                                     keystrokes."
                                ),
                                "dispatched": dispatched,
                                "failed_at": index,
                                "focus": before,
                                "resolved": resolved,
                            }),
                        );
                    }
                }
                let outcome = resolve_pending(&engine, &id, item)
                    .and_then(|(event, report)| Ok((engine.input(&id, vec![event])?, report)));
                match outcome {
                    Ok((count, report)) => {
                        dispatched += count;
                        if let Some(report) = report {
                            resolved.push(report);
                        }
                        if let Some((want, before)) = typing {
                            let landing = typed_landing(&engine, &id, index, want, before);
                            let discarded = landing["discarded"].as_bool().unwrap_or(false);
                            resolved.push(landing);
                            // The pre-dispatch guard cannot catch this one: the
                            // field was there when we checked, and the page took
                            // it away in response to the first keystroke. So the
                            // batch stops HERE instead of answering `ok: true`
                            // over text the page threw away — and it stops before
                            // the next event compounds it.
                            if discarded {
                                return Reply::Json(
                                    409,
                                    json!({
                                        "ok": false,
                                        "error": format!(
                                            "event {index} typed {want} character(s) and the page \
                                             kept none of them — the focus left the field while it \
                                             was being typed, which is what a form re-render does \
                                             mid-batch. Split the batch and assert the target \
                                             between the halves, or wait for the page to settle \
                                             first."
                                        ),
                                        "dispatched": dispatched,
                                        "failed_at": index,
                                        "resolved": resolved,
                                    }),
                                );
                            }
                        }
                    }
                    Err(error) => {
                        // 409, not 400: the batch was well-formed and the PAGE
                        // refused it. A caller retries a 409 after a
                        // `/engine/wait`; a 400 means fix the request.
                        return Reply::Json(
                            409,
                            json!({
                                "ok": false,
                                "error": error.to_string(),
                                "dispatched": dispatched,
                                "failed_at": index,
                                "resolved": resolved,
                            }),
                        );
                    }
                }
            }
            Reply::Json(
                200,
                json!({ "ok": true, "dispatched": dispatched, "resolved": resolved }),
            )
        }
        "nav" => {
            let (Some(id), Some(action)) = (
                page_id,
                request
                    .body
                    .get("action")
                    .and_then(Value::as_str)
                    .and_then(NavAction::parse),
            ) else {
                return Reply::bad(
                    400,
                    "nav needs a page_id and action: back|forward|reload|stop",
                );
            };
            match engine.nav(&id, action, GOTO_TIMEOUT) {
                Ok(_) => Reply::Json(200, page_status(&engine, &id)),
                Err(error) => Reply::bad(502, error.to_string()),
            }
        }
        "wait" => {
            let Some(id) = page_id else {
                return Reply::bad(400, "wait needs a page_id");
            };
            let timeout = Duration::from_millis(
                request.body["timeout_ms"]
                    .as_u64()
                    .unwrap_or(WAIT_TIMEOUT_MS),
            );
            match wait(&engine, &id, &request.body["until"], timeout) {
                Ok(outcome) => Reply::Json(200, outcome),
                Err(error) => Reply::bad(400, error.to_string()),
            }
        }
        "console" => {
            let Some(id) = page_id else {
                return Reply::bad(400, "console needs a page_id");
            };
            // Reading is destructive by default. A buffer nobody drains fills
            // with the same error a hundred times and buries the next one; a
            // caller who wants to keep it says so.
            let clear = request.body["clear"].as_bool().unwrap_or(true);
            match engine.eval(&id, &format!("({})({clear})", js::CONSOLE_READ)) {
                Ok(value) => {
                    let entries = value["entries"].as_array().cloned().unwrap_or_default();
                    // Journalled on READ rather than on capture: an agent that
                    // never asks still leaves the page's errors in the daemon's
                    // record for whoever reads it next, and journaling every
                    // console line as it happened would need an eval per line.
                    if !entries.is_empty() {
                        crate::daemon::journal(
                            "engine.page.console",
                            json!({ "page_id": id, "count": entries.len(), "entries": entries }),
                        );
                    }
                    Reply::Json(
                        200,
                        json!({
                            "ok": true,
                            "page_id": id,
                            "installed": value["installed"],
                            "cleared": clear,
                            "entries": entries,
                        }),
                    )
                }
                Err(error) => Reply::bad(400, error.to_string()),
            }
        }
        "dom" => {
            let Some(id) = page_id else {
                return Reply::bad(400, "dom needs a page_id");
            };
            let mode = request
                .body
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("snapshot");
            let js = match mode {
                "html" => "document.documentElement.outerHTML",
                "text" => "document.body ? document.body.innerText : ''",
                "snapshot" => js::DOM_SNAPSHOT,
                other => {
                    return Reply::bad(
                        400,
                        format!("unknown dom mode {other:?} (html|text|snapshot)"),
                    );
                }
            };
            match engine.eval(&id, js) {
                Ok(value) => Reply::Json(200, json!({ "ok": true, "mode": mode, "dom": value })),
                Err(error) => Reply::bad(400, error.to_string()),
            }
        }
        "park" => match page_id {
            None => Reply::bad(400, "park needs a page_id"),
            Some(id) => match pool::park(&engine, &id) {
                Ok(page) => Reply::Json(200, page.to_json()),
                Err(error) => Reply::bad(404, error.to_string()),
            },
        },
        "resume" => match page_id {
            None => Reply::bad(400, "resume needs a page_id"),
            Some(id) => match pool::resume(&engine, &id) {
                Ok(page) => Reply::Json(200, page.to_json()),
                Err(error) => saturated_or_bad(&error),
            },
        },
        "pool" | "metrics" => Reply::Json(200, pool().metrics()),
        "egress" => {
            // Egress is the CALLER's decision; the engine only applies it, and
            // applies it per profile rather than per page — the same
            // tunnel-reuse rule the surface path has, because churning a proxy
            // mid-session is what breaks a login loop.
            let profile = request.body["profile"]
                .as_str()
                .unwrap_or("default")
                .to_string();
            let socks = request.body["socks"].as_str().map(str::to_string);
            match engine.set_egress(&profile, socks.clone()) {
                Ok(()) => Reply::Json(
                    200,
                    json!({ "ok": true, "profile": profile, "socks": socks }),
                ),
                Err(error) => Reply::bad(400, error.to_string()),
            }
        }
        "identity" => {
            let profile = request.body["profile"]
                .as_str()
                .unwrap_or("default")
                .to_string();
            match engine.identity(&profile) {
                Ok(applied) => Reply::Json(200, applied),
                Err(error) => Reply::bad(500, error.to_string()),
            }
        }
        "budget" => {
            let budget = pool().set_budget(
                request.body["max_live"]
                    .as_u64()
                    .map(|value| value as usize),
                request.body["max_rss_mb"].as_u64(),
            );
            crate::daemon::journal("engine.governor.budget", budget_json(&budget));
            Reply::Json(200, json!({ "ok": true, "budgets": budget_json(&budget) }))
        }
        "batch" => batch(request),
        "" | "status" => Reply::Json(
            200,
            json!({
                "ok": true,
                "substrate": engine.substrate().id(),
                "display": engine.display_name(),
                "pages": engine.page_ids().unwrap_or_default(),
                // WHICH BUILD IS ANSWERING. A long-lived engine goes on serving
                // from an inode that was replaced on disk, so "the engine" and
                // "the binary at that path" drift apart silently. See
                // `crate::build`.
                "engine": crate::build::identity_json(),
            }),
        ),
        // ⛔ AN UNKNOWN VERB IS TWO DIFFERENT FACTS AND THEY NEED DIFFERENT
        // ANSWERS: the verb does not exist, or this engine predates it. A 404
        // that cannot tell them apart once nearly deleted a working feature —
        // `ctl frame` was reported as "advertised and 404s", and the fix
        // proposed was to remove it from the usage line. So the reply names the
        // build that is answering, and the CLI compares it with its own.
        other => Reply::Json(
            404,
            json!({
                "ok": false,
                "error": format!("unknown engine verb {other:?}"),
                "engine": crate::build::identity_json(),
                "known": VERBS,
            }),
        ),
    }
}

/// Every verb the router answers, in the order the CLI advertises them.
///
/// ⛔ **THE BANNER IS PART OF THE ROUTE.** `fill` shipped and the hand-written
/// usage string did not follow, so for weeks `ychrome ctl` advertised an engine
/// with no credential support at all — while `ctl fill` worked. The cost is a
/// belief rather than a crash, which is worse: every session that read the
/// banner re-derived "the engine cannot do credentials" and built around it.
///
/// So the list has ONE owner and the CLI reads it. `the_banner_lists_every_verb
/// _the_router_answers` parses the dispatcher's own match arms and fails when
/// the two disagree, which is what makes this a fact rather than a convention.
pub const VERBS: [&str; 24] = [
    "open",
    "close",
    "pages",
    "goto",
    "nav",
    "wait",
    "eval",
    "frame",
    "dom",
    "shot",
    "input",
    "fill",
    "fill-card",
    "console",
    "cookie-import",
    "park",
    "resume",
    "pool",
    "metrics",
    "budget",
    "batch",
    "egress",
    "identity",
    "status",
];

/// Verbs that drive a NAMED page, and therefore need it live and touched.
///
/// `park` is deliberately absent: parking a page must not first resume it.
/// `close` is absent for the same reason — forgetting a parked page should not
/// cost a page load.
const DRIVES_A_PAGE: [&str; 12] = [
    "goto",
    "nav",
    "eval",
    "frame",
    "shot",
    "dom",
    "input",
    "wait",
    "fill",
    "fill-card",
    "cookie-import",
    "console",
];

/// Parse the caller's cookies for `cookie-import` — an inline `cookies` array
/// or a Netscape-format `jar` file — keeping only cookies whose domain covers
/// the origin. A parse failure refuses the import rather than guessing: an
/// empty import surfaces three steps later as an unauthenticated page.
fn cookie_specs(body: &Value, origin: &str) -> Result<Vec<CookieSpec>, String> {
    let Some(host) = origin_host(origin) else {
        return Err(format!("origin {origin:?} has no parseable host"));
    };
    if let Some(list) = body.get("cookies").and_then(Value::as_array) {
        let mut specs = Vec::new();
        for item in list {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                return Err("inline cookie without a name".into());
            };
            let Some(value) = item.get("value").and_then(Value::as_str) else {
                return Err(format!("inline cookie {name:?} without a value"));
            };
            let domain = item.get("domain").and_then(Value::as_str).unwrap_or(&host);
            if !domain_matches(&host, domain) {
                continue;
            }
            specs.push(CookieSpec {
                name: name.to_string(),
                value: value.to_string(),
                domain: domain.to_string(),
                path: item
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("/")
                    .to_string(),
                secure: item.get("secure").and_then(Value::as_bool).unwrap_or(false),
                http_only: item
                    .get("httponly")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                max_age: item.get("max_age").and_then(Value::as_i64).unwrap_or(-1) as i32,
            });
        }
        return Ok(specs);
    }
    if let Some(jar) = body.get("jar").and_then(Value::as_str) {
        let text = std::fs::read_to_string(jar).map_err(|error| format!("jar {jar:?}: {error}"))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or(0);
        return Ok(parse_netscape_jar(&text, &host, now));
    }
    Err("cookie-import needs `cookies` (array) or `jar` (path)".into())
}

/// Netscape jar lines: 7 tab-separated fields, with `#HttpOnly_` prefixing an
/// HttpOnly cookie's domain. curl writes this format and so does the engine's
/// own persistent store. Expired rows are dropped rather than imported dead;
/// a `0` expiry is a session cookie and imports as one.
fn parse_netscape_jar(text: &str, host: &str, now: i64) -> Vec<CookieSpec> {
    let mut specs = Vec::new();
    for raw in text.lines() {
        let (line, http_only) = match raw.strip_prefix("#HttpOnly_") {
            Some(rest) => (rest, true),
            None => (raw, false),
        };
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 7 {
            continue;
        }
        let domain = fields[0];
        if !domain_matches(host, domain) {
            continue;
        }
        let expires: i64 = fields[4].parse().unwrap_or(0);
        if expires != 0 && expires <= now {
            continue;
        }
        let max_age = if expires == 0 {
            -1
        } else {
            (expires - now).min(i32::MAX as i64) as i32
        };
        specs.push(CookieSpec {
            name: fields[5].to_string(),
            value: fields[6].to_string(),
            domain: domain.to_string(),
            path: fields[2].to_string(),
            secure: fields[3].eq_ignore_ascii_case("TRUE"),
            http_only,
            max_age,
        });
    }
    specs
}

/// The host of an origin url, lowercased. The `url` crate does the parsing so
/// userinfo, ports and IPv6 literals cannot fool a hand-rolled split.
fn origin_host(origin: &str) -> Option<String> {
    url::Url::parse(origin)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

/// RFC-6265-style domain match: the exact host, or the host under the cookie's
/// domain. A leading dot on the jar side means the same thing and is ignored.
fn domain_matches(host: &str, cookie_domain: &str) -> bool {
    let domain = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// A reply's status and body, for callers that only need the pair.
///
/// An NDJSON stream is drained into memory HERE and only here — an in-process
/// caller (the bench, the parity run) still exercises the real streaming code
/// path and then reads the lines it produced, rather than the router growing a
/// second non-streaming batch for tests to call.
pub fn json_status(reply: Reply) -> (u16, Value) {
    match reply {
        Reply::Json(status, body) => (status, body),
        Reply::Png { png, meta } => {
            let mut body = meta;
            body["ok"] = json!(true);
            body["png_bytes"] = json!(png.len());
            (200, body)
        }
        Reply::Ndjson(write_body) => {
            let mut buffer = Vec::new();
            write_body(&mut buffer);
            let lines: Vec<Value> = String::from_utf8_lossy(&buffer)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            let summary = lines
                .iter()
                .rev()
                .find(|line| line["summary"] == json!(true))
                .cloned()
                .unwrap_or_else(|| json!({ "summary": false }));
            let mut body = summary;
            if let Some(map) = body.as_object_mut() {
                map.insert("ok".into(), json!(true));
                map.insert("lines".into(), json!(lines.len()));
            }
            (200, body)
        }
    }
}

fn budget_json(budget: &pool::Budget) -> Value {
    json!({
        "max_live": budget.max_live,
        "max_rss_mb": budget.max_rss_mb,
        "per_page_rss_mb": budget.per_page_rss_mb,
    })
}

/// Turn a pool error into a reply. Saturation is `429` with the live pressure
/// numbers attached (§5) — a script must SEE the constraint, not wait behind
/// it, and not be told a generic 400 that hides which budget it hit.
fn saturated_or_bad(error: &anyhow::Error) -> Reply {
    let message = error.to_string();
    if message.starts_with("pool_saturated") {
        let mut body = pool().metrics();
        if let Some(map) = body.as_object_mut() {
            map.insert("ok".into(), json!(false));
            map.insert("error".into(), json!("pool_saturated"));
            map.insert("detail".into(), json!(message));
        }
        return Reply::Json(429, body);
    }
    Reply::bad(400, message)
}

/// `/engine/batch` — the hundreds-of-pages verb (§4).
///
/// A convenience loop over open + wait with the governor in charge. It must not
/// bypass budgets, so every page goes through the same `pool::open` any single
/// `/engine/open` uses; when the pool saturates, the batch records the refusal
/// for that entry and keeps going rather than dying, because a 300-page crawl
/// that aborts on the first budget refusal is useless.
///
/// v1 returns the whole result array. §4 describes NDJSON streaming, which
/// needs a chunked responder the control endpoint does not have yet; Phase E
/// owns it, and this is a JSON array until then rather than a half-stream.
fn batch(request: &ParsedRequest) -> Reply {
    let entries = request.body["open"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() {
        return Reply::bad(400, "batch needs a non-empty `open` array");
    }
    let concurrency = request.body["concurrency"]
        .as_u64()
        .unwrap_or(8)
        .clamp(1, 64) as usize;

    Reply::Ndjson(Box::new(move |out: &mut dyn std::io::Write| {
        let started = Instant::now();
        crate::daemon::journal(
            "engine.batch.start",
            json!({ "count": entries.len(), "concurrency": concurrency }),
        );
        let mut line = |value: Value| {
            // Flush per line: a stream a reader cannot see until the end is a
            // JSON array with extra steps.
            let _ = writeln!(out, "{value}");
            let _ = out.flush();
        };

        let mut opened = 0;
        let mut refused = 0;
        // Chunked rather than one thread per entry: `concurrency` is a promise
        // about how much is in flight, and 300 threads would break it.
        for chunk in entries.chunks(concurrency) {
            let mut handles = Vec::new();
            for entry in chunk {
                let entry = entry.clone();
                handles.push(std::thread::spawn(move || {
                    let mut body = json!({ "page_id": new_page_id() });
                    if let Some(map) = body.as_object_mut() {
                        for (key, value) in entry.as_object().into_iter().flatten() {
                            map.insert(key.clone(), value.clone());
                        }
                    }
                    match dispatch(&request_with("open", body)) {
                        Reply::Json(status, body) => (status, body),
                        _ => (500, json!({ "error": "open did not answer with JSON" })),
                    }
                }));
            }
            for handle in handles {
                match handle.join() {
                    Ok((200, body)) => {
                        opened += 1;
                        line(body);
                    }
                    Ok((status, body)) => {
                        refused += 1;
                        line(json!({ "ok": false, "status": status, "error": body["error"] }));
                    }
                    Err(_) => {
                        refused += 1;
                        line(
                            json!({ "ok": false, "status": 500, "error": "batch worker panicked" }),
                        );
                    }
                }
            }
        }

        let (live, parked, total) = pool().counts();
        let summary = json!({
            "summary": true,
            "requested": entries.len(),
            "opened": opened,
            "refused": refused,
            "live": live,
            "parked": parked,
            "logical_pages": total,
            "elapsed_ms": started.elapsed().as_millis(),
        });
        crate::daemon::journal("engine.batch.result", summary.clone());
        // The LAST line is the summary, marked, so a reader knows the stream
        // ended on purpose rather than on a dropped connection.
        line(summary);
    }))
}

/// `/engine/wait` — the primitive that makes scripts honest (§4).
///
/// Four `until` forms. Everything except `load: finished` polls at 100 ms,
/// which is the cadence the spec names; `finished` hangs off WebKit's own
/// load signal instead, because a poll can miss a load that starts and ends
/// between two samples.
///
/// A timeout is NOT an error. It answers `{met: false, reason}` with the
/// elapsed time, because "I waited and it did not happen" is a fact a script
/// needs to branch on, not an exception to swallow.
fn wait(engine: &Engine, id: &str, until: &Value, timeout: Duration) -> Result<Value> {
    let started = Instant::now();
    let met = |elapsed: Duration, extra: Value| {
        let mut body = json!({ "ok": true, "met": true, "elapsed_ms": elapsed.as_millis() });
        if let (Some(map), Some(more)) = (body.as_object_mut(), extra.as_object()) {
            for (key, value) in more {
                map.insert(key.clone(), value.clone());
            }
        }
        body
    };
    let unmet = |reason: &str, elapsed: Duration| json!({ "ok": true, "met": false, "reason": reason, "elapsed_ms": elapsed.as_millis() });

    // `load: finished` — the signal, not a poll.
    if until.get("load").and_then(Value::as_str) == Some("finished") {
        return match engine.wait_load(id, timeout) {
            Ok(url) => Ok(met(started.elapsed(), json!({ "url": url }))),
            Err(_) => Ok(unmet(
                "load did not finish within the timeout",
                started.elapsed(),
            )),
        };
    }

    // Everything else is a truthy poll; only the expression differs.
    let (expression, reason): (String, &str) = if until.get("load").and_then(Value::as_str)
        == Some("committed")
    {
        // "Committed" to a script means the document exists and is being
        // built — exactly what leaving readyState 'loading' marks.
        (
            "document.readyState !== 'loading'".to_string(),
            "load was never committed",
        )
    } else if let Some(idle_ms) = until.get("idle_ms").and_then(Value::as_u64) {
        (
            format!("({}).idle_ms >= {idle_ms}", js::IDLE_PROBE),
            "the page never went idle",
        )
    } else if let Some(selector) = until.get("selector").and_then(Value::as_str) {
        let state = until
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("attached");
        let quoted = serde_json::to_string(selector)?;
        match state {
            "attached" => (
                format!("document.querySelector({quoted}) !== null"),
                "the selector never attached",
            ),
            "visible" => (
                format!(
                    "(() => {{ const e = document.querySelector({quoted}); if (!e) return false; \
                     const s = getComputedStyle(e); const r = e.getBoundingClientRect(); \
                     return s.display !== 'none' && s.visibility !== 'hidden' \
                     && r.width > 0 && r.height > 0; }})()"
                ),
                "the selector never became visible",
            ),
            other => bail!("unknown selector state {other:?} (attached|visible)"),
        }
    } else if let Some(js) = until.get("js").and_then(Value::as_str) {
        (format!("!!({js})"), "the expression never became truthy")
    } else {
        bail!("wait needs an `until`: {{load}}, {{idle_ms}}, {{selector,state}} or {{js}}");
    };

    loop {
        // An eval error (a page mid-navigation tears its context down) is a
        // "not yet", not a failure — the next poll asks again.
        if let Ok(value) = engine.eval(id, &expression)
            && value == json!(true)
        {
            return Ok(met(started.elapsed(), json!({})));
        }
        if started.elapsed() >= timeout {
            return Ok(unmet(reason, started.elapsed()));
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// A parsed `/engine/input` event, before the page has been consulted.
///
/// The split exists so the WHOLE batch can be shape-checked without touching
/// the page, and every selector can then be resolved against the page **as it
/// is at the moment its own event is dispatched**. Resolving a batch up front
/// meant event 2's coordinates were measured before event 1 had moved anything,
/// which is precisely the stale-rect click `target_moved` exists to refuse.
#[derive(Debug)]
enum PendingInput {
    /// Nothing left to decide: coordinates, text or a keyval, all from the
    /// caller.
    Ready(InputEvent),
    /// Sugar (§4): the engine resolves the selector in the page, scrolls it
    /// into view, and dispatches REAL coordinates.
    SelectorClick {
        selector: String,
        /// Which HITTABLE match to take. `None` walks from the first.
        nth: Option<usize>,
        /// Refuse instead of choosing when more than one match is hittable.
        require_unique: bool,
        button: u32,
        count: u32,
    },
    /// The same sugar, one document down: the selector is resolved INSIDE a
    /// cross-origin child and the point is translated by the frame's own box.
    ///
    /// A separate variant rather than an `Option<FrameTarget>` on the one above,
    /// because the two resolvers share no code and refuse for different reasons
    /// — the top-document one scrolls and walks a hittable pool, and the frame
    /// one may do neither (`frame::click_target` says why).
    FrameSelectorClick {
        frame: frame::FrameTarget,
        selector: String,
        nth: usize,
        timeout_ms: u64,
        button: u32,
        count: u32,
    },
}

/// Parse one `/engine/input` event. **Pure** — it never reads the page, so a
/// malformed batch is refused before anything at all is dispatched.
fn parse_input(event: &Value) -> Result<PendingInput> {
    let kind = event["type"].as_str().unwrap_or("");
    let point = |event: &Value| -> Result<(f64, f64)> {
        match (event["x"].as_f64(), event["y"].as_f64()) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => bail!("a {kind} event needs x and y (or, for a click, a selector)"),
        }
    };
    match kind {
        "click" => {
            let button = match event["button"].as_str().unwrap_or("left") {
                "left" => 1,
                "middle" => 2,
                "right" => 3,
                other => bail!("unknown mouse button {other:?} (left|middle|right)"),
            };
            let count = event["count"].as_u64().unwrap_or(1).clamp(1, 3) as u32;
            // ⭐ `frame=` FIRST, because a frame click and a top-document click
            // ask different questions of the same `selector`. Answering the
            // top-document one for an event that named a frame would resolve the
            // selector in the WRONG document and dispatch there — a click
            // reported as landed in a frame it never entered.
            if event.get("frame").is_some_and(|value| !value.is_null()) {
                let Some(selector) = event["selector"].as_str() else {
                    bail!(
                        "a click into a frame needs a `selector` to resolve INSIDE it — a bare \
                         x/y is already in the page's own viewport and needs no frame"
                    );
                };
                let target = frame::parse_target(event).map_err(anyhow::Error::msg)?;
                return Ok(PendingInput::FrameSelectorClick {
                    frame: target,
                    selector: selector.to_string(),
                    nth: match event.get("nth") {
                        None | Some(Value::Null) => 0,
                        Some(value) => value.as_u64().map(|n| n as usize).ok_or_else(|| {
                            anyhow::anyhow!("`nth` must be a non-negative integer, got {value}")
                        })?,
                    },
                    timeout_ms: frame::timeout_ms(event),
                    button,
                    count,
                });
            }
            match event["selector"].as_str() {
                Some(selector) => Ok(PendingInput::SelectorClick {
                    selector: selector.to_string(),
                    nth: match event.get("nth") {
                        None | Some(Value::Null) => None,
                        Some(value) => {
                            Some(value.as_u64().map(|n| n as usize).ok_or_else(|| {
                                anyhow::anyhow!("`nth` must be a non-negative integer, got {value}")
                            })?)
                        }
                    },
                    require_unique: event["require_unique"].as_bool().unwrap_or(false),
                    button,
                    count,
                }),
                None => {
                    let (x, y) = point(event)?;
                    Ok(PendingInput::Ready(InputEvent::Click {
                        x,
                        y,
                        button,
                        count,
                    }))
                }
            }
        }
        "move" => {
            let (x, y) = point(event)?;
            Ok(PendingInput::Ready(InputEvent::Move { x, y }))
        }
        "scroll" => {
            let x = event["x"].as_f64().unwrap_or(0.0);
            let y = event["y"].as_f64().unwrap_or(0.0);
            Ok(PendingInput::Ready(InputEvent::Scroll {
                x,
                y,
                dx: event["dx"].as_f64().unwrap_or(0.0),
                dy: event["dy"].as_f64().unwrap_or(0.0),
            }))
        }
        "type" => match event["text"].as_str() {
            Some(text) => Ok(PendingInput::Ready(InputEvent::Text {
                text: text.to_string(),
            })),
            None => bail!("a type event needs text"),
        },
        "key" => {
            // Reject unknown fields — a silently ignored `alt:true` produces a
            // plain keypress that reads as "the app ignored the shortcut". See
            // pending-bugs “ctl input CANNOT DELIVER NAMED KEYS...”.
            // Legacy `alt`/`ctrl` booleans are accepted for one release via the
            // mapping below, so they are not “unknown”.
            for key in event.as_object().map(|o| o.keys()).into_iter().flatten() {
                if !matches!(
                    key.as_str(),
                    "type"
                        | "key"
                        | "mods"
                        | "code"
                        | "text"
                        | "alt"
                        | "ctrl"
                        | "control"
                        | "shift"
                        | "meta"
                        | "super"
                        | "cmd"
                ) {
                    bail!("unknown field {key:?} on a key event (known: type, key, mods)");
                }
            }
            let Some(name) = event["key"].as_str() else {
                bail!("a key event needs a key name, e.g. \"Enter\"");
            };
            // `mods` is the canonical form; also accept the legacy `alt`/`ctrl`/
            // `shift`/`meta` booleans for one release, but they must not be
            // silent — map them into the mask.
            let mut mods: Vec<String> = event["mods"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            for legacy in ["alt", "ctrl", "control", "shift", "meta", "super", "cmd"] {
                if event[legacy].as_bool() == Some(true) {
                    mods.push(legacy.to_string());
                }
            }
            Ok(PendingInput::Ready(InputEvent::Key {
                keyval: super::host::keyval_from_name(name)?,
                mods: super::host::modifier_mask(&mods)?,
            }))
        }
        "mousedown" => {
            let (x, y) = point(event)?;
            let button = match event["button"].as_str().unwrap_or("left") {
                "left" => 1,
                "middle" => 2,
                "right" => 3,
                other => bail!("unknown mouse button {other:?} (left|middle|right)"),
            };
            Ok(PendingInput::Ready(InputEvent::MouseDown { x, y, button }))
        }
        "mouseup" => {
            let (x, y) = point(event)?;
            let button = match event["button"].as_str().unwrap_or("left") {
                "left" => 1,
                "middle" => 2,
                "right" => 3,
                other => bail!("unknown mouse button {other:?} (left|middle|right)"),
            };
            Ok(PendingInput::Ready(InputEvent::MouseUp { x, y, button }))
        }
        "drag" => {
            let from_x = event["from_x"]
                .as_f64()
                .or_else(|| event["x"].as_f64())
                .ok_or_else(|| anyhow::anyhow!("drag needs from_x/from_y or x/y"))?;
            let from_y = event["from_y"]
                .as_f64()
                .or_else(|| event["y"].as_f64())
                .ok_or_else(|| anyhow::anyhow!("drag needs from_x/from_y or x/y"))?;
            let to_x = event["to_x"]
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("drag needs to_x"))?;
            let to_y = event["to_y"]
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("drag needs to_y"))?;
            let steps = event["steps"].as_u64().unwrap_or(8) as usize;
            let button = match event["button"].as_str().unwrap_or("left") {
                "left" => 1,
                "middle" => 2,
                "right" => 3,
                other => bail!("unknown mouse button {other:?} (left|middle|right)"),
            };
            Ok(PendingInput::Ready(InputEvent::Drag {
                from_x,
                from_y,
                to_x,
                to_y,
                steps,
                button,
            }))
        }
        other => bail!(
            "unknown input event type {other:?} (click|move|scroll|type|key|mousedown|mouseup|drag)"
        ),
    }
}

/// Turn a parsed event into a dispatchable one, consulting the page only for
/// the selector-addressed clicks that need it. The second return is the
/// resolution report `/engine/input` echoes back, so ambiguity is never silent.
fn resolve_pending(
    engine: &Engine,
    id: &str,
    pending: PendingInput,
) -> Result<(InputEvent, Option<Value>)> {
    match pending {
        PendingInput::Ready(event) => Ok((event, None)),
        PendingInput::SelectorClick {
            selector,
            nth,
            require_unique,
            button,
            count,
        } => {
            let target = resolve_selector(engine, id, &selector, nth, require_unique)?;
            Ok((
                InputEvent::Click {
                    x: target.x,
                    y: target.y,
                    button,
                    count,
                },
                Some(target.report),
            ))
        }
        PendingInput::FrameSelectorClick {
            frame: target,
            selector,
            nth,
            timeout_ms,
            button,
            count,
        } => {
            let (x, y, report) =
                frame::click_target(engine, id, &target, &selector, nth, timeout_ms)
                    .map_err(|(_, message)| anyhow::anyhow!(message))?;
            Ok((
                InputEvent::Click {
                    x,
                    y,
                    button,
                    count,
                },
                Some(report),
            ))
        }
    }
}

/// How long typed text gets to reach the DOM before the field is re-read.
///
/// Input events cross a process boundary: the UI process dispatches them and
/// the WEB process applies them, so a readback in the next round trip can
/// legitimately arrive before the field has changed. This polls rather than
/// sleeping a flat budget — a field that took 12 ms is not made more certain by
/// waiting 300 — and it reports how long it waited, so a page that is merely
/// slow can be told apart from one that dropped the text.
const TYPED_SETTLE: Duration = Duration::from_millis(400);
const TYPED_POLL: Duration = Duration::from_millis(20);

/// What has the focus, and how much text it already holds.
///
/// A failure to read is NOT reported as "nothing focused" — that would turn an
/// instrument fault into a verdict about the page, which is the exact confusion
/// this lane exists to end. An unreadable page answers `null` and the caller
/// sees an absent measurement rather than a false one.
fn focus_readback(engine: &Engine, id: &str) -> Option<Value> {
    engine.eval(id, js::FOCUS_READBACK).ok()
}

/// Whether a field is somewhere text can actually go.
fn accepts_text(readback: Option<&Value>) -> bool {
    readback.is_some_and(|value| {
        value["focused"].as_bool().unwrap_or(false) && !value["length"].is_null()
    })
}

/// Whether focus has descended into a SUBFRAME, where this readback cannot
/// follow it.
///
/// This is the third answer the guard needs and did not have. `accepts_text`
/// splits the world into "text can go here" and "text would be discarded", and
/// a focused frame is neither: the keystrokes are delivered into the frame, and
/// whether they land is a fact only that frame's document holds. Reporting it
/// as "discarded" refuses a correct drive; reporting it as "accepts" would
/// claim a verification that never happened. So it is named, and the reply says
/// the landing was not verifiable rather than asserting either way.
fn focus_is_a_frame(readback: Option<&Value>) -> bool {
    readback.is_some_and(|value| value["frame"].as_bool().unwrap_or(false))
}

/// Measure where a `type` event's text landed, after it was dispatched.
///
/// The verdict is `grew_by`, not a copy of the text: the field's length before
/// and after, differenced. `landed` is true when it grew by exactly what was
/// typed — the honest reading of "the page kept it".
///
/// ⚠ `landed: false` over an editable target is REPORTED, never refused. A
/// phone-number mask that strips separators, and an OTP field that submits and
/// clears on its last digit, are both correct pages that grow by the wrong
/// amount. The unambiguous fault — text with nowhere to go — is caught BEFORE
/// dispatch instead, where refusing still prevents the write.
fn typed_landing(
    engine: &Engine,
    id: &str,
    index: usize,
    want: usize,
    before: Option<Value>,
) -> Value {
    let baseline = before
        .as_ref()
        .and_then(|value| value["length"].as_u64())
        .unwrap_or(0);
    let deadline = Instant::now() + TYPED_SETTLE;
    let mut after = focus_readback(engine, id);
    let mut waited_ms = 0u64;
    while Instant::now() < deadline {
        let grew = after
            .as_ref()
            .and_then(|value| value["length"].as_u64())
            .is_some_and(|length| length >= baseline + want as u64);
        if grew {
            break;
        }
        thread::sleep(TYPED_POLL);
        waited_ms += TYPED_POLL.as_millis() as u64;
        after = focus_readback(engine, id);
    }
    let now = after.as_ref().and_then(|value| value["length"].as_u64());
    json!({
        "event": index,
        "kind": "type",
        // ⛔ THE UNAMBIGUOUS FAULT, SEPARATED FROM THE MERELY SURPRISING ONE.
        // Measured on the harness 2026-08-08: a form that rebuilds itself on
        // its first `input` event blurs the focus mid-word, so the rest of the
        // text reaches `<body>` — `grew_by: 0`, `target.tag: "body"`. That is
        // not a page keeping a different amount than it was given; it is text
        // with nowhere to go, and it is the reported defect. A mask that strips
        // separators still ends on a real field and is reported, not refused.
        // A focused frame is NOT discarded text — it is text this readback
        // cannot follow. Saying "discarded" there would stop the batch on a
        // drive that was working.
        "discarded": !accepts_text(after.as_ref()) && !focus_is_a_frame(after.as_ref()),
        // ⭐ Said out loud rather than left to be inferred from a null length.
        // A caller driving a field inside a frame gets "we could not see", not
        // a silent success — the honest third answer.
        "unverifiable": focus_is_a_frame(after.as_ref()),
        "unverifiable_reason": if focus_is_a_frame(after.as_ref()) {
            json!("focus is inside a subframe; only that frame's own document can measure the field")
        } else {
            Value::Null
        },
        // The node the text went to, named the way a click's report names its
        // target. A `type` event has never carried this.
        "target": after.as_ref().map(|value| json!({
            "tag": value["tag"],
            "type": value["type"],
            "name": value["name"],
            "editable": value["editable"],
        })),
        "want": want,
        "before": before.as_ref().map(|value| value["length"].clone()),
        "after": now,
        "grew_by": now.map(|length| length as i64 - baseline as i64),
        "landed": now.is_some_and(|length| length == baseline + want as u64),
        "waited_ms": waited_ms,
    })
}

/// How long a scroll gets to settle before the rect is RE-measured.
///
/// `scrollIntoView` is not reliably synchronous — a container with CSS
/// `scroll-behavior: smooth` animates it — so a rect read in the same tick is
/// the PRE-scroll box. The visible surface plane measured this on a MUI listbox
/// and settled on 120 ms (`WEB_DO_SCROLL_SETTLE`); the same number is used here
/// so the two planes agree about when a scroll is done instead of each guessing.
const CLICK_SCROLL_SETTLE: Duration = Duration::from_millis(120);

/// How many hittable candidates the resolver will try before giving up.
///
/// Bounded because each attempt costs a scroll settle. A page where the first
/// eight hittable matches all fail re-measurement is not a page a ninth attempt
/// is going to rescue, and the refusal says the walk was capped.
const CLICK_WALK_CAP: usize = 8;

/// Where a selector-addressed click will land, and the account of how it was
/// chosen.
struct ClickTarget {
    x: f64,
    y: f64,
    /// Echoed in `/engine/input`'s reply. Reporting a `matches: 1` costs
    /// nothing and reporting a `matches: 9` is the whole point.
    report: Value,
}

/// Resolve a selector to the point a click should be dispatched at.
///
/// ## Why this is not `querySelector`
///
/// `document.querySelector` returns the FIRST match and real pages carry hidden
/// duplicates: IBKR's login page has six-plus `button[type=submit]`, five of
/// them dead, the live one third in document order. Clicking the first is a
/// click into the void, and reporting `{"dispatched":3,"ok":true}` for it cost a
/// reporting agent three wrong conclusions in one session, one of which blamed
/// the operator's vault for a 2FA failure that had never been submitted.
///
/// ## The semantics, decided (2026-07-31)
///
/// **The pool is the HITTABLE matches, in document order, and the default is the
/// first of them.** Not the first match, and not a refusal on ambiguity.
///
/// The reasoning: hidden duplicates are *noise*, not *ambiguity*. A page with
/// five dead submit buttons and one live one poses a question that has exactly
/// one answer, and refusing it would be refusing to answer an unambiguous
/// question. The visible surface plane already settled this the same way — its
/// matcher filters to live nodes, takes `nth` (default 0), and reports the
/// counts rather than refusing — and two planes answering the same question
/// differently is the divergence this codebase forbids.
///
/// Both alternatives the bug report asked about stay reachable, on the request,
/// **before** the click rather than after it:
///
/// - `"nth": k` takes the k-th HITTABLE match (never the k-th raw match — a
///   caller counting from a `/engine/dom` snapshot is counting visible things);
/// - `"require_unique": true` refuses `ambiguous_selector` when more than one
///   match is hittable, for a caller who would rather stop than guess.
///
/// And the reply always carries `{matches, hittable, hidden, zero_size, nth,
/// ambiguous}`, so a caller that took the default still learns it was one of
/// nine.
fn resolve_selector(
    engine: &Engine,
    id: &str,
    selector: &str,
    nth: Option<usize>,
    require_unique: bool,
) -> Result<ClickTarget> {
    let quoted = serde_json::to_string(selector)?;
    let pool = engine.eval(id, &format!("({})({quoted})", js::CLICK_POOL))?;
    if let Some(reason) = pool["bad_selector"].as_str() {
        bail!("{selector:?} is not a valid CSS selector ({reason})");
    }
    let matches = pool["matches"].as_u64().unwrap_or(0);
    let hittable = pool["hittable"].as_u64().unwrap_or(0) as usize;
    let hidden = pool["hidden"].as_u64().unwrap_or(0);
    let zero_size = pool["zero_size"].as_u64().unwrap_or(0);

    if matches == 0 {
        bail!("no element matches {selector:?}");
    }
    if hittable == 0 {
        // The counts are named with the tokens the visible surface plane uses
        // for the same conditions — `zero_size_element` for a `0x0` box (which
        // is what `display:none` measures) and `hidden` for the aria/style
        // kind — so a caller reads one vocabulary across both planes.
        bail!(
            "no_hittable_match ({selector:?} matched {matches} element(s) and NONE could receive \
             a click: {zero_size} zero_size_element, {hidden} hidden — refusing rather than \
             dispatching into the void)"
        );
    }
    if require_unique && hittable > 1 {
        bail!(
            "ambiguous_selector ({selector:?} has {hittable} hittable match(es) of {matches} and \
             the caller required exactly one — pass `nth` to choose between them)"
        );
    }

    let candidates: Vec<usize> = match nth {
        Some(index) if index >= hittable => bail!(
            "no element matches {selector:?} at hittable index {index} — it has {hittable} \
             hittable match(es) of {matches}"
        ),
        // An explicit `nth` is an instruction, not a hint: walking past it would
        // hand the caller a DIFFERENT element than the one they named.
        Some(index) => vec![index],
        None => (0..hittable.min(CLICK_WALK_CAP)).collect(),
    };
    let walked = candidates.len();

    let mut first_refusal: Option<String> = None;
    for index in candidates {
        let pinned = engine.eval(id, &format!("({})({index})", js::CLICK_PIN))?;
        if pinned["found"].as_bool() != Some(true) {
            first_refusal.get_or_insert(format!(
                "handle_lost ({selector:?} hittable match {index} left the pool before it could \
                 be pinned)"
            ));
            continue;
        }
        // The scroll must SETTLE before the rect is read again. See
        // `CLICK_SCROLL_SETTLE` — this sleeps the request thread, not the engine
        // thread, so other pages keep loading through it.
        std::thread::sleep(CLICK_SCROLL_SETTLE);
        let measured = engine.eval(id, js::CLICK_MEASURE)?;
        match click_point_from_measure(selector, index, &measured) {
            Ok((x, y)) => {
                return Ok(ClickTarget {
                    x,
                    y,
                    report: json!({
                        "selector": selector,
                        "matches": matches,
                        "hittable": hittable,
                        "hidden": hidden,
                        "zero_size": zero_size,
                        "nth": index,
                        // Ambiguity is counted over the HITTABLE pool: five dead
                        // duplicates and one live control is not an ambiguous
                        // question. `matches` is right there for a caller who
                        // wants the stricter reading.
                        "ambiguous": hittable > 1,
                        "x": x,
                        "y": y,
                        "tag": measured["tag"],
                    }),
                });
            }
            Err(refusal) => {
                first_refusal.get_or_insert(refusal);
            }
        }
    }
    let capped = if walked < hittable {
        format!(" (the walk stopped after {walked} of {hittable} hittable candidates)")
    } else {
        String::new()
    };
    bail!(
        "{}{capped}",
        first_refusal.unwrap_or_else(|| format!("no_hittable_match ({selector:?})"))
    );
}

/// PURE half of the resolver: turn the page's post-scroll report into a point or
/// a refusal.
///
/// Kept pure, exactly as the visible surface plane keeps
/// `web_do_resolved_from_info` pure, so every interesting failure of a
/// selector-addressed click is decided here and unit-testable without a webview.
///
/// The refusal vocabulary is the surface plane's, deliberately: `handle_lost`,
/// `rect_not_reresolved`, `detached_node`, `zero-size element`, `target_moved`.
/// A second set of words for the same failures would leave an agent unable to
/// carry what it learned on one plane over to the other.
fn click_point_from_measure(
    selector: &str,
    index: usize,
    info: &Value,
) -> std::result::Result<(f64, f64), String> {
    if info["found"].as_bool() != Some(true) {
        return Err(format!(
            "handle_lost ({selector:?} hittable match {index} was replaced between the scroll and \
             the re-measure)"
        ));
    }
    // THE RECT MUST BE THE POST-SCROLL ONE. Only the phase-B script stamps this
    // token, so a payload without it is by construction a rect measured in the
    // same tick as the scroll that moved the node.
    if info["phase"].as_str() != Some("post_scroll") {
        return Err(format!(
            "rect_not_reresolved ({selector:?} was measured before the scroll settled)"
        ));
    }
    if info["isConnected"].as_bool() != Some(true) {
        return Err(format!(
            "detached_node ({selector:?} resolved to a node that is not in the document)"
        ));
    }
    if info["visible"].as_bool() != Some(true) {
        return Err(format!("{selector:?} matched a zero-size element"));
    }
    let (Some(x), Some(y)) = (info["x"].as_f64(), info["y"].as_f64()) else {
        return Err(format!("{selector:?} resolved to no rect"));
    };
    if info["onTarget"].as_bool() != Some(true) {
        // One token, three flavours, and the message says which — an element
        // still outside the viewport after being scrolled to, a point where
        // nothing paints, and a point another element sits on top of are all
        // "the resolved point does not reach this node".
        let detail = if info["in_viewport"].as_bool() != Some(true) {
            "it is still outside the viewport after being scrolled into view".to_string()
        } else {
            match info["hit"].as_str() {
                Some(hit) => format!("`{hit}` is what receives a click there"),
                None => "nothing is painted there".to_string(),
            }
        };
        return Err(format!(
            "target_moved (the resolved point ({x:.0},{y:.0}) no longer hits {selector:?} — \
             {detail})"
        ));
    }
    Ok((x, y))
}

// ===== CAPTURE ==============================================================
//
// `/engine/shot` is four modes over ONE snapshot primitive. `viewport` and
// `full` are the two regions WebKitGTK renders natively; `element` and `rect`
// are a full-document snapshot with a window onto it, cropped from the pixels
// already in hand rather than re-snapped. Doing it that way is not an
// optimisation — a second snapshot taken a scroll or an animation frame later
// would let an element capture and the full-page capture it claims to be a part
// of show different content.

/// The response header a PNG reply carries its account in.
///
/// Spelled once, here, and read by the CLI at `ctl::SHOT_META_HEADER` — the two
/// halves of one wire name. Lower-case because HTTP header names are
/// case-insensitive and the client folds before comparing.
pub const SHOT_META_HEADER: &str = "X-Ychrome-Shot";

/// Format the capture account as ONE header line, or nothing.
///
/// Compact JSON, so it cannot contain a bare CR or LF and cannot therefore
/// split the response — a header built by string-joining caller-supplied values
/// is a response-splitting bug waiting for a page title with a newline in it,
/// and the selector account carries page-derived strings (`tag`). `serde_json`
/// escapes both, by construction.
pub fn shot_meta_header(meta: &Value) -> String {
    let line = meta.to_string();
    if line.contains(['\r', '\n']) {
        return String::new();
    }
    format!("{SHOT_META_HEADER}: {line}\r\n")
}

/// A rect in DOCUMENT-space CSS pixels — the space the page speaks.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CssRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// What `/engine/shot`'s `region` asked for.
enum CaptureMode {
    /// What is on screen right now.
    Viewport,
    /// The whole scrollable document.
    Full,
    /// One element, resolved through the SAME pool a click resolves through.
    Element {
        selector: String,
        nth: Option<usize>,
        require_unique: bool,
        /// CSS pixels of breathing room on every side. A control cropped to its
        /// exact border box is legible but contextless; a caller asking "what
        /// does this look like" usually wants a little of what is around it.
        padding: f64,
    },
    /// A caller-chosen rect. THIS is the CLI's "selection area": a human drags
    /// a rectangle, an agent names one, and both arrive here.
    Rect(CssRect),
}

impl CaptureMode {
    /// Parse the `region` argument, naming every alias it accepts.
    ///
    /// `page`/`document` alias `full` and `visible` aliases `viewport` because
    /// those are the words the rest of the world uses for these two things, and
    /// an agent that guesses one of them should get a capture rather than a
    /// lecture. Everything else is refused BY NAME with the list.
    fn parse(body: &Value) -> std::result::Result<CaptureMode, (u16, String)> {
        let region = body
            .get("region")
            .and_then(Value::as_str)
            .unwrap_or("viewport");
        let bad = |message: String| (400u16, message);
        match region {
            "viewport" | "visible" => Ok(CaptureMode::Viewport),
            "full" | "page" | "document" | "fullpage" | "full_page" => Ok(CaptureMode::Full),
            "element" => {
                let Some(selector) = body.get("selector").and_then(Value::as_str) else {
                    return Err(bad(
                        "region=element needs a `selector` (a CSS selector, resolved through the \
                         same hittable pool /engine/input clicks through)"
                            .to_string(),
                    ));
                };
                Ok(CaptureMode::Element {
                    selector: selector.to_string(),
                    nth: body.get("nth").and_then(Value::as_u64).map(|n| n as usize),
                    require_unique: body
                        .get("require_unique")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    padding: body.get("padding").and_then(Value::as_f64).unwrap_or(0.0),
                })
            }
            "rect" | "selection" | "area" => {
                let rect = body.get("rect").unwrap_or(&Value::Null);
                let read = |key: &str| rect.get(key).and_then(Value::as_f64);
                let (Some(x), Some(y), Some(w), Some(h)) =
                    (read("x"), read("y"), read("w"), read("h"))
                else {
                    return Err(bad(
                        "region=rect needs `rect={\"x\":..,\"y\":..,\"w\":..,\"h\":..}` in \
                         DOCUMENT-space CSS pixels (page coordinates, not viewport ones — add \
                         window.scrollY to a getBoundingClientRect top)"
                            .to_string(),
                    ));
                };
                if !(w > 0.0 && h > 0.0) {
                    return Err(bad(format!(
                        "region=rect needs a positive w and h; got {w}x{h}"
                    )));
                }
                Ok(CaptureMode::Rect(CssRect { x, y, w, h }))
            }
            other => Err(bad(format!(
                "unknown shot region {other:?} (viewport|full|element|rect)"
            ))),
        }
    }

    /// Which native region this mode snapshots. Everything that crops takes the
    /// FULL DOCUMENT, so an element below the fold captures without scrolling.
    fn region(&self) -> ShotRegion {
        match self {
            CaptureMode::Viewport => ShotRegion::Visible,
            _ => ShotRegion::FullDocument,
        }
    }
}

/// How long each pre-scroll step gets to let the page react.
///
/// A lazy image loads from an `IntersectionObserver` callback and a `fetch`,
/// neither of which runs while our own script holds the thread. 120 ms is the
/// same settle the click resolver uses for a `scrollIntoView` — one number for
/// "give the page a turn", not two.
const PRESCROLL_SETTLE: Duration = Duration::from_millis(120);

/// The most pre-scroll steps one capture will take.
///
/// Bounded because an infinite-scroll feed has no bottom, and a capture that
/// walks one forever is a hung request rather than a thorough one. 60 steps at
/// ~90% of a viewport each is roughly 54 screens.
const PRESCROLL_CAP: usize = 60;

/// One capture: the request body in, the pixels and the account of them out.
///
/// Errors carry their own status because they are genuinely different kinds:
/// 400 for a request the caller can fix, 404 for a page that is not there, 502
/// for a snapshot the engine could not take.
fn capture(
    engine: &Engine,
    id: &str,
    body: &Value,
) -> std::result::Result<(super::host::Shot, Value), (u16, String)> {
    let mode = CaptureMode::parse(body)?;
    let region = mode.region();

    // The pre-scroll runs FIRST, before anything is measured: it is the only
    // step that changes the document's height, and a scale computed before it
    // would be a scale for a shorter page.
    let prescroll = if body.get("prescroll").and_then(Value::as_bool) == Some(true) {
        Some(prescroll(engine, id))
    } else {
        None
    };

    let metrics = engine
        .eval(id, js::SHOT_METRICS)
        .map_err(|error| (404u16, error.to_string()))?;
    let doc_w = metrics["doc_w"].as_f64().unwrap_or(0.0);
    let doc_h = metrics["doc_h"].as_f64().unwrap_or(0.0);

    // The element's rect is read BEFORE the snapshot, in the same quiet page
    // state, so the crop and the pixels describe one instant.
    let (crop_css, selector_report) = match &mode {
        CaptureMode::Viewport | CaptureMode::Full => (None, None),
        CaptureMode::Rect(rect) => (Some(*rect), None),
        CaptureMode::Element {
            selector,
            nth,
            require_unique,
            padding,
        } => {
            let (rect, report) = element_rect(engine, id, selector, *nth, *require_unique)
                .map_err(|error| (400u16, error.to_string()))?;
            let padded = CssRect {
                x: rect.x - padding,
                y: rect.y - padding,
                w: rect.w + padding * 2.0,
                h: rect.h + padding * 2.0,
            };
            (Some(padded), Some(report))
        }
    };

    let shot = engine
        .shot_region(id, region)
        .map_err(|error| (502u16, error.to_string()))?;

    // THE SCALE IS MEASURED, NEVER ASSUMED. `devicePixelRatio` and the page
    // zoom both move it, and on a headless X server the two do not always agree
    // with each other; dividing the snapshot's real width by the width it
    // rendered cannot be wrong about the thing it is used for.
    let css_w = match region {
        ShotRegion::FullDocument => doc_w,
        ShotRegion::Visible => metrics["view_w"].as_f64().unwrap_or(0.0),
    };
    let scale = if css_w > 0.0 {
        shot.width as f64 / css_w
    } else {
        1.0
    };

    let mut meta = json!({
        "region": region.id(),
        "mode": match &mode {
            CaptureMode::Viewport => "viewport",
            CaptureMode::Full => "full",
            CaptureMode::Element { .. } => "element",
            CaptureMode::Rect(_) => "rect",
        },
        "page_id": id,
        "width": shot.width,
        "height": shot.height,
        "scale": scale,
        "document": {
            "w": doc_w,
            "h": doc_h,
            "view_w": metrics["view_w"],
            "view_h": metrics["view_h"],
            "dpr": metrics["dpr"],
            "scroll_y": metrics["scroll_y"],
        },
    });
    if let Some(prescroll) = prescroll {
        meta["prescroll"] = prescroll;
    }
    if let Some(report) = selector_report {
        meta["selector"] = report;
    }

    let Some(css) = crop_css else {
        return Ok((shot, meta));
    };
    let device =
        device_rect(css, scale, (shot.width, shot.height)).map_err(|error| (400u16, error))?;
    let cropped = shot
        .crop(device)
        .map_err(|error| (500u16, error.to_string()))?;
    meta["crop"] = json!({
        "css": { "x": css.x, "y": css.y, "w": css.w, "h": css.h },
        "device": { "x": device.x, "y": device.y, "w": device.w, "h": device.h },
    });
    meta["width"] = json!(cropped.width);
    meta["height"] = json!(cropped.height);
    Ok((cropped, meta))
}

/// Walk the document top to bottom, one viewport at a time, and put the scroll
/// back where it was.
///
/// ⚠ **This is the answer to the thing a full-document snapshot genuinely
/// cannot do.** `SnapshotRegion::FullDocument` renders the document as it is
/// LAID OUT; content that has never been near the viewport has never loaded, so
/// a lazily-loaded page captures as a full-height document of empty boxes. The
/// snapshot is not lying — the images really are not there yet.
///
/// Best-effort and reported as such: `steps` and `final_height` say what it
/// actually did, and a page that grew past [`PRESCROLL_CAP`] says `capped:
/// true` rather than pretending it reached the bottom.
fn prescroll(engine: &Engine, id: &str) -> Value {
    let start = engine
        .eval(id, js::SHOT_METRICS)
        .ok()
        .and_then(|m| m["scroll_y"].as_f64())
        .unwrap_or(0.0);
    let mut steps = 0usize;
    let mut y = 0.0f64;
    let mut height = 0.0f64;
    let mut capped = false;
    loop {
        let Ok(metrics) = engine.eval(id, js::SHOT_METRICS) else {
            break;
        };
        height = metrics["doc_h"].as_f64().unwrap_or(0.0);
        let step = (metrics["view_h"].as_f64().unwrap_or(600.0) * 0.9).max(200.0);
        if y >= height {
            break;
        }
        if steps >= PRESCROLL_CAP {
            capped = true;
            break;
        }
        if engine
            .eval(id, &format!("({})({y})", js::SHOT_SCROLL_TO))
            .is_err()
        {
            break;
        }
        std::thread::sleep(PRESCROLL_SETTLE);
        steps += 1;
        y += step;
    }
    // Put the page back where the caller left it. A capture must not be a
    // navigation side effect.
    let _ = engine.eval(id, &format!("({})({start})", js::SHOT_SCROLL_TO));
    json!({
        "steps": steps,
        "settle_ms": PRESCROLL_SETTLE.as_millis(),
        "final_height": height,
        "capped": capped,
        "restored_scroll_y": start,
    })
}

/// Resolve a selector to its DOCUMENT-space rect, through the click pool.
///
/// Same pool, same hittable filter, same `nth` default, same counts as
/// `/engine/input` — so `region=element` and a click on that selector can never
/// name different elements. That reuse is the whole reason this is six lines
/// and not a second resolver.
fn element_rect(
    engine: &Engine,
    id: &str,
    selector: &str,
    nth: Option<usize>,
    require_unique: bool,
) -> Result<(CssRect, Value)> {
    let quoted = serde_json::to_string(selector)?;
    let pool = engine.eval(id, &format!("({})({quoted})", js::CLICK_POOL))?;
    if let Some(reason) = pool["bad_selector"].as_str() {
        bail!("{selector:?} is not a valid CSS selector ({reason})");
    }
    let matches = pool["matches"].as_u64().unwrap_or(0);
    let hittable = pool["hittable"].as_u64().unwrap_or(0) as usize;
    if matches == 0 {
        bail!("no element matches {selector:?}");
    }
    if hittable == 0 {
        bail!(
            "no_hittable_match ({selector:?} matched {matches} element(s) and none of them has a \
             visible box to capture: {} zero_size_element, {} hidden)",
            pool["zero_size"].as_u64().unwrap_or(0),
            pool["hidden"].as_u64().unwrap_or(0)
        );
    }
    if require_unique && hittable > 1 {
        bail!(
            "ambiguous_selector ({selector:?} has {hittable} hittable match(es) of {matches} and \
             the caller required exactly one — pass `nth` to choose between them)"
        );
    }
    let index = nth.unwrap_or(0);
    if index >= hittable {
        bail!(
            "no element matches {selector:?} at hittable index {index} — it has {hittable} \
             hittable match(es) of {matches}"
        );
    }
    let measured = engine.eval(id, &format!("({})({index})", js::SHOT_POOL_RECT))?;
    if measured["found"].as_bool() != Some(true) {
        bail!(
            "handle_lost ({selector:?} hittable match {index} left the pool before it could be measured)"
        );
    }
    let (Some(x), Some(y), Some(w), Some(h)) = (
        measured["x"].as_f64(),
        measured["y"].as_f64(),
        measured["w"].as_f64(),
        measured["h"].as_f64(),
    ) else {
        bail!("{selector:?} resolved to no rect");
    };
    Ok((
        CssRect { x, y, w, h },
        json!({
            "selector": selector,
            "matches": matches,
            "hittable": hittable,
            "hidden": pool["hidden"],
            "zero_size": pool["zero_size"],
            "nth": index,
            "ambiguous": hittable > 1,
            "tag": measured["tag"],
        }),
    ))
}

/// CSS rect + measured scale -> the device-pixel rect to cut, clamped to the
/// snapshot.
///
/// PURE, and that is deliberate: every interesting way a crop goes wrong (an
/// element scrolled off the captured document, a rect the caller measured
/// against a different zoom, a half-pixel border) is decided here and lockable
/// without an engine.
///
/// Rounds OUTWARD — floor the origin, ceil the far edge — so a 1 px border is
/// never shaved off by rounding. Clamps to the snapshot rather than refusing a
/// rect that merely overhangs, because an element flush with the right edge of
/// the document legitimately measures a fraction of a pixel wider than the
/// document is. An EMPTY intersection is refused by name: a blank PNG looks
/// like a rendering bug and would be debugged as one.
fn device_rect(
    css: CssRect,
    scale: f64,
    bounds: (i32, i32),
) -> std::result::Result<PixelRect, String> {
    let (max_w, max_h) = bounds;
    let left = (css.x * scale).floor() as i64;
    let top = (css.y * scale).floor() as i64;
    let right = ((css.x + css.w) * scale).ceil() as i64;
    let bottom = ((css.y + css.h) * scale).ceil() as i64;
    let x = left.clamp(0, max_w as i64);
    let y = top.clamp(0, max_h as i64);
    let w = right.clamp(0, max_w as i64) - x;
    let h = bottom.clamp(0, max_h as i64) - y;
    if w <= 0 || h <= 0 {
        return Err(format!(
            "the requested crop ({:.1},{:.1} {:.1}x{:.1} CSS px at scale {scale:.3}) does not \
             overlap the {max_w}x{max_h} capture — a rect is in DOCUMENT coordinates, so a \
             viewport-relative one measured while the page was scrolled will land off the top",
            css.x, css.y, css.w, css.h
        ));
    }
    Ok(PixelRect {
        x: x as i32,
        y: y as i32,
        w: w as i32,
        h: h as i32,
    })
}

/// Record what the main frame's load actually RETURNED, on the daemon journal.
///
/// ⭐ Nothing used to write this down, and that is why three completely
/// different failures were indistinguishable from the outside: a bot-check
/// challenge loop, an asset the content filter ate, and a cookie jar that never
/// persisted all present as "the page came back and it is not the page". One
/// line per main-frame load, carrying the status and Cloudflare's own headers,
/// separates them without anybody having to reproduce the failure first.
///
/// Best-effort and silent on error: a trace that could break a navigation would
/// be worse than no trace.
fn journal_main_frame(engine: &Engine, id: &str, event: &str) {
    if let Ok(mut trace) = engine.trace_main_frame(id) {
        if let Some(object) = trace.as_object_mut() {
            object.insert("page_id".to_string(), json!(id));
        }
        crate::daemon::journal(event, trace);
    }
}

/// The one `page` status shape (§4), built in one place so no route grows its
/// own. The governance fields the spec lists (`rss_mb`, `cpu_pct_1m`, park
/// state) belong to Phase D and are absent rather than faked — a zero would
/// read as a measurement.
fn page_status(engine: &Engine, id: &str) -> Value {
    // From the VIEW, never from an `eval`. A page whose script is parked — in a
    // dialog, in a long handler — would otherwise make its own status report
    // time out, and a status is what you ask for precisely when a page is not
    // behaving.
    let live = engine.live_locations().unwrap_or_default();
    let (url, title) = live.get(id).cloned().unwrap_or_default();
    json!({
        "ok": true,
        "page_id": id,
        "url": url,
        "title": title,
        "state": "live",
        "profile": engine.page_profile(id).ok(),
        "loading": engine.is_loading(id).unwrap_or(false),
    })
}

/// Mint a page id. `pub(super)` because the engine host mints one too, for a
/// window the PAGE opened — one owner, so two generations of ids cannot collide.
pub(super) fn new_page_id() -> String {
    // Monotonic and unique within a daemon's life. Not a ULID: the spec's
    // `pg_01hxyz…` shape is cosmetic and a counter cannot collide.
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("pg_{:06}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Build a request the way the socket handler will, so callers that are not
/// HTTP (the bench and flow verbs) exercise the very same router.
pub fn request(verb: &str, body: Value) -> ParsedRequest {
    request_with(verb, body)
}

fn request_with(verb: &str, body: Value) -> ParsedRequest {
    ParsedRequest {
        method: "POST".to_string(),
        path: format!("/engine/{verb}"),
        query: String::new(),
        fido2_token: None,
        control_token: None,
        body,
    }
}

/// `ychrome engine govern [n]` — Phase D's acceptance run (§8).
///
/// Opens N logical pages through `/engine/batch` under `max_live=12` and
/// `max_rss_mb=4096`, then checks the things the AC actually asks about: that
/// the run COMPLETES, that live never exceeded the budget, that parking
/// honoured LRU, and that peak PSS stayed within budget +10%.
///
/// The pages are local `file://` fixtures, not 300 requests at a live site.
/// That keeps the measurement about the governor instead of about someone
/// else's rate limiter, and it makes the run repeatable.
pub fn govern(pages: usize) -> Result<Value> {
    let started = Instant::now();
    let dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".yggterm/ychrome/engine-pool");
    std::fs::create_dir_all(&dir)?;

    // Each fixture holds a little state so parking has something real to
    // capture: a scroll position and a filled field.
    let mut urls = Vec::with_capacity(pages);
    for index in 0..pages {
        let path = dir.join(format!("page-{index:04}.html"));
        std::fs::write(
            &path,
            format!(
                "<!doctype html><title>pool page {index}</title>\
                 <body><h1>pool page {index}</h1>\
                 <input id=\"f\" value=\"state-{index}\">\
                 <div style=\"height:2500px\"></div>\
                 <script>window.scrollTo(0, {});</script>",
                (index % 7) * 100 + 120
            ),
        )?;
        urls.push(format!("file://{}", path.display()));
    }

    let budget = pool().set_budget(Some(12), Some(4096));
    crate::daemon::journal(
        "engine.govern.start",
        json!({ "pages": pages, "budget": budget_json(&budget) }),
    );

    // Sample live-count and PSS throughout, so "never exceeded the budget" is
    // a measurement rather than an end-state guess.
    let watching = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let samples = Arc::new(Mutex::new(Vec::<(usize, u64)>::new()));
    let watcher = {
        let watching = Arc::clone(&watching);
        let samples = Arc::clone(&samples);
        std::thread::spawn(move || {
            while watching.load(std::sync::atomic::Ordering::Relaxed) {
                let (live, _, _) = pool().counts();
                let (pss, _) = pool::measure_pss();
                if let Ok(mut samples) = samples.lock() {
                    samples.push((live, pss));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        })
    };

    let open: Vec<Value> = urls
        .iter()
        .enumerate()
        .map(|(index, url)| json!({ "url": url, "tags": [format!("batch-{}", index % 3)] }))
        .collect();
    let (status, batch) = json_status(dispatch(&request(
        "batch",
        json!({ "open": open, "concurrency": 8 }),
    )));

    watching.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = watcher.join();
    let samples = samples.lock().map(|s| s.clone()).unwrap_or_default();
    let max_live_seen = samples.iter().map(|(live, _)| *live).max().unwrap_or(0);
    let peak_pss = samples.iter().map(|(_, pss)| *pss).max().unwrap_or(0);

    // ---- restore is a PLACE, not a URL ----------------------------------
    //
    // The batch alone never resumes anything, so parking would be "proven"
    // without ever showing the state comes back. Mutate a live page into a
    // condition its own markup does NOT produce, park it, resume it, and read
    // it back: a plain reload would restore `state-N` and the fixture's own
    // inline `scrollTo`, so only a real place-restore can pass this.
    let restore = pool()
        .all()
        .into_iter()
        .find(|page| page.state == pool::PageState::Live && !page.pinned)
        .map(|page| page.id.clone());
    let restored = match &restore {
        None => json!({ "ran": false, "reason": "no live page to test" }),
        Some(id) => {
            let marker = "RESTORE-MARKER";
            let _ = dispatch(&request(
                "eval",
                json!({ "page_id": id, "js": format!(
                    "(() => {{ document.getElementById('f').value = '{marker}';                       window.scrollTo(0, 1234); return 'set'; }})()"
                )}),
            ));
            let park = json_status(dispatch(&request("park", json!({ "page_id": id }))));
            let resume = json_status(dispatch(&request("resume", json!({ "page_id": id }))));
            let after = json_status(dispatch(&request(
                "eval",
                json!({ "page_id": id, "js":
                    "[document.getElementById('f').value, Math.round(window.scrollY)]" }),
            )))
            .1["value"]
                .clone();
            let value_back = after[0] == json!(marker);
            let scroll_back = (after[1].as_f64().unwrap_or(0.0) - 1234.0).abs() <= 2.0;
            json!({
                "ran": true,
                "page_id": id,
                "parked_ok": park.0 == 200,
                "resumed_ok": resume.0 == 200,
                "form_value_restored": value_back,
                "scroll_restored": scroll_back,
                "read_back": after,
                "pass": park.0 == 200 && resume.0 == 200 && value_back && scroll_back,
            })
        }
    };

    let metrics = pool().metrics();
    let (live, parked, total) = pool().counts();
    let parked_total = metrics["totals"]["parked"].as_u64().unwrap_or(0);

    // The AC, item by item.
    let completed = status == 200 && batch["opened"].as_u64().unwrap_or(0) as usize == pages;
    let live_within_budget = max_live_seen <= budget.max_live;
    let pss_within_budget = peak_pss <= budget.max_rss_mb + budget.max_rss_mb / 10;
    let parked_as_expected = parked_total as usize >= pages.saturating_sub(budget.max_live);
    let all_accounted = total == pages;
    let place_restored = restored["pass"] == json!(true);

    let report = json!({
        "bench": "phase-d-governor",
        "ok": completed && live_within_budget && pss_within_budget
              && parked_as_expected && all_accounted && place_restored,
        "requested_pages": pages,
        "budget": budget_json(&budget),
        "checks": {
            "run_completed": completed,
            "live_never_exceeded_max_live": live_within_budget,
            "peak_pss_within_budget_plus_10pct": pss_within_budget,
            "lru_parking_happened": parked_as_expected,
            "every_page_still_accounted_for": all_accounted,
            "resume_restores_the_place_not_just_the_url": place_restored,
        },
        "restore_proof": restored,
        "measured": {
            "max_live_seen": max_live_seen,
            "peak_pss_mb": peak_pss,
            "budget_plus_10pct_mb": budget.max_rss_mb + budget.max_rss_mb / 10,
            "parked_total": parked_total,
            "resumed_total": metrics["totals"]["resumed"],
            "saturated_refusals": metrics["totals"]["saturated_refusals"],
            "final_live": live,
            "final_parked": parked,
            "logical_pages": total,
            "samples": samples.len(),
            "batch_elapsed_ms": batch["elapsed_ms"],
            "elapsed_ms": started.elapsed().as_millis(),
        },
    });
    crate::daemon::journal("engine.govern.result", report.clone());
    Ok(report)
}

/// `ychrome engine bench [n]` — §6's standardised run, and the shape of Phase
/// B's acceptance criterion: open N pages CONCURRENTLY, wait for each to
/// finish loading, screenshot every one, and report the numbers.
///
/// It drives [`dispatch`] on real `/engine/*` requests from N threads, which is
/// exactly what the daemon's socket handler does with one thread per
/// connection. Proving it this way rather than through a bespoke test path is
/// the point: what runs here is the router that ships.
pub fn bench(pages: usize) -> Result<Value> {
    let started = Instant::now();
    crate::daemon::journal("engine.bench.start", json!({ "pages": pages }));

    // Concurrent opens, one thread each — the contention Phase B claims to
    // support. The engine thread serialises the GTK calls; the LOADS overlap,
    // which is where the wall-clock win is and where a deadlock would show.
    let open_started = Instant::now();
    let mut handles = Vec::new();
    for index in 0..pages {
        handles.push(std::thread::spawn(move || {
            let began = Instant::now();
            let url = format!("https://example.com/?bench={index}");
            match json_status(dispatch(&request("open", json!({ "url": url })))) {
                (200, body) => Ok((
                    body["page_id"].as_str().unwrap_or_default().to_string(),
                    began.elapsed().as_millis() as u64,
                )),
                (status, body) => Err(format!("open {index} -> {status} {body}")),
            }
        }));
    }
    let mut opened = Vec::new();
    let mut failures = Vec::new();
    for handle in handles {
        match handle.join() {
            Ok(Ok(result)) => opened.push(result),
            Ok(Err(error)) => failures.push(error),
            Err(_) => failures.push("an open thread panicked".to_string()),
        }
    }
    let open_ms = open_started.elapsed().as_millis();

    // Every page still live and listed, by the /engine/pages verb.
    let listed = match dispatch(&request("pages", json!({}))) {
        Reply::Json(200, body) => body["pages"].as_array().map(Vec::len).unwrap_or(0),
        _ => 0,
    };

    // Screenshot all of them, and assert each PNG is a PNG rather than
    // trusting the byte count: a truncated or empty readback is the failure
    // this whole engine exists to make impossible to miss.
    let shot_started = Instant::now();
    let mut shot_bytes = Vec::new();
    for (id, _) in &opened {
        match dispatch(&request("shot", json!({ "page_id": id }))) {
            Reply::Png { png, .. } if png.starts_with(b"\x89PNG\r\n\x1a\n") => {
                shot_bytes.push(png.len())
            }
            Reply::Png { .. } => failures.push(format!("{id}: readback was not a PNG")),
            other => {
                let (status, body) = json_status(other);
                failures.push(format!("{id}: shot -> {status} {body}"));
            }
        }
    }
    let shot_ms = shot_started.elapsed().as_millis();

    for (id, _) in &opened {
        if let Reply::Json(status, body) = dispatch(&request("close", json!({ "page_id": id })))
            && status != 200
        {
            failures.push(format!("{id}: close -> {status} {body}"));
        }
    }

    let mut open_latencies: Vec<u64> = opened.iter().map(|(_, ms)| *ms).collect();
    open_latencies.sort_unstable();
    let percentile = |p: f64| -> u64 {
        if open_latencies.is_empty() {
            return 0;
        }
        let index = ((open_latencies.len() as f64 - 1.0) * p).round() as usize;
        open_latencies[index]
    };

    let report = json!({
        "bench": "engine-core",
        "ok": failures.is_empty() && opened.len() == pages && listed == pages,
        "requested_pages": pages,
        "opened": opened.len(),
        "listed_live": listed,
        "shots": shot_bytes.len(),
        "open_wall_ms": open_ms,
        "open_p50_ms": percentile(0.5),
        "open_p95_ms": percentile(0.95),
        "shot_total_ms": shot_ms,
        "shot_mean_ms": if shot_bytes.is_empty() { 0 } else { shot_ms as usize / shot_bytes.len() },
        "png_bytes_min": shot_bytes.iter().min().copied().unwrap_or(0),
        "failures": failures,
        "elapsed_ms": started.elapsed().as_millis(),
    });
    crate::daemon::journal("engine.bench.result", report.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- capture ----------------------------------------------------------

    // The four regions, plus the aliases an agent will reach for. A refusal
    // here is a capture that did not happen, so every spelling this accepts is
    // written down rather than discovered.
    #[test]
    fn shot_regions_parse_including_their_aliases() {
        let mode = |body: Value| CaptureMode::parse(&body).map(|m| m.region());
        assert_eq!(mode(json!({})).unwrap(), ShotRegion::Visible);
        for word in ["viewport", "visible"] {
            assert_eq!(
                mode(json!({ "region": word })).unwrap(),
                ShotRegion::Visible,
                "{word}"
            );
        }
        for word in ["full", "page", "document", "fullpage", "full_page"] {
            assert_eq!(
                mode(json!({ "region": word })).unwrap(),
                ShotRegion::FullDocument,
                "{word}"
            );
        }
        // Everything that CROPS snapshots the full document — an element below
        // the fold must capture without the caller scrolling to it first.
        assert_eq!(
            mode(json!({ "region": "element", "selector": "h1" })).unwrap(),
            ShotRegion::FullDocument
        );
        assert_eq!(
            mode(json!({ "region": "rect", "rect": {"x":0,"y":0,"w":10,"h":10} })).unwrap(),
            ShotRegion::FullDocument
        );
    }

    // A capture the caller cannot fix must not answer 200 with a plausible
    // image of the wrong thing. Each of these is a REFUSAL, and the message
    // has to name what was missing.
    #[test]
    fn a_capture_that_cannot_be_taken_is_refused_by_name() {
        let refuse = |body: Value| match CaptureMode::parse(&body) {
            Ok(_) => panic!("{body} should have been refused"),
            Err((status, message)) => {
                assert_eq!(status, 400, "{body}");
                message
            }
        };
        assert!(refuse(json!({ "region": "element" })).contains("selector"));
        assert!(refuse(json!({ "region": "rect" })).contains("DOCUMENT-space"));
        assert!(
            refuse(json!({ "region": "rect", "rect": {"x":0,"y":0,"w":0,"h":10} }))
                .contains("positive")
        );
        assert!(refuse(json!({ "region": "thumbnail" })).contains("viewport|full|element|rect"));
    }

    // The CSS->device conversion, which is where a crop silently captures the
    // wrong part of the page. Rounds OUTWARD so a 1px border survives, and
    // clamps rather than refusing a rect that merely overhangs.
    #[test]
    fn a_crop_rounds_outward_and_clamps_to_the_capture() {
        let rect = CssRect {
            x: 10.4,
            y: 20.6,
            w: 100.2,
            h: 50.1,
        };
        // Scale 1: floor the origin, ceil the far edge.
        assert_eq!(
            device_rect(rect, 1.0, (1000, 1000)).unwrap(),
            PixelRect {
                x: 10,
                y: 20,
                w: 101,
                h: 51
            }
        );
        // Scale 2: the SAME CSS rect covers twice the pixels.
        assert_eq!(
            device_rect(rect, 2.0, (1000, 1000)).unwrap(),
            PixelRect {
                x: 20,
                y: 41,
                w: 202,
                h: 101
            }
        );
        // An element flush with the document's right edge measures a fraction
        // wider than the document; clamping is right and refusing is not.
        let overhang = CssRect {
            x: 900.0,
            y: 0.0,
            w: 100.6,
            h: 10.0,
        };
        assert_eq!(
            device_rect(overhang, 1.0, (1000, 1000)).unwrap(),
            PixelRect {
                x: 900,
                y: 0,
                w: 100,
                h: 10
            }
        );
    }

    // ⛔ AN EMPTY CROP MUST NOT PRODUCE A BLANK PNG. That is the failure that
    // gets debugged as a rendering bug for an hour, and the message says the
    // one thing a caller in this position has got wrong: viewport coordinates
    // where document coordinates were asked for.
    #[test]
    fn a_crop_that_misses_the_capture_is_refused_not_blanked() {
        let miss = CssRect {
            x: 0.0,
            y: 5000.0,
            w: 100.0,
            h: 100.0,
        };
        let error = device_rect(miss, 1.0, (1000, 900)).unwrap_err();
        assert!(error.contains("does not overlap"), "{error}");
        assert!(error.contains("DOCUMENT coordinates"), "{error}");
    }

    // The account rides in a HEADER, so it must never be able to end the
    // headers early. Page-derived strings reach it (the element's tag), and a
    // response split is what a newline in one would buy.
    #[test]
    fn the_capture_account_cannot_split_the_response() {
        let header = shot_meta_header(&json!({ "tag": "div\r\nX-Evil: 1", "region": "full" }));
        assert!(header.starts_with(SHOT_META_HEADER), "{header}");
        assert_eq!(header.matches("\r\n").count(), 1, "{header}");
        assert!(
            header.contains("X-Evil"),
            "the value is escaped, not dropped: {header}"
        );
    }

    // The jar parser is the payment lane's gatekeeper: it must keep the
    // origin's cookies (HttpOnly included), drop other domains, drop expired
    // rows, and keep a 0-expiry row as a session cookie.
    #[test]
    fn netscape_jar_rows_filter_by_domain_and_expiry() {
        let jar = concat!(
            "# Netscape HTTP Cookie File\n",
            "#HttpOnly_rtionline.gov.in\tFALSE\t/\tTRUE\t0\tPHPSESSID\tabc123\n",
            ".rtionline.gov.in\tTRUE\t/rti\tFALSE\t4102444800\tpref\tone\n",
            "othersite.example\tFALSE\t/\tFALSE\t0\tstray\tno\n",
            "rtionline.gov.in\tFALSE\t/\tFALSE\t1\texpired\tgone\n",
        );
        let specs = parse_netscape_jar(jar, "rtionline.gov.in", 1_000_000);
        let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, ["PHPSESSID", "pref"]);
        assert!(specs[0].http_only && specs[0].secure);
        assert_eq!(specs[0].max_age, -1, "0 expiry = session cookie");
        assert!(specs[1].max_age > 0);
    }

    // Domain matching accepts both spellings of the same jar fact and refuses
    // the suffix trick a hand-rolled ends_with would fall for.
    #[test]
    fn cookie_domains_match_like_rfc6265() {
        assert!(domain_matches("rtionline.gov.in", ".rtionline.gov.in"));
        assert!(domain_matches("sub.rtionline.gov.in", "rtionline.gov.in"));
        assert!(!domain_matches("rtionline.gov.in", "online.gov.in"));
        assert!(!domain_matches("evilrtionline.gov.in", "rtionline.gov.in"));
    }

    // The origin's host comes from a real url parse, not a split.
    #[test]
    fn origin_hosts_parse_or_refuse() {
        assert_eq!(
            origin_host("https://RTIonline.gov.in/request").as_deref(),
            Some("rtionline.gov.in")
        );
        assert_eq!(
            origin_host("https://user@host.example:8443/x").as_deref(),
            Some("host.example")
        );
        assert_eq!(origin_host("not a url"), None);
    }

    // Path ownership must be exact. `/engineering` is not ours, and a daemon
    // that grabbed it would shadow a legacy route by prefix accident.
    #[test]
    fn engine_owns_only_its_own_paths() {
        assert!(owns("/engine"));
        assert!(owns("/engine/open"));
        assert!(owns("/engine/shot"));
        assert!(!owns("/engineering"));
        assert!(!owns("/ping"));
        assert!(!owns("/pane/vault"));
        assert!(!owns(""));
    }

    // Page ids are unique: two opens must never name the same page, or the
    // pool would silently alias two callers' work onto one view.
    #[test]
    fn page_ids_do_not_repeat() {
        let ids: Vec<String> = (0..64).map(|_| new_page_id()).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(ids.len(), unique.len(), "page ids collided: {ids:?}");
    }

    // The bench verb and the socket handler must build the SAME request, or
    // "proven live" would be a claim about a path nobody ships.
    #[test]
    fn the_helper_builds_a_real_engine_request() {
        let built = request(
            "goto",
            json!({ "page_id": "pg_1", "url": "https://example.com/" }),
        );
        assert!(owns(&built.path));
        assert_eq!(built.path, "/engine/goto");
        assert_eq!(built.body["url"], "https://example.com/");
        // No token: the socket's permissions are the authority (§4 as corrected).
        assert!(built.control_token.is_none());
    }

    /// A post-scroll payload for a node that is genuinely clickable, with
    /// `overrides` merged over it. Every refusal below is one field away from
    /// this, which is what makes the tests about the DECISION rather than about
    /// the payload shape.
    fn measured(overrides: Value) -> Value {
        let mut info = json!({
            "found": true, "phase": "post_scroll", "isConnected": true,
            "x": 120.0, "y": 44.0, "w": 90.0, "h": 30.0,
            "visible": true, "in_viewport": true, "onTarget": true,
            "hit": "button#real", "tag": "button"
        });
        for (key, value) in overrides.as_object().cloned().unwrap_or_default() {
            info[key] = value;
        }
        info
    }

    // The happy path first, or every refusal below could be passing because the
    // function refuses everything.
    #[test]
    fn a_hittable_node_resolves_to_its_centre() {
        let point = click_point_from_measure(".go", 0, &measured(json!({})))
            .expect("a hittable node resolves");
        assert_eq!(point, (120.0, 44.0));
    }

    // ⛔ THE REPORTED BUG, as a pure decision. Each of these once came back as a
    // dispatched click that hit nothing and an `{"ok":true}` that described it.
    // The tokens are the visible surface plane's, deliberately: an agent that
    // learned `detached_node` there must not meet a second word for it here.
    #[test]
    fn every_way_a_click_can_miss_is_refused_by_name() {
        let cases = [
            (json!({ "isConnected": false }), "detached_node"),
            (json!({ "visible": false }), "matched a zero-size element"),
            (json!({ "onTarget": false }), "target_moved"),
            (json!({ "found": false }), "handle_lost"),
            (json!({ "phase": "pre_scroll" }), "rect_not_reresolved"),
        ];
        for (overrides, token) in cases {
            let Err(error) = click_point_from_measure(".go", 0, &measured(overrides.clone()))
            else {
                panic!("{overrides} must refuse, it describes a click that cannot land");
            };
            assert!(
                error.contains(token),
                "expected {token:?} for {overrides}, got {error:?}"
            );
        }
    }

    // A refusal that does not say WHAT the point hit is a refusal an agent
    // cannot act on. The three flavours of `target_moved` must be
    // distinguishable in the message even though they share one token.
    #[test]
    fn target_moved_says_which_flavour_it_is() {
        let covered = click_point_from_measure(
            ".numb",
            0,
            &measured(json!({ "onTarget": false, "hit": "div#deaf" })),
        )
        .expect_err("a covered node refuses");
        assert!(covered.contains("div#deaf"), "{covered}");

        let offscreen = click_point_from_measure(
            ".parked",
            0,
            &measured(json!({ "onTarget": false, "in_viewport": false, "hit": null })),
        )
        .expect_err("an unreachable node refuses");
        assert!(offscreen.contains("outside the viewport"), "{offscreen}");

        let empty = click_point_from_measure(
            ".ghost",
            0,
            &measured(json!({ "onTarget": false, "hit": null })),
        )
        .expect_err("a point where nothing paints refuses");
        assert!(empty.contains("nothing is painted"), "{empty}");
    }

    // The parser must be PURE — it is what lets a malformed batch be refused
    // with nothing dispatched — so a selector click leaves the page untouched
    // and comes back as a job to do later.
    #[test]
    fn a_selector_click_is_parsed_without_touching_the_page() {
        let parsed = parse_input(&json!({
            "type": "click", "selector": ".go", "nth": 2, "require_unique": true,
            "button": "right", "count": 2
        }))
        .expect("a selector click parses");
        match parsed {
            PendingInput::SelectorClick {
                selector,
                nth,
                require_unique,
                button,
                count,
            } => {
                assert_eq!(selector, ".go");
                assert_eq!(nth, Some(2));
                assert!(require_unique);
                assert_eq!(button, 3);
                assert_eq!(count, 2);
            }
            other => panic!("a selector click must not resolve at parse time: {other:?}"),
        }
        // Coordinates need nothing from the page and must stay Ready.
        assert!(matches!(
            parse_input(&json!({ "type": "click", "x": 1, "y": 2 })).expect("a point click parses"),
            PendingInput::Ready(_)
        ));
        // A malformed `nth` is a caller error, caught before anything moves.
        assert!(parse_input(&json!({ "type": "click", "selector": ".go", "nth": -1 })).is_err());
        assert!(parse_input(&json!({ "type": "click" })).is_err());
    }

    /// ⭐ A CLICK THAT NAMES A FRAME MUST NOT BE PARSED AS A TOP-DOCUMENT ONE.
    ///
    /// The two ask different questions of the same `selector`, and answering the
    /// top-document one for an event that named a frame resolves it in the WRONG
    /// document and dispatches there — a click reported as landed in a frame it
    /// never entered. The parser stays pure either way: a frame click still
    /// touches no page.
    #[test]
    fn a_click_that_names_a_frame_is_parsed_as_a_frame_click() {
        let parsed = parse_input(&json!({
            "type": "click", "frame": "#pay", "selector": "#otp", "nth": 1
        }))
        .expect("a frame click parses");
        match parsed {
            PendingInput::FrameSelectorClick {
                frame: target,
                selector,
                nth,
                ..
            } => {
                assert_eq!(target.selector, "#pay");
                assert_eq!(target.nth, 0);
                assert_eq!(selector, "#otp");
                assert_eq!(nth, 1);
            }
            other => panic!("a frame click must not become a top-document click: {other:?}"),
        }
        // The index spelling addresses the same thing.
        assert!(matches!(
            parse_input(&json!({ "type": "click", "frame": 0, "selector": "#otp" }))
                .expect("an indexed frame click parses"),
            PendingInput::FrameSelectorClick { .. }
        ));
        // ⛔ A frame with no selector is a caller error, NOT a click at the
        // frame's own box: a bare x/y is already in the page's viewport and
        // needs no frame at all, so honouring it would silently mean something
        // else than the caller wrote.
        assert!(parse_input(&json!({ "type": "click", "frame": "#pay", "x": 1, "y": 2 })).is_err());
        // And a null `frame` is absent, not an address — otherwise a caller
        // building events programmatically gets a frame refusal for a
        // top-document click.
        assert!(matches!(
            parse_input(&json!({ "type": "click", "frame": null, "selector": ".go" }))
                .expect("a null frame is no frame"),
            PendingInput::SelectorClick { .. }
        ));
    }

    /// ⛔ `frame=` ON `/engine/eval` IS REFUSED BY NAME, AND THE REFUSAL POINTS
    /// AT THE VERB THAT DOES THE JOB.
    ///
    /// This is settled design, not a gap: a bridge that evaluates a
    /// page-supplied string inside a cross-origin child is one the EMBEDDING
    /// page can drive, because cross-frame `postMessage` delivery is not
    /// world-scoped (measured, `ychrome engine worlds`). A future session
    /// reading `eval` and seeing no frame support would otherwise reasonably
    /// "finish" it.
    #[test]
    fn eval_refuses_a_frame_argument_and_names_the_read_verb() {
        let body = route_body(include_str!("api.rs"), "eval");
        // ⚠ The STRING LITERAL, not the expression around it. A predicate keyed
        // on `request.body.get("frame")` broke the moment rustfmt split that
        // call across four lines — a lock whose mechanism depends on an
        // expression's layout punishes honestly-made edits, and this file has
        // paid that price before.
        assert!(
            body.contains("\"frame\""),
            "eval must look for `frame=` — a silently ignored one reads as support"
        );
        assert!(
            body.contains("/engine/frame"),
            "the refusal must name the verb that resolves inside a frame"
        );
        // And the refusal must come BEFORE the evaluation, or a caller's script
        // runs in the top document while being told the argument was refused.
        let refusal = body.find("/engine/frame").expect("the refusal exists");
        let evaluate = body.find("engine.eval(").expect("eval evaluates");
        assert!(
            refusal < evaluate,
            "the refusal must precede the evaluation, or the script has already run"
        );
    }

    // The three scripts must agree on the globals they hand each other. Two
    // spellings of one page-side key is the same class of silent divergence as
    // two spellings of a refusal.
    #[test]
    fn the_click_scripts_share_one_page_side_vocabulary() {
        assert!(js::CLICK_POOL.contains(js::CLICK_POOL_KEY));
        assert!(js::CLICK_PIN.contains(js::CLICK_POOL_KEY));
        assert!(js::CLICK_PIN.contains(js::CLICK_PIN_KEY));
        assert!(js::CLICK_MEASURE.contains(js::CLICK_PIN_KEY));
        // Phase B must not re-run the selector: measuring a twin is how the
        // surface plane once verified a node it had not acted on.
        assert!(
            !js::CLICK_MEASURE.contains("querySelector"),
            "the re-measure must read the PIN, never the selector again"
        );
        // Only phase B stamps the contract token.
        assert!(js::CLICK_MEASURE.contains("phase: 'post_scroll'"));
        assert!(!js::CLICK_PIN.contains("post_scroll"));
        // And phase A2 must not report geometry — a rect read in the same tick
        // as the scroll is the PRE-scroll rect.
        assert!(!js::CLICK_PIN.contains("getBoundingClientRect"));
    }

    // ⛔ THE CLAUSE THAT WAS THE BUG. `elementFromPoint` over a
    // `visibility:hidden` element returns `<body>`, and `<body>.contains(el)` is
    // true for every element on the page — so accepting an ancestor hit accepted
    // the first match unconditionally and made the walk a no-op. The live proof
    // (`ychrome engine hit`, the `.numb` step) catches it too; this catches it
    // without a browser, in a second.
    #[test]
    fn an_ancestor_hit_is_never_accepted_as_hittability() {
        assert!(
            !js::CLICK_MEASURE.contains("hit.contains(el)"),
            "an ancestor that contains the target is not the target: a click there reaches the \
             ANCESTOR, and this clause is exactly how a hidden decoy passed for a live control"
        );
    }

    /// ONE top-level route's body, scraped out of the dispatcher's source.
    ///
    /// ⛔ **THE TERMINATOR IS THE ARM'S OWN CLOSING BRACE, NOT A LATER VERB
    /// SOMEONE TYPED BY HAND.** Each structural lock below used to stop at a
    /// named neighbour — `fill` stopped at `"shot" =>` — so inserting a route
    /// between them silently widened the scrape over code the lock does not
    /// govern. That fired the day `fill-card` landed: the fill lock read a
    /// comment two routes away that mentioned a password and failed a route
    /// which had not changed by a character. A lock that fails on other
    /// people's code is a lock that gets silenced, so the boundary is now the
    /// only thing that actually marks the end of an arm.
    fn route_body(source: &str, verb: &str) -> String {
        let opened = source
            .split(&format!("\"{verb}\" => {{"))
            .nth(1)
            .unwrap_or_else(|| panic!("the {verb} route exists"));
        opened
            .lines()
            // The dispatcher's arms close at eight spaces; nothing inside a
            // body is that shallow, so this is the arm and only the arm.
            .take_while(|line| *line != "        }" && *line != "        },")
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// ⛔ THE BANNER AND THE ROUTER MUST NOT BE ABLE TO DISAGREE.
    ///
    /// `fill` was reachable and unadvertised, so agents concluded the engine
    /// had no credential support. This parses the dispatcher's own match arms
    /// out of the source and holds them against [`VERBS`], so a new route that
    /// forgets the banner fails here instead of in a field report months later.
    #[test]
    fn the_banner_lists_every_verb_the_router_answers() {
        let source = include_str!("api.rs");
        let dispatcher = source
            .split("fn route(")
            .nth(1)
            .expect("the router exists")
            .split("\n/// Every verb the router answers")
            .next()
            .expect("the dispatcher ends before the verb list");
        let mut arms: Vec<String> = Vec::new();
        for line in dispatcher.lines() {
            // ⚠ TOP-LEVEL ARMS ONLY, keyed on indentation. The first version of
            // this test scraped the whole dispatcher body and reported `html` —
            // which is a `mode` inside `dom`, not a route. A nested match arm
            // is not a verb, and a test that cannot tell the difference would
            // have to be silenced, which is how a lock stops locking.
            let indent = line.len() - line.trim_start().len();
            if indent != 8 {
                continue;
            }
            let trimmed = line.trim();
            // An arm looks like:  "open" => {   or   "pool" | "metrics" => …
            let Some((head, _)) = trimmed.split_once("=>") else {
                continue;
            };
            if !head.starts_with('"') {
                continue;
            }
            for token in head.split('|') {
                let name = token.trim().trim_matches(|c| c == '"' || c == ' ');
                // `""` is the bare-status alias, not a verb a caller types.
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                    arms.push(name.to_string());
                }
            }
        }
        assert!(
            arms.len() > 15,
            "the arm scraper found only {arms:?} — it stopped matching the router's shape"
        );
        for arm in &arms {
            assert!(
                VERBS.contains(&arm.as_str()),
                "the router answers {arm:?} and the banner never mentions it — \
                 a route that is not advertised is a capability nobody knows exists"
            );
        }
        for verb in VERBS {
            assert!(
                arms.iter().any(|arm| arm == verb),
                "the banner advertises {verb:?} and the router has no arm for it"
            );
        }
    }

    /// ⛔ A `type` EVENT MUST BE MEASURED, BECAUSE IT RESOLVES NOTHING.
    ///
    /// The batch `click, type, click, type` answered `dispatched:3, ok:true`
    /// over a login that a page-side readback then showed empty. `dispatched`
    /// counts events SENT; nothing in that reply came from the page. This locks
    /// the two halves of the answer: a refusal BEFORE typing into a node that
    /// holds no text, and a readback AFTER for everything else.
    #[test]
    fn a_type_event_is_measured_against_the_page_and_not_the_request() {
        let body = route_body(include_str!("api.rs"), "input");
        assert!(
            body.contains("focus_readback") && body.contains("typed_landing"),
            "a type event must be measured before and after it is dispatched"
        );
        assert!(
            body.contains("accepts_text"),
            "text with nowhere to go must be caught BEFORE it is typed — after is too late, \
             the secret is already in the page"
        );
        assert!(
            body.contains("require_target"),
            "the refusal needs a named opt-out for pages that read bare keystrokes"
        );
        // ⛔ The pre-dispatch guard CANNOT catch the reported case: the field
        // was there when it was checked, and the page took it away in response
        // to the first keystroke. Measured on the harness — `grew_by: 0`,
        // `target.tag: "body"`. Without this second gate the batch answers
        // `ok: true` over text the page threw away, which is the whole report.
        assert!(
            body.contains("discarded"),
            "a type event whose text the page kept NONE of must stop the batch, not be \
             reported under `ok: true`"
        );
    }

    /// ⛔ A LENGTH, NEVER A VALUE. `/engine/input` is the one route a caller
    /// can put a secret through, so its own reply must not become a second
    /// copy of it — the same boundary `fill` keeps.
    #[test]
    fn the_typed_landing_report_carries_a_length_and_never_the_text() {
        let source = include_str!("api.rs");
        let report = source
            .split("fn typed_landing(")
            .nth(1)
            .expect("typed_landing exists");
        let body = &report[..report.find("\n}\n").unwrap_or(report.len())];
        assert!(
            body.contains("grew_by") && body.contains("\"landed\""),
            "the verdict must be the difference the page kept"
        );
        for leak in ["text.clone()", "\"text\":", "text.to_string()"] {
            assert!(
                !body.contains(leak),
                "the report must not carry the typed text itself ({leak})"
            );
        }
    }

    /// `landed:false` over a real field is REPORTED, not refused: an input mask
    /// that strips separators and an OTP field that submits on its last digit
    /// both grow by the wrong amount and are both correct pages. Only the
    /// unambiguous fault — no editable target at all — stops a batch.
    #[test]
    fn a_field_that_kept_the_wrong_amount_is_reported_rather_than_refused() {
        assert!(!accepts_text(None), "an unreadable page is not a target");
        assert!(
            !accepts_text(Some(&json!({ "focused": false, "length": 0 }))),
            "nothing focused is not a target"
        );
        assert!(
            !accepts_text(Some(&json!({ "focused": true, "length": Value::Null }))),
            "a focused node that holds no text — a button — is not a target"
        );
        assert!(
            accepts_text(Some(&json!({ "focused": true, "length": 7 }))),
            "a focused field holding text IS a target, however much it holds"
        );
    }

    /// ⭐ A FOCUSED FRAME AND A FOCUSED BUTTON READ IDENTICALLY, AND THEY ARE
    /// OPPOSITE FACTS.
    ///
    /// Both report `focused: true, length: null` from the top document, so
    /// `accepts_text` says no to each. For a button that is right — the text
    /// would be discarded. For a frame it is wrong: the keystrokes are
    /// delivered INTO the frame, and only that frame's document could measure
    /// where they went.
    ///
    /// Measured: with these two collapsed, `ychrome engine frames` refused its
    /// last step with "would type into <iframe>, which holds no text" — a
    /// payment field inside a bank's frame became undrivable one event after
    /// the click that focused it.
    #[test]
    fn a_focused_frame_is_not_a_button_even_though_it_reads_like_one() {
        let frame = json!({ "focused": true, "frame": true, "tag": "iframe",
                            "length": Value::Null });
        let button = json!({ "focused": true, "frame": false, "tag": "button",
                             "length": Value::Null });
        // The collapse that caused the regression: to `accepts_text` these are
        // the same answer, which is exactly why the third predicate exists.
        assert!(!accepts_text(Some(&frame)));
        assert!(!accepts_text(Some(&button)));
        assert!(
            focus_is_a_frame(Some(&frame)),
            "a focused frame must be recognised, or the guard refuses a working drive"
        );
        assert!(
            !focus_is_a_frame(Some(&button)),
            "a button must NOT be exempted, or the guard stops catching discarded text"
        );
        // An older readback, from before the flag existed, must not be read as
        // a frame — absence of the field is not evidence of one.
        assert!(!focus_is_a_frame(Some(
            &json!({ "focused": true, "length": 3 })
        )));
        assert!(!focus_is_a_frame(None));
    }

    /// The predicate is only as good as the field it reads, and that field is
    /// set in JavaScript on the other side of an `eval`. If the instrument
    /// stopped reporting `frame`, `focus_is_a_frame` would answer false for
    /// every page and the regression would return with all its tests green.
    #[test]
    fn the_focus_instrument_reports_whether_the_focused_node_is_a_frame() {
        let source = crate::engine::js::FOCUS_READBACK;
        assert!(
            source.contains("'iframe'") && source.contains("'frame'"),
            "the instrument must recognise both frame-owning tags"
        );
        assert!(
            source.contains("frame: frame") && source.contains("frame: false"),
            "every return path must carry the flag — a missing one reads as `not a frame`"
        );
    }

    /// `fill` drives a named page, so it must resume a parked one like every
    /// other page-driving verb. Leaving it out of this list is not a small
    /// bug: the governor would be free to park the page between `open` and
    /// `fill`, and the fill would then land on nothing while answering 200.
    #[test]
    fn fill_drives_a_page_and_so_must_resume_one() {
        assert!(
            DRIVES_A_PAGE.contains(&"fill"),
            "fill targets a page_id; omitting it lets the governor park the page under it"
        );
        // ⛔ And its card twin, where the cost of getting this wrong is worse:
        // a login fill that lands on a parked page is retried, a card fill that
        // does is a payment page the operator then has to redo by hand on his
        // own laptop — the exact thing this verb exists to stop.
        assert!(
            DRIVES_A_PAGE.contains(&"fill-card"),
            "fill-card targets a page_id; omitting it lets the governor park the page under it"
        );
    }

    /// ⛔ THE PAN MUST NOT BE REACHABLE FROM THIS ROUTE, and the cheapest way to
    /// keep that true is for the route never to hold it.
    ///
    /// The twin of [`the_fill_route_never_names_a_password_field`], and stricter
    /// for a reason the vault agent already documents: a password can be
    /// rotated on demand and a card number cannot, so a PAN that reaches a
    /// reply, a log line or an agent transcript is durable damage. The route
    /// asks `sidebar::vault_card_fill_script` for a SCRIPT, so the number exists
    /// only inside the page-bound string.
    #[test]
    fn the_fill_card_route_never_names_the_pan() {
        let body = route_body(include_str!("api.rs"), "fill-card");
        // The QUOTED spellings, because those are the leak: `field("number")`,
        // `reply["code"]`, a hand-rolled `"card-secret"` op. Matching the bare
        // English word would fail on a comment that explains the boundary,
        // which is a lock that punishes documenting itself.
        for leak in ["\"number\"", "\"code\"", "card_secret", "card-secret"] {
            assert!(
                !body.contains(leak),
                "the fill-card route must hand off to vault_card_fill_script, not touch the \
                 secret itself ({leak})"
            );
        }
        assert!(
            body.contains("vault_card_fill_script"),
            "the fill-card route must go through the sidebar's card path"
        );
    }

    /// ⛔ THE SECRET MUST NOT BE REACHABLE FROM THE REPLY, and the cheapest way
    /// to keep that true is for this route never to hold the password itself.
    /// It asks `sidebar::vault_fill_script` for a SCRIPT and evals it, so the
    /// value exists only inside the page-bound string.
    ///
    /// This test guards the shape rather than the runtime, because the runtime
    /// needs an unlocked agent: if someone ever "simplifies" this route by
    /// fetching the password here to build the JSON, the literal below appears
    /// and this fails.
    #[test]
    fn the_fill_route_never_names_a_password_field() {
        let body = route_body(include_str!("api.rs"), "fill");
        assert!(
            !body.contains("password"),
            "the fill route must hand off to vault_fill_script, not touch the secret itself"
        );
        assert!(
            body.contains("vault_fill_script"),
            "the fill route must go through the sidebar's vault path"
        );
    }
}
