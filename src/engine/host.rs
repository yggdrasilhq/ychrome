//! The engine host: one GTK thread, N pages, blocking page verbs.
//!
//! ## The loop story (the thing Phase A had to settle, not hand-wave)
//!
//! A WebKit view needs a running `GMainContext` on the thread that created it,
//! and every call against that view must happen on that thread. ychrome
//! already has a GTK loop — `tao`'s — but it is owned by the BROWSER process's
//! main thread and there can only be one per process. So the engine does not
//! borrow it. The engine runs inside the **daemon** process, which owns no
//! windows and no tao loop, and there it takes a dedicated thread:
//!
//! - the engine thread calls `gtk::init()` and then `gtk::main()`, which
//!   acquires the global default `GMainContext` for that thread;
//! - every page object lives in a `thread_local` on that thread and is never
//!   sent anywhere;
//! - callers on any other thread submit a closure with `glib::idle_add_once`
//!   (which posts to the global default context from any thread) and block on
//!   an `mpsc` reply.
//!
//! That last hop is what makes the verbs look synchronous to a control-plane
//! handler while WebKit's API stays callback-shaped underneath: the closure
//! carries a `Responder` that it can move into WebKit's own callback and fire
//! whenever the answer actually arrives. `goto` returning means the load
//! finished; it does not mean a request was posted.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use cairo::ImageSurface;
use gdk::prelude::*;
use glib::translate::{ToGlibPtr, ToGlibPtrMut};
use gtk::prelude::*;
use javascriptcore::ValueExt;
use serde_json::{Value, json};
use webkit2gtk::{
    CookieManagerExt, LoadEvent, ScriptDialogType, SettingsExt, SnapshotOptions, SnapshotRegion,
    URIRequestExt, WebView, WebViewExt, WebsiteDataManagerExt,
};

use super::substrate::{self, HeadlessDisplay, Probe, Substrate};

/// One logical page's engine resources. Lives only on the engine thread.
struct Page {
    window: gtk::Window,
    view: WebView,
    profile: String,
}

thread_local! {
    /// The page table. `thread_local` rather than a mutex because these are
    /// GTK objects: they are not `Send`, and the ONLY thread that may touch
    /// them is the one running `gtk::main()`. Every closure below arrives via
    /// `idle_add_once`, so it is already on that thread by construction.
    static PAGES: RefCell<HashMap<String, Page>> = RefCell::new(HashMap::new());
}

/// A one-shot reply channel handed to a job on the engine thread. The job may
/// fire it immediately, or move it into a WebKit callback and fire it later.
pub struct Responder<T> {
    tx: Sender<Result<T, String>>,
}

impl<T> Responder<T> {
    pub fn ok(self, value: T) {
        let _ = self.tx.send(Ok(value));
    }

    pub fn fail(self, message: impl Into<String>) {
        let _ = self.tx.send(Err(message.into()));
    }
}

/// A `Responder` that two signal handlers can race for. Whoever gets there
/// first wins; the loser finds `None` and does nothing. Both `load-changed`
/// and `load-failed` can fire for one navigation.
type Shared<T> = Rc<RefCell<Option<Responder<T>>>>;

/// Submit a job to the engine thread and block for its reply.
fn on_engine<T, F>(timeout: Duration, job: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(Responder<T>) + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    glib::idle_add_once(move || job(Responder { tx }));
    match rx.recv_timeout(timeout) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => bail!("{error}"),
        Err(_) => bail!("engine call did not answer within {timeout:?}"),
    }
}

/// Run `f` against a page, or fail with a uniform not-found message. Always
/// called from inside a job, i.e. already on the engine thread.
fn with_page<T>(id: &str, responder: Responder<T>, f: impl FnOnce(&Page, Responder<T>)) {
    let view = PAGES.with(|pages| {
        pages.borrow().get(id).map(|page| Page {
            window: page.window.clone(),
            view: page.view.clone(),
            profile: page.profile.clone(),
        })
    });
    match view {
        Some(page) => f(&page, responder),
        None => responder.fail(super::pool::no_such_page(id).to_string()),
    }
}

/// One cookie to place into a profile's live store, already parsed and
/// domain-filtered by the router. Values never appear in any reply.
pub struct CookieSpec {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// Seconds until expiry, or -1 for a session cookie.
    pub max_age: i32,
}

/// Which region of the page a snapshot covers.
///
/// ⭐ **WebKitGTK answers BOTH natively**, and that is the finding that decides
/// the whole capture design. `WebKitSnapshotRegion` has a `FULL_DOCUMENT`
/// member, so a full-page capture is ONE engine call that renders the document
/// at its laid-out size — not a scroll-and-stitch. The stitch was the obvious
/// fallback and it is strictly worse: it seams at every step, it duplicates
/// every `position: fixed` header once per tile, and it leaves the page
/// scrolled somewhere the caller did not put it.
///
/// What full-document does NOT fix is content that has never been laid out
/// because it has never been near the viewport. That is a lazy-load problem,
/// not a snapshot problem, and `/engine/shot`'s `prescroll` is its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShotRegion {
    /// What is on screen: the view's own width x height.
    Visible,
    /// The whole scrollable document, however far below the fold it runs.
    FullDocument,
}

impl ShotRegion {
    fn webkit(self) -> SnapshotRegion {
        match self {
            ShotRegion::Visible => SnapshotRegion::Visible,
            ShotRegion::FullDocument => SnapshotRegion::FullDocument,
        }
    }

    /// The name this region answers to on the wire, and in the capture's
    /// metadata. One spelling, read by the router and echoed to the caller.
    pub fn id(self) -> &'static str {
        match self {
            ShotRegion::Visible => "viewport",
            ShotRegion::FullDocument => "full",
        }
    }
}

/// A crop, in DEVICE pixels of the snapshot surface it applies to.
///
/// Device pixels, not CSS pixels, because that is the only space in which a
/// crop is unambiguous: the caller speaks CSS and the conversion happens once,
/// against the snapshot's MEASURED size, in `api::device_rect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// A viewport readback: the PNG bytes AND the raw pixels behind them.
///
/// Both, deliberately. The PNG is the artifact a human looks at; the raw
/// buffer is what a pixel assertion reads, because re-decoding our own PNG to
/// check it would only prove the encoder round-trips. Cairo hands us
/// premultiplied BGRA (`ARGB32`, little-endian).
pub struct Shot {
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub png: Vec<u8>,
    pub bgra: Vec<u8>,
}

impl Shot {
    /// Cut a device-pixel rect out of this shot, re-encoding the PNG from the
    /// pixels already in hand.
    ///
    /// No second engine round trip, and that is the point: an element capture
    /// and a rect capture are the SAME full-document snapshot with a different
    /// window onto it, so they cannot disagree about what the page looked like.
    /// Two snapshots taken a scroll apart could, and on an animating page they
    /// would.
    ///
    /// `rect` must already be clamped inside `width x height` — `device_rect`
    /// is the one place that clamping happens, and a rect that survived it is
    /// non-empty by construction. This refuses rather than panics if it did
    /// not, because a silently empty crop is a blank PNG that looks like a
    /// rendering bug.
    pub fn crop(&self, rect: PixelRect) -> Result<Shot> {
        if rect.w <= 0 || rect.h <= 0 {
            bail!("crop {rect:?} is empty");
        }
        if rect.x < 0 || rect.y < 0 || rect.x + rect.w > self.width || rect.y + rect.h > self.height
        {
            bail!(
                "crop {rect:?} falls outside the {}x{} snapshot",
                self.width,
                self.height
            );
        }
        let stride = cairo::Format::ARgb32
            .stride_for_width(rect.w as u32)
            .map_err(|error| anyhow::anyhow!("cairo stride for {}: {error}", rect.w))?;
        let mut out = vec![0u8; (stride * rect.h) as usize];
        for row in 0..rect.h {
            let src = ((rect.y + row) * self.stride + rect.x * 4) as usize;
            let dst = (row * stride) as usize;
            let bytes = (rect.w * 4) as usize;
            let Some(slice) = self.bgra.get(src..src + bytes) else {
                bail!("crop {rect:?} ran off the end of the pixel buffer");
            };
            out[dst..dst + bytes].copy_from_slice(slice);
        }
        let surface = ImageSurface::create_for_data(
            out.clone(),
            cairo::Format::ARgb32,
            rect.w,
            rect.h,
            stride,
        )
        .map_err(|error| anyhow::anyhow!("cairo surface for crop: {error}"))?;
        let mut png = Vec::new();
        surface
            .write_to_png(&mut png)
            .map_err(|error| anyhow::anyhow!("PNG encode failed: {error}"))?;
        Ok(Shot {
            width: rect.w,
            height: rect.h,
            stride,
            png,
            bgra: out,
        })
    }

    /// Count pixels darker than `threshold` luminance inside a rect. The
    /// engine's answer to "did those words actually paint" — a blank canvas
    /// scores zero no matter what the DOM claims.
    pub fn dark_pixels(&self, x: i32, y: i32, w: i32, h: i32, threshold: u32) -> u32 {
        let mut count = 0;
        for row in y.max(0)..(y + h).min(self.height) {
            for col in x.max(0)..(x + w).min(self.width) {
                let offset = (row * self.stride + col * 4) as usize;
                let Some(pixel) = self.bgra.get(offset..offset + 4) else {
                    continue;
                };
                // Rec. 601 luma on the premultiplied BGRA byte order.
                let luma =
                    (pixel[2] as u32 * 299 + pixel[1] as u32 * 587 + pixel[0] as u32 * 114) / 1000;
                if luma < threshold {
                    count += 1;
                }
            }
        }
        count
    }
}

/// The engine. Owns the substrate's display and the GTK thread; dropping it
/// stops the loop and tears the display down.
pub struct Engine {
    substrate: Substrate,
    probes: Vec<Probe>,
    display: Option<HeadlessDisplay>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    /// Select a substrate, bring up its display, and start the engine thread.
    /// Returns only once GTK is initialised and the loop is accepting jobs.
    pub fn start(width: i32, height: i32) -> Result<Engine> {
        let (substrate, probes) = substrate::select()?;
        if substrate != Substrate::WebKitGtkHeadless {
            bail!(
                "substrate {} is selected but only {} is implemented — \
                 add its driver in engine::host before selecting it",
                substrate.id(),
                Substrate::WebKitGtkHeadless.id()
            );
        }

        let display = HeadlessDisplay::start(width, height)?;
        display.install_env();

        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("ychrome-engine".into())
            .spawn(move || {
                if let Err(error) = gtk::init() {
                    let _ = ready_tx.send(Err(format!("gtk::init failed: {error}")));
                    return;
                }
                // Announce readiness from INSIDE the loop, not before entering
                // it: a caller that starts submitting jobs while the context is
                // unowned gets them queued but never run.
                glib::idle_add_once(move || {
                    let _ = ready_tx.send(Ok(()));
                });
                gtk::main();
            })
            .context("spawning the engine thread")?;

        match ready_rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => bail!("{error}"),
            Err(_) => bail!("the engine thread did not reach its main loop within 20s"),
        }

        Ok(Engine {
            substrate,
            probes,
            display: Some(display),
            thread: Some(thread),
        })
    }

    pub fn substrate(&self) -> Substrate {
        self.substrate
    }

    pub fn probes(&self) -> &[Probe] {
        &self.probes
    }

    pub fn display_name(&self) -> String {
        self.display
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_default()
    }

    /// The headless display's screen size. Worth reporting next to a
    /// snapshot's size: a readback smaller than the screen is the squish class
    /// of bug, and the two numbers side by side is how you see it.
    pub fn display_size(&self) -> (i32, i32) {
        self.display
            .as_ref()
            .map(|d| (d.width, d.height))
            .unwrap_or((0, 0))
    }

    /// Create a page: a mapped window of the requested size with one WebView
    /// filling it.
    ///
    /// The window is mapped rather than offscreen because WebKit only paints a
    /// view that is realised and visible, and a snapshot of an unpainted view
    /// is the blank-canvas lie this engine exists to end. "Headless" here means
    /// the DISPLAY has no screen anyone can see, not that the view is unmapped.
    pub fn open(&self, id: &str, width: i32, height: i32, profile: &str) -> Result<()> {
        let id = id.to_string();
        let profile = profile.to_string();
        on_engine(Duration::from_secs(240), move |responder| {
            let exists = PAGES.with(|pages| pages.borrow().contains_key(&id));
            if exists {
                responder.fail(format!("page {id:?} is already open"));
                return;
            }
            match build_page(width, height, &profile, None) {
                Ok(page) => {
                    PAGES.with(|pages| pages.borrow_mut().insert(id, page));
                    responder.ok(())
                }
                Err(error) => responder.fail(error.to_string()),
            }
        })
    }

    /// Every open page's url and title, read from the VIEW rather than from
    /// anything the caller once asked for.
    ///
    /// One round trip for the whole table: a listing verb must not cost a hop
    /// per page. Never an `eval` — a page whose script is busy would make a
    /// listing hang, and the listing is exactly what an agent reaches for when
    /// a page is misbehaving.
    pub fn live_locations(&self) -> Result<HashMap<String, (String, String)>> {
        on_engine(Duration::from_secs(10), |responder| {
            let map = PAGES.with(|pages| {
                pages
                    .borrow()
                    .iter()
                    .map(|(id, page)| {
                        (
                            id.clone(),
                            (
                                page.view.uri().map(Into::into).unwrap_or_default(),
                                page.view.title().map(Into::into).unwrap_or_default(),
                            ),
                        )
                    })
                    .collect::<HashMap<String, (String, String)>>()
            });
            responder.ok(map)
        })
    }

    /// Apply the per-site zoom this host has recorded for a URL's host.
    ///
    /// Per SITE, so it belongs to navigation rather than to page creation:
    /// `webzoom` is the owner and the engine only asks it. A host with no
    /// recorded zoom gets 1.0 rather than whatever the previous page used.
    pub fn apply_zoom(&self, id: &str, url: &str) -> Result<f64> {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string));
        let sites = crate::webzoom::sites();
        let percent = host
            .as_deref()
            .and_then(|host| crate::webzoom::zoom_for_host(&sites, host))
            .unwrap_or(100.0);
        let level = percent / 100.0;
        let id = id.to_string();
        on_engine(Duration::from_secs(10), move |responder| {
            with_page(&id, responder, move |page, responder| {
                page.view.set_zoom_level(level);
                responder.ok(level)
            })
        })
    }

    /// Apply the browser identity this host has recorded for a URL's host.
    ///
    /// Per SITE, like [`Engine::apply_zoom`], and for the same reason: the
    /// override belongs to the site, not to the page object. Unlike zoom it must
    /// run BEFORE the navigation — the UA is a request header, so applying it
    /// afterwards would identify the browser correctly only from the SECOND load
    /// onwards, which is the load a bot check has already scored.
    ///
    /// A host with no override falls back to the profile's identity, and with no
    /// global preset either that is `None` — WebKitGTK's own UA, which is the
    /// coherent one. Setting `None` explicitly matters: without it a page would
    /// keep whatever the previous navigation set, so one visit to an
    /// override-marked site would leak that identity onto every site after it.
    pub fn apply_identity(&self, id: &str, url: &str) -> Result<Option<String>> {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string));
        let agent = match host.as_deref() {
            Some(host) => crate::useragent::effective_for_host(host),
            None => crate::useragent::effective(),
        };
        let id = id.to_string();
        let applied = agent.clone();
        on_engine(Duration::from_secs(10), move |responder| {
            let agent = applied.clone();
            with_page(&id, responder, move |page, responder| {
                let settings: webkit2gtk::Settings =
                    WebViewExt::settings(&page.view).unwrap_or_default();
                settings.set_user_agent(agent.as_deref());
                WebViewExt::set_settings(&page.view, &settings);
                responder.ok(agent)
            })
        })
    }

    /// Journal what the MAIN FRAME's last load actually returned: the committed
    /// url, the HTTP status, and the Cloudflare headers that name a bot check.
    ///
    /// ⭐ This exists because a challenge loop, a blocked asset and a jar that
    /// does not persist all LOOK IDENTICAL from outside — the page comes back,
    /// it is not what you asked for, and nothing anywhere says why. `cf-mitigated`
    /// is the header Cloudflare sets when it served a challenge instead of the
    /// origin's response, so a run of loads carrying it IS the loop, stated.
    ///
    /// Best-effort by construction: `main_resource` is `None` before the first
    /// commit and a data: URL has no response. A missing field is reported
    /// absent, never guessed.
    pub fn trace_main_frame(&self, id: &str) -> Result<Value> {
        let id = id.to_string();
        on_engine(Duration::from_secs(10), move |responder| {
            with_page(&id, responder, move |page, responder| {
                responder.ok(main_frame_trace(&page.view))
            })
        })
    }

    /// Point a profile's network at a SOCKS endpoint (or back to direct).
    pub fn set_egress(&self, profile: &str, socks: Option<String>) -> Result<()> {
        let profile = profile.to_string();
        on_engine(
            Duration::from_secs(240),
            move |responder| match super::identity::for_profile(&profile) {
                Ok(identity) => {
                    super::identity::set_egress(&identity, socks.as_deref());
                    responder.ok(())
                }
                Err(error) => responder.fail(error.to_string()),
            },
        )
    }

    /// What identity a profile actually resolved to — the jar, the UA, how many
    /// userscripts attached, and whether the content filter loaded or compiled.
    pub fn identity(&self, profile: &str) -> Result<Value> {
        let profile = profile.to_string();
        on_engine(
            Duration::from_secs(240),
            move |responder| match super::identity::for_profile(&profile) {
                Ok(identity) => responder.ok(identity.applied),
                Err(error) => responder.fail(error.to_string()),
            },
        )
    }

    /// Which profile a page was opened on.
    pub fn page_profile(&self, id: &str) -> Result<String> {
        let id = id.to_string();
        on_engine(Duration::from_secs(10), move |responder| {
            with_page(&id, responder, move |page, responder| {
                responder.ok(page.profile.clone())
            })
        })
    }

    pub fn close(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        on_engine(Duration::from_secs(20), move |responder| {
            let removed = PAGES.with(|pages| pages.borrow_mut().remove(&id));
            match removed {
                Some(page) => {
                    unsafe { page.window.destroy() };
                    responder.ok(())
                }
                None => responder.fail(super::pool::no_such_page(&id).to_string()),
            }
        })
    }

    pub fn page_ids(&self) -> Result<Vec<String>> {
        on_engine(Duration::from_secs(10), |responder| {
            let mut ids = PAGES.with(|pages| pages.borrow().keys().cloned().collect::<Vec<_>>());
            ids.sort();
            responder.ok(ids)
        })
    }

    /// Navigate and wait for the load to FINISH. Returns the committed URL.
    pub fn goto(&self, id: &str, url: &str, timeout: Duration) -> Result<String> {
        let id = id.to_string();
        let url = url.to_string();
        on_engine(timeout, move |responder| {
            with_page(&id, responder, move |page, responder| {
                arm_load_wait(&page.view, responder);
                page.view.load_uri(&url);
            })
        })
    }

    /// Put cookies into a page's PROFILE store and answer with the store's own
    /// readback for `origin` — the observation, never the adds' success flags.
    ///
    /// This is the missing half of the jar being shared Netscape text on disk:
    /// the network process reads the jar once at startup, so handing a session
    /// from curl to the engine needs a LIVE-store write, not a file edit
    /// (probed 2026-08-06: an appended jar line was invisible to a fresh page,
    /// which stopped an otherwise-staged government filing).
    pub fn cookie_import(
        &self,
        id: &str,
        cookies_in: Vec<CookieSpec>,
        origin: String,
    ) -> Result<Value> {
        let id = id.to_string();
        on_engine(Duration::from_secs(30), move |responder| {
            with_page(&id, responder, move |page, responder| {
                let identity = match super::identity::for_profile(&page.profile) {
                    Ok(identity) => identity,
                    Err(error) => return responder.fail(error.to_string()),
                };
                let Some(manager) = identity.data.cookie_manager() else {
                    return responder.fail("profile has no cookie manager");
                };
                let total = cookies_in.len();
                let pending = Rc::new(Cell::new(total));
                let add_failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
                let slot: Shared<Value> = Rc::new(RefCell::new(Some(responder)));
                // The finish step runs ONCE, after the last add lands: read the
                // store back for the origin and answer with what a request
                // would actually carry. Same law as every injector on this
                // plane — the verb's own success field is an assumption.
                let finish: Rc<dyn Fn()> = {
                    let manager = manager.clone();
                    let origin = origin.clone();
                    let add_failures = add_failures.clone();
                    let slot = slot.clone();
                    Rc::new(move || {
                        let add_failures = add_failures.clone();
                        let slot = slot.clone();
                        let total = total;
                        manager.cookies(
                            &origin,
                            None::<&gio::Cancellable>,
                            move |result: std::result::Result<Vec<soup::Cookie>, glib::Error>| {
                            let Some(responder) = slot.borrow_mut().take() else {
                                return;
                            };
                            match result {
                                Ok(list) => {
                                    let visible: Vec<String> = list
                                        .into_iter()
                                        .map(|mut cookie| {
                                            cookie
                                                .name()
                                                .map(|name| name.to_string())
                                                .unwrap_or_default()
                                        })
                                        .collect();
                                    responder.ok(json!({
                                        "ok": true,
                                        "imported": total,
                                        "add_failures": add_failures.borrow().clone(),
                                        "visible_for_origin": visible,
                                    }));
                                }
                                Err(error) => {
                                    responder.fail(format!("cookie readback failed: {error}"))
                                }
                            }
                        });
                    })
                };
                if total == 0 {
                    finish();
                    return;
                }
                for spec in cookies_in {
                    let mut cookie = soup::Cookie::new(
                        &spec.name,
                        &spec.value,
                        &spec.domain,
                        &spec.path,
                        spec.max_age,
                    );
                    cookie.set_secure(spec.secure);
                    cookie.set_http_only(spec.http_only);
                    let pending = pending.clone();
                    let add_failures = add_failures.clone();
                    let finish = finish.clone();
                    let name = spec.name.clone();
                    manager.add_cookie(&mut cookie, None::<&gio::Cancellable>, move |result| {
                        if let Err(error) = result {
                            add_failures.borrow_mut().push(format!("{name}: {error}"));
                        }
                        pending.set(pending.get() - 1);
                        if pending.get() == 0 {
                            finish();
                        }
                    });
                }
            })
        })
    }

    /// Load literal HTML under `base_uri` and wait for the load to finish. The
    /// gate's trusted-input page uses this so the differential needs no
    /// network and no fixture server.
    pub fn load_html(&self, id: &str, html: &str, base_uri: &str, timeout: Duration) -> Result<()> {
        let id = id.to_string();
        let html = html.to_string();
        let base_uri = base_uri.to_string();
        on_engine(timeout, move |responder| {
            with_page(&id, responder, move |page, responder| {
                arm_load_wait_unit(&page.view, responder);
                page.view.load_html(&html, Some(&base_uri));
            })
        })
    }

    /// Evaluate JS and return the result as JSON.
    ///
    /// JSON, not a display string: `JSCValue::to_json` is the engine's own
    /// serialiser, so `/eval` hands back a typed value instead of a string a
    /// caller has to guess the shape of.
    pub fn eval(&self, id: &str, js: &str) -> Result<Value> {
        let id = id.to_string();
        let js = js.to_string();
        let raw = on_engine(Duration::from_secs(30), move |responder| {
            with_page(&id, responder, move |page, responder| {
                page.view.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    move |result| match result {
                        Ok(value) => match value.to_json(0) {
                            Some(json) => responder.ok(json.to_string()),
                            // `undefined` has no JSON form; say so rather than
                            // inventing null, which a caller could not tell
                            // apart from a real null.
                            None => responder.ok("undefined".to_string()),
                        },
                        Err(error) => responder.fail(error.to_string()),
                    },
                );
            })
        })?;
        if raw == "undefined" {
            return Ok(Value::Null);
        }
        serde_json::from_str(&raw).with_context(|| format!("engine eval returned non-JSON: {raw}"))
    }

    /// Snapshot the visible viewport. The default capture, and the one every
    /// caller before regions existed was asking for.
    pub fn shot(&self, id: &str) -> Result<Shot> {
        self.shot_region(id, ShotRegion::Visible)
    }

    /// Snapshot one region of a page.
    ///
    /// The budget scales with the region because they are not the same job: a
    /// viewport readback is a compositor blit, while a full-document snapshot
    /// re-renders a document that may be twenty screens tall. 30 s was tuned
    /// for the former and a long page hits it honestly.
    pub fn shot_region(&self, id: &str, region: ShotRegion) -> Result<Shot> {
        let id = id.to_string();
        let budget = match region {
            ShotRegion::Visible => Duration::from_secs(30),
            ShotRegion::FullDocument => Duration::from_secs(120),
        };
        on_engine(budget, move |responder| {
            with_page(&id, responder, move |page, responder| {
                page.view.snapshot(
                    region.webkit(),
                    SnapshotOptions::NONE,
                    None::<&gio::Cancellable>,
                    move |result| {
                        let surface = match result {
                            Ok(surface) => surface,
                            Err(error) => return responder.fail(error.to_string()),
                        };
                        surface.flush();
                        let mut image = match ImageSurface::try_from(surface) {
                            Ok(image) => image,
                            Err(_) => {
                                return responder.fail("snapshot was not an image surface");
                            }
                        };
                        let (width, height, stride) =
                            (image.width(), image.height(), image.stride());
                        let mut png = Vec::new();
                        if let Err(error) = image.write_to_png(&mut png) {
                            return responder.fail(format!("PNG encode failed: {error}"));
                        }
                        let bgra = match image.data() {
                            Ok(data) => data.to_vec(),
                            Err(error) => {
                                return responder.fail(format!("pixel borrow failed: {error}"));
                            }
                        };
                        responder.ok(Shot {
                            width,
                            height,
                            stride,
                            png,
                            bgra,
                        })
                    },
                );
            })
        })
    }

    /// Dispatch a REAL pointer click at viewport coordinates.
    ///
    /// The events go in as `GdkEvent`s through `gtk_main_do_event`, which is
    /// the same path a physical mouse takes: WebKitGTK builds its
    /// `NativeWebMouseEvent` from the GdkEvent, so the page sees
    /// `isTrusted === true`, focus moves, and default actions fire. A
    /// `dispatchEvent` from injected JS cannot do any of that — that
    /// difference is Phase A's fifth proof and the reason the engine exists.
    pub fn click_trusted(&self, id: &str, x: f64, y: f64) -> Result<u32> {
        let id = id.to_string();
        on_engine(Duration::from_secs(20), move |responder| {
            with_page(
                &id,
                responder,
                move |page, responder| match dispatch_click(&page.view, x, y) {
                    Ok(count) => responder.ok(count),
                    Err(error) => responder.fail(error.to_string()),
                },
            )
        })
    }

    /// Dispatch a batch of REAL input events.
    ///
    /// Every variant below goes in as a `GdkEvent` through `gtk_main_do_event`,
    /// exactly like [`click_trusted`](Self::click_trusted). None of them is a
    /// `dispatchEvent`: an engine that reached for synthetic DOM events for the
    /// "easy" verbs would reintroduce, one verb at a time, precisely the
    /// instrument-lying that gate proof 5 exists to close. `isTrusted` is true
    /// for all of them, hover really hovers, and a key press produces text.
    pub fn input(&self, id: &str, events: Vec<InputEvent>) -> Result<u32> {
        // Text becomes individual key events HERE, so every event below is one
        // job, and the engine loop gets to spin between them.
        let mut expanded = Vec::new();
        for event in events {
            match event {
                InputEvent::Text { text } => {
                    for ch in text.chars() {
                        // SAFETY: a pure value conversion in gdk, no pointers
                        // and no display involved.
                        let keyval = unsafe { gdk::ffi::gdk_unicode_to_keyval(ch as u32) };
                        expanded.push(InputEvent::Key { keyval, mods: 0 });
                    }
                }
                other => expanded.push(other),
            }
        }

        let mut dispatched = 0;
        for event in expanded {
            dispatched += self.input_one(id, event)?;
            // The engine loop MUST run between events.
            //
            // WebKitGTK hands a key event to the web process and waits to hear
            // whether the page consumed it before it will take the next one.
            // Dispatching a whole batch inside one job never lets that reply
            // arrive, so the queue collapses: measured, typing "ada lovelace"
            // dispatched all 24 events and landed exactly ONE character. This
            // is a settle, not a sleep on the engine thread — other pages keep
            // loading through it.
            self.settle(INPUT_SETTLE)?;
        }
        // Drain whatever the last event left pending, then give the web process
        // one more slice to apply it, so a read issued straight after this call
        // sees the effect rather than racing it.
        self.drain()?;
        self.settle(INPUT_FLUSH)?;
        Ok(dispatched)
    }

    /// Run the engine loop until it has nothing pending.
    fn drain(&self) -> Result<()> {
        on_engine(Duration::from_secs(20), move |responder| {
            let context = glib::MainContext::default();
            let mut spins = 0;
            while context.pending() && spins < 10_000 {
                context.iteration(false);
                spins += 1;
            }
            responder.ok(())
        })
    }

    /// One input event, one job on the engine thread.
    fn input_one(&self, id: &str, event: InputEvent) -> Result<u32> {
        let id = id.to_string();
        on_engine(Duration::from_secs(20), move |responder| {
            with_page(&id, responder, move |page, responder| match dispatch_input(
                &page.view, &event,
            ) {
                Ok(count) => responder.ok(count),
                Err(error) => responder.fail(error.to_string()),
            })
        })
    }

    /// History and reload (`/engine/nav`). `Stop` returns at once; the others
    /// wait for the load they start, so a caller that gets a reply has a page
    /// that has actually settled.
    pub fn nav(&self, id: &str, action: NavAction, timeout: Duration) -> Result<String> {
        let id = id.to_string();
        on_engine(timeout, move |responder| {
            with_page(&id, responder, move |page, responder| {
                let view = &page.view;
                match action {
                    NavAction::Back if !view.can_go_back() => {
                        responder.fail("cannot go back: no earlier entry in this page's history")
                    }
                    NavAction::Forward if !view.can_go_forward() => {
                        responder.fail("cannot go forward: no later entry in this page's history")
                    }
                    NavAction::Stop => {
                        view.stop_loading();
                        responder.ok(view.uri().map(|uri| uri.to_string()).unwrap_or_default())
                    }
                    NavAction::Back => {
                        arm_load_wait(view, responder);
                        view.go_back();
                    }
                    NavAction::Forward => {
                        arm_load_wait(view, responder);
                        view.go_forward();
                    }
                    NavAction::Reload => {
                        arm_load_wait(view, responder);
                        view.reload();
                    }
                }
            })
        })
    }

    /// Is WebKit still loading this page? The signal behind `/engine/wait`'s
    /// `load` form, read as a property so a poll can ask cheaply.
    pub fn is_loading(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        on_engine(Duration::from_secs(10), move |responder| {
            with_page(&id, responder, move |page, responder| {
                responder.ok(page.view.is_loading())
            })
        })
    }

    /// Wait for the CURRENT navigation to finish, without starting one.
    pub fn wait_load(&self, id: &str, timeout: Duration) -> Result<String> {
        let id = id.to_string();
        on_engine(timeout, move |responder| {
            with_page(&id, responder, move |page, responder| {
                if !page.view.is_loading() {
                    return responder.ok(page
                        .view
                        .uri()
                        .map(|uri| uri.to_string())
                        .unwrap_or_default());
                }
                arm_load_wait(&page.view, responder);
            })
        })
    }

    /// Let the engine thread run for a while without blocking a verb on it —
    /// used after input, so the page's handlers and any resulting layout land
    /// before the next read.
    pub fn settle(&self, duration: Duration) -> Result<()> {
        on_engine(duration + Duration::from_secs(10), move |responder| {
            glib::timeout_add_once(duration, move || responder.ok(()));
        })
    }
}

/// How long the engine loop is given to deliver one input event to the web
/// process before the next is dispatched. See [`Engine::input`].
const INPUT_SETTLE: Duration = Duration::from_millis(12);

/// An extra barrier after the LAST event of a batch.
///
/// ⚠ **This is a heuristic, and the race it mitigates is real.** WebKitGTK
/// queues key events in the UI process and only sends the next one after the
/// previous is acknowledged, while `evaluate_javascript` is sent immediately —
/// so a read issued right after a `type` can overtake the final keystroke and
/// observe the field one character short. Measured before this barrier existed:
/// `"ada lovelace"` came back as `"ada lovelac"` on roughly two runs in three.
///
/// There is no public API for "the key queue has drained", so this spins the
/// loop until it has nothing pending and then waits once more. It has held over
/// repeated runs, but a caller that must be certain should `/engine/wait` on the
/// state it expects rather than trusting this alone — which is what `wait`
/// exists for.
const INPUT_FLUSH: Duration = Duration::from_millis(60);

/// What `/engine/nav` can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    Back,
    Forward,
    Reload,
    Stop,
}

impl NavAction {
    pub fn parse(name: &str) -> Option<NavAction> {
        match name {
            "back" => Some(NavAction::Back),
            "forward" => Some(NavAction::Forward),
            "reload" => Some(NavAction::Reload),
            "stop" => Some(NavAction::Stop),
            _ => None,
        }
    }
}

/// One input event, already validated. `/engine/input`'s JSON is parsed into
/// these in `api`, so this layer never sees a half-checked event.
#[derive(Debug, Clone)]
pub enum InputEvent {
    Click {
        x: f64,
        y: f64,
        button: u32,
        count: u32,
    },
    Move {
        x: f64,
        y: f64,
    },
    Scroll {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    Key {
        keyval: u32,
        mods: u32,
    },
    Text {
        text: String,
    },
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Quit from INSIDE the loop; `gtk::main_quit` from another thread is
        // not sound. Once the loop returns, the thread drops its pages (GTK
        // objects, on their own thread) and exits, and only then is it safe to
        // kill the display out from under them.
        glib::idle_add_once(|| {
            PAGES.with(|pages| pages.borrow_mut().clear());
            gtk::main_quit();
        });
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        drop(self.display.take());
    }
}

/// The headers worth naming in a load trace. Deliberately a SHORT list of
/// non-secret response headers: `cf-mitigated` says a bot check answered instead
/// of the origin, `cf-ray` is the id Cloudflare's own support asks for, `server`
/// says who answered at all, and `content-type` separates "a challenge page" from
/// "the JSON your app expected". No `set-cookie`, ever — a clearance cookie is a
/// credential and a journal is not the place for one.
const TRACED_HEADERS: [&str; 4] = ["cf-mitigated", "cf-ray", "server", "content-type"];

/// Read the main frame's last response. See [`Engine::trace_main_frame`].
fn main_frame_trace(view: &WebView) -> Value {
    use webkit2gtk::{URIResponseExt, WebResourceExt};

    let uri = view.uri().map(|uri| uri.to_string());
    let Some(response) = view
        .main_resource()
        .and_then(|resource| resource.response())
    else {
        // Honest absence. Before the first commit there IS no response, and a
        // zero status would read as a measurement.
        return json!({ "url": uri, "response": Value::Null });
    };
    let status = response.status_code();
    let mut headers = serde_json::Map::new();
    if let Some(raw) = response.http_headers() {
        for name in TRACED_HEADERS {
            if let Some(value) = raw.one(name) {
                headers.insert(name.to_string(), json!(value.to_string()));
            }
        }
    }
    json!({
        "url": uri,
        "response": {
            "status": status,
            "headers": headers,
            // The named verdict, so a caller does not have to know Cloudflare's
            // header vocabulary to see the loop. `cf-mitigated: challenge` is
            // set on exactly the responses where a bot check replaced the page.
            "bot_check": headers.get("cf-mitigated").is_some()
                || matches!(status, 403 | 503) && headers.get("cf-ray").is_some(),
        },
    })
}

/// The longest a dialog message may be in the journal. A page's own text, not a
/// secret, but a journal line is for reading.
const DIALOG_MESSAGE_MAX: usize = 200;

/// Build one page's engine resources under a profile's identity, armed and
/// mapped. The ONE place a `WebView` is born, so a view the PAGE asked for
/// cannot come out a different browser than a view an agent asked for.
///
/// The window is mapped rather than offscreen because WebKit only paints a view
/// that is realised and visible, and a snapshot of an unpainted view is the
/// blank-canvas lie this engine exists to end. "Headless" here means the DISPLAY
/// has no screen anyone can see, not that the view is unmapped.
///
/// `opener` is `Some` only for a window the page itself asked for. WebKit
/// requires the child of a `create` to be built *with the related view* — that
/// relation is what gives the popup its opener, its `window.name` and the same
/// web process, and a view built without it cannot be returned from the signal.
fn build_page(width: i32, height: i32, profile: &str, opener: Option<&WebView>) -> Result<Page> {
    // The profile's identity — jar, adblock filter, userscripts, UA — from its
    // owners, built once per profile and reused. This is what makes the engine
    // and the visible surface the SAME browser.
    let identity = super::identity::for_profile(profile)?;
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_default_size(width, height);
    // `related-view` and `web-context` are mutually exclusive construct
    // properties — WebKit takes the context FROM the related view and warns at
    // CRITICAL level if both are passed. They would name the same context here
    // (a popup keeps its opener's profile), so setting both would be a warning
    // for no gain.
    let view = match opener {
        Some(opener) => WebView::builder()
            .related_view(opener)
            .user_content_manager(&identity.content)
            .build(),
        None => WebView::builder()
            .web_context(&identity.context)
            .user_content_manager(&identity.content)
            .build(),
    };
    if let Some(agent) = &identity.user_agent {
        let settings: webkit2gtk::Settings = WebViewExt::settings(&view).unwrap_or_default();
        settings.set_user_agent(Some(agent.as_str()));
        WebViewExt::set_settings(&view, &settings);
    }
    window.add(&view);
    match opener {
        None => window.show_all(),
        // ⛔ A popup must NOT be shown before `create` returns. Realising the
        // view runs WebKit's page-proxy setup, and doing that while WebKit is
        // still inside `createNewPage` loses the navigation it was handing us:
        // measured, the view came back listed at the gateway's url with
        // `location.href === "about:blank"` and a load that never finished —
        // the exact symptom this whole fix started from, reproduced one layer
        // in. Showing it on the next main-loop turn is after `create` has
        // returned, by construction, since we are inside a main-loop callback.
        Some(_) => {
            let window = window.clone();
            glib::idle_add_local_once(move || window.show_all());
        }
    }
    arm_new_window(&view, profile);
    arm_script_dialogs(&view);
    Ok(Page {
        window,
        view,
        profile: profile.to_string(),
    })
}

/// Answer WebKit when a PAGE asks for a new window, by making it a page.
///
/// ⛔ **Without this the hand-off is dropped and nothing says so.** A view with
/// no `create` handler answers `window.open` with `null` and silently discards a
/// `target="_blank"` submit: measured on a two-origin fixture, **not one byte
/// left the host** while `/engine/input` reported `{"dispatched":3,"ok":true}`
/// and the page sat where it was. That is the shape a bank-payment gateway
/// takes — the merchant form targets a popup — so an agent driving a payment saw
/// a successful click and a page that never moved.
///
/// A popup is therefore a PAGE, with its own id, listed by `/engine/pages` and
/// drivable by every verb. Collapsing it into the opener instead would be a
/// second lie: the page asked for two documents and would get one.
fn arm_new_window(view: &WebView, profile: &str) {
    let profile = profile.to_string();
    view.connect_create(move |opener, action| {
        let requested = action
            .request()
            .and_then(|request| request.uri())
            .map(|uri| uri.to_string())
            .unwrap_or_default();
        let (width, height) = opener
            .toplevel()
            .map(|top| (top.allocated_width(), top.allocated_height()))
            .filter(|(w, h)| *w > 0 && *h > 0)
            .unwrap_or((super::api::DEFAULT_W, super::api::DEFAULT_H));
        let id = super::api::new_page_id();
        match build_page(width, height, &profile, Some(opener)) {
            Ok(page) => {
                let child = page.view.clone();
                arm_load_trace(&child, &id);
                PAGES.with(|pages| pages.borrow_mut().insert(id.clone(), page));
                // The pool must learn about it too, or the listing would not
                // show the very page the agent now has to drive.
                super::pool::adopt(&id, &requested, &profile, (width, height));
                crate::daemon::journal(
                    "engine.window.create",
                    json!({
                        "page_id": id,
                        "url": requested,
                        "profile": profile,
                        "opener_url": opener.uri().map(|uri| uri.to_string()),
                    }),
                );
                Some(child.upcast::<gtk::Widget>())
            }
            Err(error) => {
                // Say it rather than dropping it in silence — the silence is
                // the bug this handler exists to end.
                crate::daemon::journal(
                    "engine.window.refused",
                    json!({ "url": requested, "profile": profile, "error": error.to_string() }),
                );
                None
            }
        }
    });
}

/// Journal a popup's own load, because nobody else can.
///
/// Every other navigation in this engine is somebody's `goto` and answers a
/// responder. A popup's first load belongs to the PAGE: no verb was called, no
/// caller is waiting, and if it fails there is no reply to carry the reason. The
/// journal is the only place that can hold it.
fn arm_load_trace(view: &WebView, id: &str) {
    let page_id = id.to_string();
    view.connect_load_changed({
        let page_id = page_id.clone();
        move |view, event| {
            crate::daemon::journal(
                "engine.window.load",
                json!({
                    "page_id": page_id,
                    "event": format!("{event:?}").to_lowercase(),
                    "url": view.uri().map(|uri| uri.to_string()),
                }),
            );
        }
    });
    view.connect_load_failed(move |_view, _event, uri, error| {
        crate::daemon::journal(
            "engine.window.load_failed",
            json!({ "page_id": page_id, "url": uri, "error": error.to_string() }),
        );
        false
    });
}

/// Answer `alert` / `confirm` / `prompt` instead of raising a modal nobody can
/// reach.
///
/// ⛔ **An unanswered script dialog wedges the page for good.** WebKitGTK's
/// default handler puts up a modal on a display with no viewer; the page's own
/// script stays parked inside `alert()`, so the navigation it was about to make
/// never happens and every later verb on that page times out. Measured: one
/// `alert()` on submit turned `ctl eval` into `engine call did not answer within
/// 30s`, which is character-for-character what a live run against a government
/// payment page recorded before the cause was known.
///
/// The answers are the ones that let a flow continue, because the alternative is
/// not "the operator decides" — there is no operator on this display — it is a
/// page that hangs until the daemon dies. Every dialog is journaled with the
/// answer given, so what the engine decided on the agent's behalf is attributable
/// rather than invisible.
fn arm_script_dialogs(view: &WebView) {
    view.connect_script_dialog(|view, dialog| {
        let kind = dialog.dialog_type();
        let answer = match kind {
            ScriptDialogType::Confirm | ScriptDialogType::BeforeUnloadConfirm => {
                dialog.confirm_set_confirmed(true);
                "accepted"
            }
            ScriptDialogType::Prompt => {
                // The page's own default, which is what a user who pressed OK
                // without typing would send. Inventing text would be answering
                // a question nobody asked us.
                let default = dialog
                    .prompt_get_default_text()
                    .map(|text| text.to_string())
                    .unwrap_or_default();
                dialog.prompt_set_text(&default);
                "default-text"
            }
            _ => "dismissed",
        };
        let mut message = dialog.message().map(|m| m.to_string()).unwrap_or_default();
        let truncated = message.len() > DIALOG_MESSAGE_MAX;
        if truncated {
            message.truncate(DIALOG_MESSAGE_MAX);
        }
        crate::daemon::journal(
            "engine.script.dialog",
            json!({
                "type": format!("{kind:?}").to_lowercase(),
                "answer": answer,
                "message": message,
                "truncated": truncated,
                "url": view.uri().map(|uri| uri.to_string()),
            }),
        );
        // TRUE means handled here and now. Returning FALSE hands it back to
        // WebKitGTK's modal, which is the wedge.
        true
    });
}

/// Arm a one-shot wait for the next load to settle, resolving to the committed
/// URL. `load-failed` resolves it too, so a dead network is an error rather
/// than a timeout.
fn arm_load_wait(view: &WebView, responder: Responder<String>) {
    let slot: Shared<String> = Rc::new(RefCell::new(Some(responder)));
    let handlers: Rc<RefCell<Vec<glib::SignalHandlerId>>> = Rc::new(RefCell::new(Vec::new()));

    let finished = view.connect_load_changed({
        let slot = slot.clone();
        let handlers = handlers.clone();
        move |view, event| {
            if event != LoadEvent::Finished {
                return;
            }
            if let Some(responder) = slot.borrow_mut().take() {
                responder.ok(view.uri().map(|uri| uri.to_string()).unwrap_or_default());
            }
            disconnect_all(view, &handlers);
        }
    });
    let failed = view.connect_load_failed({
        let slot = slot.clone();
        let handlers = handlers.clone();
        move |view, _event, uri, error| {
            if let Some(responder) = slot.borrow_mut().take() {
                responder.fail(format!("load of {uri} failed: {error}"));
            }
            disconnect_all(view, &handlers);
            false
        }
    });
    handlers.borrow_mut().extend([finished, failed]);
}

/// The `()`-valued twin of `arm_load_wait`, for `load_html`.
fn arm_load_wait_unit(view: &WebView, responder: Responder<()>) {
    let slot: Shared<()> = Rc::new(RefCell::new(Some(responder)));
    let handlers: Rc<RefCell<Vec<glib::SignalHandlerId>>> = Rc::new(RefCell::new(Vec::new()));

    let finished = view.connect_load_changed({
        let slot = slot.clone();
        let handlers = handlers.clone();
        move |view, event| {
            if event != LoadEvent::Finished {
                return;
            }
            if let Some(responder) = slot.borrow_mut().take() {
                responder.ok(());
            }
            disconnect_all(view, &handlers);
        }
    });
    let failed = view.connect_load_failed({
        let slot = slot.clone();
        let handlers = handlers.clone();
        move |view, _event, uri, error| {
            if let Some(responder) = slot.borrow_mut().take() {
                responder.fail(format!("load of {uri} failed: {error}"));
            }
            disconnect_all(view, &handlers);
            false
        }
    });
    handlers.borrow_mut().extend([finished, failed]);
}

/// Drop every handler this wait armed, so a second navigation on the same page
/// does not resolve a stale responder.
fn disconnect_all(view: &WebView, handlers: &Rc<RefCell<Vec<glib::SignalHandlerId>>>) {
    for handler in handlers.borrow_mut().drain(..) {
        view.disconnect(handler);
    }
}

/// Everything an event needs to look like it came from the seat.
struct Seat {
    window: gdk::Window,
    pointer: gdk::Device,
    keyboard: Option<gdk::Device>,
    keymap: gdk::Keymap,
    time: u32,
}

/// A strictly increasing event timestamp.
///
/// `g_get_monotonic_time() / 1000` gave a whole batch of events the SAME
/// timestamp, because they are built well inside one millisecond. That was my
/// first hypothesis for the one-character typing bug and it was WRONG — fixing
/// it changed nothing, and the real cause was the main loop (see
/// [`Engine::input`]). It is kept anyway because duplicate timestamps are
/// independently wrong: multi-click detection is defined in terms of the gap
/// between press times, so `count: 2` needs two distinct ones. The clock starts
/// at the real monotonic time and then only moves forward.
fn next_event_time() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static CLOCK: AtomicU32 = AtomicU32::new(0);
    let now = (unsafe { glib::ffi::g_get_monotonic_time() } / 1000) as u32;
    // Two steps per call: every event kind here spends at most two ticks
    // (press then release) before the next `seat()`.
    CLOCK
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
            Some(if now > last.saturating_add(2) {
                now
            } else {
                last.saturating_add(2)
            })
        })
        .unwrap_or(now)
}

fn seat(view: &WebView) -> Result<Seat> {
    let window = view
        .window()
        .context("the view has no GdkWindow yet — it was never realised")?;
    let display = gdk::Display::default().context("no gdk display")?;
    let gdk_seat = display.default_seat().context("no default seat")?;
    let pointer = gdk_seat
        .pointer()
        .context("no pointer device on the engine's display")?;
    Ok(Seat {
        window,
        pointer,
        keyboard: gdk_seat.keyboard(),
        keymap: gdk::Keymap::for_display(&display).context("no keymap for the engine's display")?,
        time: next_event_time(),
    })
}

/// Dispatch one input event as real seat input.
fn dispatch_input(view: &WebView, event: &InputEvent) -> Result<u32> {
    match event {
        InputEvent::Click {
            x,
            y,
            button,
            count,
        } => {
            // A click begins with the pointer ARRIVING. Without the motion,
            // WebKit's last-known pointer position is stale, so `:hover` never
            // applies and a menu that opens on hover is never open by the time
            // the press lands. This is the difference between "the click was
            // delivered" and "the click did what a user's click does".
            let mut dispatched = dispatch_motion(view, *x, *y)?;
            for _ in 0..(*count).max(1) {
                dispatched += dispatch_button(view, *x, *y, *button)?;
            }
            Ok(dispatched)
        }
        InputEvent::Move { x, y } => dispatch_motion(view, *x, *y),
        InputEvent::Scroll { x, y, dx, dy } => dispatch_scroll(view, *x, *y, *dx, *dy),
        InputEvent::Key { keyval, mods } => dispatch_key(view, *keyval, *mods),
        InputEvent::Text { text } => {
            let mut dispatched = 0;
            for ch in text.chars() {
                // SAFETY: a pure value conversion in gdk, no pointers involved.
                let keyval = unsafe { gdk::ffi::gdk_unicode_to_keyval(ch as u32) };
                dispatched += dispatch_key(view, keyval, 0)?;
            }
            Ok(dispatched)
        }
    }
}

/// Fill the fields every GdkEvent shares. Returns the raw pointer for the
/// caller to finish filling as its own union member.
///
/// # Safety
/// `event` must have been created by `gdk_event_new` with a type whose union
/// member starts with these fields — which is every event type here.
unsafe fn fill_common(event: &mut gdk::Event, seat: &Seat) -> *mut gdk::ffi::GdkEvent {
    let ptr: *mut gdk::ffi::GdkEvent = event.to_glib_none_mut().0;
    let any = ptr as *mut gdk::ffi::GdkEventAny;
    unsafe {
        (*any).window = seat.window.to_glib_full();
        // `send_event: 0` says "this came from the server". WebKit does not
        // read it, but a GTK widget in the chain might, and an event that
        // claims to be synthetic invites exactly the special-casing this
        // engine exists to avoid.
        (*any).send_event = 0;
    }
    ptr
}

fn dispatch_motion(view: &WebView, x: f64, y: f64) -> Result<u32> {
    let seat = seat(view)?;
    let mut event = gdk::Event::new(gdk::EventType::MotionNotify);
    // SAFETY: freshly created MotionNotify event; the motion union member is
    // the live one, and every pointer we store is either owned (`to_glib_full`)
    // or null.
    unsafe {
        let raw = fill_common(&mut event, &seat) as *mut gdk::ffi::GdkEventMotion;
        (*raw).time = seat.time;
        (*raw).x = x;
        (*raw).y = y;
        (*raw).axes = std::ptr::null_mut();
        (*raw).state = 0;
        (*raw).is_hint = 0;
        (*raw).x_root = x;
        (*raw).y_root = y;
    }
    event.set_device(Some(&seat.pointer));
    gtk::main_do_event(&mut event);
    Ok(1)
}

fn dispatch_scroll(view: &WebView, x: f64, y: f64, dx: f64, dy: f64) -> Result<u32> {
    let seat = seat(view)?;
    let mut event = gdk::Event::new(gdk::EventType::Scroll);
    // SAFETY: freshly created Scroll event; the scroll union member is live.
    unsafe {
        let raw = fill_common(&mut event, &seat) as *mut gdk::ffi::GdkEventScroll;
        (*raw).time = seat.time;
        (*raw).x = x;
        (*raw).y = y;
        (*raw).state = 0;
        // Smooth scrolling, which is what a modern seat sends and what lets a
        // page read fractional deltas instead of quantised clicks.
        (*raw).direction = gdk::ffi::GDK_SCROLL_SMOOTH;
        (*raw).x_root = x;
        (*raw).y_root = y;
        (*raw).delta_x = dx;
        (*raw).delta_y = dy;
        (*raw).is_stop = 0;
    }
    event.set_device(Some(&seat.pointer));
    gtk::main_do_event(&mut event);
    Ok(1)
}

/// A key press/release pair.
///
/// `hardware_keycode` is looked up in the real keymap rather than left zero:
/// WebKit derives the DOM `code` and much of its text input from it, and a
/// zero keycode is how "the key arrived but nothing was typed" happens.
fn dispatch_key(view: &WebView, keyval: u32, mods: u32) -> Result<u32> {
    let seat = seat(view)?;
    let entry = seat.keymap.entries_for_keyval(keyval).into_iter().next();
    let (keycode, group) = entry
        .map(|key| (key.keycode() as u16, key.group() as u8))
        .unwrap_or((0, 0));
    if keycode == 0 {
        bail!(
            "no key on this keymap produces keyval {keyval} \
             (0x{keyval:04x}) — the engine will not pretend it typed it"
        );
    }

    let mut dispatched = 0;
    for (step, kind) in [gdk::EventType::KeyPress, gdk::EventType::KeyRelease]
        .into_iter()
        .enumerate()
    {
        let mut event = gdk::Event::new(kind);
        // SAFETY: freshly created key event; the key union member is live and
        // `string` is left null (it is deprecated; WebKit reads keyval).
        unsafe {
            let raw = fill_common(&mut event, &seat) as *mut gdk::ffi::GdkEventKey;
            (*raw).time = seat.time + step as u32;
            (*raw).state = mods;
            (*raw).keyval = keyval;
            (*raw).length = 0;
            (*raw).string = std::ptr::null_mut();
            (*raw).hardware_keycode = keycode;
            (*raw).group = group;
            (*raw).is_modifier = 0;
        }
        if let Some(keyboard) = &seat.keyboard {
            event.set_device(Some(keyboard));
        }
        gtk::main_do_event(&mut event);
        dispatched += 1;
    }
    Ok(dispatched)
}

/// Modifier names to GDK's mask bits. One owner, so `/engine/input` and any
/// future keyboard caller cannot disagree about what "ctrl" means.
pub fn modifier_mask(names: &[String]) -> Result<u32> {
    let mut mask = 0;
    for name in names {
        mask |= match name.to_ascii_lowercase().as_str() {
            "shift" => gdk::ModifierType::SHIFT_MASK.bits(),
            "ctrl" | "control" => gdk::ModifierType::CONTROL_MASK.bits(),
            "alt" => gdk::ModifierType::MOD1_MASK.bits(),
            "meta" | "super" | "cmd" => gdk::ModifierType::SUPER_MASK.bits(),
            other => bail!("unknown modifier {other:?} (known: shift, ctrl, alt, meta)"),
        };
    }
    Ok(mask)
}

/// A key NAME (`Enter`, `Tab`, `a`, `F5`) to its GDK keyval.
pub fn keyval_from_name(name: &str) -> Result<u32> {
    let c_name = std::ffi::CString::new(name).context("key name has an interior NUL")?;
    // SAFETY: `gdk_keyval_from_name` takes a NUL-terminated string and returns
    // a plain value; `c_name` outlives the call.
    let keyval = unsafe { gdk::ffi::gdk_keyval_from_name(c_name.as_ptr()) };
    if keyval == gdk::ffi::GDK_KEY_VoidSymbol as u32 || keyval == 0 {
        bail!("unknown key name {name:?}");
    }
    Ok(keyval)
}

/// Build and deliver a press/release pair as seat input. Returns how many
/// events were dispatched.
///
/// `gdk_event_new` returns a zeroed GdkEvent whose union is already tagged with
/// the type, so filling the matching member is the documented way to build one.
/// gdk-rs 0.18 exposes getters for `GdkEventButton` but no setters, and the
/// adblock FFI in the surface path is the same precedent. `to_glib_full` hands
/// the event an owned reference on the window, which `gdk_event_free` releases.
fn dispatch_button(view: &WebView, x: f64, y: f64, button: u32) -> Result<u32> {
    let seat = seat(view)?;
    let mut dispatched = 0;
    for (step, kind) in [gdk::EventType::ButtonPress, gdk::EventType::ButtonRelease]
        .into_iter()
        .enumerate()
    {
        let mut event = gdk::Event::new(kind);
        // SAFETY: freshly created button event; the button union member is
        // live and `axes` is left null.
        unsafe {
            let raw = fill_common(&mut event, &seat) as *mut gdk::ffi::GdkEventButton;
            (*raw).time = seat.time + step as u32;
            (*raw).x = x;
            (*raw).y = y;
            (*raw).axes = std::ptr::null_mut();
            (*raw).state = 0;
            (*raw).button = button;
            (*raw).x_root = x;
            (*raw).y_root = y;
        }
        event.set_device(Some(&seat.pointer));
        gtk::main_do_event(&mut event);
        dispatched += 1;
    }
    Ok(dispatched)
}

/// The Phase A gate's click, now one case of [`dispatch_input`].
fn dispatch_click(view: &WebView, x: f64, y: f64) -> Result<u32> {
    dispatch_input(
        view,
        &InputEvent::Click {
            x,
            y,
            button: 1,
            count: 1,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The ink metric is what turns "the DOM says the heading is there" into
    // "the heading PAINTED". It must count only inside the rect and must score
    // a blank buffer zero, or the pixel proof proves nothing.
    fn solid(width: i32, height: i32, value: u8) -> Shot {
        Shot {
            width,
            height,
            stride: width * 4,
            png: Vec::new(),
            bgra: vec![value; (width * height * 4) as usize],
        }
    }

    // ---- crop -------------------------------------------------------------

    // A crop must move the PIXELS, not just the numbers. This paints a marker
    // block, cuts it out, and reads the marker back at the crop's origin — a
    // crop that only resized the header would score zero here.
    #[test]
    fn a_crop_carries_the_pixels_it_names() {
        let mut shot = solid(64, 64, 0xff);
        for row in 20..30 {
            for col in 10..25 {
                let offset = (row * shot.stride + col * 4) as usize;
                shot.bgra[offset..offset + 4].copy_from_slice(&[0, 0, 0, 0xff]);
            }
        }
        let cut = shot
            .crop(PixelRect {
                x: 10,
                y: 20,
                w: 15,
                h: 10,
            })
            .expect("crop inside the surface");
        assert_eq!((cut.width, cut.height), (15, 10));
        // Every pixel of the crop is the marker, and none of the white around it.
        assert_eq!(cut.dark_pixels(0, 0, 15, 10, 128), 15 * 10);
        // And it really re-encoded: a PNG, not the parent's bytes.
        assert!(
            cut.png.starts_with(b"\x89PNG\r\n\x1a\n"),
            "crop must re-encode a PNG"
        );
        assert!(shot.png.is_empty(), "the parent shot is not mutated");
    }

    // A crop the caller could not have meant is a REFUSAL. An empty or
    // out-of-bounds one used to be the sort of thing that produced a blank
    // image and a confused hour.
    #[test]
    fn a_crop_outside_the_surface_is_refused() {
        let shot = solid(32, 32, 0xff);
        for rect in [
            PixelRect {
                x: 0,
                y: 0,
                w: 0,
                h: 10,
            },
            PixelRect {
                x: -1,
                y: 0,
                w: 10,
                h: 10,
            },
            PixelRect {
                x: 30,
                y: 0,
                w: 10,
                h: 10,
            },
            PixelRect {
                x: 0,
                y: 30,
                w: 10,
                h: 10,
            },
        ] {
            assert!(
                shot.crop(rect).is_err(),
                "{rect:?} should have been refused"
            );
        }
    }

    // The wire names of the two regions are read by the router and echoed to
    // the caller; they are the CLI's vocabulary, so they are pinned.
    #[test]
    fn the_shot_regions_answer_to_their_wire_names() {
        assert_eq!(ShotRegion::Visible.id(), "viewport");
        assert_eq!(ShotRegion::FullDocument.id(), "full");
    }

    #[test]
    fn a_blank_canvas_has_no_ink() {
        let shot = solid(16, 16, 0xff);
        assert_eq!(shot.dark_pixels(0, 0, 16, 16, 128), 0);
    }

    #[test]
    fn dark_pixels_are_counted_only_inside_the_rect() {
        let mut shot = solid(16, 16, 0xff);
        for row in 0..4 {
            for col in 0..4 {
                let offset = ((row * shot.stride) + col * 4) as usize;
                shot.bgra[offset..offset + 4].copy_from_slice(&[0, 0, 0, 0xff]);
            }
        }
        assert_eq!(shot.dark_pixels(0, 0, 4, 4, 128), 16);
        assert_eq!(shot.dark_pixels(8, 8, 8, 8, 128), 0);
        // Out-of-bounds rects clamp rather than panic: a selector can report a
        // rect that runs past the viewport and that must not kill the engine.
        assert_eq!(shot.dark_pixels(-8, -8, 64, 64, 128), 16);
    }
}
