//! Injected JavaScript the engine owns.
//!
//! These are page-side helpers, kept out of `api.rs` so the router stays
//! readable and so each script has one home. They run in the page's own world
//! via `/eval`'s path, which means a page CAN see them — none of them carries
//! a secret, and the DOM extractor deliberately refuses to read one back out.

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
