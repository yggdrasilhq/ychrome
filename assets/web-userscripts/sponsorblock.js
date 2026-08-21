// ==UserScript==
// @name        SponsorBlock
// @version     2.1.0
// @match       https://*.youtube.com/*
// @match       https://youtube.com/*
// @world       isolated
// @run-at      document-start
// ==/UserScript==
// ychrome's SponsorBlock client. Queries the community API at
// https://sponsor.ajay.app and acts on the segments it returns.
//
// PROVENANCE. The category names, their action types, the vote and lock rules
// and the seek-bar colours come from the SponsorBlock project (GPL-3.0) and its
// public API; ychrome is GPL-3.0-or-later, so adopting them is lawful and they
// are attributed in THIRD-PARTY-NOTICES.md rather than folded in silently. The
// code here was written against the API's documented behaviour; no SponsorBlock
// source is copied.
//
// ⚠ THE SEGMENT DATABASE IS CC BY-NC-SA 4.0. The clause governs DISTRIBUTION,
// and the line is: no segment data in a released binary. A user's own browser
// fetching segments for the video they are watching is not distribution, and
// neither is keeping what it fetched — so the session cache below is fine, and
// a persistent one would be too. `sponsorblock.rs::no_segment_data_is_baked_
// into_the_binary` locks the half that is not fine. Attribution is owed either
// way and is in THIRD-PARTY-NOTICES.md.
//
// PRIVACY. It asks by HASH PREFIX, never by video id. Asking
// `/api/skipSegments?videoID=<id>` tells sponsor.ajay.app exactly what you are
// watching, every time. Instead the first 4 hex characters of SHA-256(videoID)
// go up, the server answers with every video sharing that prefix, and the match
// happens here. There is deliberately NO fallback to the by-id endpoint: a
// privacy property that silently degrades is not a privacy property.
//
// WORLD. `isolated`, and it stays there. This script patches no page API — it
// reads the DOM (the player, its <video>, its progress bar) and calls `fetch`,
// all of which an isolated world has. Being isolated keeps its globals out of
// YouTube's reach and YouTube's out of ours.
//
// INSTRUMENT. An isolated-world global is INVISIBLE to a page-world `eval`, so
// probing `window.__ysb` from `ychrome ctl eval` reads `undefined` on a working
// script and has already cost one investigation. The state is therefore also
// published to the DOM, which both worlds share:
//
//     ychrome ctl eval page_id=<id> \
//       js='document.documentElement.getAttribute("data-ysb")'
//
// SETTINGS. `window.__ysbConfig` is injected beside this script by ychrome from
// ~/.yggterm/web-userscripts/sponsorblock.config.json (see src/sponsorblock.rs,
// which owns the catalogue). DEFAULTS below is the fallback for a copy of this
// file deployed by hand with nothing to configure it, and a Rust test locks the
// two tables together.
// Is `host` a place this script may act?
//
// YouTube by name, then the user's own list from `window.__ysbConfig.hosts`. A
// configured host matches itself and its sub-domains, which is what the engine's
// `@match` patterns do — the two must agree.
//
// ⛔ Compared as whole LABELS, never as a substring: `indexOf` would let
// `notyoutube.com.evil.test` through, and the configured list is exactly the
// place a typo becomes a permission.
function ysbHostAllowed(host) {
    host = String(host || '').toLowerCase();
    var allowed = ['youtube.com'];
    try {
        var extra = (window.__ysbConfig && window.__ysbConfig.hosts) || [];
        for (var i = 0; i < extra.length; i += 1) {
            if (typeof extra[i] === 'string' && extra[i]) allowed.push(extra[i].toLowerCase());
        }
    } catch (_error) {
        // A malformed config must not cost the user YouTube itself.
    }
    for (var j = 0; j < allowed.length; j += 1) {
        if (host === allowed[j] || host.endsWith('.' + allowed[j])) return true;
    }
    return false;
}

(function () {
    'use strict';
    if (window.__ysb) return;
    // WHERE THIS RUNS. YouTube always, plus any host the user configured
    // (src/sponsorblock.rs owns the list and injects it here). A front-end
    // serving YouTube's catalogue serves the same VIDEO IDS, so the community
    // database answers for it exactly as it does for YouTube.
    //
    // ⛔ The engine has already gated injection on the matching `@match`
    // patterns; this is the second half of the same rule, and both come from
    // one owner. A gate that disagreed with the patterns would give a script
    // that loads and then refuses to act — which looks like a broken feature
    // rather than a misconfigured one.
    //
    // ⚠ No host is hardcoded here beyond YouTube itself. An instance address is
    // the user's own infrastructure and belongs in their config, not in a
    // shipped asset.
    if (!ysbHostAllowed(location.hostname)) return;

    // ---------------------------------------------------------------- config

    // ⚠ MIRRORED IN src/sponsorblock.rs. `the_script_defaults_match_this_module`
    // parses this exact table out of this file; change one side only and it
    // goes red.
    var DEFAULTS = {
        sponsor: 'auto',
        selfpromo: 'auto',
        interaction: 'auto',
        intro: 'manual',
        outro: 'manual',
        preview: 'manual',
        music_offtopic: 'manual',
        filler: 'off',
        poi_highlight: 'show',
        exclusive_access: 'show',
        chapter: 'show'
    };
    var FALLBACK_COLORS = {
        sponsor: '#00d400',
        selfpromo: '#ffff00',
        interaction: '#cc00ff',
        intro: '#00ffff',
        outro: '#0202ed',
        preview: '#008fd6',
        music_offtopic: '#ff9900',
        filler: '#7300ff',
        poi_highlight: '#ff1684',
        exclusive_access: '#008a5c',
        chapter: '#ffd983'
    };
    var LABELS = {
        sponsor: 'sponsor',
        selfpromo: 'self-promotion',
        interaction: 'interaction reminder',
        intro: 'intro',
        outro: 'endcards',
        preview: 'preview',
        music_offtopic: 'non-music section',
        filler: 'filler',
        poi_highlight: 'highlight',
        exclusive_access: 'exclusive access',
        chapter: 'chapter'
    };

    // ⛔⛔ THE CATEGORIES LIVE UNDER `.categories`, AND READING THEM ONE LEVEL
    // TOO HIGH IS A BUG THAT SHIPPED. Until v2.1.0 this read `injected[id]`
    // while ychrome has always written `{categories:{...}, hosts:[...]}` — so
    // EVERY per-category choice made in the settings pane was silently ignored
    // and the table below was what actually ran. Nothing reported it: the pane
    // stored the choice, the preamble carried it, and the script read past it.
    //
    // ⚠ The old test could not catch it. It asserted that each category id and
    // colour APPEARED somewhere in the preamble text, which is true of the
    // nested shape too. `the_script_reads_the_behaviour_ychrome_writes` now runs
    // this file against a real preamble under node and reads the answer back.
    function config() {
        var injected = window.__ysbConfig;
        var table = (injected && injected.categories) || {};
        var out = {};
        for (var id in DEFAULTS) {
            if (!Object.prototype.hasOwnProperty.call(DEFAULTS, id)) continue;
            var entry = table[id];
            var behaviour = entry && typeof entry.behaviour === 'string'
                ? entry.behaviour
                : DEFAULTS[id];
            out[id] = {
                behaviour: behaviour,
                color: (entry && entry.color) || FALLBACK_COLORS[id] || '#888888'
            };
        }
        return out;
    }

    // The settings that are not per-category. `src/sponsorblock.rs` owns the
    // defaults; these are the fallback for a copy of this file deployed by hand
    // with nothing to configure it, and they must agree with that module — which
    // `the_script_preference_defaults_match_this_module` locks.
    function prefs() {
        var injected = window.__ysbConfig || {};
        var min = typeof injected.min_duration_secs === 'number' && isFinite(injected.min_duration_secs)
            ? Math.max(0, Math.min(30, injected.min_duration_secs))
            : 0;
        return {
            skip_notice: injected.skip_notice !== false,
            seek_bar_markers: injected.seek_bar_markers !== false,
            min_duration_secs: min,
            // ⛔ Off unless ychrome says otherwise, in BOTH directions: a copy of
            // this file with no preamble must never start contributing to a
            // shared public database on its own.
            voting: injected.voting === true,
            submission: injected.submission === true,
            // The write credential. Present only while contributing is on.
            userId: typeof injected.private_user_id === 'string' ? injected.private_user_id : null
        };
    }

    // --------------------------------------------------------------- the API

    // The PREFIX endpoint. `/api/skipSegments/<sha256-prefix>` returns every
    // video whose id hashes to that prefix, so the id itself never leaves.
    var API = 'https://sponsor.ajay.app/api/skipSegments/';
    // 4 is what the real extension uses: enough to keep the answer small, short
    // enough that the answer names thousands of videos and identifies none.
    var HASH_PREFIX_LENGTH = 4;
    // Two answers to the same question must not both be believed: a segment
    // whose submitter recorded a different video length was submitted against a
    // different cut of the video, and its timestamps mean nothing here.
    var DURATION_TOLERANCE = 2;
    // The community's own verdict. -1 is the real extension's threshold.
    var MIN_VOTES = -1;

    var state = {
        videoId: null,
        segments: [],   // acted on: duration-matched, vote-filtered, merged
        raw: [],        // as the API answered, before the duration filter
        skipped: 0,
        lookups: 0,
        duration: null,
        lastError: null,
        cached: false,  // this video's answer came from the session cache
        ignored: {}     // UUIDs the user undid, this video, this page-life only
    };

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

    // What to ask for, derived from the settings. The API defaults to
    // `categories=["sponsor"]` and `actionTypes=["skip"]`, and NOT overriding
    // those was the whole bug in v1: eight of the eleven categories, and every
    // mute/full/poi/chapter segment, were invisible to a script that then
    // looked broken to anyone watching a video whose only segment was an intro.
    function query(cfg) {
        var categories = [];
        var actionTypes = ['skip'];
        var anySkippable = false;
        for (var id in cfg) {
            if (!Object.prototype.hasOwnProperty.call(cfg, id)) continue;
            if (cfg[id].behaviour === 'off') continue;
            categories.push(id);
            if (id !== 'poi_highlight' && id !== 'chapter') anySkippable = true;
        }
        if (!categories.length) return null;
        // A `mute` or `full` segment can be submitted under any skippable
        // category, so they ride along with the categories rather than with a
        // per-category setting.
        if (anySkippable) actionTypes.push('mute', 'full');
        if (cfg.poi_highlight && cfg.poi_highlight.behaviour !== 'off') actionTypes.push('poi');
        if (cfg.chapter && cfg.chapter.behaviour !== 'off') actionTypes.push('chapter');
        return '?categories=' + encodeURIComponent(JSON.stringify(categories)) +
            '&actionTypes=' + encodeURIComponent(JSON.stringify(actionTypes));
    }

    // A SESSION cache, deliberately in memory and deliberately not on disk.
    //
    // What it is for: YouTube's back button, autoplay-then-back, and a page
    // reload all re-ask for a video this tab already looked up. One request per
    // video WATCHED is the honest cost; one per video VISIT is waste, both for
    // the user and for a volunteer-run API.
    //
    // Why not persistent: the licence does not forbid it (see the header), but
    // the data is community-EDITED. A downvote or a retraction has to take
    // effect, and a cache on disk turns that into a TTL knob trading
    // correctness for a request the browser makes at most once per video. The
    // repeats that actually happen are within one browsing session, which is
    // exactly what this covers.
    var CACHE = {};
    var CACHE_ORDER = [];
    var CACHE_MAX = 64;
    var CACHE_TTL = 30 * 60 * 1000;

    function cacheGet(key) {
        var hit = CACHE[key];
        if (!hit) return null;
        if (Date.now() - hit.at > CACHE_TTL) { cacheDrop(key); return null; }
        return hit.rows;
    }

    function cacheDrop(key) {
        delete CACHE[key];
        var at = CACHE_ORDER.indexOf(key);
        if (at >= 0) CACHE_ORDER.splice(at, 1);
    }

    function cachePut(key, rows) {
        if (!CACHE[key]) CACHE_ORDER.push(key);
        CACHE[key] = { rows: rows, at: Date.now() };
        while (CACHE_ORDER.length > CACHE_MAX) cacheDrop(CACHE_ORDER[0]);
    }

    function fetchSegments(videoId) {
        var params = query(config());
        if (!params) { publish(); return; }
        // Keyed by the QUERY too: a settings change changes what the API would
        // answer, and a hit from before it would be the old answer wearing the
        // new settings' name.
        var key = videoId + '|' + params;
        var cached = cacheGet(key);
        if (cached) {
            state.raw = cached;
            state.lastError = null;
            state.cached = true;
            recompute();
            return;
        }
        hashPrefix(videoId).then(function (prefix) {
            state.lookups += 1;
            return fetch(API + prefix + params).then(function (resp) {
                if (resp.status === 404) return [];
                if (!resp.ok) throw new Error('sponsorblock http ' + resp.status);
                return resp.json();
            });
        }).then(function (rows) {
            if (state.videoId !== videoId) return;
            // The prefix answer covers thousands of videos; ours is one of
            // them, and the match is made HERE, in the browser.
            var mine = [];
            (rows || []).forEach(function (row) {
                if (row.videoID !== videoId) return;
                (row.segments || []).forEach(function (seg) {
                    if (!seg || !seg.segment || seg.segment.length !== 2) return;
                    if (typeof seg.votes === 'number' && seg.votes < MIN_VOTES) return;
                    mine.push(seg);
                });
            });
            // ⚠ Only OUR video's rows are kept. The prefix answer names ~120
            // other videos, and holding those would be keeping a slice of the
            // database rather than the answer to our own question.
            cachePut(key, mine);
            state.raw = mine;
            state.lastError = null;
            state.cached = false;
            recompute();
        }).catch(function (error) {
            if (state.videoId !== videoId) return;
            // Never break playback. Record it so the DOM instrument can say
            // "asked and failed" rather than looking the same as "never asked".
            state.lastError = String(error && error.message ? error.message : error);
            publish();
        });
    }

    // ------------------------------------------------------- segment shaping

    // A LOCKED segment is one the community has settled. Where a category has
    // one, the unlocked submissions for that category are noise on top of a
    // decided answer, so they are dropped rather than merged in.
    function preferLocked(segments) {
        var locked = {};
        segments.forEach(function (seg) {
            if (seg.locked) locked[seg.category] = true;
        });
        return segments.filter(function (seg) {
            return !locked[seg.category] || seg.locked;
        });
    }

    // The shortest segment worth acting on. A sub-second submission fires a
    // notice for a seek nobody would have noticed; past a few seconds the knob
    // stops filtering noise and starts hiding sponsors, so ychrome caps it.
    //
    // ⛔ LABEL segments are exempt. `poi_highlight` is a single INSTANT and
    // `exclusive_access` is a whole-video notice — measuring either against a
    // minimum length would silently delete the two categories that have no
    // length to speak of.
    function longEnough(seg) {
        var min = prefs().min_duration_secs;
        if (!min) return true;
        if (seg.actionType === 'poi' || seg.actionType === 'full') return true;
        return (seg.end - seg.start) >= min;
    }

    function durationOk(seg, duration) {
        // 0 means the submitter recorded no duration: no claim, so no refusal.
        if (!seg.videoDuration) return true;
        if (!duration || !isFinite(duration)) return true;
        return Math.abs(seg.videoDuration - duration) <= DURATION_TOLERANCE;
    }

    // What this script will DO with one segment: its category's setting crossed
    // with the action type the submitter chose. The submitter's action type
    // wins where the two disagree about kind — a `full` segment labels the whole
    // video, and seeking to the end of it would skip the video.
    function behaviourOf(seg, cfg) {
        var setting = cfg[seg.category];
        if (!setting || setting.behaviour === 'off') return null;
        var action = seg.actionType || 'skip';
        if (action === 'full') return 'label';
        if (action === 'poi') return 'highlight';
        if (action === 'chapter') return 'chapter';
        if (action === 'mute') return 'mute';
        if (action !== 'skip') return null;
        if (setting.behaviour === 'auto') return 'auto';
        if (setting.behaviour === 'manual') return 'manual';
        if (setting.behaviour === 'mute') return 'mute';
        return 'marker';
    }

    // Overlapping submissions of the same category and the same behaviour are
    // one thing to the viewer; skipping to the end of the first and then
    // immediately again to the end of the second reads as a stutter.
    function merge(segments) {
        var out = [];
        segments.forEach(function (seg) {
            var last = out.length ? out[out.length - 1] : null;
            if (last && last.category === seg.category && last.behaviour === seg.behaviour &&
                seg.start <= last.end + 0.1) {
                last.end = Math.max(last.end, seg.end);
                return;
            }
            out.push(seg);
        });
        return out;
    }

    function recompute() {
        var cfg = config();
        var duration = state.duration;
        var shaped = [];
        preferLocked(state.raw).forEach(function (seg) {
            if (!durationOk(seg, duration)) return;
            if (!longEnough(seg)) return;
            var behaviour = behaviourOf(seg, cfg);
            if (!behaviour) return;
            if (state.ignored[seg.UUID]) return;
            shaped.push({
                start: seg.segment[0],
                end: seg.segment[1],
                category: seg.category,
                behaviour: behaviour,
                uuid: seg.UUID,
                actionType: seg.actionType || 'skip',
                description: seg.description || '',
                color: cfg[seg.category].color
            });
        });
        shaped.sort(function (a, b) { return a.start - b.start || a.end - b.end; });
        state.segments = merge(shaped);
        renderMarkers();
        schedule();
        publish();
    }

    // ------------------------------------------------------- the player, once

    // ⚠ NOT `document.querySelector('video')`. YouTube spawns a <video> for
    // every thumbnail you hover, and a capture-phase `timeupdate` listener on
    // `document` hears all of them — v1 would happily seek a hover preview
    // using the watch page's segments.
    function playerVideo() {
        var player = document.getElementById('movie_player');
        var video = player && player.querySelector('video');
        return video || document.querySelector('video.html5-main-video') || null;
    }

    function adShowing() {
        var player = document.getElementById('movie_player');
        if (!player) return false;
        return player.classList.contains('ad-showing') ||
            player.classList.contains('ad-interrupting');
    }

    var bound = null;
    function bind() {
        var video = playerVideo();
        if (!video || video === bound) return video;
        bound = video;
        video.addEventListener('timeupdate', onTick, false);
        video.addEventListener('seeked', onSeek, false);
        video.addEventListener('play', schedule, false);
        video.addEventListener('pause', schedule, false);
        video.addEventListener('ratechange', schedule, false);
        video.addEventListener('durationchange', onDuration, false);
        video.addEventListener('loadedmetadata', onDuration, false);
        onDuration();
        return video;
    }

    function onDuration() {
        var video = bound || playerVideo();
        var duration = video && isFinite(video.duration) ? video.duration : null;
        if (duration === state.duration) return;
        state.duration = duration;
        recompute();
    }

    // ------------------------------------------------------------- the skip

    var timer = null;
    var muteHold = null;   // {segment, wasMuted} while a mute segment is playing
    // The scheduled timer and a `timeupdate` already in flight can both reach
    // `act()` for the SAME segment: the tick was queued with the pre-seek time,
    // so it re-decides on stale input. Measured live — one intro skip counted
    // twice and raised two notices. The seek itself is idempotent; the count and
    // the toast are not, so the segment is latched instead.
    var lastActed = { uuid: null, at: 0 };
    var RE_ACT_GUARD_MS = 1000;

    function alreadyActed(uuid) {
        return lastActed.uuid === uuid && Date.now() - lastActed.at < RE_ACT_GUARD_MS;
    }

    function segmentAt(t, kinds) {
        for (var i = 0; i < state.segments.length; i++) {
            var seg = state.segments[i];
            if (kinds.indexOf(seg.behaviour) < 0) continue;
            if (t >= seg.start && t < seg.end) return seg;
        }
        return null;
    }

    // Schedule the NEXT automatic action to the moment it is due, instead of
    // waiting for the next `timeupdate`. `timeupdate` fires about four times a
    // second, so relying on it alone shows up to a quarter-second of the thing
    // the user asked not to see. `onTick` stays as the safety net.
    function schedule() {
        if (timer) { clearTimeout(timer); timer = null; }
        var video = bound;
        if (!video || video.paused || adShowing()) return;
        var t = video.currentTime;
        var rate = video.playbackRate || 1;
        var next = null;
        for (var i = 0; i < state.segments.length; i++) {
            var seg = state.segments[i];
            if (seg.behaviour !== 'auto' && seg.behaviour !== 'mute') continue;
            if (seg.end <= t) continue;
            if (!next || seg.start < next.start) next = seg;
        }
        if (!next) return;
        var delay = Math.max(0, (next.start - t) / rate) * 1000;
        timer = setTimeout(function () { timer = null; act(); }, Math.min(delay, 60000));
    }

    function act() {
        var video = bound;
        if (!video || video.paused || adShowing()) return;
        var t = video.currentTime;

        var skip = segmentAt(t, ['auto']);
        if (skip && alreadyActed(skip.uuid)) return;
        if (skip && t < skip.end - 0.05) {
            lastActed = { uuid: skip.uuid, at: Date.now() };
            var from = t;
            video.currentTime = skip.end;
            state.skipped += 1;
            notice('Skipped ' + (LABELS[skip.category] || skip.category), function () {
                state.ignored[skip.uuid] = true;
                video.currentTime = from;
                recompute();
            });
            schedule();
            publish();
            return;
        }

        var mute = segmentAt(t, ['mute']);
        if (mute) {
            if (!muteHold) {
                muteHold = { segment: mute, wasMuted: video.muted };
                video.muted = true;
                notice('Muted ' + (LABELS[mute.category] || mute.category), null);
            }
        } else if (muteHold) {
            video.muted = muteHold.wasMuted;
            muteHold = null;
        }
        schedule();
    }

    function onSeek() {
        // Seeking into a segment RE-SKIPS it, which is both what v1 did and
        // what the real extension does. The tempting alternative — treat a seek
        // as "the user wants this bit" — silently accumulates dead segments for
        // anyone who scrubs, and takes the feature away without saying so. Undo
        // is the one thing that disables a segment, and it says so on a button.
        var video = bound;
        if (!video) return;
        if (muteHold && !segmentAt(video.currentTime, ['mute'])) {
            video.muted = muteHold.wasMuted;
            muteHold = null;
        }
        schedule();
        publish();
    }

    function onTick() {
        var video = bound;
        if (!video || video.paused || adShowing()) return;
        act();
        updateButtons(video.currentTime);
    }

    // ------------------------------------------------------------------- UI

    var ui = null;
    function overlay() {
        if (ui && ui.root.isConnected) return ui;
        if (!document.body) return null;
        var root = document.createElement('div');
        root.id = 'ychrome-sponsorblock';
        root.style.cssText = 'position:fixed;right:16px;bottom:76px;z-index:2147483000;' +
            'display:flex;flex-direction:column;align-items:flex-end;gap:8px;' +
            'font:13px/1.4 system-ui,sans-serif;pointer-events:none;';
        document.body.appendChild(root);
        ui = { root: root, notice: null, buttons: {} };
        return ui;
    }

    function pill(text, actionLabel, onAction) {
        var el = document.createElement('div');
        el.style.cssText = 'background:rgba(20,20,24,.92);color:#e8e8ea;padding:8px 14px;' +
            'border-radius:9px;box-shadow:0 2px 12px rgba(0,0,0,.4);' +
            'display:flex;align-items:center;gap:12px;pointer-events:auto;' +
            'transition:opacity .3s;opacity:1;';
        var label = document.createElement('span');
        label.textContent = text;
        el.appendChild(label);
        if (actionLabel) {
            var button = document.createElement('button');
            button.textContent = actionLabel;
            button.style.cssText = 'all:unset;cursor:pointer;color:#8ab4f8;font-weight:600;' +
                'padding:2px 4px;border-radius:4px;';
            button.addEventListener('click', function (event) {
                event.preventDefault();
                event.stopPropagation();
                onAction();
            }, false);
            el.appendChild(button);
        }
        return el;
    }

    // ⛔ `skip_notice` off silences the pill and NOTHING ELSE — the skip still
    // happens. And it takes the UNDO with it, which is the whole cost of the
    // setting: undo lives on the notice and there is no other way back into a
    // segment ychrome has just seeked past. That is why the default is on and
    // why `sidebar` says so beside the switch rather than in a document.
    function notice(text, onUndo) {
        if (!prefs().skip_notice) return;
        var host = overlay();
        if (!host) return;
        if (host.notice) host.notice.remove();
        var el = pill(text, onUndo ? 'Undo' : null, function () {
            el.remove();
            if (host.notice === el) host.notice = null;
            onUndo();
        });
        host.notice = el;
        host.root.appendChild(el);
        setTimeout(function () {
            if (!el.isConnected) return;
            el.style.opacity = '0';
            setTimeout(function () {
                el.remove();
                if (host.notice === el) host.notice = null;
            }, 400);
        }, onUndo ? 5000 : 2200);
    }

    // A persistent button for the things the user has to decide about: a manual
    // segment they are inside, and a highlight they have not reached.
    function button(key, text, onClick) {
        var host = overlay();
        if (!host) return;
        var existing = host.buttons[key];
        if (existing && existing.isConnected) {
            if (existing.dataset.text !== text) {
                existing.dataset.text = text;
                existing.firstChild.textContent = text;
            }
            existing.__onClick = onClick;
            return;
        }
        var el = pill(text, '→', function () { if (el.__onClick) el.__onClick(); });
        el.dataset.text = text;
        el.__onClick = onClick;
        host.buttons[key] = el;
        host.root.appendChild(el);
    }

    function dropButton(key) {
        if (!ui || !ui.buttons[key]) return;
        ui.buttons[key].remove();
        delete ui.buttons[key];
    }

    function updateButtons(t) {
        var video = bound;
        if (!video) return;
        var manual = segmentAt(t, ['manual']);
        if (manual) {
            button('manual', 'Skip ' + (LABELS[manual.category] || manual.category), function () {
                video.currentTime = manual.end;
                dropButton('manual');
                state.skipped += 1;
                publish();
            });
        } else {
            dropButton('manual');
        }

        var highlight = null;
        for (var i = 0; i < state.segments.length; i++) {
            if (state.segments[i].behaviour === 'highlight') { highlight = state.segments[i]; break; }
        }
        if (highlight && t < highlight.start - 1) {
            button('highlight', 'Jump to the highlight', function () {
                video.currentTime = highlight.start;
                dropButton('highlight');
            });
        } else {
            dropButton('highlight');
        }

        // The opt-in half. It draws nothing at all unless ychrome's settings
        // turned it on, so a user who never asked to contribute sees exactly
        // the surface they saw before it existed.
        updateContribution(t);
    }

    var labelled = null;
    function announceLabel() {
        for (var i = 0; i < state.segments.length; i++) {
            var seg = state.segments[i];
            if (seg.behaviour !== 'label') continue;
            if (labelled === state.videoId) return;
            labelled = state.videoId;
            notice('This entire video is ' + (LABELS[seg.category] || seg.category), null);
            return;
        }
    }

    // ------------------------------------------------------- seek-bar markers

    // The recognisable half of SponsorBlock: the segments drawn on the scrubber
    // so you can see what is coming. Colours adopted from the extension.
    var markers = null;
    function renderMarkers() {
        var bar = document.querySelector('.ytp-progress-bar');
        // Switched off: take down anything already drawn rather than merely
        // stopping — a marker left behind by the render that ran before the
        // setting changed would sit on the scrubber for the rest of the page.
        if (!prefs().seek_bar_markers) {
            if (markers) { markers.remove(); markers = null; }
            return;
        }
        if (!bar || !state.duration) {
            if (markers) { markers.remove(); markers = null; }
            return;
        }
        if (!markers || !markers.isConnected) {
            markers = document.createElement('div');
            markers.id = 'ychrome-sponsorblock-bar';
            markers.style.cssText = 'position:absolute;left:0;top:0;width:100%;height:100%;' +
                'pointer-events:none;z-index:20;';
            if (getComputedStyle(bar).position === 'static') bar.style.position = 'relative';
            bar.appendChild(markers);
        }
        markers.textContent = '';
        state.segments.forEach(function (seg) {
            var left = Math.max(0, Math.min(100, (seg.start / state.duration) * 100));
            var width = Math.max(0.15, Math.min(100 - left, ((seg.end - seg.start) / state.duration) * 100));
            var mark = document.createElement('div');
            mark.style.cssText = 'position:absolute;top:0;height:100%;' +
                'left:' + left + '%;width:' + width + '%;' +
                'background:' + seg.color + ';opacity:.7;';
            if (seg.behaviour === 'highlight') {
                mark.style.width = '4px';
                mark.style.opacity = '1';
            }
            mark.title = (LABELS[seg.category] || seg.category) +
                (seg.description ? ': ' + seg.description : '');
            markers.appendChild(mark);
        });
    }

    // --------------------------------------------------- contributing (opt-in)

    // ⛔⛔ EVERYTHING BELOW WRITES TO A SHARED PUBLIC DATABASE, and none of it
    // runs unless ychrome's settings pane says the user turned it on. Reading
    // segments is anonymous by construction (the hash-prefix query above);
    // contributing cannot be, because the server counts votes per user and
    // publishes a submitter's record. So:
    //
    //   * `prefs().voting` / `prefs().submission` gate every call site,
    //   * `prefs().userId` is absent unless one of them is on, and a missing id
    //     REFUSES the request rather than sending an anonymous one — an
    //     unattributed vote is not a lighter version of a vote, it is a
    //     malformed one,
    //   * ⚠ A SUBMISSION SENDS THE VIDEO ID IN THE CLEAR. It has to: the point
    //     is to say "this video has a sponsor here". The submit button says so
    //     at the moment of pressing, because that is the one place a privacy
    //     cost can still be declined.
    var CONTRIB_API = 'https://sponsor.ajay.app/api/';
    // The categories a user may submit under. Deliberately the SKIPPABLE ones
    // only: `chapter` needs a name this UI has no field for, and the two label
    // categories are not things a viewer marks a range for.
    var SUBMIT_CATEGORIES = ['sponsor', 'selfpromo', 'interaction', 'intro', 'outro', 'preview', 'music_offtopic', 'filler'];

    var contrib = {
        marking: null,  // { start } while a range is half-marked
        queue: [],      // { start, end, category } marked but not sent
        voted: {},      // UUID -> 1 | 0, so a row can show what you already said
        busy: false,
        lastError: null
    };

    function contribFetch(path, options) {
        contrib.busy = true;
        publish();
        return fetch(CONTRIB_API + path, options).then(function (resp) {
            contrib.busy = false;
            if (!resp.ok) {
                // The API answers a refusal in plain text, and it is worth
                // showing: "duplicate", "rate limited" and "your submission
                // overlaps" are three different things the user can act on.
                return resp.text().then(function (text) {
                    throw new Error((text || '').slice(0, 160) || ('HTTP ' + resp.status));
                });
            }
            return resp;
        }).catch(function (error) {
            contrib.busy = false;
            contrib.lastError = String(error && error.message || error);
            publish();
            throw error;
        });
    }

    // A vote on one community segment. `up` true = it is right, false = it is
    // wrong. Upstream's own encoding: type=1 upvote, type=0 downvote.
    function voteOn(seg, up) {
        var cfg = prefs();
        if (!cfg.voting || !cfg.userId || !seg || !seg.uuid) return;
        var params = 'UUID=' + encodeURIComponent(seg.uuid) +
            '&userID=' + encodeURIComponent(cfg.userId) +
            '&type=' + (up ? '1' : '0');
        contribFetch('voteOnSponsorTime?' + params, { method: 'POST' }).then(function () {
            contrib.voted[seg.uuid] = up ? 1 : 0;
            notice('Voted: this ' + (LABELS[seg.category] || seg.category) + ' is ' +
                (up ? 'right' : 'wrong'), null);
            publish();
        }).catch(function (error) {
            notice('Vote failed: ' + error.message, null);
        });
    }

    function markStart() {
        if (!bound) return;
        contrib.marking = { start: bound.currentTime };
        publish();
    }

    function markEnd() {
        if (!bound || !contrib.marking) return;
        var start = contrib.marking.start;
        var end = bound.currentTime;
        contrib.marking = null;
        if (end <= start) {
            notice('That end is before the start — mark again.', null);
            publish();
            return;
        }
        contrib.queue.push({ start: start, end: end, category: null });
        publish();
    }

    function abandonMark() {
        contrib.marking = null;
        publish();
    }

    function dropQueued(index) {
        contrib.queue.splice(index, 1);
        publish();
    }

    // ⛔ Nothing leaves this browser until every queued range has a category AND
    // the user presses submit. A half-described segment sent on a timer would be
    // a submission the user never made.
    function submitQueue() {
        var cfg = prefs();
        if (!cfg.submission || !cfg.userId || !state.videoId) return;
        var ready = contrib.queue.filter(function (row) { return !!row.category; });
        if (!ready.length) {
            notice('Give each marked range a category first.', null);
            return;
        }
        var body = {
            videoID: state.videoId,
            userID: cfg.userId,
            segments: ready.map(function (row) {
                return {
                    segment: [row.start, row.end],
                    category: row.category,
                    actionType: 'skip'
                };
            })
        };
        contribFetch('skipSegments', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body)
        }).then(function () {
            contrib.queue = contrib.queue.filter(function (row) { return !row.category; });
            notice('Submitted ' + ready.length + ' segment' + (ready.length === 1 ? '' : 's') +
                '. Thank you.', null);
            // Re-ask, so what was just contributed appears like anyone else's.
            cacheDrop(state.videoId);
            if (state.videoId) fetchSegments(state.videoId);
            publish();
        }).catch(function (error) {
            notice('Submission refused: ' + error.message, null);
        });
    }

    // A pill carrying SEVERAL buttons — the vote pair, the category chooser.
    // `pill` above takes one action and is the right shape for a notice; this is
    // the right shape for a choice.
    function choicePill(key, text, choices) {
        var host = overlay();
        if (!host) return;
        var existing = host.buttons[key];
        if (existing && existing.isConnected && existing.dataset.text === text) return;
        if (existing) { existing.remove(); }
        var el = document.createElement('div');
        el.dataset.text = text;
        el.style.cssText = 'background:rgba(20,20,24,.92);color:#e8e8ea;padding:8px 14px;' +
            'border-radius:9px;box-shadow:0 2px 12px rgba(0,0,0,.4);' +
            'display:flex;align-items:center;gap:8px;pointer-events:auto;' +
            'flex-wrap:wrap;max-width:min(420px,60vw);';
        var label = document.createElement('span');
        label.textContent = text;
        el.appendChild(label);
        choices.forEach(function (choice) {
            var button = document.createElement('button');
            button.textContent = choice.label;
            button.title = choice.title || choice.label;
            button.style.cssText = 'all:unset;cursor:pointer;color:#8ab4f8;font-weight:600;' +
                'padding:2px 6px;border-radius:4px;border:1px solid rgba(138,180,248,.35);';
            button.addEventListener('click', function (event) {
                event.preventDefault();
                event.stopPropagation();
                choice.run();
            }, false);
            el.appendChild(button);
        });
        host.buttons[key] = el;
        host.root.appendChild(el);
    }

    function updateContribution(t) {
        var cfg = prefs();

        // VOTE: on the segment the playhead is inside, whatever ychrome does
        // with it — you can only judge one you can see.
        var inside = null;
        for (var i = 0; i < state.segments.length; i++) {
            var seg = state.segments[i];
            if (seg.behaviour === 'label') continue;
            if (t >= seg.start && t <= seg.end) { inside = seg; break; }
        }
        if (cfg.voting && cfg.userId && inside && contrib.voted[inside.uuid] === undefined) {
            (function (seg) {
                choicePill('vote', 'This ' + (LABELS[seg.category] || seg.category) + '?', [
                    { label: '👍', title: 'the segment is right', run: function () { voteOn(seg, true); dropButton('vote'); } },
                    { label: '👎', title: 'the segment is wrong', run: function () { voteOn(seg, false); dropButton('vote'); } }
                ]);
            })(inside);
        } else {
            dropButton('vote');
        }

        // SUBMIT: mark a range, name it, send it.
        if (!cfg.submission || !cfg.userId) {
            dropButton('mark');
            dropButton('category');
            dropButton('submit');
            return;
        }
        if (contrib.marking) {
            choicePill('mark', 'Marking from ' + clock(contrib.marking.start), [
                { label: 'End here', title: 'close the range at the playhead', run: markEnd },
                { label: 'Cancel', title: 'forget this range', run: abandonMark }
            ]);
        } else {
            choicePill('mark', 'Mark a segment', [
                { label: 'Start here', title: 'open a range at the playhead', run: markStart }
            ]);
        }

        var unnamed = -1;
        for (var q = 0; q < contrib.queue.length; q++) {
            if (!contrib.queue[q].category) { unnamed = q; break; }
        }
        if (unnamed >= 0) {
            (function (index, row) {
                var choices = SUBMIT_CATEGORIES.map(function (id) {
                    return {
                        label: LABELS[id] || id,
                        title: 'file ' + clock(row.start) + '–' + clock(row.end) + ' as ' + (LABELS[id] || id),
                        run: function () { row.category = id; dropButton('category'); publish(); }
                    };
                });
                choices.push({ label: 'Discard', title: 'drop this range', run: function () { dropQueued(index); dropButton('category'); } });
                choicePill('category', clock(row.start) + '–' + clock(row.end) + ' is a…', choices);
            })(unnamed, contrib.queue[unnamed]);
        } else {
            dropButton('category');
        }

        var ready = contrib.queue.filter(function (row) { return !!row.category; }).length;
        if (ready) {
            // ⚠ The privacy cost, at the one moment it can still be declined.
            choicePill('submit', ready + ' ready — submitting names this video publicly', [
                { label: contrib.busy ? 'Sending…' : 'Submit', title: 'send to sponsor.ajay.app', run: submitQueue }
            ]);
        } else {
            dropButton('submit');
        }
    }

    function clock(seconds) {
        var whole = Math.max(0, Math.floor(seconds));
        var mins = Math.floor(whole / 60);
        var secs = whole % 60;
        return mins + ':' + (secs < 10 ? '0' : '') + secs;
    }

    // ------------------------------------------------- the instrument, and SPA

    function publish() {
        try {
            var payload = {
                version: '2.1.0',
                videoId: state.videoId,
                lookups: state.lookups,
                skipped: state.skipped,
                duration: state.duration,
                error: state.lastError,
                cached: state.cached,
                bound: !!bound,
                adShowing: adShowing(),
                // ⭐ What is switched on, so "SponsorBlock did nothing" can be
                // told apart from "SponsorBlock is off here" without opening
                // the settings pane. `contributing` reports the SWITCHES and
                // the QUEUE — never the id, which is a write credential.
                prefs: (function () {
                    var cfg = prefs();
                    return {
                        skip_notice: cfg.skip_notice,
                        seek_bar_markers: cfg.seek_bar_markers,
                        min_duration_secs: cfg.min_duration_secs,
                        voting: cfg.voting,
                        submission: cfg.submission,
                        identified: !!cfg.userId
                    };
                })(),
                contributing: {
                    marking: !!contrib.marking,
                    queued: contrib.queue.length,
                    named: contrib.queue.filter(function (row) { return !!row.category; }).length,
                    votes: Object.keys(contrib.voted).length,
                    busy: contrib.busy,
                    error: contrib.lastError
                },
                segments: state.segments.map(function (seg) {
                    return {
                        start: Math.round(seg.start * 1000) / 1000,
                        end: Math.round(seg.end * 1000) / 1000,
                        category: seg.category,
                        behaviour: seg.behaviour
                    };
                })
            };
            document.documentElement.setAttribute('data-ysb', JSON.stringify(payload));
        } catch (e) { /* the instrument must never be the thing that breaks */ }
    }

    function currentVideoId() {
        try {
            if (location.pathname === '/watch') {
                return new URLSearchParams(location.search).get('v');
            }
            var direct = location.pathname.match(/^\/(?:shorts|live|embed)\/([\w-]{6,})/);
            if (direct) return direct[1];
        } catch (e) { /* ignore */ }
        return null;
    }

    var lastHref = null;
    function rescan() {
        bind();
        var videoId = currentVideoId();
        if (videoId !== state.videoId) {
            state.videoId = videoId;
            state.raw = [];
            state.segments = [];
            state.ignored = {};
            state.lastError = null;
            state.cached = false;
            state.duration = null;
            if (muteHold && bound) { bound.muted = muteHold.wasMuted; }
            muteHold = null;
            // ⛔ A half-marked range belongs to the video it was marked in. A
            // queue that survived an SPA navigation would submit one video's
            // timestamps against another's id — a wrong submission the user
            // would never see themselves make.
            contrib.marking = null;
            contrib.queue = [];
            contrib.voted = {};
            contrib.lastError = null;
            dropButton('manual');
            dropButton('highlight');
            dropButton('vote');
            dropButton('mark');
            dropButton('category');
            dropButton('submit');
            onDuration();
            if (videoId) fetchSegments(videoId);
            publish();
            return;
        }
        // Same video: the player, its duration and YouTube's own scrubber all
        // get rebuilt under us (theatre mode, fullscreen, a chapter change), so
        // re-assert what we drew rather than assuming it survived.
        onDuration();
        if (!markers || !markers.isConnected) renderMarkers();
        announceLabel();
        if (bound) updateButtons(bound.currentTime);
    }

    // YouTube changes the video WITHOUT a page load, which is exactly what broke
    // the ad blocker. Three independent nets: the SPA event, a cheap href
    // watcher for the transitions that do not fire it, and the tick itself.
    document.addEventListener('yt-navigate-finish', rescan, true);
    document.addEventListener('yt-page-data-updated', rescan, true);
    document.addEventListener('yt-player-updated', rescan, true);
    setInterval(function () {
        if (location.href !== lastHref) { lastHref = location.href; rescan(); return; }
        rescan();
    }, 500);

    window.__ysb = {
        state: state,
        config: config,
        prefs: prefs,
        contrib: contrib,
        rescan: rescan,
        recompute: recompute
    };
    // Kept so an older probe that learned these names still reads something
    // true rather than `undefined`. `data-ysb` is the one to use.
    window.__ysb_loaded = true;
    window.__ysb_state = state;

    rescan();
})();
