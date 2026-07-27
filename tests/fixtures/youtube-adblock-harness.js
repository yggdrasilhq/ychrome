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

const scriptPath = process.argv[2];
const hostname = process.argv[3] || 'www.youtube.com';
const source = fs.readFileSync(scriptPath, 'utf8');

const PLAYER_URL = 'https://www.youtube.com/youtubei/v1/player?key=AIza&prettyPrint=false';
const ASSET_URL = 'https://www.youtube.com/s/player/abc123/base.js';

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
    [PLAYER_URL]: JSON.stringify(playerResponseFixture()),
    // A non-player URL whose body is VALID JSON carrying the ad field names. It
    // is valid on purpose: if the URL guard is fake, the prune bites here and
    // the body comes back rewritten, which the pass-through check catches.
    [ASSET_URL]: '{"adPlacements":[1],"adSlots":[2],"note":"not a player response"}',
};

const sandbox = {
    console,
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
sandbox.document = {
    readyState: 'complete',
    documentElement: {},
    addEventListener() {},
    querySelector() {
        return null;
    },
    querySelectorAll() {
        return [];
    },
};
const domListeners = [];
sandbox.addEventListener = (name, fn) => domListeners.push([name, fn]);
sandbox.MutationObserver = function MutationObserver() {
    this.observe = () => {};
    this.disconnect = () => {};
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
    console.log(`ALL OK (${checks.length} checks, host=${hostname})`);
    process.exit(0);
}

check('the script loaded on a youtube host', sandbox.__yga_loaded === true);
check('the fetch hook replaced window.fetch', sandbox.fetch !== originalFetch);
check(
    'the XHR hook replaced XMLHttpRequest.prototype.send',
    FakeXHR.prototype.send.toString().indexOf('_raw') === -1,
);

function assertPruned(label, data) {
    for (const field of AD_FIELDS) {
        check(`${label} drops ${field}`, data[field] === undefined, JSON.stringify(data[field]));
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
    const text = JSON.stringify(data);
    for (const field of AD_FIELDS) {
        check(`${label} leaves no trace of ${field}`, text.indexOf(field) === -1);
    }
}

async function main() {
    // 1. fetch, the path the modern player actually uses.
    const resp = await sandbox.fetch(PLAYER_URL);
    check('the fetch hook passes the request through to the real fetch', originalFetchCalls === 1);
    const pruned = JSON.parse(await resp.text());
    assertPruned('fetch', pruned);

    // 2. A URL the guard must NOT touch comes back byte-identical.
    const asset = await sandbox.fetch(ASSET_URL);
    const assetText = await asset.text();
    check('a non-player URL is passed through unrewritten', assetText === bodies[ASSET_URL]);

    // 3. XMLHttpRequest, the path older player builds use.
    const xhr = new sandbox.XMLHttpRequest();
    xhr.open('POST', PLAYER_URL);
    xhr.send();
    assertPruned('xhr.responseText', JSON.parse(xhr.responseText));
    assertPruned('xhr.response', JSON.parse(xhr.response));

    const plain = new sandbox.XMLHttpRequest();
    plain.open('GET', ASSET_URL);
    plain.send();
    check('a non-player XHR is passed through unrewritten', plain.responseText === bodies[ASSET_URL]);

    // 4. The inline `var ytInitialPlayerResponse = {...}` a cold load ships.
    sandbox.ytInitialPlayerResponse = playerResponseFixture();
    assertPruned('ytInitialPlayerResponse', sandbox.ytInitialPlayerResponse);

    console.log(`ALL OK (${checks.length} checks, host=${hostname})`);
}

main().then(
    () => process.exit(0),
    (err) => {
        console.error(String(err && err.message ? err.message : err));
        process.exit(1);
    },
);
