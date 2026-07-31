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
use super::pool::{self, pool};
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
/// Writes an NDJSON stream, one JSON object per line, flushing as it goes.
pub type NdjsonBody = Box<dyn FnOnce(&mut dyn std::io::Write) + Send>;

pub enum Reply {
    Json(u16, Value),
    Png(Vec<u8>),
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
        Reply::Png(_) | Reply::Ndjson(_) => (200, None),
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
                // Zoom is per SITE, so it is applied per navigation, from
                // `webzoom`'s recorded sites — never remembered here.
                let done = engine.goto(&id, url, GOTO_TIMEOUT);
                let _ = engine.apply_zoom(&id, url);
                done
            };
            pool().unpin(&id);
            if let Err(error) = loaded {
                return Reply::bad(502, error.to_string());
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
            match engine.goto(&id, url, GOTO_TIMEOUT) {
                Ok(_) => {
                    let _ = engine.apply_zoom(&id, url);
                    Reply::Json(200, page_status(&engine, &id))
                }
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
            }),
        ),
        other => Reply::bad(404, format!("unknown engine verb {other:?}")),
    }
}

/// Verbs that drive a NAMED page, and therefore need it live and touched.
///
/// `park` is deliberately absent: parking a page must not first resume it.
/// `close` is absent for the same reason — forgetting a parked page should not
/// cost a page load.
const DRIVES_A_PAGE: [&str; 7] = ["goto", "nav", "eval", "shot", "dom", "input", "wait"];

/// A reply's status and body, for callers that only need the pair.
///
/// An NDJSON stream is drained into memory HERE and only here — an in-process
/// caller (the bench, the parity run) still exercises the real streaming code
/// path and then reads the lines it produced, rather than the router growing a
/// second non-streaming batch for tests to call.
pub fn json_status(reply: Reply) -> (u16, Value) {
    match reply {
        Reply::Json(status, body) => (status, body),
        Reply::Png(png) => (200, json!({ "png_bytes": png.len() })),
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
    // ⛔ FIRST MATCH IS NOT GOOD ENOUGH, AND ZERO AREA IS NOT THE ONLY WAY TO
    // MISS. A `display:none` duplicate ahead of the real control measures 0x0
    // and is caught below — but a `visibility:hidden`, `opacity:0`, or simply
    // COVERED duplicate measures a perfectly good rect, so the click dispatched
    // into nothing and `{"dispatched":n,"ok":true}` described a click that hit
    // no one. That lie-of-success cost a reporting agent three wrong
    // conclusions in one session, one of which blamed the operator.
    //
    // So: walk every match and take the first that is genuinely hittable, and
    // decide hittability the way the browser does — `elementFromPoint` at the
    // centre must land on the element or inside it. That single test subsumes
    // zero-area, off-screen, hidden AND covered, instead of enumerating the
    // ways an element can be unclickable and missing one.
    let quoted = serde_json::to_string(selector)?;
    let value = engine.eval(
        id,
        &format!(
            "(() => {{ const all = Array.from(document.querySelectorAll({quoted})); \
             if (!all.length) return null; \
             let rejected = 0, reason = null; \
             for (const e of all) {{ \
               e.scrollIntoView({{block:'center', inline:'center', behavior:'instant'}}); \
               const r = e.getBoundingClientRect(); \
               if (r.width <= 0 || r.height <= 0) {{ rejected++; reason = reason || 'zero_size_element'; continue; }} \
               const x = r.x + r.width / 2, y = r.y + r.height / 2; \
               if (x < 0 || y < 0 || x > innerWidth || y > innerHeight) {{ rejected++; reason = reason || 'offscreen_element'; continue; }} \
               const hit = document.elementFromPoint(x, y); \
               if (!hit || !(hit === e || e.contains(hit) || hit.contains(e))) {{ rejected++; reason = reason || 'element_not_hittable'; continue; }} \
               return {{ x, y, ok: true, total: all.length, rejected }}; \
             }} \
             return {{ ok: false, total: all.length, rejected, reason }}; }})()"
        ),
    )?;
    if value.is_null() {
        bail!("selector {selector:?} matched nothing on this page");
    }
    if value["ok"].as_bool() != Some(true) {
        let total = value["total"].as_u64().unwrap_or(0);
        let reason = value["reason"].as_str().unwrap_or("element_not_hittable");
        bail!(
            "selector {selector:?} matched {total} element(s) and NONE was clickable ({reason}) \
             — refusing rather than dispatching into the void"
        );
    }
    let (Some(x), Some(y)) = (value["x"].as_f64(), value["y"].as_f64()) else {
        bail!("selector {selector:?} resolved to no rect");
    };
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
        "profile": engine.page_profile(id).ok(),
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
            Reply::Png(png) if png.starts_with(b"\x89PNG\r\n\x1a\n") => shot_bytes.push(png.len()),
            Reply::Png(_) => failures.push(format!("{id}: readback was not a PNG")),
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
