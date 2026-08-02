// Harness for the `youtube-adblock` bundled userscript, run by the Rust test in
// src/extensions.rs. It EXECUTES the script the catalog serves — not a copy —
// against a fixture `/youtubei/v1/player` response, so a prune that stops
// pruning fails here even if every needle in the source still matches.
//
//   node youtube-adblock-harness.js <path-to-userscript.js> <hostname>
//
// Prints one `ok <check>` line per passing check and exits 0; throws (exit 1) on
// the first failure. `<hostname>` picks the case: a youtube.com host exercises
// the hooks, any other host exercises the self-guard.
'use strict';

const fs = require('fs');
const vm = require('vm');

// ⚠ THE HARNESS'S OWN PARSER, CAPTURED BEFORE THE SCRIPT UNDER TEST RUNS.
// The sandbox is handed the HOST's `JSON` and `Response` on purpose (the script
// must patch the very objects a page would see), which means that from the
// moment it runs, a plain `JSON.parse` here goes through ITS hook. Every
// assertion below would then be reading an object the parse hook had already
// cleaned — and a completely dead `fetch` hook would still pass. So the harness
// parses with these, and the script never gets to answer a question about
// itself.
const nativeParse = JSON.parse;
const nativeStringify = JSON.stringify;
const nativeResponseJson = Response.prototype.json;

const scriptPath = process.argv[2];
const hostname = process.argv[3] || 'www.youtube.com';
const source = fs.readFileSync(scriptPath, 'utf8');

const PLAYER_URL = 'https://www.youtube.com/youtubei/v1/player?key=AIza&prettyPrint=false';
const ASSET_URL = 'https://www.youtube.com/s/player/abc123/base.js';
// A REAL response, captured from https://www.youtube.com/watch?v=dQw4w9WgXcQ on
// 2026-07-31 and scrubbed of tokens and URLs (every leaf string over 60 chars,
// plus visitorData / trackingParams / signatureCipher / cpn, became
// "<scrubbed>"). The KEY STRUCTURE is untouched, which is the part the prune
// walks. A synthetic fixture proves the walk works on a shape someone imagined;
// this proves AD_FIELDS still names what YouTube is really sending.
const REAL_PLAYER_URL = 'https://www.youtube.com/youtubei/v1/player?real=1';
const realPlayerResponse = () => nativeParse(fs.readFileSync(
    require('path').join(__dirname, 'youtube-player-response-captured.json'), 'utf8'));

// The shape this whole script exists to edit. Ad fields at the top level, one
// nested a level down, and one buried deep — the walk must reach all three, and
// must leave the video itself untouched.
function playerResponseFixture() {
    return {
        responseContext: { visitorData: 'CgtF' },
        playabilityStatus: { status: 'OK' },
        streamingData: { formats: [{ itag: 18, url: 'https://rr1.googlevideo.com/x' }] },
        videoDetails: { videoId: 'dQw4w9WgXcQ', title: 'The actual video' },
        adPlacements: [{ adPlacementRenderer: { config: {} } }],
        adSlots: [{ adSlotRenderer: { adSlotMetadata: {} } }],
        playerAds: [{ playerLegacyDesktopWatchAdsRenderer: {} }],
        adBreakHeartbeatParams: 'CAEQARgB',
        playerConfig: { audioConfig: { loudnessDb: 1.5 }, adBreakHeartbeatParams: 'nested' },
        contents: {
            twoColumnWatchNextResults: {
                results: { contents: [{ itemSectionRenderer: { adSlots: ['buried'] } }] },
            },
        },
    };
}

const AD_FIELDS = ['adPlacements', 'adSlots', 'playerAds', 'adBreakHeartbeatParams'];

const checks = [];
function check(name, condition, detail) {
    if (!condition) {
        throw new Error(`FAILED ${name}${detail ? `: ${detail}` : ''}`);
    }
    checks.push(name);
    console.log(`ok ${name}`);
}

// ---- browser stubs -------------------------------------------------------
// Deliberately thin: enough surface for the script to install its hooks, and
// nothing that would answer a question on its behalf.

const bodies = {
    [PLAYER_URL]: nativeStringify(playerResponseFixture()),
    // A non-player URL whose body is VALID JSON carrying the ad field names. It
    // is valid on purpose: if the URL guard is fake, the prune bites here and
    // the body comes back rewritten, which the pass-through check catches.
    [ASSET_URL]: '{"adPlacements":[1],"adSlots":[2],"note":"not a player response"}',
    [REAL_PLAYER_URL]: nativeStringify(realPlayerResponse()),
};

const warnings = [];
const sandbox = {
    console: Object.assign(Object.create(console), {
        warn: (...args) => { warnings.push(args.join(' ')); },
    }),
    Response,
    Set,
    Object,
    Array,
    JSON,
    isFinite,
    Promise,
};
sandbox.window = sandbox;
sandbox.globalThis = sandbox;
sandbox.location = {
    hostname,
    pathname: '/watch',
    href: `https://${hostname}/watch?v=dQw4w9WgXcQ`,
};
// A player the script can actually BIND to, so layer 2 (the DOM belt) is driven
// rather than grepped: a source needle cannot tell a working `defuse()` from one
// whose first statement is `return;`. Only what the script touches — the
// `ad-showing` class, a <video> with a duration, and the skip button — each
// reporting whether it was used.
function makePlayer(options) {
    const opts = options || {};
    const video = {
        duration: opts.duration === undefined ? 30 : opts.duration,
        currentTime: 0,
        playbackRate: 1,
    };
    const button = opts.skipButton
        ? { offsetHeight: 20, offsetParent: {}, clicks: 0, click() { this.clicks += 1; } }
        : null;
    const classes = new Set(['html5-video-player']);
    if (opts.adShowing) classes.add('ad-showing');
    return {
        isConnected: true,
        video,
        button,
        classList: {
            contains: (name) => classes.has(name),
            add: (name) => classes.add(name),
            remove: (name) => classes.delete(name),
        },
        querySelector(selector) {
            if (selector === 'video') return video;
            if (button && selector === opts.skipButton) return button;
            return null;
        },
    };
}
let currentPlayer = null;
const observers = [];
sandbox.document = {
    readyState: 'complete',
    documentElement: {},
    addEventListener() {},
    querySelector(selector) {
        if (selector === '.html5-video-player') return currentPlayer;
        return null;
    },
    querySelectorAll() {
        return [];
    },
};
const domListeners = [];
sandbox.addEventListener = (name, fn) => domListeners.push([name, fn]);
sandbox.MutationObserver = function MutationObserver(callback) {
    this.callback = callback;
    this.observe = () => observers.push(this);
    this.disconnect = () => {
        const at = observers.indexOf(this);
        if (at >= 0) observers.splice(at, 1);
    };
};
// No timers: the script must not be able to keep this process alive.
sandbox.setInterval = () => 0;
sandbox.clearInterval = () => {};
sandbox.setTimeout = () => 0;

let originalFetchCalls = 0;
const originalFetch = async function (input) {
    originalFetchCalls += 1;
    const url = typeof input === 'string' ? input : input.url;
    const body = Object.prototype.hasOwnProperty.call(bodies, url) ? bodies[url] : '{}';
    return new Response(body, { status: 200, headers: { 'content-type': 'application/json' } });
};
sandbox.fetch = originalFetch;

// XMLHttpRequest with `responseText`/`response` as PROTOTYPE ACCESSORS, which is
// what the real one has and what the script's instance-level shadowing needs.
function FakeXHR() {
    this._url = null;
    this._raw = null;
}
FakeXHR.prototype.open = function (method, url) {
    this._url = url;
};
FakeXHR.prototype.send = function () {
    this._raw = Object.prototype.hasOwnProperty.call(bodies, this._url) ? bodies[this._url] : '{}';
};
Object.defineProperty(FakeXHR.prototype, 'responseText', {
    configurable: true,
    get() {
        return this._raw;
    },
});
Object.defineProperty(FakeXHR.prototype, 'response', {
    configurable: true,
    get() {
        return this._raw;
    },
});
sandbox.XMLHttpRequest = FakeXHR;

vm.createContext(sandbox);
vm.runInContext(source, sandbox, { filename: scriptPath });

// ---- the self-guard case -------------------------------------------------

const onYouTube = /(^|\.)youtube\.com$/.test(hostname);
if (!onYouTube) {
    check('self-guard leaves the load flag unset off youtube', sandbox.__yga_loaded !== true);
    check('self-guard leaves window.fetch untouched off youtube', sandbox.fetch === originalFetch);
    check(
        'self-guard leaves XMLHttpRequest.prototype.send untouched off youtube',
        Object.prototype.hasOwnProperty.call(FakeXHR.prototype, 'send')
            && FakeXHR.prototype.send.toString().indexOf('_raw') !== -1,
    );
    // The parse hooks are the widest thing this script installs — `JSON.parse`
    // is called on every site in the browser, thousands of times a page. If the
    // host guard ever stops covering them, this blocker becomes a tax on the
    // whole web instead of a fix for one site.
    check('self-guard leaves JSON.parse untouched off youtube', sandbox.JSON.parse === nativeParse);
    check(
        'self-guard leaves Response.prototype.json untouched off youtube',
        Response.prototype.json === nativeResponseJson,
    );
    console.log(`ALL OK (${checks.length} checks, host=${hostname})`);
    process.exit(0);
}

check('the script loaded on a youtube host', sandbox.__yga_loaded === true);
check('the fetch hook replaced window.fetch', sandbox.fetch !== originalFetch);
check(
    'the XHR hook replaced XMLHttpRequest.prototype.send',
    FakeXHR.prototype.send.toString().indexOf('_raw') === -1,
);
check('the parse hook replaced JSON.parse', sandbox.JSON.parse !== nativeParse);
check(
    'the parse hook replaced Response.prototype.json',
    Response.prototype.json !== nativeResponseJson,
);

function assertPruned(label, data) {
    for (const field of AD_FIELDS) {
        check(`${label} drops ${field}`, data[field] === undefined, nativeStringify(data[field]));
    }
    check(
        `${label} drops the nested adBreakHeartbeatParams`,
        data.playerConfig && data.playerConfig.adBreakHeartbeatParams === undefined,
    );
    check(
        `${label} drops the buried adSlots`,
        data.contents.twoColumnWatchNextResults.results.contents[0].itemSectionRenderer.adSlots
            === undefined,
    );
    check(`${label} keeps streamingData`, data.streamingData.formats[0].itag === 18);
    check(`${label} keeps videoDetails`, data.videoDetails.title === 'The actual video');
    check(
        `${label} keeps playerConfig's non-ad fields`,
        data.playerConfig.audioConfig.loudnessDb === 1.5,
    );
    const text = nativeStringify(data);
    for (const field of AD_FIELDS) {
        check(`${label} leaves no trace of ${field}`, text.indexOf(field) === -1);
    }
}

async function main() {
    // 1. fetch, the path the modern player actually uses.
    const resp = await sandbox.fetch(PLAYER_URL);
    check('the fetch hook passes the request through to the real fetch', originalFetchCalls === 1);
    const prunedText = await resp.text();
    // THE TRAP THIS LOCKS. The fetch hook decides whether to rewrite a body by
    // parsing it and seeing whether anything was removed. If it parsed with the
    // HOOKED `JSON.parse`, the parse hook would clean the object first, the
    // fetch hook would find nothing left to remove, and it would hand back the
    // ORIGINAL TEXT — ad fields intact — to any caller that reads the body as
    // text. Two working layers, cancelling out, silently. Reading the raw text
    // (never the hooked parser) is the only way to see it.
    check(
        'the fetch hook REWRITES the body text, not just the object a parse yields',
        !AD_FIELDS.some((field) => prunedText.indexOf(field) !== -1),
        prunedText.slice(0, 200),
    );
    const pruned = nativeParse(prunedText);
    assertPruned('fetch', pruned);

    // 2. A URL the guard must NOT touch comes back byte-identical.
    const asset = await sandbox.fetch(ASSET_URL);
    const assetText = await asset.text();
    check('a non-player URL is passed through unrewritten', assetText === bodies[ASSET_URL]);

    // 3. XMLHttpRequest, the path older player builds use.
    const xhr = new sandbox.XMLHttpRequest();
    xhr.open('POST', PLAYER_URL);
    xhr.send();
    assertPruned('xhr.responseText', nativeParse(xhr.responseText));
    assertPruned('xhr.response', nativeParse(xhr.response));

    const plain = new sandbox.XMLHttpRequest();
    plain.open('GET', ASSET_URL);
    plain.send();
    check('a non-player XHR is passed through unrewritten', plain.responseText === bodies[ASSET_URL]);

    // 3b. THE PARSE FUNNEL — `JSON.parse`. This is the route the modern player
    // actually reads its answer through: measured on one cold watch-page load,
    // NINE parses of 60-260 KB strings, every one still carrying all four ad
    // fields, none of them through the fetch hook. Everything above this line
    // could work perfectly and the user would still see ads.
    //
    // Note this parses the SAME text the fetch case used. If the prune only
    // happened on the way in through `fetch`, this comes back unpruned.
    const parsedByHook = sandbox.JSON.parse(nativeStringify(playerResponseFixture()));
    assertPruned('JSON.parse', parsedByHook);
    check(
        'the JSON.parse hook records which funnel bit',
        sandbox.__yga_state.hooks.json_parse >= 1,
        nativeStringify(sandbox.__yga_state.hooks),
    );

    // …and the negative, which is the whole cost argument: a string with no ad
    // field in it is parsed and handed back untouched, with the same keys and
    // the same values. `JSON.parse` runs thousands of times a page.
    const innocentText = '{"a":1,"b":{"c":[1,2,3]},"note":"nothing to do with ads"}';
    const innocent = sandbox.JSON.parse(innocentText);
    check(
        'a JSON string with no ad field is returned untouched',
        nativeStringify(innocent) === innocentText,
        nativeStringify(innocent),
    );

    // 3c. THE PARSE FUNNEL — `Response.prototype.json`. The player replays
    // payloads it already holds by CONSTRUCTING a Response in JavaScript and
    // reading it back; measured, three such calls per cold load, each carrying
    // the four ad fields, each on a Response whose `.url` is the empty string
    // because nothing fetched it. A URL-matching hook cannot see those at all,
    // so a synthetic Response is exactly the case to lock.
    const synthetic = new Response(nativeStringify(playerResponseFixture()), {
        status: 200,
        headers: { 'content-type': 'application/json' },
    });
    check('a JS-constructed Response really has no url (or this proves nothing)',
        synthetic.url === '');
    // ⚠ THE JS PARSER IS TAKEN AWAY FOR THIS ONE, AND THAT IS THE POINT.
    // A browser implements `Response.prototype.json()` natively: it decodes the
    // body and parses it without ever consulting the page's `JSON.parse`, which
    // is exactly why this second hook has to exist. Node's implementation does
    // route through the live global `JSON.parse`, so leaving the parse hook in
    // place would let it do all the work and this check would pass on a script
    // whose `Response.json` hook was deleted. Restoring the native parser for
    // the duration makes node behave like the engine we ship on.
    const hookedParse = JSON.parse;
    JSON.parse = nativeParse;
    let syntheticJson;
    try {
        syntheticJson = await synthetic.json();
    } finally {
        JSON.parse = hookedParse;
    }
    assertPruned('Response.json', syntheticJson);
    check(
        'the Response.json hook records which funnel bit',
        sandbox.__yga_state.hooks.response_json >= 1,
        nativeStringify(sandbox.__yga_state.hooks),
    );

    // 4. The inline `var ytInitialPlayerResponse = {...}` a cold load ships.
    sandbox.ytInitialPlayerResponse = playerResponseFixture();
    assertPruned('ytInitialPlayerResponse', sandbox.ytInitialPlayerResponse);

    // 4b. THE REAL RESPONSE. The synthetic fixture above is a shape somebody
    // wrote down; this one is what YouTube actually served. If AD_FIELDS ever
    // stops naming the live ad fields, this check fails and the synthetic one
    // still passes — which is the whole reason it is here.
    const realBefore = realPlayerResponse();
    check(
        'the captured response really does carry ad fields (or this proves nothing)',
        realBefore.adPlacements !== undefined && realBefore.adBreakHeartbeatParams !== undefined,
    );
    const realResp = await sandbox.fetch(REAL_PLAYER_URL);
    const real = nativeParse(await realResp.text());
    for (const field of AD_FIELDS) {
        check(`real capture: ${field} is gone`, real[field] === undefined);
    }
    check(
        'real capture: no ad field name survives anywhere in the body',
        !AD_FIELDS.some((field) => nativeStringify(real).indexOf(field) !== -1),
    );
    for (const kept of ['streamingData', 'videoDetails', 'playabilityStatus', 'captions',
        'playerConfig', 'microformat', 'annotations', 'storyboards']) {
        if (realBefore[kept] === undefined) continue;
        check(`real capture: ${kept} survives`, real[kept] !== undefined);
    }
    check(
        'real capture: the video itself is intact',
        nativeStringify(real.streamingData) === nativeStringify(realBefore.streamingData)
            && nativeStringify(real.videoDetails) === nativeStringify(realBefore.videoDetails),
    );

    // 5. LAYER 2 — the DOM belt, DRIVEN. `dispatchNavigate` fires the same
    // `yt-navigate-finish` the script binds to, which is how a real SPA
    // transition reaches `ensureBound` → `onAdState` → `defuse`.
    const dispatchNavigate = () => {
        domListeners
            .filter(([name]) => name === 'yt-navigate-finish')
            .forEach(([, fn]) => fn({ type: 'yt-navigate-finish' }));
    };

    // (a) A skippable ad: the button the DOM offers must be clicked and counted.
    currentPlayer = makePlayer({ adShowing: true, skipButton: '.ytp-ad-skip-button' });
    dispatchNavigate();
    check('layer 2 clicks the skip button an ad offers', currentPlayer.button.clicks >= 1);
    check('…and counts the skip', sandbox.__yga_state.skipped >= 1);

    // (b) UNSKIPPABLE: no button, so the ad is SEEKED past — and the rate is
    // never touched. A forced playbackRate is clamped by WebKit to about 2×,
    // so it never skipped anything; it made the user watch every ad at double
    // speed while disguising a dead layer 1. That is the bug that was reported.
    currentPlayer = makePlayer({ adShowing: true, duration: 42 });
    dispatchNavigate();
    check('layer 2 seeks an unskippable ad to the end', currentPlayer.video.currentTime === 42);
    check('…and counts it', sandbox.__yga_state.forwarded >= 1);
    check(
        '…and NEVER touches playbackRate',
        currentPlayer.video.playbackRate === 1,
        `playbackRate=${currentPlayer.video.playbackRate}`,
    );

    // (c) An ad on screen means layer 1 missed, and the belt must SAY so. A
    // silent fallback is how "the blocker is broken in a weird way" reaches a
    // user instead of "the blocker needs attention".
    check('layer 2 warns that the network prune missed', warnings.length >= 1);
    check(
        '…and the warning names the world/AD_FIELDS check when nothing was pruned',
        warnings.some((line) => line.indexOf('youtube-adblock') !== -1),
        nativeStringify(warnings),
    );

    // (d) The negative that matters: with no ad on screen, nothing is touched.
    currentPlayer = makePlayer({ adShowing: false, skipButton: '.ytp-ad-skip-button' });
    dispatchNavigate();
    check('with no ad up, layer 2 clicks nothing', currentPlayer.button.clicks === 0);
    check('…and leaves the rate alone', currentPlayer.video.playbackRate === 1);

    console.log(`ALL OK (${checks.length} checks, host=${hostname})`);
}

main().then(
    () => process.exit(0),
    (err) => {
        console.error(String(err && err.message ? err.message : err));
        process.exit(1);
    },
);
