//! Injected JavaScript the engine owns.
//!
//! These are page-side helpers, kept out of `api.rs` so the router stays
//! readable and so each script has one home. They run in the page's own world
//! via `/eval`'s path, which means a page CAN see them — none of them carries
//! a secret, and the DOM extractor deliberately refuses to read one back out.

/// The page-side globals the three click scripts hand each other.
///
/// They appear as literals inside the scripts — a `const` cannot be
/// interpolated into another `const` string — so this is where the shared
/// spelling is written down once, and `the_click_scripts_share_one_page_side_vocabulary`
/// is what holds all three to it. Two spellings of one key is the same class of
/// silent divergence as two spellings of a refusal.
#[cfg(test)]
pub const CLICK_POOL_KEY: &str = "__ychromeClickPool";
#[cfg(test)]
pub const CLICK_PIN_KEY: &str = "__ychromeClickPin";

/// Phase A of a selector-addressed click: classify EVERY match, keep the live
/// ones, and pin that pool for the phases that follow.
///
/// ⛔ **`document.querySelector` returning the first match is the bug this
/// exists to end.** Real pages carry hidden duplicates — IBKR's login page has
/// six-plus `button[type=submit]`, five of them dead, the live one third in
/// document order — so "the first match" and "the thing a human would click"
/// are routinely different elements, and dispatching at the first one is a
/// click into the void reported as `{"dispatched":3,"ok":true}`.
///
/// Liveness is the same predicate the visible surface plane's matcher uses
/// (`__yggLive` in yggterm's `shell.rs`): an `aria-hidden` ancestor, a zero-area
/// rect, `display:none` or `visibility:hidden` all mean "not a thing anyone can
/// click". One vocabulary across both planes, deliberately — two planes with
/// different words for the same refusal is exactly the divergence this codebase
/// forbids.
///
/// The rect test comes BEFORE the style test on purpose: `display:none` measures
/// `0x0`, so it is reported as `zero_size_element`, which is the token the bug
/// report asks for and the phrase the surface plane already uses.
///
/// `opacity: 0` is deliberately NOT hidden here. A fully transparent element is
/// still hit-testable, a real click on it really fires, and `elementFromPoint`
/// in phase B is the honest arbiter of whether the point lands. Filtering it
/// out would refuse a click that would have worked.
pub const CLICK_POOL: &str = r#"(selector) => {
  let all;
  try { all = Array.prototype.slice.call(document.querySelectorAll(selector)); }
  catch (e) { return { bad_selector: String((e && e.message) || e) }; }
  const live = [];
  let hidden = 0, zero_size = 0;
  for (const el of all) {
    let ariaHidden = false;
    let p = el, guard = 0;
    while (p && guard < 24) {
      if (p.getAttribute && p.getAttribute('aria-hidden') === 'true') { ariaHidden = true; break; }
      p = p.parentElement; guard++;
    }
    if (ariaHidden) { hidden++; continue; }
    const r = el.getBoundingClientRect();
    if (!(r.width > 0 && r.height > 0)) { zero_size++; continue; }
    let style = null;
    try { style = window.getComputedStyle(el); } catch (err) { style = null; }
    if (style && (style.visibility === 'hidden' || style.display === 'none')) { hidden++; continue; }
    live.push(el);
  }
  window.__ychromeClickPool = live;
  return { matches: all.length, hittable: live.length, hidden: hidden, zero_size: zero_size };
}"#;

/// Phase A2: pin one candidate out of the pool and scroll it into view.
///
/// It reports NO geometry, and that is the whole point of the split. A rect read
/// in the same tick as the `scrollIntoView` that moved the node is the
/// pre-scroll rect — the measured cause, on the surface plane, of a click that
/// came back `accepted` + `delivered` + `is_trusted` with nothing selected,
/// because the event landed where the element used to be.
pub const CLICK_PIN: &str = r#"(index) => {
  const pool = window.__ychromeClickPool || [];
  const el = pool[index] || null;
  window.__ychromeClickPin = el;
  if (!el) return { found: false };
  const connected = (el.isConnected !== undefined)
    ? !!el.isConnected
    : !!(document.contains && document.contains(el));
  try { el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' }); }
  catch (e) { try { el.scrollIntoView(true); } catch (e2) { /* nothing to scroll */ } }
  return { found: true, isConnected: connected };
}"#;

/// Phase B: re-measure the PINNED node once the scroll has settled, and report
/// everything the DOM knows about it at that instant.
///
/// It must not scroll (a second scroll would invalidate its own measurement) and
/// must not re-run the selector (that is how you end up measuring a twin).
/// `phase: 'post_scroll'` is a contract token: the Rust side REFUSES a payload
/// without it, so collapsing the two phases back into one cannot pass silently.
///
/// ⛔ **`hit.contains(el)` IS NOT HITTABILITY, AND BELIEVING IT WAS IS THE BUG.**
/// `elementFromPoint` over a `visibility:hidden` element returns whatever paints
/// there — on a plain page, `<body>` — and `<body>` contains every element on it.
/// So a test of `hit === el || el.contains(hit) || hit.contains(el)` accepts the
/// FIRST match unconditionally on any normal page, which made the whole
/// walk-every-match loop a no-op. Measured on the fixture: the decoy's centre
/// hit `BODY`, `hitContainsE` was `true`, and the click went to (87.875, 21.5)
/// where nothing listens. A click reaches an element only when the point lands
/// ON it or on a DESCENDANT of it, because only then does the event path run
/// through it. An ancestor hit means the ancestor was clicked, not this node.
pub const CLICK_MEASURE: &str = r#"(() => {
  const el = window.__ychromeClickPin;
  if (!el) return { found: false, phase: 'post_scroll', handle: 'lost' };
  const connected = (el.isConnected !== undefined)
    ? !!el.isConnected
    : !!(document.contains && document.contains(el));
  const r = el.getBoundingClientRect();
  const x = r.left + r.width / 2, y = r.top + r.height / 2;
  const inViewport = x >= 0 && y >= 0 && x <= window.innerWidth && y <= window.innerHeight;
  const hit = inViewport ? document.elementFromPoint(x, y) : null;
  const onTarget = !!(hit && (hit === el || (el.contains && el.contains(hit))));
  const name = (n) => n ? (String(n.tagName || '').toLowerCase()
    + (n.id ? '#' + n.id : '')) : null;
  return {
    found: true, phase: 'post_scroll', isConnected: connected,
    x: x, y: y, w: r.width, h: r.height,
    visible: r.width > 0 && r.height > 0,
    in_viewport: inViewport, onTarget: onTarget,
    hit: name(hit), tag: String(el.tagName || '').toLowerCase()
  };
})()"#;

/// The structured interactable read behind `/engine/dom {mode:"snapshot"}`.
///
/// This is the verb an agent leans on hardest — it is how a script answers
/// "what can I act on" — so its selectors carry a contract rather than a hope:
/// **every selector it emits is verified, in the page, to resolve to exactly
/// the element it describes.** One that does not is returned as `null` and
/// counted, so a caller can never feed `/engine/input` a selector that silently
/// addresses a different node. A snapshot that quietly mislabels its own
/// handles would be a new instrument lie, which is the one thing this engine
/// may not ship.
///
/// Skipped: `display:none`, `visibility:hidden`, fully transparent, and
/// zero-area elements. Kept but flagged: elements scrolled out of the viewport
/// (`in_viewport: false`) — they are real targets once scrolled to, and hiding
/// them would make the snapshot lie by omission.
///
/// Not covered in v1, and named rather than silently missed: shadow roots and
/// same-origin iframes. `truncated` says when the node cap was hit.
pub const DOM_SNAPSHOT: &str = r#"(() => {
  const MAX_NODES = 500;
  const MAX_TEXT = 160;
  const SELECTOR = [
    'a[href]', 'button', 'input', 'select', 'textarea', 'summary',
    '[role]', '[contenteditable=""]', '[contenteditable="true"]', '[onclick]'
  ].join(',');

  const esc = (s) => (window.CSS && CSS.escape) ? CSS.escape(s)
                   : String(s).replace(/[^a-zA-Z0-9_-]/g, (c) => '\\' + c);
  const uniqueId = (el) =>
    el.id && document.querySelectorAll('#' + esc(el.id)).length === 1;

  function selectorFor(el) {
    if (uniqueId(el)) return '#' + esc(el.id);
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1) {
      if (uniqueId(node)) { parts.unshift('#' + esc(node.id)); break; }
      if (node === document.documentElement) { parts.unshift('html'); break; }
      let part = node.localName;
      const parent = node.parentNode;
      if (parent && parent.children) {
        const kin = Array.prototype.filter.call(
          parent.children, (c) => c.localName === node.localName);
        if (kin.length > 1) part += ':nth-of-type(' + (kin.indexOf(node) + 1) + ')';
      }
      parts.unshift(part);
      node = node.parentElement;
    }
    return parts.join(' > ');
  }

  function roleOf(el) {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit.trim();
    switch (el.localName) {
      case 'a': return el.hasAttribute('href') ? 'link' : 'generic';
      case 'button': return 'button';
      case 'select': return 'combobox';
      case 'textarea': return 'textbox';
      case 'summary': return 'button';
      case 'input': {
        const t = (el.getAttribute('type') || 'text').toLowerCase();
        if (t === 'checkbox') return 'checkbox';
        if (t === 'radio') return 'radio';
        if (t === 'submit' || t === 'button' || t === 'reset') return 'button';
        if (t === 'range') return 'slider';
        if (t === 'file') return 'button';
        return 'textbox';
      }
    }
    return el.isContentEditable ? 'textbox' : 'generic';
  }

  function nameOf(el) {
    const aria = el.getAttribute('aria-label');
    if (aria && aria.trim()) return aria.trim();
    const by = el.getAttribute('aria-labelledby');
    if (by) {
      const text = by.split(/\s+/)
        .map((id) => { const n = document.getElementById(id); return n ? n.textContent : ''; })
        .join(' ').trim();
      if (text) return text;
    }
    if (el.labels && el.labels.length && el.labels[0].textContent.trim()) {
      return el.labels[0].textContent.trim();
    }
    const placeholder = el.getAttribute('placeholder');
    if (placeholder && placeholder.trim()) return placeholder.trim();
    if (el.localName === 'img') return (el.getAttribute('alt') || '').trim();
    const own = (el.innerText || el.textContent || '').replace(/\s+/g, ' ').trim();
    if (own) return own;
    const title = el.getAttribute('title');
    if (title && title.trim()) return title.trim();
    return (el.getAttribute('name') || '').trim();
  }

  const nodes = [];
  let truncated = false;
  let unresolved = 0;

  for (const el of document.querySelectorAll(SELECTOR)) {
    if (nodes.length >= MAX_NODES) { truncated = true; break; }
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') continue;
    if (parseFloat(style.opacity) === 0) continue;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;

    let selector = selectorFor(el);
    // The contract. A selector that does not resolve back to THIS element is
    // not returned as if it did.
    let matches;
    try { matches = document.querySelectorAll(selector); } catch (e) { matches = []; }
    if (matches.length !== 1 || matches[0] !== el) { selector = null; unresolved++; }

    const node = {
      role: roleOf(el),
      tag: el.localName,
      text: nameOf(el).slice(0, MAX_TEXT),
      selector: selector,
      rect: { x: Math.round(r.x), y: Math.round(r.y),
              w: Math.round(r.width), h: Math.round(r.height) },
      in_viewport: r.bottom > 0 && r.right > 0
                && r.top < window.innerHeight && r.left < window.innerWidth
    };

    const type = (el.getAttribute('type') || '').toLowerCase();
    if (type === 'password') {
      // Never read a password field's value back out. The vault fills these;
      // an agent reading the snapshot has no business seeing the result, and
      // "no secret in a log line" applies to a structured read too.
      node.value = null;
      node.redacted = true;
    } else if (typeof el.value === 'string') {
      node.value = el.value.slice(0, MAX_TEXT);
    }
    if (el.disabled === true) node.disabled = true;
    if ((type === 'checkbox' || type === 'radio') && typeof el.checked === 'boolean') {
      node.checked = el.checked;
    }
    if (el.localName === 'a' && el.href) node.href = el.href;
    nodes.push(node);
  }

  return {
    url: location.href,
    title: document.title,
    viewport: { w: window.innerWidth, h: window.innerHeight },
    scroll: { x: Math.round(window.scrollX), y: Math.round(window.scrollY) },
    nodes: nodes,
    node_count: nodes.length,
    truncated: truncated,
    unresolved_selectors: unresolved,
    shadow_roots_skipped: document.querySelectorAll('*').length > 0
      ? Array.prototype.filter.call(document.querySelectorAll('*'), (e) => e.shadowRoot).length
      : 0
  };
})()"#;

/// Activity tracker behind `/engine/wait {"idle_ms": N}`.
///
/// Installs once per page and reports how long the page has been quiet. "Quiet"
/// is defined as BOTH halves the spec asks for and is measured, not assumed:
///
/// - **layout** — a `MutationObserver` over the whole document stamps the
///   clock on any subtree, attribute or text change;
/// - **network** — `fetch` and `XMLHttpRequest` are wrapped to keep an
///   in-flight counter, and the clock is stamped when a request settles.
///
/// While anything is in flight the idle time reads 0, so a caller can never be
/// told a page is idle in the middle of a fetch. Returns `{idle_ms, inflight}`.
pub const IDLE_PROBE: &str = r#"(() => {
  if (!window.__ychromeIdle) {
    const state = { last: performance.now(), inflight: 0 };
    window.__ychromeIdle = state;
    const stamp = () => { state.last = performance.now(); };
    try {
      new MutationObserver(stamp).observe(document, {
        subtree: true, childList: true, attributes: true, characterData: true
      });
    } catch (e) { /* a document that refuses observation still gets network */ }
    const fetch0 = window.fetch;
    if (fetch0) {
      window.fetch = function () {
        state.inflight++; stamp();
        return fetch0.apply(this, arguments).finally(() => { state.inflight--; stamp(); });
      };
    }
    const open0 = window.XMLHttpRequest && window.XMLHttpRequest.prototype.open;
    if (open0) {
      window.XMLHttpRequest.prototype.open = function () {
        this.addEventListener('loadstart', () => { state.inflight++; stamp(); });
        this.addEventListener('loadend', () => { state.inflight--; stamp(); });
        return open0.apply(this, arguments);
      };
    }
  }
  const s = window.__ychromeIdle;
  return { idle_ms: s.inflight > 0 ? 0 : Math.round(performance.now() - s.last),
           inflight: s.inflight };
})()"#;

/// Capture the PLACE for `/engine/park` (§5).
///
/// Scroll offset plus best-effort form state, keyed by the same selector shape
/// `DOM_SNAPSHOT` emits so the two agree about what names an element.
///
/// **Password and file inputs are never captured.** Parking is a memory
/// optimisation; it must not become a way to spill a credential into a pool
/// entry that outlives the view. A field we refuse to capture is simply not
/// restored, which is the correct failure — the vault refills it.
pub const CAPTURE_PLACE: &str = r#"(() => {
  const form = {};
  const nth = (el) => {
    const p = el.parentNode;
    if (!p || !p.children) return el.localName;
    const kin = Array.prototype.filter.call(p.children, (c) => c.localName === el.localName);
    return kin.length > 1
      ? el.localName + ':nth-of-type(' + (kin.indexOf(el) + 1) + ')'
      : el.localName;
  };
  const path = (el) => {
    if (el.id) { try { if (document.querySelectorAll('#' + CSS.escape(el.id)).length === 1) return '#' + CSS.escape(el.id); } catch (e) {} }
    const parts = []; let n = el;
    while (n && n.nodeType === 1 && n !== document.documentElement) { parts.unshift(nth(n)); n = n.parentElement; }
    return parts.join(' > ');
  };
  for (const el of document.querySelectorAll('input, textarea, select')) {
    const t = (el.getAttribute('type') || '').toLowerCase();
    if (t === 'password' || t === 'file') continue;
    if (t === 'checkbox' || t === 'radio') { form[path(el)] = { checked: !!el.checked }; }
    else if (typeof el.value === 'string' && el.value !== '') { form[path(el)] = { value: el.value }; }
  }
  return { url: location.href, scroll_x: window.scrollX, scroll_y: window.scrollY, form_state: form };
})()"#;

/// Restore a captured place after `/engine/resume` re-navigates.
///
/// Reports what it actually put back rather than claiming success: a page whose
/// markup changed between park and resume may no longer have the fields, and
/// §5's rule is "documented best-effort; never claim more than captured".
pub const RESTORE_PLACE: &str = r#"(place) => {
  let fields = 0, missing = 0;
  const form = (place && place.form_state) || {};
  for (const selector of Object.keys(form)) {
    let el = null;
    try { el = document.querySelector(selector); } catch (e) { el = null; }
    if (!el) { missing++; continue; }
    const saved = form[selector];
    if ('checked' in saved) { el.checked = saved.checked; }
    else { el.value = saved.value; }
    // A restored value that fires no event is invisible to a framework that
    // tracks its own state, so say it changed.
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    fields++;
  }
  window.scrollTo(place.scroll_x || 0, place.scroll_y || 0);
  return { fields_restored: fields, fields_missing: missing,
           scroll_y: Math.round(window.scrollY) };
}"#;

// ===== CAPTURE (docs/agent-engine.md §4, `/engine/shot`) =====================
//
// Three reads and one driver, and between them they are the whole reason a
// cropped capture can be trusted. The engine never converts CSS pixels to
// device pixels by ASSUMING a ratio: it takes the snapshot, divides its real
// width by the document width these probes report, and crops with the number
// that came back. `devicePixelRatio` is reported alongside so a caller can see
// when the two disagree, but nothing depends on it.

/// The page's own geometry, in CSS pixels, at the instant a capture crops
/// against it.
///
/// `doc_w`/`doc_h` are the SCROLLABLE document — the region
/// `SnapshotRegion::FullDocument` renders — and they are the denominator of the
/// CSS→device scale. `view_w`/`view_h` are what a `region=viewport` capture
/// covers. All five are reported in the capture's metadata header, because a
/// crop that silently used the wrong denominator produces a plausible-looking
/// PNG of the wrong part of the page, and there is no way to see that from the
/// image alone.
pub const SHOT_METRICS: &str = r#"(() => {
  const d = document.documentElement, b = document.body;
  const max = (...v) => v.reduce((a, n) => (n > a ? n : a), 0);
  return {
    doc_w: max(d ? d.scrollWidth : 0, b ? b.scrollWidth : 0,
               d ? d.clientWidth : 0, window.innerWidth || 0),
    doc_h: max(d ? d.scrollHeight : 0, b ? b.scrollHeight : 0,
               d ? d.clientHeight : 0, window.innerHeight || 0),
    view_w: window.innerWidth || 0,
    view_h: window.innerHeight || 0,
    scroll_x: Math.round(window.scrollX || 0),
    scroll_y: Math.round(window.scrollY || 0),
    dpr: window.devicePixelRatio || 1
  };
})()"#;

/// The DOCUMENT-space CSS rect of one member of the click pool.
///
/// Deliberately reads `__ychromeClickPool` rather than running its own
/// `querySelectorAll`: `region=element` then resolves a selector through
/// EXACTLY the machinery `/engine/input` clicks through — same hittable filter,
/// same `nth` default, same `{matches, hittable, hidden, zero_size}` account —
/// so "screenshot the button I am about to click" cannot pick a different
/// element than the click will. A second selector resolver beside that one is
/// precisely the divergence AGENTS.md forbids.
///
/// Document space, not viewport space, because a full-document snapshot is
/// addressed in document space and an element below the fold has to be
/// croppable without scrolling to it first.
pub const SHOT_POOL_RECT: &str = r#"(index) => {
  const pool = window.__ychromeClickPool || [];
  const el = pool[index] || null;
  if (!el) return { found: false };
  const r = el.getBoundingClientRect();
  return {
    found: true,
    tag: el.tagName ? el.tagName.toLowerCase() : '',
    x: r.left + (window.scrollX || 0),
    y: r.top + (window.scrollY || 0),
    w: r.width,
    h: r.height
  };
}"#;

/// One step of the lazy-load pre-scroll, and the read that proves it landed.
///
/// ⚠ **A pre-scroll cannot be one `eval`.** Lazy images load from an
/// `IntersectionObserver` callback and a `fetch`, both of which need event-loop
/// turns the page never gets while a single synchronous script is running. A
/// loop that scrolls the whole document inside one eval therefore returns
/// having triggered nothing. So the loop lives in Rust, one step per call with
/// a settle between them, and this is the step.
pub const SHOT_SCROLL_TO: &str = r#"(y) => {
  window.scrollTo(0, y);
  return { scroll_y: Math.round(window.scrollY || 0) };
}"#;

/// Where the page instrument keeps what it saw. Non-enumerable, so a page that
/// walks `window` does not trip over it.
pub const CONSOLE_KEY: &str = "__ychromeConsole";

/// The page instrument: uncaught errors, unhandled promise rejections, and
/// console messages, from as early as the engine can inject.
///
/// ⛔ **This exists because an SDK that throws looks exactly like an SDK that
/// does nothing.** A payment SDK builds its iframe and then navigates it a few
/// statements later; if anything in between raises, the element is there, the
/// frame is empty, and every DOM probe an agent can run says "constructed, never
/// navigated" — which is a symptom, not a cause. The reason was always in a
/// console message nobody could read, because the engine captured none. Days of
/// live investigation on a government payment gateway ended at that wall.
///
/// **Errors and rejections are captured ADDITIVELY** — `addEventListener` on
/// `error` and `unhandledrejection` — so nothing the page can observe changes:
/// no handler is replaced, `window.onerror` is left alone for the page to own,
/// and a listener is not enumerable from script.
///
/// ⚠ **`console` is different and the difference is stated rather than hidden.**
/// There is no additive listener for it, so the methods ARE wrapped. Each
/// wrapper calls the original, and `toString` is restored to the native text so
/// the cheapest detection fails — but a determined fingerprint can still tell.
/// That is the honest cost of seeing what a page logs, and it is why this is the
/// only patched thing here.
pub const PAGE_INSTRUMENT: &str = r#"(() => {
  const KEY = "__ychromeConsole";
  if (Object.prototype.hasOwnProperty.call(window, KEY)) { return "already"; }
  const MAX = 400;
  const buffer = [];
  const push = (kind, level, text, extra) => {
    if (buffer.length >= MAX) { buffer.shift(); }
    const row = { kind: kind, level: level, text: String(text).slice(0, 2000), at: Date.now() };
    if (extra) { for (const k in extra) { row[k] = extra[k]; } }
    buffer.push(row);
  };
  Object.defineProperty(window, KEY, {
    value: buffer, enumerable: false, configurable: true, writable: false,
  });

  // Additive, and therefore invisible to the page.
  window.addEventListener("error", (event) => {
    if (event.message) {
      push("error", "error", event.message, {
        source: event.filename || "", line: event.lineno || 0, col: event.colno || 0,
        stack: (event.error && event.error.stack) ? String(event.error.stack).slice(0, 2000) : "",
      });
    } else if (event.target && event.target.src) {
      // A subresource that failed to load fires `error` with no message. An SDK
      // whose module 404s dies exactly here and says nothing else.
      push("resource", "error", "failed to load", { source: String(event.target.src) });
    }
  }, true);
  window.addEventListener("unhandledrejection", (event) => {
    const reason = event.reason;
    // The MESSAGE first, and the stack beside it. WebKit's `Error.stack` is
    // bare frames with no message line, so preferring the stack reported
    // "@sdk.js:79:32" for a rejection whose whole value was its sentence.
    let text = reason;
    if (reason instanceof Error) { text = reason.name + ": " + reason.message; }
    else if (reason && typeof reason === "object") {
      try { text = JSON.stringify(reason); } catch (_) { text = String(reason); }
    }
    push("rejection", "error", text, {
      stack: (reason && reason.stack) ? String(reason.stack).slice(0, 2000) : "",
    });
  });

  // The one patched thing. See the doc comment above.
  const native = Function.prototype.toString;
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    const original = console[level];
    if (typeof original !== "function") { continue; }
    const wrapper = function () {
      try {
        push("console", level, Array.prototype.map.call(arguments, (arg) => {
          if (typeof arg === "string") { return arg; }
          try { return JSON.stringify(arg); } catch (_) { return String(arg); }
        }).join(" "));
      } catch (_) { /* an instrument must never break the page */ }
      return original.apply(console, arguments);
    };
    try {
      Object.defineProperty(wrapper, "toString", {
        value: () => native.call(original), configurable: true,
      });
      Object.defineProperty(wrapper, "name", { value: level, configurable: true });
    } catch (_) { /* cosmetic only */ }
    console[level] = wrapper;
  }
  return "installed";
})()"#;

/// Read the instrument's buffer, optionally emptying it.
pub const CONSOLE_READ: &str = r#"(clear) => {
  const rows = window.__ychromeConsole;
  if (!rows) { return { installed: false, entries: [] }; }
  const out = rows.slice();
  if (clear) { rows.length = 0; }
  return { installed: true, entries: out };
}"#;

/// Where typed text is about to go, and how much is there already.
///
/// ## Why a `type` event needs this and a click does not
///
/// A selector click resolves to a NODE before it is dispatched, so
/// `/engine/input` can say what it aimed at. A `type` event resolves nothing —
/// it goes to `document.activeElement`, whoever that is at the instant the key
/// events arrive. So the batch `click #user, type, click #pw, type` has exactly
/// two unaddressed events in it, and when an earlier event re-rendered the form
/// the text lands on `<body>` and is discarded by the page.
///
/// That is the measured failure (2026-08-07): `{"dispatched":3,"ok":true}` over
/// a login where a page-side readback then showed `user:""`, `pwlen:0`.
/// **`dispatched` counts events SENT.** Nothing in the reply was derived from
/// the page, so nothing in it could disagree with the request.
///
/// ⛔ A LENGTH, NEVER A VALUE — the same boundary the vault fill keeps. A
/// caller who types a secret through `/engine/input` already holds it, but the
/// daemon's journal and the reply must not become a second copy.
pub const FOCUS_READBACK: &str = r#"(() => {
  const el = document.activeElement;
  if (!el || el === document.body || el === document.documentElement) {
    return { focused: false, tag: el ? (el.tagName || '').toLowerCase() : null, length: 0 };
  }
  const editable = el.isContentEditable === true;
  const text = typeof el.value === 'string'
    ? el.value
    : (editable ? (el.textContent || '') : null);
  return {
    focused: true,
    tag: (el.tagName || '').toLowerCase(),
    type: (el.getAttribute ? (el.getAttribute('type') || '') : '').toLowerCase(),
    name: el.getAttribute ? (el.getAttribute('name') || el.getAttribute('id') || '') : '',
    editable: editable,
    // `null` and `0` are different answers: a focused <button> can hold no
    // text at all, and typing into one is a different fault from typing into
    // an empty input.
    length: text === null ? null : text.length,
  };
})()"#;

#[cfg(test)]
mod instrument_tests {
    use super::*;

    /// The instrument's whole licence to exist on every page is that a page
    /// cannot tell. Errors and rejections must be captured ADDITIVELY — the
    /// moment one of these becomes an assignment, we have taken a handler the
    /// page owns, and a site that sets `window.onerror` after us would silently
    /// lose ours (or we would lose theirs, which is worse).
    #[test]
    fn errors_and_rejections_are_captured_without_taking_anything_from_the_page() {
        assert!(PAGE_INSTRUMENT.contains(r#"addEventListener("error""#));
        assert!(PAGE_INSTRUMENT.contains(r#"addEventListener("unhandledrejection""#));
        assert!(
            !PAGE_INSTRUMENT.contains("window.onerror ="),
            "assigning window.onerror takes a handler the page owns"
        );
        assert!(
            !PAGE_INSTRUMENT.contains("onunhandledrejection ="),
            "same rule for rejections"
        );
    }

    /// `console` is the ONE patched thing, and the patch must call through and
    /// hide its own source. A wrapper that swallowed the call would change what
    /// the user sees in a real browser session on the same profile.
    #[test]
    fn the_console_patch_calls_through_and_does_not_advertise_itself() {
        assert!(
            PAGE_INSTRUMENT.contains("original.apply(console, arguments)"),
            "the wrapper must call the real console, or the page loses its output"
        );
        assert!(
            PAGE_INSTRUMENT.contains("native.call(original)"),
            "toString must report the NATIVE source, or the cheapest sniff finds us"
        );
    }

    /// An instrument that throws is worse than no instrument: it would break the
    /// very page it is there to explain.
    #[test]
    fn the_instrument_never_lets_its_own_failure_reach_the_page() {
        assert!(PAGE_INSTRUMENT.contains("an instrument must never break the page"));
        assert!(
            PAGE_INSTRUMENT.contains("enumerable: false"),
            "a page that walks window must not trip over our buffer"
        );
        assert!(
            PAGE_INSTRUMENT.contains(r#"hasOwnProperty.call(window, KEY)"#),
            "re-injection must be a no-op, not a second set of wrappers"
        );
    }

    /// A rejection's value is its sentence. WebKit's `Error.stack` carries bare
    /// frames and no message line, so preferring the stack reported
    /// "@sdk.js:79:32" for a rejection that said why it failed — measured.
    #[test]
    fn a_rejection_reports_its_message_and_keeps_the_stack_beside_it() {
        let listener = PAGE_INSTRUMENT
            .split("unhandledrejection")
            .nth(1)
            .expect("the rejection listener exists");
        let body = &listener[..listener.find("});").unwrap_or(listener.len())];
        assert!(
            body.contains("reason.name") && body.contains("reason.message"),
            "the message must be the text"
        );
        assert!(body.contains("stack:"), "the stack rides alongside, not instead");
    }

    #[test]
    fn reading_the_buffer_clears_it_only_when_asked() {
        assert!(CONSOLE_READ.contains("if (clear) { rows.length = 0; }"));
        assert!(
            CONSOLE_READ.contains("installed: false"),
            "a page with no instrument must say so, not answer an empty list"
        );
    }
}
