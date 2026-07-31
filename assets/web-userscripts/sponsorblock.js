// ==UserScript==
// @name        SponsorBlock
// @version     1.1.0
// @match       https://*.youtube.com/*
// @match       https://youtube.com/*
// @world       isolated
// @run-at      document-start
// ==/UserScript==
// yggterm bundled userscript: SponsorBlock substitute for ychrome surfaces.
// Auto-skips sponsor segments on YouTube using the community SponsorBlock API
// (https://sponsor.ajay.app).
//
// PRIVACY: it asks by HASH PREFIX, never by video id. The real extension does
// this and it is the reason it can be trusted; asking `/api/skipSegments?
// videoID=<id>` tells sponsor.ajay.app exactly what you are watching, every
// time. Instead the first 4 hex characters of SHA-256(videoID) go up,
// sponsor.ajay.app answers with every video sharing that prefix (77 rows for a
// sample prefix, measured), and the match happens here. There is deliberately
// NO fallback to the by-id endpoint: a privacy property that silently
// degrades is not a privacy property.
//
// Injected at document-start into the top frame;
// deploy to ~/.yggterm/web-userscripts/ (shared across profiles) or a
// per-profile userscripts/ dir. Disable = rename away from .js.
//
// @match keeps it off every non-YouTube tab — the engine does that matching, so
// this script costs nothing anywhere else. It still self-checks the hostname
// below: a copy of this file deployed by hand to a GUI too old to understand
// @match would otherwise run everywhere.
(function () {
    'use strict';
    if (window.__ysb_loaded) return;
    if (!/(^|\.)youtube\.com$/.test(location.hostname)) return;
    window.__ysb_loaded = true;

    // The PREFIX endpoint. `/api/skipSegments/<sha256-prefix>` returns every
    // video whose id hashes to that prefix, so the id itself never leaves.
    var API = 'https://sponsor.ajay.app/api/skipSegments/';
    var CATEGORIES = ['sponsor', 'selfpromo', 'interaction'];
    // How much of the hash to send. 4 is what the real extension uses: enough
    // to keep the answer small, short enough that the answer names thousands of
    // videos and identifies none.
    var HASH_PREFIX_LENGTH = 4;
    var state = { videoId: null, segments: [], skipped: 0, lookups: 0 };
    window.__ysb_state = state;

    function currentVideoId() {
        try {
            if (location.pathname === '/watch') {
                return new URLSearchParams(location.search).get('v');
            }
            var shorts = location.pathname.match(/^\/shorts\/([\w-]{6,})/);
            if (shorts) return shorts[1];
        } catch (e) { /* ignore */ }
        return null;
    }

    function hashPrefix(videoId) {
        // No crypto.subtle means no hash, and NO REQUEST. Falling back to the
        // by-id endpoint would trade the user's privacy for a feature without
        // telling them, which is not a trade this script gets to make.
        if (!window.crypto || !window.crypto.subtle || !window.TextEncoder) {
            return Promise.reject(new Error('no crypto.subtle: refusing to ask by video id'));
        }
        return window.crypto.subtle
            .digest('SHA-256', new TextEncoder().encode(videoId))
            .then(function (buffer) {
                var bytes = new Uint8Array(buffer);
                var hex = '';
                for (var i = 0; i < bytes.length && hex.length < HASH_PREFIX_LENGTH; i++) {
                    hex += (bytes[i] < 16 ? '0' : '') + bytes[i].toString(16);
                }
                return hex.slice(0, HASH_PREFIX_LENGTH);
            });
    }

    function fetchSegments(videoId) {
        hashPrefix(videoId).then(function (prefix) {
            state.lookups += 1;
            var url = API + prefix +
                '?categories=' + encodeURIComponent(JSON.stringify(CATEGORIES));
            return fetch(url).then(function (resp) {
                if (resp.status === 404) return [];
                if (!resp.ok) throw new Error('sponsorblock http ' + resp.status);
                return resp.json();
            });
        }).then(function (rows) {
            if (state.videoId !== videoId) return;
            // The prefix answer covers thousands of videos; ours is one of
            // them, and the match is made HERE, in the browser.
            var mine = (rows || []).filter(function (row) { return row.videoID === videoId; });
            var segments = [];
            mine.forEach(function (row) {
                (row.segments || []).forEach(function (seg) {
                    // `actionType` matters: only `skip` means "seek past this".
                    // A `mute` segment should be muted, not skipped, and a
                    // `full` one labels the whole video — treating either as a
                    // skip would jump the user out of content they wanted.
                    if (seg.actionType && seg.actionType !== 'skip') return;
                    // A segment the community voted down is one the community
                    // says is wrong. -1 is the real extension's threshold.
                    if (typeof seg.votes === 'number' && seg.votes < -1) return;
                    segments.push({
                        start: seg.segment[0],
                        end: seg.segment[1],
                        category: seg.category,
                    });
                });
            });
            state.segments = segments.sort(function (a, b) { return a.start - b.start; });
        }).catch(function () {
            // Network/API failure, or no crypto.subtle: leave segments empty
            // and never break playback.
        });
    }

    function toast(text) {
        try {
            var el = document.createElement('div');
            el.textContent = text;
            el.style.cssText = 'position:fixed;bottom:72px;right:16px;z-index:99999;' +
                'background:rgba(20,20,24,.92);color:#e8e8ea;padding:8px 14px;' +
                'border-radius:9px;font:13px system-ui;pointer-events:none;' +
                'transition:opacity .4s;opacity:1;';
            document.body.appendChild(el);
            setTimeout(function () { el.style.opacity = '0'; }, 1600);
            setTimeout(function () { el.remove(); }, 2100);
        } catch (e) { /* ignore */ }
    }

    function onTimeUpdate(event) {
        var video = event.target;
        if (!state.segments.length || video.paused) return;
        var t = video.currentTime;
        for (var i = 0; i < state.segments.length; i++) {
            var seg = state.segments[i];
            // Skip when inside a segment (with a small lead so the first
            // sponsor frame never shows). Seeking mid-segment re-skips.
            if (t >= seg.start && t < seg.end - 0.3) {
                video.currentTime = seg.end;
                state.skipped += 1;
                toast('Skipped ' + seg.category);
                break;
            }
        }
    }

    function rescan() {
        var videoId = currentVideoId();
        if (videoId === state.videoId) return;
        state.videoId = videoId;
        state.segments = [];
        if (videoId) fetchSegments(videoId);
    }

    // Media elements appear/replace across YouTube's SPA navigation; a
    // capture-phase listener on document sees timeupdate from all of them.
    document.addEventListener('timeupdate', onTimeUpdate, true);
    window.addEventListener('yt-navigate-finish', rescan, true);
    setInterval(rescan, 2000); // fallback for missed SPA transitions
    rescan();
})();
