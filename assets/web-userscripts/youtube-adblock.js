// ==UserScript==
// @name        YouTube Ad Defense
// @version     1.0.0
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
// Two layers, deliberately:
//   1. NETWORK PRUNE (the real one). `window.fetch` and `XMLHttpRequest` are
//      patched at document-start — before the player script exists — so every
//      player response is pruned on the way in. The same prune runs over the
//      inline `ytInitialPlayerResponse` the first page load ships in its HTML.
//   2. DOM FALLBACK (the belt). If the response shape shifts and an ad reaches
//      the screen anyway, click the skip button, and fast-forward the ad video
//      when there is no skip button. This layer exists for the day layer 1
//      stops matching; it is not expected to fire.
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

    var state = { pruned: 0, skipped: 0, forwarded: 0 };
    window.__yga_state = state;

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
    function pruneText(text) {
        if (typeof text !== 'string' || !text) return text;
        var data;
        try {
            data = JSON.parse(text);
        } catch (e) {
            return text;
        }
        if (!pruneAds(data)) return text;
        try {
            return JSON.stringify(data);
        } catch (e) {
            return text;
        }
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
                    var out = pruneText(text);
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
                cacheOut = pruneText(raw);
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
                            pruneAds(raw);
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
    if (inlinePlayerResponse) pruneAds(inlinePlayerResponse);
    try {
        Object.defineProperty(window, 'ytInitialPlayerResponse', {
            configurable: true,
            get: function () {
                return inlinePlayerResponse;
            },
            set: function (value) {
                if (value && typeof value === 'object') pruneAds(value);
                inlinePlayerResponse = value;
            },
        });
    } catch (e) {
        // Already non-configurable: the eager prune above is all we get.
    }

    // ---- Layer 2: the DOM fallback -------------------------------------------
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
    var forcedRate = false;

    function defuse() {
        var player = boundPlayer;
        if (!player || !player.isConnected) return;
        var video = player.querySelector('video');
        if (!player.classList.contains('ad-showing')) {
            // The SAME <video> element plays the content once the ad ends, so a
            // forced rate MUST be handed back or the video itself runs fast.
            if (forcedRate && video) {
                forcedRate = false;
                video.playbackRate = 1;
            }
            return;
        }
        for (var i = 0; i < SKIP_SELECTORS.length; i++) {
            var button = player.querySelector(SKIP_SELECTORS[i]);
            if (button && (button.offsetParent || button.offsetHeight > 0)) {
                button.click();
                state.skipped += 1;
                return;
            }
        }
        // Unskippable: run the ad out instead of watching it. playbackRate first,
        // because some builds ignore a currentTime jump on an ad stream.
        if (video && isFinite(video.duration) && video.duration > 0) {
            forcedRate = true;
            video.playbackRate = 16;
            try {
                video.currentTime = video.duration;
            } catch (e) { /* seek refused: the rate still runs it out */ }
            state.forwarded += 1;
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
