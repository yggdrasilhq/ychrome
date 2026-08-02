// ==UserScript==
// @name        YouTube Ad Defense
// @version     1.2.0
// @match       https://*.youtube.com/*
// @match       https://youtube.com/*
// @world       main
// @run-at      document-start
// ==/UserScript==
// ychrome bundled userscript: YouTube ad defense.
//
// YouTube's ads are FIRST-PARTY — they arrive on the same origin, in the same
// response, as the video itself, so no URL-matching content blocker can touch
// them. The only thing that works is editing the player's own answer: YouTube
// asks `/youtubei/v1/player` what to play, and the reply carries the ad break
// schedule alongside the video. Delete those fields before the page reads them
// and the player has nothing to schedule.
//
// ⚠ THE TRANSPORT IS NOT ONLY `fetch`, AND ASSUMING IT WAS IS WHY ADS CAME BACK.
// Measured on www.youtube.com/watch in the ychrome engine on 2026-07-31, one
// cold load, with every entry point instrumented:
//
//   window.fetch → /youtubei/v1/player .... 1 call, and NOT the video's player
//                                           response (a 328-byte field probe)
//   JSON.parse(<60-260 KB string>) ........ 9 calls, EVERY ONE still carrying
//                                           adPlacements, adSlots, playerAds
//                                           and adBreakHeartbeatParams
//   Response.prototype.json() ............. 3 calls carrying the same fields,
//                                           on Responses whose `.url` is EMPTY
//                                           — i.e. built in JS by the page,
//                                           never seen by a fetch hook
//
// So the player reads its answer through `JSON.parse` and through
// JS-constructed `Response` objects, and a blocker that only wraps `fetch`,
// `XMLHttpRequest` and the inline `ytInitialPlayerResponse` prunes three copies
// while the player quietly parses a fourth. Hooking the two PARSE points is
// what closes it — they are the narrowest place every route has to pass
// through, whatever carried the bytes.
//
// Three layers, deliberately:
//   1. NETWORK PRUNE. `window.fetch` and `XMLHttpRequest` are patched at
//      document-start — before the player script exists — so every player
//      response is pruned on the way in. The same prune runs over the inline
//      `ytInitialPlayerResponse` the first page load ships in its HTML.
//   2. PARSE PRUNE (the one that actually covers the modern player).
//      `JSON.parse` and `Response.prototype.json` are the two funnels a player
//      response cannot avoid: whether the bytes came from fetch, XHR, a cache
//      replay, a prefetch or an inline script, an object only reaches the
//      player by passing through one of them.
//   3. DOM FALLBACK (the belt). If the response shape shifts and an ad reaches
//      the screen anyway, click the skip button, or seek past an unskippable
//      one. It also WARNS on the console the first time it fires, because an ad
//      on screen means the prune did not cover it, and that is the thing worth
//      knowing. It deliberately does NOT force playbackRate any more: WebKit
//      clamps a forced rate to about 2x, so the user watched every ad anyway,
//      faster and weirder, while the dead primary path stayed invisible.
//
// Injected at document-start into the top frame; self-guards to YouTube.
// Deploy to ~/.yggterm/web-userscripts/ (shared) or a per-profile userscripts/
// dir. Disable = rename away from .js.
(function () {
    'use strict';
    if (window.__yga_loaded) return;
    // `music.youtube.com` and `www.youtube.com` both match; anything else exits
    // before a single hook is installed.
    if (!/(^|\.)youtube\.com$/.test(location.hostname)) return;
    window.__yga_loaded = true;

    // ---- LOAD-BEARING YOUTUBE SHAPE ------------------------------------------
    // These are the field names YouTube puts in a player response to describe
    // its ads. This list IS the policy; everything below is plumbing. When
    // YouTube renames them, ads come back and THIS is the line to update —
    // check a `/youtubei/v1/player` response in devtools for the new names.
    //   adPlacements ........ the ad break schedule (pre/mid/post roll)
    //   adSlots ............. the newer slot-based encoding of the same thing
    //   playerAds ........... player-attached ad payloads
    //   adBreakHeartbeatParams  the token the player beats back with per break
    var AD_FIELDS = [
        'adPlacements',
        'adSlots',
        'playerAds',
        'adBreakHeartbeatParams',
    ];

    // Endpoints whose response carries a player response. `/youtubei/v1/player`
    // is the one that matters on a watch page; the others nest a playerResponse
    // for shorts, playlists and up-next.
    var AD_BEARING_PATHS = [
        '/youtubei/v1/player',
        '/youtubei/v1/next',
        '/youtubei/v1/reel/reel_item_watch',
        '/playlist',
    ];
    // --------------------------------------------------------------------------

    // ⚠ THE NATIVE PARSER, CAPTURED BEFORE WE HOOK IT. Layer 2 replaces
    // `JSON.parse`, and `pruneText` below parses a body in order to decide
    // whether to REWRITE it. If `pruneText` used the hooked parser, the hook
    // would prune the object first, `pruneAds` would then find nothing left to
    // remove, and `pruneText` would hand back the ORIGINAL TEXT — ad fields and
    // all — to a caller that reads the body as text rather than as JSON. The
    // two layers would cancel each other out and the failure would be silent.
    var nativeParse = JSON.parse;
    var nativeStringify = JSON.stringify;

    // `pruned` is the headline count a maintainer reads first; `hooks` says
    // WHICH funnel bit, which is the difference between "YouTube renamed a
    // field" and "YouTube moved the response to a route we do not watch".
    var state = {
        pruned: 0,
        skipped: 0,
        forwarded: 0,
        hooks: { fetch: 0, xhr: 0, inline: 0, json_parse: 0, response_json: 0 },
    };
    window.__yga_state = state;

    function pruneVia(hook, root) {
        var before = state.pruned;
        var removed = pruneAds(root);
        if (state.pruned !== before) state.hooks[hook] += 1;
        return removed;
    }

    function isAdBearingUrl(url) {
        if (!url) return false;
        for (var i = 0; i < AD_BEARING_PATHS.length; i++) {
            if (url.indexOf(AD_BEARING_PATHS[i]) !== -1) return true;
        }
        return false;
    }

    // Delete every AD_FIELDS key anywhere in `root`, in place. A walk rather
    // than a fixed path list because the same player response arrives top-level
    // on /player and nested under `playerResponse` elsewhere — one rule covers
    // both, and it keeps working when the nesting moves. Bounded so a
    // pathological response can never cost more than the JSON.parse that
    // produced it. Returns true when something was removed.
    function pruneAds(root) {
        if (!root || typeof root !== 'object') return false;
        var removed = 0;
        var budget = 100000;
        var seen = new Set();
        var stack = [root];
        while (stack.length && budget-- > 0) {
            var node = stack.pop();
            if (!node || typeof node !== 'object' || seen.has(node)) continue;
            seen.add(node);
            if (!Array.isArray(node)) {
                for (var i = 0; i < AD_FIELDS.length; i++) {
                    if (Object.prototype.hasOwnProperty.call(node, AD_FIELDS[i])) {
                        delete node[AD_FIELDS[i]];
                        removed += 1;
                    }
                }
            }
            for (var key in node) {
                var value = node[key];
                if (value && typeof value === 'object') stack.push(value);
            }
        }
        if (removed) state.pruned += removed;
        return removed > 0;
    }
    // Exposed for the repo's test harness and for a maintainer poking at a live
    // page (`__yga_prune(structuredClone(ytInitialPlayerResponse))`).
    window.__yga_prune = pruneAds;

    // Prune a JSON body given as text. Returns the ORIGINAL string untouched
    // when there was nothing to remove (or it was not JSON at all), so a
    // non-player response is never rewritten.
    function pruneText(hook, text) {
        if (typeof text !== 'string' || !text) return text;
        var data;
        try {
            // nativeParse, NOT JSON.parse — see the note where it is captured.
            data = nativeParse(text);
        } catch (e) {
            return text;
        }
        if (!pruneVia(hook, data)) return text;
        try {
            return nativeStringify(data);
        } catch (e) {
            return text;
        }
    }

    // A cheap pre-filter for the parse hooks: does this text even MENTION an ad
    // field? `JSON.parse` is on YouTube's hot path, so the hook must cost
    // nothing on the strings that are not player responses. Scanning is O(n)
    // and the parse the caller already asked for is more expensive than that,
    // so the ceiling on this is a small constant factor — and it is only paid
    // by strings that survive the length test.
    var AD_NEEDLES = [];
    // The shortest text that could contain the shortest needle at all: the
    // needle, plus the `{`, `:`, one character of value and `}` around it.
    // DERIVED from AD_FIELDS rather than chosen, so it cannot drift out of step
    // with the field list, and so it can never skip a body that had something
    // to remove.
    var MIN_PARSE_SCAN = 0;
    for (var needleAt = 0; needleAt < AD_FIELDS.length; needleAt++) {
        var needle = '"' + AD_FIELDS[needleAt] + '"';
        AD_NEEDLES.push(needle);
        if (MIN_PARSE_SCAN === 0 || needle.length + 4 < MIN_PARSE_SCAN) {
            MIN_PARSE_SCAN = needle.length + 4;
        }
    }
    function mentionsAnAdField(text) {
        for (var i = 0; i < AD_NEEDLES.length; i++) {
            if (text.indexOf(AD_NEEDLES[i]) !== -1) return true;
        }
        return false;
    }

    // Is this parsed object a player response, or something carrying one? Used
    // by the `Response.prototype.json` hook to decide whether to walk. The
    // measured shapes are all three of these: ad fields at the top level, a
    // `playerResponse` wrapper, and a bare player response identified by
    // `streamingData`/`videoDetails`.
    function carriesAPlayerResponse(data) {
        if (!data || typeof data !== 'object') return false;
        for (var i = 0; i < AD_FIELDS.length; i++) {
            if (Object.prototype.hasOwnProperty.call(data, AD_FIELDS[i])) return true;
        }
        return !!(data.playerResponse || data.streamingData || data.videoDetails);
    }

    // ---- Layer 1a: fetch -----------------------------------------------------
    // Captured at document-start, so `origFetch` is the browser's, not another
    // script's wrapper.
    var origFetch = window.fetch;
    if (typeof origFetch === 'function') {
        window.fetch = function (input, init) {
            var url = '';
            try {
                url = typeof input === 'string' ? input : (input && input.url) || '';
            } catch (e) { /* exotic input: fall through unpruned */ }
            var pending = origFetch.apply(this, arguments);
            if (!isAdBearingUrl(url)) return pending;
            return pending.then(function (resp) {
                if (!resp || !resp.ok || typeof resp.text !== 'function') return resp;
                // Reading the body consumes it, so a status that cannot carry
                // one must be handed back untouched: re-wrapping it would throw
                // and leave the caller a Response it can no longer read.
                if (resp.status === 204 || resp.status === 205) return resp;
                return resp.text().then(function (text) {
                    var out = pruneText('fetch', text);
                    // Nothing removed: hand back a Response with the identical
                    // body rather than the consumed original.
                    return new Response(out, {
                        status: resp.status,
                        statusText: resp.statusText,
                        headers: resp.headers,
                    });
                }).catch(function () {
                    return resp;
                });
            });
        };
    }

    // ---- Layer 1b: XMLHttpRequest -------------------------------------------
    // The player still reaches for XHR on some builds. The pruned body is served
    // through INSTANCE-level accessors installed at send() time, which shadow
    // the prototype's: order-independent, so it does not matter whether the page
    // registered its load handler before or after us.
    var xhrProto = window.XMLHttpRequest && window.XMLHttpRequest.prototype;
    if (xhrProto) {
        var origOpen = xhrProto.open;
        var origSend = xhrProto.send;
        var textDesc = Object.getOwnPropertyDescriptor(xhrProto, 'responseText');
        var respDesc = Object.getOwnPropertyDescriptor(xhrProto, 'response');

        var guard = function (xhr) {
            var cacheIn = null;
            var cacheOut = null;
            var prunedOnce = function (raw) {
                if (raw === cacheIn) return cacheOut;
                cacheIn = raw;
                cacheOut = pruneText('xhr', raw);
                return cacheOut;
            };
            if (textDesc && textDesc.get) {
                Object.defineProperty(xhr, 'responseText', {
                    configurable: true,
                    get: function () {
                        return prunedOnce(textDesc.get.call(this));
                    },
                });
            }
            if (respDesc && respDesc.get) {
                Object.defineProperty(xhr, 'response', {
                    configurable: true,
                    get: function () {
                        var raw = respDesc.get.call(this);
                        // responseType 'json' hands back a live object; prune it
                        // in place. 'text'/'' hands back a string. Anything else
                        // (blob, arraybuffer) is not a player response.
                        if (typeof raw === 'string') return prunedOnce(raw);
                        if (raw && typeof raw === 'object') {
                            pruneVia('xhr', raw);
                            return raw;
                        }
                        return raw;
                    },
                });
            }
        };

        xhrProto.open = function (method, url) {
            try {
                this.__yga_url = String(url || '');
            } catch (e) { /* ignore */ }
            return origOpen.apply(this, arguments);
        };
        xhrProto.send = function () {
            try {
                if (isAdBearingUrl(this.__yga_url)) guard(this);
            } catch (e) { /* ignore */ }
            return origSend.apply(this, arguments);
        };
    }

    // ---- Layer 1c: the inline player response --------------------------------
    // A cold load ships the first player response inside the HTML as
    // `var ytInitialPlayerResponse = {...}` — no request to intercept. A setter
    // on window catches that assignment (a top-level `var` assigns through an
    // existing accessor), and the eager prune covers a page that already ran.
    var inlinePlayerResponse = window.ytInitialPlayerResponse;
    if (inlinePlayerResponse) pruneVia('inline', inlinePlayerResponse);
    try {
        Object.defineProperty(window, 'ytInitialPlayerResponse', {
            configurable: true,
            get: function () {
                return inlinePlayerResponse;
            },
            set: function (value) {
                if (value && typeof value === 'object') pruneVia('inline', value);
                inlinePlayerResponse = value;
            },
        });
    } catch (e) {
        // Already non-configurable: the eager prune above is all we get.
    }

    // ---- Layer 2a: JSON.parse ------------------------------------------------
    // THE FUNNEL. Layer 1 wraps three ways bytes can ARRIVE; this wraps the one
    // way they become an object the player can read. Measured on a single cold
    // watch-page load: nine `JSON.parse` calls on 60-260 KB strings, every one
    // of them still carrying all four ad fields, every one from YouTube's own
    // `kevlar_base_module` — none of which passed through the fetch hook. That
    // gap is the whole of "the userscript runs, the fields are gone from
    // `ytInitialPlayerResponse`, and the user still sees ads".
    //
    // The object is edited IN PLACE and handed straight back, so a caller's
    // reference, prototype and property order are the parser's, not ours. No
    // re-serialisation: a player response round-tripped through
    // stringify/parse would be a different object graph for no reason.
    var origParse = JSON.parse;
    JSON.parse = function (text) {
        var out = origParse.apply(this, arguments);
        try {
            if (typeof text === 'string' && text.length >= MIN_PARSE_SCAN
                && mentionsAnAdField(text)) {
                pruneVia('json_parse', out);
            }
        } catch (e) { /* a prune must never break the page's own parse */ }
        return out;
    };

    // ---- Layer 2b: Response.prototype.json -----------------------------------
    // The player replays payloads it already holds by BUILDING a `Response` in
    // JavaScript and reading it back — measured: three such calls per cold
    // load, each carrying the four ad fields, each on a Response whose `.url`
    // is the empty string because nothing ever fetched it. A URL-matching hook
    // is structurally unable to see those, which is why the empty URL is
    // treated as a reason TO prune rather than a reason to skip.
    var responseProto = typeof Response !== 'undefined' && Response.prototype;
    if (responseProto && typeof responseProto.json === 'function') {
        var origJson = responseProto.json;
        responseProto.json = function () {
            var url = '';
            try {
                url = this.url || '';
            } catch (e) { /* an exotic Response: treat it as urlless */ }
            var pending = origJson.apply(this, arguments);
            if (!pending || typeof pending.then !== 'function') return pending;
            return pending.then(function (data) {
                try {
                    if (url === '' || isAdBearingUrl(url) || carriesAPlayerResponse(data)) {
                        pruneVia('response_json', data);
                    }
                } catch (e) { /* ignore */ }
                return data;
            });
        };
    }

    // ---- Layer 3: the DOM fallback -------------------------------------------
    // `ad-showing` is the class YouTube puts on the player container while an ad
    // is on screen — the one bit that says "right now". Watching just that
    // attribute keeps this observer off the hot path of YouTube's DOM churn.
    var SKIP_SELECTORS = [
        '.ytp-skip-ad-button',
        '.ytp-ad-skip-button',
        '.ytp-ad-skip-button-modern',
    ];
    var boundPlayer = null;
    var playerObserver = null;
    var adPoll = null;
    var warnedUnpruned = false;

    // AN AD ON SCREEN MEANS THE PRUNE DID NOT FIRE. Layer 3 exists to soften
    // that, never to hide it — this warning is the difference between a user
    // who knows the blocker needs attention and one who just thinks it is
    // broken in a weird way. The counters separate the failures a maintainer
    // would chase, and `state.hooks` is the one that matters most now:
    //   pruned === 0        -> nothing bit at all. Either AD_FIELDS has been
    //                          renamed by YouTube, or the script is running in
    //                          the ISOLATED world, where its patches are
    //                          invisible to the page (see crate::provision for
    //                          how exactly that happened once).
    //   hooks.json_parse and hooks.response_json both 0, others non-zero
    //                       -> the funnels never saw a player response. That is
    //                          the shape of a NEW transport, and the thing to
    //                          measure is which call the player parses through.
    //   pruned > 0          -> the prune works and this break arrived carrying
    //                          a field AD_FIELDS does not name.
    function warnLayerOneMissed() {
        if (warnedUnpruned) return;
        warnedUnpruned = true;
        try {
            console.warn(
                '[ychrome youtube-adblock] an ad reached the player, so the prune ' +
                'did not cover it. Pruned fields so far: ' + state.pruned + '. ' +
                'Per hook: ' + nativeStringify(state.hooks) + '. ' +
                (state.pruned === 0
                    ? 'NOTHING has been pruned this session — check that this script runs in ' +
                      'the MAIN world (@world main) and that AD_FIELDS still matches a live ' +
                      '/youtubei/v1/player response.'
                    : 'The prune is working, so this break arrived carrying a field ' +
                      'AD_FIELDS does not name.')
            );
        } catch (e) { /* no console: the counters still carry it */ }
    }

    function defuse() {
        var player = boundPlayer;
        if (!player || !player.isConnected) return;
        if (!player.classList.contains('ad-showing')) return;
        warnLayerOneMissed();
        for (var i = 0; i < SKIP_SELECTORS.length; i++) {
            var button = player.querySelector(SKIP_SELECTORS[i]);
            if (button && (button.offsetParent || button.offsetHeight > 0)) {
                button.click();
                state.skipped += 1;
                return;
            }
        }
        // Unskippable: seek past it. The seek is INVISIBLE when it works and a
        // no-op when the build refuses it.
        //
        // There used to be a forced playback rate of 16x here, and removing it
        // is the point of this version. WebKit clamps that to about 2x, so the
        // user still watched every ad, just faster and stranger — which is
        // precisely what they reported ("I still see youtube ads! They are sped
        // up to 2x automatically!") while the real cause was that layer 1 was
        // dead. A fallback that visibly degrades playback AND disguises a
        // broken primary path is worse than no fallback: it converts a legible
        // failure into a mystery. The warning above replaces it.
        var video = player.querySelector('video');
        if (video && isFinite(video.duration) && video.duration > 0) {
            try {
                video.currentTime = video.duration;
                state.forwarded += 1;
            } catch (e) { /* seek refused: the ad plays, honestly */ }
        }
    }

    function onAdState() {
        var showing = !!(boundPlayer && boundPlayer.classList.contains('ad-showing'));
        if (showing && !adPoll) {
            // Poll only WHILE an ad is up: the skip button appears seconds after
            // the class does, and that arrival is a child insertion, not a class
            // change. Idle cost is zero.
            adPoll = setInterval(defuse, 250);
        } else if (!showing && adPoll) {
            clearInterval(adPoll);
            adPoll = null;
        }
        defuse();
    }

    function ensureBound() {
        var player = document.querySelector('.html5-video-player');
        if (!player) return;
        if (player === boundPlayer && player.isConnected) return;
        boundPlayer = player;
        if (playerObserver) playerObserver.disconnect();
        playerObserver = new MutationObserver(onAdState);
        playerObserver.observe(player, { attributes: true, attributeFilter: ['class'] });
        onAdState();
    }

    // SPA navigation replaces the player; rebind on YouTube's own event, with a
    // cheap interval as the fallback for a missed transition (one querySelector).
    window.addEventListener('yt-navigate-finish', ensureBound, true);
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', ensureBound, { once: true });
    }
    setInterval(ensureBound, 2000);
    ensureBound();
})();
