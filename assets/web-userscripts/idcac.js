// ==UserScript==
// @name        I still don't care about cookies
// @version     1.1.0
// @world       isolated
// @all-frames
// @run-at      document-start
// ==/UserScript==
// I still don't care about cookies — the half a script can actually do.
//
// ## What changed, and why this file got SMALLER in ambition
//
// This used to try to be the extension. It cannot be, and now it does not need
// to be: ychrome's adblock ruleset ingests the real upstream consent
// databases — the i-dont-care-about-cookies ABP list, EasyList Cookie, and
// uBlock Origin's annoyances-cookies — which between them name **27,859
// distinct domains**. Measured on 2026-07-31, **all 19** of the banner
// selectors this script hard-codes are already in those lists. The HIDING job
// belongs to the ruleset now, per-domain and maintained by people who do it
// full time, and a second copy here could only diverge from it.
//
// ## What the ruleset genuinely cannot do, and this can
//
// A declarative content blocker decides what LOADS and what is hidden. It
// cannot press a button and it cannot set a CSS property. So two jobs are left,
// and they are the two this file now exists for:
//
//   1. **Click a REJECT / decline control.** Hiding a banner leaves it
//      unanswered, and an unanswered banner is asked again on the next page and
//      the next site. Rejecting ENDS it. Nothing declarative can do this.
//   2. **Give the page back its scrolling.** A banner's real weapon is
//      `overflow:hidden` on <body>. `css-display-none` can only hide an
//      element; it cannot set `overflow`. The upstream lists know it too —
//      they carry rules like
//      `body.didomi-popup-open:style(overflow: auto !important;)` — and
//      `:style()` is exactly the procedural form WebKit refuses. 205 domains
//      ride on that one rule alone.
//
// Hiding stays only as a last resort, for a banner the ruleset does not know
// and that offers no reject control.
//
// ## What it will NOT handle, stated plainly
//
// It has no per-site knowledge: 19 container selectors against 27,859 domains.
// On a site whose banner is not one of the shapes below AND is not in the
// ruleset, nothing happens. It does not answer a consent dialog rendered
// inside a cross-origin iframe it cannot reach. It does not fill in a
// per-purpose preference form. And it never clicks "accept all" — consenting
// on the user's behalf is not a convenience, it is a decision, and a script has
// no standing to make it.

(function () {
  'use strict';
  if (window.__yggIdcac) return;
  window.__yggIdcac = true;

  // Reject controls, in preference order. Matched on the button's own text, so
  // no per-site database is needed. Lowercased before comparison.
  // This list is the one thing here with no upstream substitute, so it is
  // where breadth is worth paying for. Non-English entries are REJECT phrases
  // only; a consent phrase must never appear, in any language (the repo test
  // `idcac_clicks_nothing_that_consents` enforces the English half, and the
  // rule for the rest is the same one: if you are not certain a phrase means
  // "no", it does not go here).
  var REJECT_TEXT = [
    // English
    'reject all', 'reject all cookies', 'reject non-essential',
    'reject optional', 'reject cookies', 'reject',
    'decline all', 'decline all cookies', 'decline optional', 'decline cookies',
    'decline',
    'refuse all', 'refuse cookies', 'refuse',
    'only necessary', 'only essential', 'only required',
    'strictly necessary only', 'essential only', 'essential cookies only',
    'use necessary cookies only', 'necessary cookies only',
    'necessary only', 'required only',
    'continue without accepting', 'without accepting',
    'deny all', 'deny cookies', 'deny',
    'disagree', 'do not consent', 'do not sell my personal information',
    'manage rejection', 'save without consent',
    // German
    'alle ablehnen', 'ablehnen', 'nur notwendige', 'nur notwendige cookies',
    'nur essenzielle cookies', 'nur erforderliche',
    // French
    'tout refuser', 'refuser tout', 'refuser', 'continuer sans accepter',
    // Spanish / Portuguese
    'rechazar todo', 'rechazar', 'solo las necesarias', 'rejeitar tudo',
    'rejeitar',
    // Italian
    'rifiuta tutto', 'rifiuta',
    // Dutch
    'alles weigeren', 'weigeren', 'alleen noodzakelijke',
    // Nordic
    'avvis alle', 'avvisa alla', 'afvis alle', 'kun nodvendige',
    // Polish
    'odrzuc wszystko', 'odrzuc',
  ];

  // Containers that are consent banners often enough to hide on sight. Kept
  // deliberately short: a wrong hit here removes something the user wanted.
  // These are SCOPES for finding a reject button, not a hiding database — the
  // ruleset owns hiding. Every entry is a CMP root that means the same thing on
  // every site that uses it, which is why they are safe without a domain. The
  // additions came from counting how many domains each selector is applied to
  // across the three upstream consent lists (2026-07-31): a selector like
  // `.cookie` or `.modal` scores higher still and is deliberately NOT here,
  // because off its domain scope it would swallow things the user wanted.
  var BANNER_SELECTORS = [
    '#onetrust-consent-sdk', '#onetrust-banner-sdk',
    '#CybotCookiebotDialog', '#cookiescript_injected',
    '#usercentrics-root', '#didomi-host', '#didomi-popup',
    '.qc-cmp2-container', '.fc-consent-root', '.cmp-container',
    '#cmpbox', '#truste-consent-track', '#hs-eu-cookie-confirmation',
    '#cc-main', '#cc--main', '.cc-window', '#klaro',
    '#cookie-law-info-bar', '#cookie-notice', '#cookie-bar', '#cookiesModal',
    '#cookie-consent-banner', '#cookie-consent-popup', '#__tealiumGDPRecModal',
    '.sliding-popup-bottom', '.consent-banner-root', '.CookieConsent',
    '#consent-manager', '#gdpr-consent-tool-wrapper',
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
  var rejected = 0;
  var hidden = 0;
  // Readable from the console, and from an agent probe, so "did it do
  // anything on this page" is a question with an answer.
  window.__yggIdcacState = function () {
    return { rejected: rejected, hidden: hidden };
  };

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
          // is asked again on every page. This is the job nothing declarative
          // can do, so it is the one this script is really for.
          reject.click();
          rejected += 1;
        } else {
          // LAST RESORT. The adblock ruleset hides consent banners across
          // 27,859 domains; if we are still looking at one, it is a banner the
          // ruleset does not know, offering no way to say no.
          banner.style.setProperty('display', 'none', 'important');
          hidden += 1;
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
