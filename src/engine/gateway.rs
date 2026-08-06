//! The gateway hand-off proof (`ychrome engine gateway`).
//!
//! A bank-payment gateway is the shape that broke: a merchant page holds a
//! form, the form POSTs to the bank's own origin, and the bank answers by
//! redirecting, by self-submitting onward, or by taking a window of its own.
//! Three of those the engine already did. The fourth — **the window of its
//! own** — it dropped on the floor, silently, while `/engine/input` reported a
//! dispatched click. An agent driving a real government payment saw a
//! successful click and a page that had not moved.
//!
//! Two things make this proof different from `flow`'s, and both are the reason
//! it is its own verb rather than four more steps there:
//!
//! - **it needs two ORIGINS.** `file://` cannot express a cross-origin POST at
//!   all — a form POST needs an HTTP responder, and "cross-origin" needs two of
//!   them. So this fixture is two real listeners on ephemeral loopback ports.
//! - **it is about what the engine does with what the PAGE asks for**, not
//!   about a verb an agent calls. Nothing here would go red if every verb in
//!   `flow` were deleted.
//!
//! Every step goes through [`super::api::dispatch`], the router the daemon
//! socket calls, so a pass is a statement about the shipping path.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::api::{dispatch, json_status as json_reply, request};

/// How long a step waits for a navigation the PAGE started. Nothing here calls
/// `goto`, so there is no responder to block on — the page moves on its own
/// clock and the proof polls for it, exactly as an agent must.
const SETTLE: Duration = Duration::from_millis(250);
const SETTLE_TRIES: u32 = 40;

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
// The fixture: two origins
// ---------------------------------------------------------------------------

/// What one origin answers: `(method, path) -> (status, extra headers, body)`.
type Route = Arc<dyn Fn(&str, &str) -> (u16, Vec<String>, String) + Send + Sync>;

/// Every request an origin actually received, as `METHOD path`.
///
/// The browser's own url is not evidence that a request was made — a view can
/// be LISTED at a url it never fetched, which is precisely how the bug under
/// proof presented. The server's log is the only thing that knows.
type Hits = Arc<Mutex<Vec<String>>>;

/// Serve `reply` on an ephemeral loopback port, forever, on its own thread.
///
/// Deliberately the smallest thing that can hold a POST: a proof server that
/// grew features would start having bugs of its own, and then a red step would
/// mean two things.
fn spawn_origin(listener: TcpListener, reply: Route) -> Result<(u16, Hits)> {
    let port = listener.local_addr()?.port();
    let hits: Hits = Arc::new(Mutex::new(Vec::new()));
    let log = hits.clone();
    std::thread::Builder::new()
        .name(format!("ychrome-gateway-fixture-{port}"))
        .spawn(move || {
            // ⛔ A THREAD PER CONNECTION, and this is not tidiness. WebKit
            // opens a speculative connection ahead of a navigation and sends
            // nothing on it; a serial accept loop blocks on that socket's
            // first line and never reaches the connection carrying the real
            // POST. Measured before this: the hand-off looked like a flat 10 s
            // stall with an empty request log, which reads exactly like the
            // engine dropping it — a fixture that cannot tell those apart is
            // worse than no fixture.
            for stream in listener.incoming().flatten() {
                let reply = reply.clone();
                let log = log.clone();
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
                    let _ = serve_one(stream, &reply, &log);
                });
            }
        })
        .context("spawning a fixture origin")?;
    Ok((port, hits))
}

fn serve_one(mut stream: TcpStream, reply: &Route, hits: &Hits) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut length = 0usize;
    let mut expects_continue = false;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            length = rest.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("expect:") && lower.contains("100-continue") {
            expects_continue = true;
        }
    }
    // ⚠ WebKit sends `Expect: 100-continue` for a form POST that carries a
    // body, and WAITS for the interim reply before writing it — measured here
    // as a flat 10 s stall that read exactly like the engine dropping the
    // hand-off. A fixture that cannot answer this cannot tell a slow browser
    // from a broken one, which is the one thing it exists to do.
    // Logged BEFORE the body is read: the request is a fact the moment its
    // head arrives, and logging it after the body would date it to whenever
    // the browser got around to writing one.
    if let Ok(mut log) = hits.lock() {
        log.push(format!("{method} {path}"));
    }
    if expects_continue {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        stream.flush()?;
    }
    if length > 0 {
        // Read it and drop it: the fixture proves the hand-off ARRIVED, and a
        // body it echoed would tempt a future step into asserting on the echo
        // instead of on the page.
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
    }
    let (status, extra, body) = reply(&method, &path);
    let reason = match status {
        303 => "See Other",
        404 => "Not Found",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for header in extra {
        head.push_str(&header);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

fn document(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font:16px sans-serif\">{body}</body></html>"
    )
}

/// The merchant's pages. Each holds ONE form, in the shape a real payment page
/// carries it — named `frmPayment`, submitted by `#SubmitPayment`.
pub fn merchant_page(path: &str, gateway: &str) -> Option<String> {
    match path {
        // The shape that was dropped: the bank takes a window of its own.
        "/blank-target" => Some(document(
            "MERCHANT blank-target",
            &format!(
                "<form id=\"frmPayment\" method=\"post\" target=\"_blank\" action=\"{gateway}/gw\">\
                 <input type=\"hidden\" name=\"EncryptTrans\" value=\"A1B2C3\">\
                 <button type=\"submit\" id=\"SubmitPayment\">Pay</button></form>"
            ),
        )),
        // The shape that wedged the page: a dialog nobody can reach.
        "/alert-on-submit" => Some(document(
            "MERCHANT alert-on-submit",
            &format!(
                "<form id=\"frmPayment\" method=\"post\" action=\"{gateway}/gw-redirect\" \
                 onsubmit=\"alert('Redirecting you to your bank');\">\
                 <button type=\"submit\" id=\"SubmitPayment\">Pay</button></form>"
            ),
        )),
        "/landed" => Some(document(
            "MERCHANT LANDED",
            "<p id=\"landed\">the gateway handed back</p>",
        )),
        _ => None,
    }
}

/// What the bank answers.
pub fn gateway_page(path: &str, merchant: &str) -> Option<(u16, Vec<String>, String)> {
    match path {
        "/gw" => Some((
            200,
            Vec::new(),
            document("GATEWAY", "<p id=\"gwlanded\">the bank page rendered</p>"),
        )),
        "/gw-redirect" => Some((
            303,
            vec![format!("Location: {merchant}/landed")],
            "redirecting".to_string(),
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// Poll `/engine/pages` until `f` accepts the listing, or give up. Polling is
/// not laziness here: the page navigates on its own, `eval` is the one verb
/// that can block behind a busy script, and `pages` is what an agent is told to
/// poll for exactly this reason.
fn settle_pages(mut f: impl FnMut(&[Value]) -> bool) -> (bool, Vec<Value>) {
    let mut last = Vec::new();
    for _ in 0..SETTLE_TRIES {
        let (_, body) = json_reply(dispatch(&request("pages", json!({}))));
        last = body["pages"].as_array().cloned().unwrap_or_default();
        if f(&last) {
            return (true, last);
        }
        std::thread::sleep(SETTLE);
    }
    (false, last)
}

fn url_of(page: &Value) -> &str {
    page["url"].as_str().unwrap_or_default()
}

pub fn run() -> Result<Value> {
    let started = Instant::now();

    // Each origin's pages name the OTHER one, and neither address exists until
    // it is bound — so bind both listeners first, then serve on them.
    let merchant_listener = TcpListener::bind("127.0.0.1:0").context("binding the merchant")?;
    let gateway_listener = TcpListener::bind("127.0.0.1:0").context("binding the gateway")?;
    let merchant_base = format!(
        "http://127.0.0.1:{}",
        merchant_listener.local_addr()?.port()
    );
    let gateway_base = format!("http://127.0.0.1:{}", gateway_listener.local_addr()?.port());

    let gw = gateway_base.clone();
    let (_, merchant_hits) = spawn_origin(
        merchant_listener,
        Arc::new(move |_method, path| match merchant_page(path, &gw) {
            Some(body) => (200u16, Vec::new(), body),
            None => (404u16, Vec::new(), "no such merchant route".to_string()),
        }),
    )?;
    let mb = merchant_base.clone();
    let (_, gateway_hits) = spawn_origin(
        gateway_listener,
        Arc::new(move |_method, path| match gateway_page(path, &mb) {
            Some(reply) => reply,
            None => (404u16, Vec::new(), "no such gateway route".to_string()),
        }),
    )?;

    crate::daemon::journal(
        "engine.gateway.start",
        json!({ "merchant": merchant_base, "gateway": gateway_base }),
    );
    let mut steps: Vec<Step> = Vec::new();
    let mut record = |step: Step| {
        crate::daemon::journal("engine.gateway.step", step.to_json());
        steps.push(step);
    };

    // ---- the hand-off that takes a window of its own ---------------------
    let (status, body) = json_reply(dispatch(&request(
        "open",
        json!({ "url": format!("{merchant_base}/blank-target"), "profile": "engine-gateway-proof" }),
    )));
    let opener = body["page_id"].as_str().unwrap_or_default().to_string();
    record(Step {
        name: "the merchant page loads",
        pass: status == 200 && url_of(&body).ends_with("/blank-target"),
        detail: json!({ "status": status, "url": body["url"] }),
    });

    let known: Vec<String> = json_reply(dispatch(&request("pages", json!({})))).1["pages"]
        .as_array()
        .map(|pages| {
            pages
                .iter()
                .filter_map(|p| p["page_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let (status, clicked) = json_reply(dispatch(&request(
        "input",
        json!({ "page_id": opener, "events": [{ "type": "click", "selector": "#SubmitPayment" }] }),
    )));
    record(Step {
        name: "the submit button takes a trusted click",
        pass: status == 200 && clicked["ok"] == json!(true),
        detail: clicked.clone(),
    });

    // ⛔ THE STEP THIS VERB EXISTS FOR. Before the `create` handler this could
    // not pass: the new-window navigation was discarded, no request left the
    // host, and the listing never grew a row — while the click above still
    // reported ok.
    let gw = gateway_base.clone();
    let seen = known.clone();
    let (found, listing) = settle_pages(move |pages| {
        pages.iter().any(|page| {
            let id = page["page_id"].as_str().unwrap_or_default();
            !seen.iter().any(|k| k == id) && url_of(page).starts_with(&gw)
        })
    });
    let popup = listing
        .iter()
        .find(|page| {
            let id = page["page_id"].as_str().unwrap_or_default();
            !known.iter().any(|k| k == id) && url_of(page).starts_with(&gateway_base)
        })
        .and_then(|page| page["page_id"].as_str())
        .unwrap_or_default()
        .to_string();
    record(Step {
        name: "a target=_blank cross-origin POST becomes a page of its own",
        pass: found && !popup.is_empty(),
        detail: json!({
            "new_page": popup,
            "urls": listing.iter().map(|p| url_of(p)).collect::<Vec<_>>(),
        }),
    });

    // A row in a listing is not a document. Read the bank's own markup out of
    // it, or "the popup exists" could be true of an empty view.
    //
    // ⚠ A popup is LISTED at its provisional url — it appears the moment the
    // navigation is committed, before the document is parsed. So this waits,
    // exactly as an agent must: the listing tells you a page exists, `wait`
    // tells you it is a document.
    let rendered = if popup.is_empty() {
        json!(null)
    } else {
        let _ = dispatch(&request(
            "wait",
            json!({ "page_id": popup, "until": { "load": "finished" }, "timeout_ms": 10000 }),
        ));
        json_reply(dispatch(&request(
            "eval",
            json!({ "page_id": popup, "js": "JSON.stringify({href: location.href, ready: document.readyState, title: document.title, text: (document.getElementById('gwlanded')||{}).textContent || null, bytes: document.documentElement.outerHTML.length})" }),
        )))
        .1["value"]
            .clone()
    };
    let seen: Value = rendered
        .as_str()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(Value::Null);
    let gateway_saw = gateway_hits
        .lock()
        .map(|log| log.clone())
        .unwrap_or_default();
    record(Step {
        name: "the bank's document really rendered in it",
        pass: seen["text"] == json!("the bank page rendered"),
        detail: json!({ "page_id": popup, "seen": seen, "gateway_saw": gateway_saw }),
    });

    let _ = dispatch(&request("close", json!({ "page_id": popup })));
    let _ = dispatch(&request("close", json!({ "page_id": opener })));

    // ---- the dialog that used to wedge the page --------------------------
    let (status, body) = json_reply(dispatch(&request(
        "open",
        json!({ "url": format!("{merchant_base}/alert-on-submit"), "profile": "engine-gateway-proof" }),
    )));
    let dialog_page = body["page_id"].as_str().unwrap_or_default().to_string();
    record(Step {
        name: "the alert-on-submit page loads",
        pass: status == 200,
        detail: json!({ "status": status, "url": body["url"] }),
    });

    let _ = dispatch(&request(
        "input",
        json!({ "page_id": dialog_page, "events": [{ "type": "click", "selector": "#SubmitPayment" }] }),
    ));

    // Unanswered, the `alert()` parks the page's script and the submit never
    // happens — so ARRIVING at the landed url is the whole proof that the
    // dialog was answered. It also proves the listing follows an in-page
    // navigation, which `page.url` alone never did.
    let target = format!("{merchant_base}/landed");
    let wanted = target.clone();
    let id = dialog_page.clone();
    let (landed, listing) = settle_pages(move |pages| {
        pages
            .iter()
            .any(|page| page["page_id"].as_str() == Some(id.as_str()) && url_of(page) == wanted)
    });
    record(Step {
        name: "an alert() on submit is answered, so the hand-off completes",
        pass: landed,
        detail: json!({
            "expected": target,
            "listed": listing.iter().filter(|p| p["page_id"].as_str() == Some(dialog_page.as_str()))
                .map(|p| url_of(p)).collect::<Vec<_>>(),
        }),
    });

    // The page must still ANSWER. Before the dialog handler this is where the
    // run died: `eval` returned "engine call did not answer within 30s", every
    // time, for the life of the page.
    let (status, answered) = json_reply(dispatch(&request(
        "eval",
        json!({ "page_id": dialog_page, "js": "document.getElementById('landed') ? 'yes' : 'no'" }),
    )));
    record(Step {
        name: "the page is still drivable afterwards (eval does not time out)",
        pass: status == 200 && answered["value"] == json!("yes"),
        detail: answered.clone(),
    });

    let _ = dispatch(&request("close", json!({ "page_id": dialog_page })));

    let pass = steps.iter().all(|step| step.pass);
    let report = json!({
        "flow": "gateway-handoff",
        "merchant_saw": merchant_hits.lock().map(|log| log.clone()).unwrap_or_default(),
        "gateway_saw": gateway_hits.lock().map(|log| log.clone()).unwrap_or_default(),
        "pass": pass,
        "merchant": merchant_base,
        "gateway": gateway_base,
        "elapsed_ms": started.elapsed().as_millis(),
        "steps": steps.iter().map(Step::to_json).collect::<Vec<_>>(),
    });
    crate::daemon::journal("engine.gateway.result", report.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixture that could pass without the behaviour it tests is worse than
    // none: it reads as coverage. These are the three properties every step
    // above leans on.
    #[test]
    fn the_fixture_hands_off_to_a_window_of_its_own() {
        let page = merchant_page("/blank-target", "http://gw.invalid").expect("the page exists");
        assert!(
            page.contains(r#"target="_blank""#),
            "without target=_blank this is an ordinary POST, which never broke"
        );
        assert!(
            page.contains("http://gw.invalid/gw"),
            "the action must be the OTHER origin, or nothing is cross-origin"
        );
    }

    #[test]
    fn the_fixture_raises_a_dialog_the_submit_has_to_get_past() {
        let page = merchant_page("/alert-on-submit", "http://gw.invalid").expect("the page exists");
        assert!(
            page.contains("onsubmit=\"alert("),
            "the alert must be ON the submit path, or answering it proves nothing"
        );
    }

    #[test]
    fn the_gateway_answers_both_shapes_and_refuses_the_rest() {
        let (status, headers, _) = gateway_page("/gw-redirect", "http://m.invalid").expect("route");
        assert_eq!(status, 303, "the redirect shape must really redirect");
        assert!(
            headers
                .iter()
                .any(|h| h == "Location: http://m.invalid/landed"),
            "a 303 with no Location proves nothing"
        );
        assert!(gateway_page("/nope", "http://m.invalid").is_none());
    }
}
