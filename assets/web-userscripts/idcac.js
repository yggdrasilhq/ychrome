// ==UserScript==
// @name        I still don't care about cookies
// @world       isolated
// @all-frames
// @run-at      document-start
// ==/UserScript==
// I still don't care about cookies — ychrome's userscript substitute.
//
// WebKitGTK cannot run the real extension (no .crx), and the real extension's
// value is a database of per-site rules that no honest 200-line script can
// match. This does the three things that actually make a consent banner stop
// mattering, and says so plainly in the settings pane:
//
//   1. Click a REJECT/decline control when there is one. Rejecting is the
//      choice the user would make, and it is the one that ends the dialog for
//      good (a hidden-but-unanswered banner comes back on the next page).
//   2. Otherwise hide the banner's container.
//   3. Either way, give the page back its scrolling — a banner's real weapon is
//      `overflow:hidden` on <body>, and hiding the banner without undoing that
//      leaves a page that cannot be read.
//
// It never clicks "accept all". Consenting on the user's behalf is not a
// convenience, it is a decision, and a script has no standing to make it.

(function () {
  'use strict';
  if (window.__yggIdcac) return;
  window.__yggIdcac = true;

  // Reject controls, in preference order. Matched on the button's own text, so
  // no per-site database is needed. Lowercased before comparison.
  var REJECT_TEXT = [
    'reject all', 'reject non-essential', 'reject cookies', 'reject',
    'decline all', 'decline optional', 'decline',
    'refuse all', 'refuse',
    'only necessary', 'only essential', 'strictly necessary only',
    'use necessary cookies only', 'necessary cookies only',
    'continue without accepting', 'without accepting',
    'deny all', 'deny',
  ];

  // Containers that are consent banners often enough to hide on sight. Kept
  // deliberately short: a wrong hit here removes something the user wanted.
  var BANNER_SELECTORS = [
    '#onetrust-consent-sdk', '#onetrust-banner-sdk',
    '#CybotCookiebotDialog', '#cookiescript_injected',
    '#usercentrics-root', '#didomi-host', '#didomi-popup',
    '.qc-cmp2-container', '.fc-consent-root', '.cmp-container',
    '#cmpbox', '#truste-consent-track', '#hs-eu-cookie-confirmation',
    '[id^="sp_message_container"]', '[class*="cookie-banner"]',
    '[class*="cookie-consent"]', '[id*="cookie-banner"]',
    '[aria-label="Cookie banner"]', '[aria-label="Cookie Consent"]',
  ];

  function textOf(el) {
    return (el.innerText || el.textContent || '').trim().toLowerCase();
  }

  // A reject control that is actually a reject control: visible, clickable, and
  // whose text IS one of the phrases (not merely contains it — "reject" inside
  // a paragraph of prose is not a button).
  function findReject(root) {
    var candidates = root.querySelectorAll(
      'button, a[role="button"], [role="button"], input[type="button"], input[type="submit"]'
    );
    for (var i = 0; i < REJECT_TEXT.length; i++) {
      var want = REJECT_TEXT[i];
      for (var j = 0; j < candidates.length; j++) {
        var el = candidates[j];
        if (!el.offsetParent && el.offsetHeight === 0) continue;
        var text = textOf(el);
        if (text === want || text.replace(/[^a-z ]/g, '').trim() === want) return el;
      }
    }
    return null;
  }

  function unlockScrolling() {
    [document.documentElement, document.body].forEach(function (el) {
      if (!el) return;
      var style = getComputedStyle(el);
      if (style.overflow === 'hidden' || style.overflowY === 'hidden') {
        el.style.setProperty('overflow', 'auto', 'important');
      }
      if (style.position === 'fixed') {
        el.style.setProperty('position', 'static', 'important');
      }
    });
  }

  var handled = false;

  function sweep() {
    var found = false;
    for (var i = 0; i < BANNER_SELECTORS.length; i++) {
      var nodes = document.querySelectorAll(BANNER_SELECTORS[i]);
      for (var j = 0; j < nodes.length; j++) {
        var banner = nodes[j];
        if (banner.getAttribute('data-ygg-idcac') === 'done') continue;
        banner.setAttribute('data-ygg-idcac', 'done');
        found = true;
        var reject = findReject(banner);
        if (reject) {
          // Answering the banner is better than hiding it: an unanswered banner
          // is asked again on every page.
          reject.click();
        } else {
          banner.style.setProperty('display', 'none', 'important');
        }
      }
    }
    if (found) {
      handled = true;
      unlockScrolling();
    }
    // A scroll lock with no banner we recognise is still a scroll lock the user
    // did not ask for, but only undo it once a banner has been seen — otherwise
    // this would fight every modal on the web.
    if (handled) unlockScrolling();
  }

  function start() {
    sweep();
    // Consent dialogs are injected late and re-injected on route changes, so
    // watch rather than run once. Disconnected after 15s: by then the page has
    // either shown its banner or has none, and an observer that outlives its
    // purpose is a tax on every DOM write the page makes.
    var observer = new MutationObserver(sweep);
    observer.observe(document.documentElement, { childList: true, subtree: true });
    setTimeout(function () { observer.disconnect(); }, 15000);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', start, { once: true });
  } else {
    start();
  }
})();
