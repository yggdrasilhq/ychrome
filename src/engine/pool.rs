//! Phase D — the page pool and the governor (`docs/agent-engine.md` §5).
//!
//! The RAM truth the spec starts from: a real page's WebKitWebProcess costs
//! 80-300 MB PSS, so a hundred live views would be 10-30 GB and nobody gets
//! that. The answer is that **a logical page is not a live view**. The pool
//! holds hundreds of logical pages — identity, url, tags, and the PLACE the
//! user was at — while only a working set of them owns engine resources.
//!
//! Parking loses nothing durable: cookies and localStorage already live in the
//! profile's jar on disk. What parking must preserve is the *place*, and
//! `resume` restores it — the same restore-is-a-PLACE rule the tab store
//! learned, which is why [`Place`] carries scroll offset and form state rather
//! than just a URL.
//!
//! The governor enforces the budget rather than advising it, and it never
//! lies: every park, resume and refusal is journaled with the numbers that
//! caused it, and `/engine/open` answers `429 pool_saturated` with the current
//! pressure instead of queueing forever behind a budget it cannot meet.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::host::Engine;
use super::js;

/// Governor defaults, straight from §5.
pub const DEFAULT_MAX_LIVE: usize = 12;
pub const DEFAULT_MAX_RSS_MB: u64 = 4096;
pub const DEFAULT_PER_PAGE_RSS_MB: u64 = 1500;

/// How often the governor tick measures and acts (§5).
pub const GOVERNOR_TICK: Duration = Duration::from_secs(2);

/// The ONE wording for "that page is not here", and the one place that names
/// which daemon generation is saying so.
///
/// `no page "pg_000001"` alone cannot distinguish *never opened*, *closed*, and
/// *opened on a daemon that is no longer the one answering you* — and the third
/// was the one that broke every recipe under `assets/engine-recipes/`, since
/// each is a sequence of `ctl` calls against a single `page_id`. An agent had no
/// way to see that two calls had reached two different daemons. Naming the
/// generation is what makes that visible in the message itself.
pub fn no_such_page(id: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "no page {id:?} on the daemon that answered ({}). A page belongs to the \
         daemon generation that opened it: if the open landed on an earlier \
         daemon, this is what that looks like",
        crate::daemon::generation_label()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageState {
    Live,
    Parked,
    Crashed,
}

impl PageState {
    pub fn id(self) -> &'static str {
        match self {
            PageState::Live => "live",
            PageState::Parked => "parked",
            PageState::Crashed => "crashed",
        }
    }
}

/// Where the user was. Restoring a parked page means restoring THIS, not just
/// re-fetching the URL.
#[derive(Debug, Clone, Default)]
pub struct Place {
    pub scroll_x: f64,
    pub scroll_y: f64,
    /// Form state as `{selector: value}`, best-effort by the injected
    /// extractor. Never includes a password field — see [`js::CAPTURE_PLACE`].
    pub form_state: Value,
}

#[derive(Debug, Clone)]
pub struct LogicalPage {
    pub id: String,
    pub url: String,
    pub tags: Vec<String>,
    pub state: PageState,
    pub place: Place,
    pub viewport: (i32, i32),
    /// The profile whose identity this page browses under. Carried on the
    /// LOGICAL page so a resume rebuilds the view on the same jar, adblock
    /// filter and userscripts it was parked from — a page that came back as a
    /// different identity would be a silent logout.
    pub profile: String,
    pub opened_at_ms: u128,
    pub last_used_ms: u128,
    /// Why this page is `Crashed`, when it is. Absent otherwise.
    pub error: Option<String>,
    /// A page that has been admitted but whose first load has not finished.
    ///
    /// The governor must not evict one. Without this, a batch worker could
    /// have its brand-new view parked by ANOTHER worker's `make_room` in the
    /// window between admission and `goto`, and the load would then fail with
    /// "no page" — measured, and the reason two of twenty-four pages vanished.
    pub pinned: bool,
}

impl LogicalPage {
    pub fn to_json(&self) -> Value {
        json!({
            "page_id": self.id,
            "url": self.url,
            "tags": self.tags,
            "state": self.state.id(),
            "profile": self.profile,
            "viewport": { "w": self.viewport.0, "h": self.viewport.1 },
            "scroll": { "x": self.place.scroll_x, "y": self.place.scroll_y },
            "opened_at_ms": self.opened_at_ms,
            "last_used_ms": self.last_used_ms,
            // NOT a number we can honestly produce per page — see
            // `measure_pss`. A zero here would read as a measurement.
            "rss_mb": Value::Null,
            "error": self.error,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub max_live: usize,
    pub max_rss_mb: u64,
    pub per_page_rss_mb: u64,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget {
            max_live: DEFAULT_MAX_LIVE,
            max_rss_mb: DEFAULT_MAX_RSS_MB,
            per_page_rss_mb: DEFAULT_PER_PAGE_RSS_MB,
        }
    }
}

impl Budget {
    fn to_json(self) -> Value {
        json!({
            "max_live": self.max_live,
            "max_rss_mb": self.max_rss_mb,
            "per_page_rss_mb": self.per_page_rss_mb,
        })
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Memory measurement
// ---------------------------------------------------------------------------

/// Aggregate engine PSS in MB, plus the per-process itemisation behind it.
///
/// **Aggregate, not per page, and that is a limitation rather than a choice.**
/// webkit2gtk 2.0.2 exposes no web-process identifier
/// (`webkit_web_view_get_web_process_identifier` is absent from the crate and
/// from the 2.52.5 headers), so there is no honest way to say which
/// WebKitWebProcess belongs to which view. The budget that §5 actually enforces
/// — "over `max_rss_mb` → park LRU until under" — needs only the aggregate, so
/// that is what the governor uses. `per_page_rss_mb` and its
/// `terminate_web_process` kill are NOT implemented, because implementing them
/// would mean inventing an attribution we cannot measure.
///
/// PSS, not RSS: shared pages counted once, which is the only number that adds
/// up across a process tree. Read from `smaps_rollup`, walking every descendant
/// of this process — WebKit's children are ours, and a process tree is not a
/// process name.
pub fn measure_pss() -> (u64, Vec<Value>) {
    let mut parents: HashMap<i32, i32> = HashMap::new();
    let mut names: HashMap<i32, String> = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return (0, Vec::new());
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            continue;
        };
        let mut ppid = 0;
        let mut name = String::new();
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("PPid:") {
                ppid = rest.trim().parse().unwrap_or(0);
            } else if let Some(rest) = line.strip_prefix("Name:") {
                name = rest.trim().to_string();
            }
        }
        parents.insert(pid, ppid);
        names.insert(pid, name);
    }

    let own = std::process::id() as i32;
    let mut tree = vec![own];
    let mut index = 0;
    while index < tree.len() {
        let parent = tree[index];
        for (&pid, &ppid) in &parents {
            if ppid == parent && !tree.contains(&pid) {
                tree.push(pid);
            }
        }
        index += 1;
    }

    let mut total_kb = 0u64;
    let mut items = Vec::new();
    for pid in tree {
        let Ok(rollup) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) else {
            continue;
        };
        let pss_kb: u64 = rollup
            .lines()
            .find_map(|line| line.strip_prefix("Pss:"))
            .and_then(|rest| rest.trim().trim_end_matches(" kB").trim().parse().ok())
            .unwrap_or(0);
        total_kb += pss_kb;
        items.push(json!({
            "pid": pid,
            "name": names.get(&pid).cloned().unwrap_or_default(),
            "pss_mb": (pss_kb as f64 / 1024.0 * 10.0).round() / 10.0,
        }));
    }
    (total_kb / 1024, items)
}

// ---------------------------------------------------------------------------
// The pool
// ---------------------------------------------------------------------------

struct Inner {
    pages: HashMap<String, LogicalPage>,
    budget: Budget,
    parked_total: u64,
    resumed_total: u64,
    saturated_total: u64,
    peak_pss_mb: u64,
}

pub struct Pool {
    inner: Mutex<Inner>,
    /// Serialises ADMISSION and EVICTION.
    ///
    /// Budget decisions cannot be made concurrently: two threads that each
    /// read `live` before either inserts will both believe there is room, and
    /// two that pick the same LRU victim will both try to park it — one wins,
    /// the other's error propagates out and kills an unrelated open. Measured
    /// before this lock existed: 20 evictions produced 13 parks, `max_live`
    /// overshot 12 to 16, and five of twenty-four pages never entered the pool.
    ///
    /// Page LOADS stay concurrent: this is released before the navigation.
    admission: Mutex<()>,
}

static POOL: OnceLock<Pool> = OnceLock::new();

pub fn pool() -> &'static Pool {
    POOL.get_or_init(|| Pool {
        admission: Mutex::new(()),
        inner: Mutex::new(Inner {
            pages: HashMap::new(),
            budget: Budget::default(),
            parked_total: 0,
            resumed_total: 0,
            saturated_total: 0,
            peak_pss_mb: 0,
        }),
    })
}

impl Pool {
    pub fn budget(&self) -> Budget {
        self.inner
            .lock()
            .map(|inner| inner.budget)
            .unwrap_or_default()
    }

    pub fn set_budget(&self, max_live: Option<usize>, max_rss_mb: Option<u64>) -> Budget {
        let mut inner = self.inner.lock().expect("pool lock");
        if let Some(value) = max_live {
            inner.budget.max_live = value.max(1);
        }
        if let Some(value) = max_rss_mb {
            inner.budget.max_rss_mb = value.max(64);
        }
        inner.budget
    }

    pub fn get(&self, id: &str) -> Option<LogicalPage> {
        self.inner.lock().ok()?.pages.get(id).cloned()
    }

    pub fn all(&self) -> Vec<LogicalPage> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let mut pages: Vec<LogicalPage> = inner.pages.values().cloned().collect();
        pages.sort_by(|a, b| a.id.cmp(&b.id));
        pages
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let Ok(inner) = self.inner.lock() else {
            return (0, 0, 0);
        };
        let live = inner
            .pages
            .values()
            .filter(|p| p.state == PageState::Live)
            .count();
        let parked = inner
            .pages
            .values()
            .filter(|p| p.state == PageState::Parked)
            .count();
        (live, parked, inner.pages.len())
    }

    fn insert(&self, page: LogicalPage) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.pages.insert(page.id.clone(), page);
        }
    }

    pub fn remove(&self, id: &str) -> Option<LogicalPage> {
        self.inner.lock().ok()?.pages.remove(id)
    }

    /// Mark a page used. LRU is defined on this clock, so every verb that
    /// touches a page must call it or the governor will park the page a script
    /// is actively driving.
    pub fn touch(&self, id: &str) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(page) = inner.pages.get_mut(id)
        {
            page.last_used_ms = now_ms();
        }
    }

    /// Release a page for eviction once its first load has settled.
    ///
    /// Called on BOTH the success and failure paths of that load: a page whose
    /// navigation failed must still become evictable, or it would hold a live
    /// slot for the rest of the daemon's life.
    pub fn unpin(&self, id: &str) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(page) = inner.pages.get_mut(id)
        {
            page.pinned = false;
        }
    }

    /// A page whose view could not be brought back. It stays in the pool with
    /// its reason attached rather than vanishing: a caller that opened 300
    /// pages needs to see WHICH ones died and why, and a silently dropped
    /// entry would read as one that was never requested.
    fn mark_crashed(&self, id: &str, error: &str) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(page) = inner.pages.get_mut(id)
        {
            page.state = PageState::Crashed;
            page.error = Some(error.to_string());
        }
        crate::daemon::journal(
            "engine.governor.crashed",
            json!({ "page_id": id, "error": error }),
        );
    }

    /// The least-recently-used LIVE page, excluding `keep`.
    fn lru_live(&self, keep: &str) -> Option<LogicalPage> {
        let inner = self.inner.lock().ok()?;
        inner
            .pages
            .values()
            .filter(|page| page.state == PageState::Live && page.id != keep && !page.pinned)
            .min_by_key(|page| page.last_used_ms)
            .cloned()
    }

    pub fn metrics(&self) -> Value {
        let (pss_mb, items) = measure_pss();
        let (live, parked, total) = self.counts();
        let mut inner = self.inner.lock().expect("pool lock");
        inner.peak_pss_mb = inner.peak_pss_mb.max(pss_mb);
        let budget = inner.budget;
        json!({
            "ok": true,
            "live": live,
            "parked": parked,
            "logical_pages": total,
            "budgets": budget.to_json(),
            "pressure": {
                "pss_mb": pss_mb,
                "peak_pss_mb": inner.peak_pss_mb,
                "rss_headroom_mb": budget.max_rss_mb as i64 - pss_mb as i64,
                "live_headroom": budget.max_live as i64 - live as i64,
            },
            "totals": {
                "parked": inner.parked_total,
                "resumed": inner.resumed_total,
                "saturated_refusals": inner.saturated_total,
            },
            // The itemisation behind the aggregate. Per-PROCESS, because
            // per-page is not measurable here (see `measure_pss`).
            "processes": items,
        })
    }
}

// ---------------------------------------------------------------------------
// Park / resume / capacity
// ---------------------------------------------------------------------------

/// Capture the place and drop the view. The logical page stays.
///
/// Takes the admission lock, because an explicit `/engine/park` races the
/// governor for the same page exactly as two governor ticks would.
pub fn park(engine: &Engine, id: &str) -> Result<LogicalPage> {
    let _admission = pool()
        .admission
        .lock()
        .map_err(|_| anyhow::anyhow!("admission lock"))?;
    park_locked(engine, id)
}

/// The body of [`park`]. The caller MUST already hold the admission lock.
fn park_locked(engine: &Engine, id: &str) -> Result<LogicalPage> {
    let Some(mut page) = pool().get(id) else {
        return Err(no_such_page(id));
    };
    if page.state != PageState::Live {
        return Ok(page);
    }
    let started = Instant::now();

    // Best-effort: a page that is mid-navigation or has torn down its context
    // still has to park, or the budget can never be met. What we could not
    // capture is simply not restored, and the journal says so.
    let captured = engine.eval(id, js::CAPTURE_PLACE).ok();
    if let Some(value) = &captured {
        page.place = Place {
            scroll_x: value["scroll_x"].as_f64().unwrap_or(0.0),
            scroll_y: value["scroll_y"].as_f64().unwrap_or(0.0),
            form_state: value["form_state"].clone(),
        };
        if let Some(url) = value["url"].as_str()
            && !url.is_empty()
        {
            page.url = url.to_string();
        }
    }
    engine.close(id)?;
    page.state = PageState::Parked;
    pool().insert(page.clone());
    if let Ok(mut inner) = pool().inner.lock() {
        inner.parked_total += 1;
    }

    crate::daemon::journal(
        "engine.governor.park",
        json!({
            "page_id": id,
            "url": page.url,
            "place_captured": captured.is_some(),
            "scroll_y": page.place.scroll_y,
            "elapsed_ms": started.elapsed().as_millis(),
        }),
    );
    Ok(page)
}

/// Recreate the view and restore the place.
pub fn resume(engine: &Engine, id: &str) -> Result<LogicalPage> {
    let Some(mut page) = pool().get(id) else {
        return Err(no_such_page(id));
    };
    if page.state == PageState::Live {
        return Ok(page);
    }
    let started = Instant::now();
    let _admission = pool()
        .admission
        .lock()
        .map_err(|_| anyhow::anyhow!("admission lock"))?;
    make_room_locked(engine, id)?;

    if let Err(error) = engine.open(id, page.viewport.0, page.viewport.1, &page.profile) {
        pool().mark_crashed(id, &error.to_string());
        return Err(error);
    }
    if let Err(error) = engine.goto(id, &page.url, Duration::from_secs(45)) {
        // The view exists but the page will not come back. Drop the view so it
        // stops costing a live slot, and keep the logical page with its reason.
        let _ = engine.close(id);
        pool().mark_crashed(id, &error.to_string());
        return Err(error);
    }

    // Restore is a PLACE, not a URL: scroll offset and form state come back
    // too, or "resume" would silently mean "reload".
    let restore = json!({
        "scroll_x": page.place.scroll_x,
        "scroll_y": page.place.scroll_y,
        "form_state": page.place.form_state,
    });
    let restored = engine
        .eval(id, &format!("({})({})", js::RESTORE_PLACE, restore))
        .ok();

    page.state = PageState::Live;
    page.last_used_ms = now_ms();
    pool().insert(page.clone());
    if let Ok(mut inner) = pool().inner.lock() {
        inner.resumed_total += 1;
    }

    crate::daemon::journal(
        "engine.governor.resume",
        json!({
            "page_id": id,
            "url": page.url,
            "restored": restored,
            "elapsed_ms": started.elapsed().as_millis(),
        }),
    );
    Ok(page)
}

/// Park LRU pages until there is room for one more live view.
///
/// Returns `Err` with `pool_saturated` when the budget cannot be met even with
/// nothing left to park — the caller turns that into a 429 with the pressure
/// numbers, because a script that is over budget needs to SEE the constraint,
/// not wait behind it forever.
pub fn make_room(engine: &Engine, keep: &str) -> Result<()> {
    let _admission = pool()
        .admission
        .lock()
        .map_err(|_| anyhow::anyhow!("admission lock"))?;
    make_room_locked(engine, keep)
}

/// The body of [`make_room`]. The caller MUST already hold the admission lock.
fn make_room_locked(engine: &Engine, keep: &str) -> Result<()> {
    loop {
        let budget = pool().budget();
        let (live, _, _) = pool().counts();
        let (pss_mb, _) = measure_pss();
        if let Ok(mut inner) = pool().inner.lock() {
            inner.peak_pss_mb = inner.peak_pss_mb.max(pss_mb);
        }

        let over_live = live >= budget.max_live;
        let over_rss = pss_mb > budget.max_rss_mb;
        if !over_live && !over_rss {
            return Ok(());
        }

        let Some(victim) = pool().lru_live(keep) else {
            // Nothing left to park and still over budget.
            if let Ok(mut inner) = pool().inner.lock() {
                inner.saturated_total += 1;
            }
            crate::daemon::journal(
                "engine.governor.saturated",
                json!({
                    "keep": keep,
                    "live": live,
                    "pss_mb": pss_mb,
                    "budget": budget.to_json(),
                    "reason": if over_rss { "max_rss_mb" } else { "max_live" },
                }),
            );
            bail!(
                "pool_saturated: {live} live, {pss_mb} MB PSS, budget {} live / {} MB — \
                 nothing left to park",
                budget.max_live,
                budget.max_rss_mb
            );
        };
        crate::daemon::journal(
            "engine.governor.evict",
            json!({
                "victim": victim.id,
                "keep": keep,
                "live": live,
                "pss_mb": pss_mb,
                "reason": if over_rss { "max_rss_mb" } else { "max_live" },
                "budget": budget.to_json(),
                "victim_idle_ms": now_ms().saturating_sub(victim.last_used_ms),
            }),
        );
        park_locked(engine, &victim.id)?;
    }
}

/// Register a brand-new logical page and give it a live view.
pub fn open(
    engine: &Engine,
    id: &str,
    url: &str,
    profile: &str,
    tags: Vec<String>,
    viewport: (i32, i32),
) -> Result<LogicalPage> {
    let _admission = pool()
        .admission
        .lock()
        .map_err(|_| anyhow::anyhow!("admission lock"))?;
    make_room_locked(engine, id)?;
    let now = now_ms();
    pool().insert(LogicalPage {
        id: id.to_string(),
        url: url.to_string(),
        tags,
        state: PageState::Live,
        place: Place::default(),
        viewport,
        profile: profile.to_string(),
        opened_at_ms: now,
        last_used_ms: now,
        error: None,
        // Admitted but not yet loaded: not evictable until the caller unpins.
        pinned: true,
    });
    engine.open(id, viewport.0, viewport.1, profile)?;
    Ok(pool().get(id).expect("just inserted"))
}

/// Make sure a page has a live view before a verb touches it.
///
/// A parked page RESUMES rather than erroring. That is what makes the pool
/// transparent: §5's promise is "hundreds of logical pages, only the working
/// set live", and a script that had to notice parking would be doing the
/// governor's job by hand.
pub fn ensure_live(engine: &Engine, id: &str) -> Result<()> {
    match pool().get(id) {
        None => Ok(()), // not pooled: a raw engine page, nothing to govern
        Some(page) if page.state == PageState::Live => {
            pool().touch(id);
            Ok(())
        }
        Some(_) => {
            resume(engine, id)?;
            Ok(())
        }
    }
}

/// Start the governor tick. Idempotent; the first `/engine/*` call starts it.
///
/// Takes a **`Weak`**, not an `Arc`, and that is load-bearing rather than
/// tidy. `shutdown` reaps the headless display by dropping the last strong
/// reference — but this thread loops forever, so an `Arc` here would keep the
/// count above zero and `Engine::drop` would never run. Measured with the
/// `Arc`: seven orphaned Xvfb processes after seven governor runs, the exact
/// leak the CLI's own teardown was written to prevent, reintroduced by the one
/// thread that outlives everything. A failed `upgrade` is also how the governor
/// learns to exit.
pub fn start_governor(engine: Weak<Engine>) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("ychrome-governor".into())
        .spawn(move || {
            loop {
                std::thread::sleep(GOVERNOR_TICK);
                // The engine is gone: so is our reason to exist.
                let Some(engine) = engine.upgrade() else {
                    return;
                };
                let budget = pool().budget();
                let (live, _, total) = pool().counts();
                if total == 0 {
                    continue;
                }
                let (pss_mb, _) = measure_pss();
                if let Ok(mut inner) = pool().inner.lock() {
                    inner.peak_pss_mb = inner.peak_pss_mb.max(pss_mb);
                }
                if live > budget.max_live || pss_mb > budget.max_rss_mb {
                    // `make_room` journals its own numbers. A saturated pool is
                    // not an error on the tick — there is simply nothing to do
                    // until a caller closes something.
                    let _ = make_room(&engine, "");
                }
                // Drop the strong reference BEFORE the next sleep, or shutdown
                // would block for a whole tick behind it.
                drop(engine);
            }
        })
        .ok();
}
