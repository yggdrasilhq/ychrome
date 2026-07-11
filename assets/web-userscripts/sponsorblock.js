// yggterm bundled userscript: SponsorBlock substitute for ychrome surfaces.
// Auto-skips sponsor segments on YouTube using the community SponsorBlock API
// (https://sponsor.ajay.app). Injected at document-start into the top frame;
// deploy to ~/.yggterm/web-userscripts/ (shared across profiles) or a
// per-profile userscripts/ dir. Disable = rename away from .js.
(function () {
    'use strict';
    if (window.__ysb_loaded) return;
    if (!/(^|\.)youtube\.com$/.test(location.hostname)) return;
    window.__ysb_loaded = true;

    var API = 'https://sponsor.ajay.app/api/skipSegments';
    var CATEGORIES = ['sponsor', 'selfpromo', 'interaction'];
    var state = { videoId: null, segments: [], skipped: 0 };
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

    function fetchSegments(videoId) {
        var url = API + '?videoID=' + encodeURIComponent(videoId) +
            '&categories=' + encodeURIComponent(JSON.stringify(CATEGORIES));
        fetch(url).then(function (resp) {
            if (resp.status === 404) return [];
            if (!resp.ok) throw new Error('sponsorblock http ' + resp.status);
            return resp.json();
        }).then(function (rows) {
            if (state.videoId !== videoId) return;
            state.segments = (rows || []).map(function (row) {
                return { start: row.segment[0], end: row.segment[1], category: row.category };
            }).sort(function (a, b) { return a.start - b.start; });
        }).catch(function () {
            // Network/API failure: leave segments empty; never break playback.
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
