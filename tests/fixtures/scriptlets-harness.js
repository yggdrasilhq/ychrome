// Harness for the GENERATED `scriptlets` userscript, run by the Rust test in
// src/extensions.rs. It EXECUTES the body the catalog serves — not a copy —
// against a stub page, so a scriptlet that stops working fails here even when
// every source needle still matches.
//
//   node scriptlets-harness.js <path-to-scriptlets.js> <hostname>
//
// Prints one `ok <check>` line per passing check and exits 0; throws (exit 1)
// on the first failure. `<hostname>` picks the case: `ychrome-probe.test`
// carries a synthetic rule for every implemented scriptlet, and any other host
// exercises the performance contract (a page with no rules must not even build
// the runtime's state).
'use strict';

const fs = require('fs');
const vm = require('vm');

const nativeParse = JSON.parse;
const nativeStringify = JSON.stringify;

const scriptPath = process.argv[2];
const hostname = process.argv[3] || 'ychrome-probe.test';
const generated = fs.readFileSync(scriptPath, 'utf8');

const checks = [];
function check(name, condition, detail) {
    if (!condition) throw new Error(`FAILED ${name}${detail ? `: ${detail}` : ''}`);
    checks.push(name);
    console.log(`ok ${name}`);
}

// The runtime is spliced into the generated file as a function expression; the
// harness drives it directly with its OWN rules so a check does not depend on
// what some upstream list happens to say this week. Pulling it out of the
// generated body (rather than reading runtime.js) is what makes this a test of
// the shipped artefact.
const BEGIN = 'ychrome-scriptlet-runtime:begin';
const END = 'ychrome-scriptlet-runtime:end';
const from = generated.indexOf(BEGIN);
const to = generated.indexOf(END);
check('the generated body marks where its runtime starts and ends',
    from !== -1 && to > from);
const runtimeSource = generated
    .slice(generated.indexOf('*/', from) + 2, generated.lastIndexOf('/*', to))
    .trim()
    .replace(/^var run =\s*/, '')
    .replace(/;$/, '');
check('the generated body carries the runtime as a function of (RULES, TABLE, win)',
    runtimeSource.indexOf('function (RULES, TABLE, win)') !== -1);

// ---- the stub page -------------------------------------------------------
// Deliberately thin: enough for the scriptlets to install, and nothing that
// answers a question on their behalf.

const listeners = [];
const removedNodes = [];
function makeElement(tag, attrs, text) {
    const el = {
        tagName: tag.toUpperCase(),
        _attrs: Object.assign({}, attrs),
        textContent: text || '',
        classList: {
            _set: new Set((attrs && attrs.class ? attrs.class.split(' ') : [])),
            contains(n) { return this._set.has(n); },
            remove(n) { this._set.delete(n); },
            add(n) { this._set.add(n); },
        },
        hasAttribute(n) { return Object.prototype.hasOwnProperty.call(this._attrs, n); },
        removeAttribute(n) { delete this._attrs[n]; },
        getAttribute(n) { return Object.prototype.hasOwnProperty.call(this._attrs, n) ? this._attrs[n] : null; },
        setAttribute(n, v) { this._attrs[n] = v; },
    };
    el.parentNode = { removeChild() { removedNodes.push(el); } };
    return el;
}
const dom = {
    'a[href]': [makeElement('a', { href: 'https://tracker.example/out?https%3A%2F%2Freal.example%2Fpage' })],
    '.keepme': [makeElement('div', { 'data-ad': '1', class: 'promo keepme' }, 'hello')],
    'script': [makeElement('script', {}, 'window.antiAdblockTripwire()')],
};

const cookies = [];
const sandbox = {
    console,
    JSON, Object, Array, Set, Map, Promise, RegExp, Math, Error, ReferenceError, TypeError,
    isFinite, parseInt, parseFloat, decodeURIComponent, encodeURIComponent, String, Number,
    Boolean, Function, Date, URL, URLSearchParams, Response, Request,
    setTimeout, clearTimeout, setInterval, clearInterval,
};
// node has no XMLHttpRequest and no DOM Event; the stubs are the thin surface
// `no-xhr-if` installs against.
function FakeXHR() {}
FakeXHR.prototype.open = function (m, u) { this._m = m; this._u = u; };
FakeXHR.prototype.send = function () { FakeXHR.sent += 1; };
FakeXHR.prototype.dispatchEvent = function () {};
FakeXHR.sent = 0;
sandbox.XMLHttpRequest = FakeXHR;
sandbox.Event = function StubEvent(type) { this.type = type; };
sandbox.window = sandbox;
sandbox.globalThis = sandbox;
sandbox.location = { hostname, href: `https://${hostname}/page`, protocol: 'https:' };
sandbox.EventTarget = function EventTarget() {};
sandbox.EventTarget.prototype.addEventListener = function (type, fn) {
    listeners.push([type, fn]);
};
sandbox.addEventListener = () => {};
sandbox.MutationObserver = function MO(cb) {
    this.cb = cb;
    this.observe = () => {};
    this.disconnect = () => {};
};
sandbox.requestAnimationFrame = (fn) => { sandbox.__frames.push(fn); return 1; };
sandbox.__frames = [];
sandbox.document = {
    readyState: 'complete',
    documentElement: {},
    currentScript: null,
    addEventListener() {},
    querySelectorAll(sel) { return dom[sel] || []; },
    querySelector(sel) { return (dom[sel] || [])[0] || null; },
    // A real `document.cookie` returns only `name=value` pairs but ACCEPTS the
    // whole attribute string, and the attributes are what an expiry is made of.
    // The stub keeps both so a check can see either.
    get cookie() { return cookies.map((c) => c.split(';')[0]).join('; '); },
    set cookie(value) { cookies.push(String(value)); },
};
const storage = {};
sandbox.localStorage = {
    setItem(k, v) { storage[k] = String(v); },
    removeItem(k) { delete storage[k]; },
    getItem(k) { return Object.prototype.hasOwnProperty.call(storage, k) ? storage[k] : null; },
    // `length` and `key(i)` are the only way to answer a REGEX key, and a stub
    // without them would let a literal-only implementation pass.
    get length() { return Object.keys(storage).length; },
    key(i) { return Object.keys(storage)[i] ?? null; },
};
sandbox.sessionStorage = sandbox.localStorage;
let realFetchCalls = 0;
sandbox.fetch = function (url) { realFetchCalls += 1; return Promise.resolve(new Response('real')); };
let realOpenCalls = 0;
sandbox.open = function () { realOpenCalls += 1; return { real: true }; };
let realEvalCalls = 0;
sandbox.eval = function () { realEvalCalls += 1; return 'real'; };
sandbox.RTCPeerConnection = function () { return { real: true }; };
sandbox.DOMException = function (m, n) { this.message = m; this.name = n; };

vm.createContext(sandbox);
// The newline matters: the runtime opens with a comment block, and `(` on the
// same line would comment the function away.
const run = vm.runInContext(`(\n${runtimeSource}\n)`, sandbox, { filename: 'runtime.js' });
check('the runtime evaluates to a function', typeof run === 'function');

// ---- the performance contract --------------------------------------------
// A page with no rules must cost nothing: no state object, no observer, no
// timers replaced. 5,338 domains carry rules and the whole web does not.
if (hostname !== 'ychrome-probe.test') {
    const before = sandbox.setTimeout;
    const out = run({ 'ychrome-probe.test': [0] }, [['set-constant', 'x', 'true']], sandbox);
    check('a page with no rules gets no state at all', out === null && sandbox.__yggScriptlets === undefined);
    check('…and no global is replaced', sandbox.setTimeout === before);
    console.log(`ALL OK (${checks.length} checks, host=${hostname})`);
    process.exit(0);
}

// ---- one synthetic rule per implemented scriptlet ------------------------
// The generated payload is whatever upstream says today; these are the
// BEHAVIOURS, pinned.
const TABLE = [
    ['set-constant', 'ychromeProbe.flag', 'true'],
    ['abort-on-property-read', 'ychromeBaitRead'],
    ['abort-on-property-write', 'ychromeBaitWrite'],
    ['abort-current-script', 'ychromeAcs', 'tripwire'],
    ['no-setTimeout-if', 'adTimer'],
    ['no-setInterval-if', 'adPoller'],
    ['adjust-setInterval', 'slowLoop', '', '0.1'],
    ['adjust-setTimeout', 'slowWait', '', '0.1'],
    ['addEventListener-defuser', 'click', 'adHandler'],
    ['no-window-open-if', 'popunder'],
    ['set-cookie', 'consent', 'rejected'],
    ['remove-cookie', 'trackme'],
    ['set-local-storage-item', 'adPrefs', '$remove$'],
    // A REGEX key, which is how the lists really write this one. Treating it
    // as a literal reports success and removes nothing — found live on
    // soundcloud.com, where the tracking id survived a "successful" removal.
    ['set-local-storage-item', '/^track_/', '$remove$'],
    ['set-session-storage-item', 'adSession', 'off'],
    ['json-prune', 'adPlacements adSlots'],
    ['no-fetch-if', 'ads.example'],
    ['no-xhr-if', 'ads.example'],
    ['noeval-if', 'adPayload'],
    ['nowebrtc'],
    ['remove-attr', 'data-ad', '.keepme'],
    ['remove-class', 'promo', '.keepme'],
    ['remove-node-text', 'script', 'antiAdblockTripwire'],
    ['href-sanitizer', 'a[href]', '?'],
];
const RULES = { 'ychrome-probe.test': TABLE.map((_, i) => i) };

sandbox.ychromeProbe = {};
cookies.push('trackme=1');
storage.adPrefs = 'keepme';
storage.track_id = 'abc';
storage.keep_id = 'xyz';

const state = run(RULES, TABLE, sandbox);
check('every rule installed (none threw, none was unimplemented)',
    state.failed === 0, nativeStringify(state));
check('the state object is published for diagnosis', sandbox.__yggScriptlets === state);

// set-constant
check('set-constant freezes a page global', sandbox.ychromeProbe.flag === true);
sandbox.ychromeProbe.flag = false;
check('…and an assignment from the page is swallowed, not thrown on',
    sandbox.ychromeProbe.flag === true);

// abort-on-property-read / -write
let threwRead = false;
try { void sandbox.ychromeBaitRead; } catch (e) { threwRead = /abort/.test(e.message); }
check('abort-on-property-read stops the reader', threwRead);
let threwWrite = false;
try { sandbox.ychromeBaitWrite = 1; } catch (e) { threwWrite = /abort/.test(e.message); }
check('abort-on-property-write stops the writer', threwWrite);

// abort-current-script: only the matching INLINE script, nothing else.
sandbox.document.currentScript = null;
let readOk = true;
try { void sandbox.ychromeAcs; } catch (e) { readOk = false; }
check('abort-current-script leaves an ordinary read alone', readOk);
sandbox.document.currentScript = { src: '', textContent: 'var x = tripwire();' };
let threwAcs = false;
try { void sandbox.ychromeAcs; } catch (e) { threwAcs = /abort/.test(e.message); }
check('abort-current-script stops the matching inline script', threwAcs);
sandbox.document.currentScript = { src: 'https://cdn/x.js', textContent: 'tripwire' };
let externalOk = true;
try { void sandbox.ychromeAcs; } catch (e) { externalOk = false; }
check('…and never an EXTERNAL script, whatever its text says', externalOk);
sandbox.document.currentScript = null;

// timers
let adTimerRan = false;
let realTimerRan = false;
sandbox.setTimeout(function adTimer() { adTimerRan = true; }, 0);
sandbox.setTimeout(function keepMe() { realTimerRan = true; }, 0);
let adPollerId = sandbox.setInterval(function adPoller() {}, 10);
clearInterval(adPollerId);
let boosted = null;
const realSetInterval = setInterval;
sandbox.setInterval = ((orig) => function (fn, ms) { boosted = ms; return orig(() => {}, 1e9); })(realSetInterval);

// no-* refusals are counted, which is the observable the page cannot see.
check('no-setTimeout-if refused a matching timer', state.refused >= 1);

// addEventListener-defuser
sandbox.EventTarget.prototype.addEventListener('click', function adHandler() {});
sandbox.EventTarget.prototype.addEventListener('click', function keepHandler() {});
check('addEventListener-defuser drops the matching listener',
    listeners.length === 1 && /keepHandler/.test(String(listeners[0][1])),
    nativeStringify(listeners.map((l) => String(l[1]).slice(0, 40))));

// no-window-open-if
const decoy = sandbox.open('https://popunder.example/x');
check('no-window-open-if refuses the popup', realOpenCalls === 0 && decoy.real !== true);
check('…and answers with a DECOY, never null', decoy && typeof decoy.close === 'function');
sandbox.open('https://legit.example/x');
check('…and lets an unmatched window.open through', realOpenCalls === 1);

// cookies
check('set-cookie writes the cookie the filter asks for',
    cookies.some((c) => c.indexOf('consent=rejected') !== -1), nativeStringify(cookies));
check('remove-cookie expires the matching cookie',
    cookies.some((c) => /^trackme=;\s*Max-Age=0/.test(c)), nativeStringify(cookies));
check('…and leaves a cookie it was not asked about alone',
    !cookies.some((c) => /^consent=;/.test(c)), nativeStringify(cookies));

// storage
check('set-local-storage-item removes on $remove$',
    !Object.prototype.hasOwnProperty.call(storage, 'adPrefs'));
check('…and honours a REGEX key, which is how the lists write it',
    !Object.prototype.hasOwnProperty.call(storage, 'track_id'), nativeStringify(storage));
check('…and leaves a key the pattern does not match', storage.keep_id === 'xyz');
check('set-session-storage-item writes a value', storage.adSession === 'off');

// json-prune — the scriptlet this whole lane started from.
const parsed = sandbox.JSON.parse(nativeStringify({
    adPlacements: [1], adSlots: [2], streamingData: { formats: [1] },
}));
check('json-prune drops the named paths out of JSON.parse',
    parsed.adPlacements === undefined && parsed.adSlots === undefined);
check('…and keeps everything it was not asked to remove',
    parsed.streamingData.formats.length === 1);

// no-fetch-if / no-xhr-if
let fetchBlocked = null;
let fetchAllowed = null;

// DOM sweeps run on the animation frame the runtime queued.
sandbox.__frames.forEach((fn) => fn());
check('remove-attr strips the attribute', dom['.keepme'][0].hasAttribute('data-ad') === false);
check('remove-class strips the class', dom['.keepme'][0].classList.contains('promo') === false);
check('…and leaves the element itself alone', dom['.keepme'][0].classList.contains('keepme'));
check('remove-node-text removes the matching node', removedNodes.length === 1);
check('href-sanitizer rewrites the redirect to its destination',
    dom['a[href]'][0].getAttribute('href') === 'https://real.example/page',
    dom['a[href]'][0].getAttribute('href'));

// nowebrtc
let rtcThrew = false;
try { new sandbox.RTCPeerConnection(); } catch (e) { rtcThrew = true; }
check('nowebrtc refuses RTCPeerConnection', rtcThrew);

// noeval-if
check('noeval-if refuses matching source', sandbox.eval('var adPayload = 1') === undefined);
check('…and runs source it does not match', sandbox.eval('1 + 1') === 'real');

Promise.all([
    sandbox.fetch('https://ads.example/x').then((r) => r.text()).then((t) => { fetchBlocked = t; }),
    sandbox.fetch('https://real.example/x').then((r) => r.text()).then((t) => { fetchAllowed = t; }),
]).then(() => {
    check('no-fetch-if answers a matching request without sending it', fetchBlocked === '');
    check('…and lets an unmatched request go out', fetchAllowed === 'real' && realFetchCalls === 1);
    console.log(`ALL OK (${checks.length} checks, host=${hostname})`);
    process.exit(0);
}).catch((err) => {
    console.error(String(err && err.message ? err.message : err));
    process.exit(1);
});
