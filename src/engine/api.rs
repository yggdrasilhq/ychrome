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
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::host::{Engine, InputEvent, NavAction};
use super::js;
use crate::sidebar::ParsedRequest;

/// Default page viewport. The spec's `page` shape carries a per-page viewport;
/// v1 gives every page the engine's own size unless the caller says otherwise.
const DEFAULT_W: i32 = 1280;
const DEFAULT_H: i32 = 900;

/// How long a navigation may take before `/engine/goto` gives up.
const GOTO_TIMEOUT: Duration = Duration::from_secs(45);

/// `/engine/wait`'s default budget, and its poll cadence (§4).
const WAIT_TIMEOUT_MS: u64 = 15_000;
const POLL_MS: u64 = 100;

/// What a route hands back. Most replies are JSON; `/engine/shot` is PNG bytes
/// with its own content type, as §4 specifies — a base64 blob inside JSON
/// would have been a second encoding of an image the HTTP layer can carry
/// natively.
pub enum Reply {
    Json(u16, Value),
    Png(Vec<u8>),
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
        Reply::Png(_) => (200, None),
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

    match verb {
        "open" => {
            let id = page_id.unwrap_or_else(new_page_id);
            let width = request.body["viewport"]["w"]
                .as_i64()
                .unwrap_or(DEFAULT_W as i64) as i32;
            let height = request.body["viewport"]["h"]
                .as_i64()
                .unwrap_or(DEFAULT_H as i64) as i32;
            if let Err(error) = engine.open(&id, width, height) {
                return Reply::bad(400, error.to_string());
            }
            // `{url}` on open is sugar for open-then-goto, and it waits, so a
            // caller that passed a url gets a page that has actually loaded.
            if let Some(url) = request.body.get("url").and_then(Value::as_str)
                && let Err(error) = engine.goto(&id, url, GOTO_TIMEOUT)
            {
                return Reply::bad(502, error.to_string());
            }
            Reply::Json(200, page_status(&engine, &id))
        }
        "close" => match page_id {
            None => Reply::bad(400, "close needs a page_id"),
            Some(id) => match engine.close(&id) {
                Ok(()) => Reply::Json(200, json!({ "ok": true, "closed": id })),
                Err(error) => Reply::bad(404, error.to_string()),
            },
        },
        "pages" => match engine.page_ids() {
            Ok(ids) => Reply::Json(
                200,
                json!({
                    "ok": true,
                    "pages": ids.iter().map(|id| page_status(&engine, id)).collect::<Vec<_>>(),
                }),
            ),
            Err(error) => Reply::bad(500, error.to_string()),
        },
        "goto" => {
            let (Some(id), Some(url)) = (page_id, request.body.get("url").and_then(Value::as_str))
            else {
                return Reply::bad(400, "goto needs a page_id and a url");
            };
            match engine.goto(&id, url, GOTO_TIMEOUT) {
                Ok(_) => Reply::Json(200, page_status(&engine, &id)),
                Err(error) => Reply::bad(502, error.to_string()),
            }
        }
        "eval" => {
            let (Some(id), Some(js)) = (page_id, request.body.get("js").and_then(Value::as_str))
            else {
                return Reply::bad(400, "eval needs a page_id and js");
            };
            match engine.eval(&id, js) {
                Ok(value) => Reply::Json(200, json!({ "ok": true, "value": value })),
                Err(error) => Reply::bad(400, error.to_string()),
            }
        }
        "shot" => match page_id {
            None => Reply::bad(400, "shot needs a page_id"),
            Some(id) => match engine.shot(&id) {
                Ok(shot) => Reply::Png(shot.png),
                Err(error) => Reply::bad(404, error.to_string()),
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
            // Parse the WHOLE batch before dispatching any of it. A batch that
            // failed halfway would leave the page in a state no caller asked
            // for and none can name.
            let mut events = Vec::with_capacity(raw.len());
            for event in &raw {
                match parse_input(&engine, &id, event) {
                    Ok(parsed) => events.push(parsed),
                    Err(error) => return Reply::bad(400, error.to_string()),
                }
            }
            match engine.input(&id, events) {
                Ok(dispatched) => Reply::Json(200, json!({ "ok": true, "dispatched": dispatched })),
                Err(error) => Reply::bad(400, error.to_string()),
            }
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
        "" | "status" => Reply::Json(
            200,
            json!({
                "ok": true,
                "substrate": engine.substrate().id(),
                "display": engine.display_name(),
                "pages": engine.page_ids().unwrap_or_default(),
            }),
        ),
        other => Reply::bad(404, format!("unknown engine verb {other:?}")),
    }
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

/// Parse one `/engine/input` event.
///
/// Selector-addressed clicks are sugar (§4): the engine resolves the selector's
/// centre in the page, scrolls it into view, and dispatches REAL coordinates.
/// One resolver, shared by `/input` and `/dom`, so a selector means the same
/// thing to both.
fn parse_input(engine: &Engine, id: &str, event: &Value) -> Result<InputEvent> {
    let kind = event["type"].as_str().unwrap_or("");
    let point = |event: &Value| -> Result<(f64, f64)> {
        match (event["x"].as_f64(), event["y"].as_f64()) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => bail!("a {kind} event needs x and y (or, for a click, a selector)"),
        }
    };
    match kind {
        "click" => {
            let (x, y) = match event["selector"].as_str() {
                Some(selector) => resolve_selector(engine, id, selector)?,
                None => point(event)?,
            };
            let button = match event["button"].as_str().unwrap_or("left") {
                "left" => 1,
                "middle" => 2,
                "right" => 3,
                other => bail!("unknown mouse button {other:?} (left|middle|right)"),
            };
            let count = event["count"].as_u64().unwrap_or(1).clamp(1, 3) as u32;
            Ok(InputEvent::Click {
                x,
                y,
                button,
                count,
            })
        }
        "move" => {
            let (x, y) = point(event)?;
            Ok(InputEvent::Move { x, y })
        }
        "scroll" => {
            let x = event["x"].as_f64().unwrap_or(0.0);
            let y = event["y"].as_f64().unwrap_or(0.0);
            Ok(InputEvent::Scroll {
                x,
                y,
                dx: event["dx"].as_f64().unwrap_or(0.0),
                dy: event["dy"].as_f64().unwrap_or(0.0),
            })
        }
        "type" => match event["text"].as_str() {
            Some(text) => Ok(InputEvent::Text {
                text: text.to_string(),
            }),
            None => bail!("a type event needs text"),
        },
        "key" => {
            let Some(name) = event["key"].as_str() else {
                bail!("a key event needs a key name, e.g. \"Enter\"");
            };
            let mods: Vec<String> = event["mods"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Ok(InputEvent::Key {
                keyval: super::host::keyval_from_name(name)?,
                mods: super::host::modifier_mask(&mods)?,
            })
        }
        other => bail!("unknown input event type {other:?} (click|move|scroll|type|key)"),
    }
}

/// Scroll a selector into view and return the viewport centre of its rect.
fn resolve_selector(engine: &Engine, id: &str, selector: &str) -> Result<(f64, f64)> {
    let quoted = serde_json::to_string(selector)?;
    let value = engine.eval(
        id,
        &format!(
            "(() => {{ const e = document.querySelector({quoted}); if (!e) return null; \
             e.scrollIntoView({{block:'center', inline:'center', behavior:'instant'}}); \
             const r = e.getBoundingClientRect(); \
             return {{ x: r.x + r.width / 2, y: r.y + r.height / 2, \
                       w: r.width, h: r.height }}; }})()"
        ),
    )?;
    if value.is_null() {
        bail!("selector {selector:?} matched nothing on this page");
    }
    let (Some(x), Some(y)) = (value["x"].as_f64(), value["y"].as_f64()) else {
        bail!("selector {selector:?} resolved to no rect");
    };
    if value["w"].as_f64().unwrap_or(0.0) <= 0.0 || value["h"].as_f64().unwrap_or(0.0) <= 0.0 {
        bail!("selector {selector:?} has zero area — it cannot be clicked");
    }
    Ok((x, y))
}

/// The one `page` status shape (§4), built in one place so no route grows its
/// own. The governance fields the spec lists (`rss_mb`, `cpu_pct_1m`, park
/// state) belong to Phase D and are absent rather than faked — a zero would
/// read as a measurement.
fn page_status(engine: &Engine, id: &str) -> Value {
    let url = engine.eval(id, "location.href").ok();
    let title = engine.eval(id, "document.title").ok();
    json!({
        "ok": true,
        "page_id": id,
        "url": url,
        "title": title,
        "state": "live",
        "loading": engine.is_loading(id).unwrap_or(false),
    })
}

fn new_page_id() -> String {
    // Monotonic and unique within a daemon's life. Not a ULID: the spec's
    // `pg_01hxyz…` shape is cosmetic and a counter cannot collide.
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("pg_{:06}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Build a request the way the socket handler will, so callers that are not
/// HTTP (the bench verb) exercise the very same router.
pub fn request(verb: &str, body: Value) -> ParsedRequest {
    ParsedRequest {
        method: "POST".to_string(),
        path: format!("/engine/{verb}"),
        query: String::new(),
        fido2_token: None,
        control_token: None,
        body,
    }
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
            let reply = dispatch(&request("open", json!({ "url": url })));
            match reply {
                Reply::Json(200, body) => Ok((
                    body["page_id"].as_str().unwrap_or_default().to_string(),
                    began.elapsed().as_millis() as u64,
                )),
                Reply::Json(status, body) => Err(format!("open {index} -> {status} {body}")),
                Reply::Png(_) => Err(format!("open {index} returned a PNG")),
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
            Reply::Png(png) if png.starts_with(b"\x89PNG\r\n\x1a\n") => shot_bytes.push(png.len()),
            Reply::Png(_) => failures.push(format!("{id}: readback was not a PNG")),
            Reply::Json(status, body) => failures.push(format!("{id}: shot -> {status} {body}")),
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
}
