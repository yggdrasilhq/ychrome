// ychrome scriptlet runtime — the library behind `##+js(...)`.
//
// ⚠ WRITTEN FROM THE DOCUMENTED BEHAVIOUR OF THE FILTER SYNTAX, NOT FROM
// ANOTHER BLOCKER'S SOURCE. Every function below was written against the
// argument grammar the filter lists use.
//
// The original reason was licence incompatibility -- uBO is GPLv3 and this
// project shipped Apache. That reason is GONE: the browser is now
// GPL-3.0-or-later. The rule survives anyway, for two that outlive it.
// Provenance here stays clean and auditable whatever licence the repo wears;
// and `crates/ychrome-vault*` next door are Apache-2.0, where GPL code must
// never land, so a habit of transcribing is a hazard one directory away.
//
// Adopting uBO code here is now LAWFUL. If you do, ATTRIBUTE it and mark it,
// in the file and in THIRD-PARTY-NOTICES.md -- do not fold it in silently.
//
// The contract: this file is a FUNCTION EXPRESSION. `abp::generate_scriptlet_script`
// splices it into a generated userscript next to its payload and calls it.
// Keeping it a standalone expression is what lets the node harness drive the
// real thing instead of a copy.
//
//   RULES  domain -> [index, ...] into TABLE
//   TABLE  [[canonicalName, arg, ...], ...], each distinct rule exactly once
//
// The indirection is not decoration: one upstream filter names thousands of
// domains, so spelling every invocation out per domain repeated 8,736 rows over
// 2,428 distinct rules and cost 845 KB.
//
// Every scriptlet is DATA driven by a filter, never bespoke per-site code. A new
// site costs a line in a list; only a new *primitive* costs code here.
function (RULES, TABLE, win) {
    'use strict';
    var W = win || (typeof window !== 'undefined' ? window : globalThis);
    var doc = W.document;

    // ---- host matching ---------------------------------------------------
    // Longest-suffix, the same rule webzoom and cosmetic-filters use: a rule for
    // `example.com` covers `www.example.com`, and a bare TLD never matches.
    var host = String((W.location && W.location.hostname) || '').toLowerCase();
    var mine = [];
    for (var key in RULES) {
        if (host === key || (host.length > key.length
            && host.slice(-(key.length + 1)) === '.' + key)) {
            var indices = RULES[key];
            for (var n = 0; n < indices.length; n++) {
                var row = TABLE[indices[n]];
                if (row) mine.push(row);
            }
        }
    }
    if (!mine.length) return null;

    var state = { applied: 0, refused: 0, failed: 0, by: {} };
    W.__yggScriptlets = state;

    // ---- shared helpers --------------------------------------------------

    // The pattern grammar the lists use, in one place:
    //   `/re/flags`  a regular expression
    //   `!thing`     negated
    //   ``/absent    matches everything
    //   anything else is a literal substring, with `*` as a wildcard
    // Returns a predicate over a string.
    function matcher(raw) {
        if (raw === undefined || raw === null || raw === '') {
            return function () { return true; };
        }
        var text = String(raw);
        var negate = false;
        if (text.charAt(0) === '!') { negate = true; text = text.slice(1); }
        var test;
        if (text.length > 2 && text.charAt(0) === '/' && text.lastIndexOf('/') > 0) {
            var end = text.lastIndexOf('/');
            var body = text.slice(1, end);
            var flags = text.slice(end + 1);
            try {
                var re = new RegExp(body, flags);
                test = function (s) { return re.test(s); };
            } catch (e) {
                test = function (s) { return s.indexOf(text) !== -1; };
            }
        } else if (text.indexOf('*') !== -1) {
            var parts = text.split('*').map(function (p) {
                return p.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
            });
            try {
                var wild = new RegExp(parts.join('[\\s\\S]*'));
                test = function (s) { return wild.test(s); };
            } catch (e) {
                test = function (s) { return s.indexOf(text) !== -1; };
            }
        } else {
            test = function (s) { return String(s).indexOf(text) !== -1; };
        }
        return negate ? function (s) { return !test(s); } : test;
    }

    // The literal values the `set`-family arguments name. A value the grammar
    // does not name is REFUSED rather than guessed at: setting a page global to
    // the wrong thing breaks the site in a way the user cannot diagnose.
    var REFUSED = {};
    function noopFunc() {}
    function trueFunc() { return true; }
    function falseFunc() { return false; }
    function constantValue(raw) {
        switch (raw) {
            case 'undefined': return undefined;
            case 'false': return false;
            case 'true': return true;
            case 'null': return null;
            case 'emptyObj': return {};
            case 'emptyArr': return [];
            case 'noopFunc': return noopFunc;
            case 'trueFunc': return trueFunc;
            case 'falseFunc': return falseFunc;
            case 'noopPromiseResolve':
                return function () { return W.Promise.resolve(); };
            case 'noopPromiseReject':
                return function () { return W.Promise.reject(); };
            case '': case "''": return '';
            default: break;
        }
        if (/^-?\d+$/.test(raw)) {
            var n = parseInt(raw, 10);
            // The lists only ever set small integers here. A huge one is far
            // more likely to be a misparse than an intent.
            return (n >= -32768 && n <= 32767) ? n : REFUSED;
        }
        if (/^'[^']*'$/.test(raw) || /^"[^"]*"$/.test(raw)) return raw.slice(1, -1);
        return REFUSED;
    }

    // Walk a dotted path, defining an accessor on the LAST segment. The parents
    // may not exist yet — a page global is usually created by a script that has
    // not run — so a missing parent is created as a plain object, and one that
    // is later assigned wholesale is re-decorated through its own accessor.
    // Returns true when the accessor is in place.
    function defineOnPath(root, path, makeDescriptor) {
        var parts = String(path).split('.');
        var leaf = parts.pop();
        function step(owner, index) {
            if (index === parts.length) {
                try {
                    var existing = Object.getOwnPropertyDescriptor(owner, leaf);
                    if (existing && existing.configurable === false) return false;
                    Object.defineProperty(owner, leaf, makeDescriptor(owner, leaf, existing));
                    return true;
                } catch (e) { return false; }
            }
            var name = parts[index];
            var current = owner[name];
            if (current && (typeof current === 'object' || typeof current === 'function')) {
                return step(current, index + 1);
            }
            // Not there yet: hold the slot, and continue down the chain the
            // moment the page assigns it.
            var held = current;
            try {
                Object.defineProperty(owner, name, {
                    configurable: true,
                    get: function () { return held; },
                    set: function (value) {
                        held = value;
                        if (value && (typeof value === 'object' || typeof value === 'function')) {
                            step(value, index + 1);
                        }
                    },
                });
            } catch (e) { return false; }
            return true;
        }
        return step(root, 0);
    }

    // A thrown ReferenceError is what stops the script that was reading the
    // property. The token makes it identifiable in a console without pretending
    // to be a real error the page produced.
    function abort(what) {
        throw new ReferenceError('ychrome-scriptlet-abort:' + what);
    }

    function sourceOf(fn) {
        try { return typeof fn === 'function' ? Function.prototype.toString.call(fn) : String(fn); }
        catch (e) { return ''; }
    }

    // ---- the scriptlets --------------------------------------------------
    // Each takes the filter's arguments as strings and returns true when it
    // installed something. Names are the CANONICAL ones; the generator resolves
    // every alias the lists use before it gets here, so there is exactly one
    // implementation per behaviour.
    var SCRIPTLETS = {

        // set-constant(path, value) — freeze a page global at a constant.
        'set-constant': function (path, raw) {
            var value = constantValue(String(raw));
            if (value === REFUSED) return false;
            return defineOnPath(W, path, function () {
                return {
                    configurable: true,
                    get: function () { return value; },
                    // Assignment is SWALLOWED, not thrown on: the page's own
                    // code assigns these, and throwing there stops the script
                    // that was going to run the rest of the page.
                    set: function () {},
                };
            });
        },

        // abort-on-property-read(path) — reading it stops the reader.
        'abort-on-property-read': function (path) {
            return defineOnPath(W, path, function () {
                return {
                    configurable: true,
                    get: function () { abort('read ' + path); },
                    set: function () {},
                };
            });
        },

        // abort-on-property-write(path) — writing it stops the writer.
        'abort-on-property-write': function (path) {
            return defineOnPath(W, path, function (owner, leaf, existing) {
                var held = existing && 'value' in existing ? existing.value : undefined;
                return {
                    configurable: true,
                    get: function () { return held; },
                    set: function () { abort('write ' + path); },
                };
            });
        },

        // abort-current-script(path, search) — stop only the INLINE script that
        // is reading `path` and whose own text matches `search`. Everything else
        // reads the property normally, which is what makes this narrower (and
        // safer) than abort-on-property-read.
        'abort-current-script': function (path, search) {
            var wanted = matcher(search);
            return defineOnPath(W, path, function (owner, leaf, existing) {
                var held = existing && 'value' in existing ? existing.value : owner[leaf];
                return {
                    configurable: true,
                    get: function () {
                        var current = doc && doc.currentScript;
                        if (current && !current.src && wanted(current.textContent || '')) {
                            abort('inline script reading ' + path);
                        }
                        return held;
                    },
                    set: function (value) { held = value; },
                };
            });
        },

        // no-setTimeout-if(search, delay) — drop timers whose callback source
        // matches. The timer is not run and not rescheduled; the id is real so
        // a clearTimeout on it is still valid.
        'no-setTimeout-if': function (search, delay) {
            var wanted = matcher(search);
            var wantDelay = delay === undefined || delay === '' ? null : parseInt(delay, 10);
            var orig = W.setTimeout;
            if (typeof orig !== 'function') return false;
            W.setTimeout = function (fn, ms) {
                if (wanted(sourceOf(fn)) && (wantDelay === null || wantDelay === (ms | 0))) {
                    state.refused += 1;
                    return orig.call(W, noopFunc, 0);
                }
                return orig.apply(W, arguments);
            };
            return true;
        },

        'no-setInterval-if': function (search, delay) {
            var wanted = matcher(search);
            var wantDelay = delay === undefined || delay === '' ? null : parseInt(delay, 10);
            var orig = W.setInterval;
            if (typeof orig !== 'function') return false;
            W.setInterval = function (fn, ms) {
                if (wanted(sourceOf(fn)) && (wantDelay === null || wantDelay === (ms | 0))) {
                    state.refused += 1;
                    return orig.call(W, noopFunc, 0x7FFFFFFF);
                }
                return orig.apply(W, arguments);
            };
            return true;
        },

        // adjust-setInterval(search, delay, boost) — a timer the page uses to
        // stall the user is made to fire sooner (or later). Nothing is dropped.
        'adjust-setInterval': function (search, delay, boost) {
            return adjustTimer('setInterval', search, delay, boost);
        },
        'adjust-setTimeout': function (search, delay, boost) {
            return adjustTimer('setTimeout', search, delay, boost);
        },

        // addEventListener-defuser(type, search) — refuse a listener
        // registration. The page's call still returns normally; the handler is
        // simply never attached.
        'addEventListener-defuser': function (type, search) {
            var wantType = matcher(type);
            var wantFn = matcher(search);
            var proto = W.EventTarget && W.EventTarget.prototype;
            if (!proto || typeof proto.addEventListener !== 'function') return false;
            var orig = proto.addEventListener;
            proto.addEventListener = function (name, handler) {
                try {
                    if (wantType(String(name)) && wantFn(sourceOf(handler))) {
                        state.refused += 1;
                        return undefined;
                    }
                } catch (e) { /* fall through and attach it */ }
                return orig.apply(this, arguments);
            };
            return true;
        },

        // no-window-open-if(pattern, delay, decoy) — refuse a popup. A DECOY
        // object is returned rather than null, because a page that gets null
        // very often decides it is being blocked and reacts.
        'no-window-open-if': function (pattern) {
            var wanted = matcher(pattern);
            var orig = W.open;
            if (typeof orig !== 'function') return false;
            W.open = function (url) {
                if (wanted(String(url === undefined ? '' : url))) {
                    state.refused += 1;
                    return decoyWindow();
                }
                return orig.apply(W, arguments);
            };
            return true;
        },

        // set-cookie(name, value, path, ...) — the consent-shaped scriptlets.
        // The trusted variant takes an arbitrary value; the plain one is
        // restricted upstream to a small vocabulary, and we do not police that
        // here because the generator only accepts lines from lists we chose.
        'set-cookie': function (name, value, path) {
            if (!doc || !name) return false;
            var parts = [encodeURIComponent(name) + '=' + encodeURIComponent(String(value)),
                'path=' + (path && path !== '/' ? path : '/')];
            try {
                doc.cookie = parts.join('; ');
                return true;
            } catch (e) { return false; }
        },

        // remove-cookie(pattern) — delete matching cookies now, and keep
        // deleting them: the page usually rewrites the one it wants.
        'remove-cookie': function (pattern) {
            var wanted = matcher(pattern);
            if (!doc) return false;
            var sweep = function () {
                var all = String(doc.cookie || '').split(';');
                for (var i = 0; i < all.length; i++) {
                    var name = all[i].split('=')[0].trim();
                    if (!name || !wanted(name)) continue;
                    var stem = W.location ? String(W.location.hostname || '') : '';
                    var domains = ['', stem, '.' + stem];
                    for (var d = 0; d < domains.length; d++) {
                        doc.cookie = name + '=; Max-Age=0; path=/'
                            + (domains[d] ? '; domain=' + domains[d] : '');
                    }
                    state.applied += 1;
                }
            };
            sweep();
            W.addEventListener && W.addEventListener('load', sweep, false);
            return true;
        },

        // set-local-storage-item(key, value) — `$remove$` deletes the key.
        'set-local-storage-item': function (key, value) {
            return storageItem('localStorage', key, value);
        },
        'set-session-storage-item': function (key, value) {
            return storageItem('sessionStorage', key, value);
        },

        // json-prune(paths, needlePaths) — THE ONE THIS LANE STARTED FROM.
        // Delete dotted paths out of every parsed JSON object. `JSON.parse` and
        // `Response.prototype.json` are the two funnels a parsed body cannot
        // avoid, and hooking `fetch` instead is what let YouTube's ads through
        // for a whole release: measured, the player reads its answer through
        // `JSON.parse` 30 times a page and through JS-CONSTRUCTED `Response`
        // objects that no URL hook can see.
        'json-prune': function (paths, needles) {
            if (!paths) return false;
            var wanted = String(paths).split(/\s+/).filter(Boolean);
            var required = needles ? String(needles).split(/\s+/).filter(Boolean) : [];
            var prune = function (data) {
                if (!data || typeof data !== 'object') return data;
                for (var i = 0; i < required.length; i++) {
                    if (readPath(data, required[i]) === undefined) return data;
                }
                for (var j = 0; j < wanted.length; j++) deletePath(data, wanted[j]);
                state.applied += 1;
                return data;
            };
            var origParse = W.JSON.parse;
            W.JSON.parse = function () {
                return prune(origParse.apply(this, arguments));
            };
            if (W.Response && W.Response.prototype
                && typeof W.Response.prototype.json === 'function') {
                var origJson = W.Response.prototype.json;
                W.Response.prototype.json = function () {
                    var pending = origJson.apply(this, arguments);
                    if (!pending || typeof pending.then !== 'function') return pending;
                    return pending.then(prune);
                };
            }
            return true;
        },

        // no-fetch-if(conditions) — answer a matching request with an empty
        // 200 rather than letting it go out. The conditions are `key:value`
        // pairs over the request; a bare word matches the URL.
        'no-fetch-if': function (conditions) {
            var wanted = requestMatcher(conditions);
            var orig = W.fetch;
            if (typeof orig !== 'function') return false;
            W.fetch = function (input, init) {
                var url = '';
                try { url = typeof input === 'string' ? input : (input && input.url) || ''; }
                catch (e) { /* exotic input: let it through */ }
                if (wanted(url, init)) {
                    state.refused += 1;
                    return W.Promise.resolve(new W.Response('', { status: 200, statusText: 'OK' }));
                }
                return orig.apply(this, arguments);
            };
            return true;
        },

        // no-xhr-if(conditions) — the same refusal for XMLHttpRequest. The
        // request is never sent; the object completes with an empty 200 so a
        // page waiting on `load` is not left hanging.
        'no-xhr-if': function (conditions) {
            var wanted = requestMatcher(conditions);
            var proto = W.XMLHttpRequest && W.XMLHttpRequest.prototype;
            if (!proto) return false;
            var origOpen = proto.open;
            var origSend = proto.send;
            proto.open = function (method, url) {
                try { this.__yggBlocked = wanted(String(url || ''), { method: method }); }
                catch (e) { this.__yggBlocked = false; }
                return origOpen.apply(this, arguments);
            };
            proto.send = function () {
                if (!this.__yggBlocked) return origSend.apply(this, arguments);
                state.refused += 1;
                var self = this;
                Object.defineProperty(self, 'readyState', { configurable: true, get: function () { return 4; } });
                Object.defineProperty(self, 'status', { configurable: true, get: function () { return 200; } });
                Object.defineProperty(self, 'responseText', { configurable: true, get: function () { return ''; } });
                Object.defineProperty(self, 'response', { configurable: true, get: function () { return ''; } });
                W.setTimeout(function () {
                    try { self.onreadystatechange && self.onreadystatechange(); } catch (e) {}
                    try { self.dispatchEvent(new W.Event('readystatechange')); } catch (e) {}
                    try { self.dispatchEvent(new W.Event('load')); } catch (e) {}
                    try { self.dispatchEvent(new W.Event('loadend')); } catch (e) {}
                }, 0);
                return undefined;
            };
            return true;
        },

        // noeval() / noeval-if(pattern) — refuse `eval` of matching source.
        'noeval-if': function (pattern) {
            var wanted = matcher(pattern);
            var orig = W.eval;
            if (typeof orig !== 'function') return false;
            W.eval = function (source) {
                if (wanted(String(source))) { state.refused += 1; return undefined; }
                return orig.apply(this, arguments);
            };
            return true;
        },

        // nowebrtc() — refuse RTCPeerConnection, which is how a page reads a
        // local IP that no other API will give it.
        'nowebrtc': function () {
            var names = ['RTCPeerConnection', 'webkitRTCPeerConnection', 'mozRTCPeerConnection'];
            var did = false;
            for (var i = 0; i < names.length; i++) {
                if (typeof W[names[i]] !== 'function') continue;
                try {
                    W[names[i]] = function () {
                        state.refused += 1;
                        throw new W.DOMException('ychrome: WebRTC is refused here',
                            'NotAllowedError');
                    };
                    did = true;
                } catch (e) { /* non-writable: leave it */ }
            }
            return did;
        },

        // remove-attr(attrs, selector) / remove-class(classes, selector) — the
        // DOM edits a CSS rule cannot express, applied on a coalesced observer.
        'remove-attr': function (attrs, selector) {
            return domSweep(selector, function (el) {
                var names = String(attrs).split(/[|\s]+/).filter(Boolean);
                var hit = false;
                for (var i = 0; i < names.length; i++) {
                    if (el.hasAttribute && el.hasAttribute(names[i])) {
                        el.removeAttribute(names[i]);
                        hit = true;
                    }
                }
                return hit;
            });
        },
        'remove-class': function (classes, selector) {
            return domSweep(selector, function (el) {
                var names = String(classes).split(/[|\s]+/).filter(Boolean);
                var hit = false;
                for (var i = 0; i < names.length; i++) {
                    if (el.classList && el.classList.contains(names[i])) {
                        el.classList.remove(names[i]);
                        hit = true;
                    }
                }
                return hit;
            });
        },

        // remove-node-text(tag, pattern) — drop a node whose own text matches.
        // This is the DOM half of the html-filtering the content blocker cannot
        // do; it runs after the node exists rather than editing the response.
        'remove-node-text': function (tag, pattern) {
            var wanted = matcher(pattern);
            var selector = String(tag || '*');
            return domSweep(selector, function (el) {
                if (!wanted(el.textContent || '')) return false;
                if (el.parentNode) { el.parentNode.removeChild(el); return true; }
                return false;
            });
        },

        // href-sanitizer(selector, source) — rewrite a tracking redirect to the
        // destination it is hiding. `source` names a query parameter, or `?`
        // for "the whole query is the url".
        'href-sanitizer': function (selector, source) {
            var from = source || '?';
            return domSweep(selector || 'a[href]', function (el) {
                var href = el.getAttribute && el.getAttribute('href');
                if (!href) return false;
                var target = null;
                try {
                    var url = new W.URL(href, W.location.href);
                    if (from === '?') {
                        target = decodeURIComponent(url.search.slice(1));
                    } else if (from.charAt(0) === '?') {
                        target = url.searchParams.get(from.slice(1));
                    } else if (from === 'text') {
                        target = (el.textContent || '').trim();
                    }
                } catch (e) { return false; }
                if (!target || !/^https?:\/\//i.test(target) || target === href) return false;
                el.setAttribute('href', target);
                return true;
            });
        },
    };

    // ---- helpers the scriptlets share ------------------------------------

    function adjustTimer(which, search, delay, boost) {
        var wanted = matcher(search);
        var want = delay === undefined || delay === '' || delay === '*'
            ? null : parseInt(delay, 10);
        var factor = parseFloat(boost);
        if (!isFinite(factor) || factor <= 0) factor = 0.02;
        var orig = W[which];
        if (typeof orig !== 'function') return false;
        W[which] = function (fn, ms) {
            var current = ms | 0;
            if (wanted(sourceOf(fn)) && (want === null || want === current)) {
                state.applied += 1;
                var args = Array.prototype.slice.call(arguments);
                args[1] = Math.round(current * factor);
                return orig.apply(W, args);
            }
            return orig.apply(W, arguments);
        };
        return true;
    }

    function storageItem(which, key, value) {
        var store;
        try { store = W[which]; } catch (e) { return false; }
        if (!store || !key) return false;
        // ⚠ THE KEY CAN BE A PATTERN, and treating it as a literal is a silent
        // no-op. Measured live on soundcloud.com 2026-07-31:
        // `+js(set-local-storage-item, /sc_tracking_anonymous_id|statsig/, $remove$)`
        // reported success and left the tracking id sitting in localStorage,
        // because `removeItem('/sc_.../')` removes a key nobody ever wrote.
        var isPattern = String(key).charAt(0) === '/' && String(key).lastIndexOf('/') > 0;
        var wanted = isPattern ? matcher(key) : null;
        var keysToTouch = function () {
            if (!wanted) return [key];
            var out = [];
            try {
                for (var i = 0; i < store.length; i++) {
                    var name = store.key(i);
                    if (name !== null && wanted(name)) out.push(name);
                }
            } catch (e) { /* a page that sealed the store */ }
            return out;
        };
        var write = function () {
            var keys = keysToTouch();
            for (var i = 0; i < keys.length; i++) {
                try {
                    if (value === '$remove$') store.removeItem(keys[i]);
                    else store.setItem(keys[i], String(value));
                    state.applied += 1;
                } catch (e) { /* quota, or a page that sealed it */ }
            }
        };
        write();
        // The page usually writes its own value back once it boots, so the
        // removal has to happen again after that.
        if (W.addEventListener) W.addEventListener('load', write, false);
        return true;
    }

    function decoyWindow() {
        var noop = function () {};
        return {
            blur: noop, close: noop, focus: noop,
            closed: false, opener: null, parent: null, top: null,
            document: { open: noop, close: noop, write: noop, writeln: noop },
            location: { href: 'about:blank', assign: noop, replace: noop, reload: noop },
        };
    }

    // `key:value` conditions over a request, space-separated. A bare word (or
    // an unknown key) matches the URL, which is what most filters mean.
    function requestMatcher(conditions) {
        var text = String(conditions === undefined ? '' : conditions).trim();
        if (!text) return function () { return true; };
        var tests = [];
        text.split(/\s+/).forEach(function (token) {
            var at = token.indexOf(':');
            var key = at === -1 ? 'url' : token.slice(0, at);
            var pattern = at === -1 ? token : token.slice(at + 1);
            if (key !== 'method' && key !== 'url') key = 'url';
            tests.push({ key: key, test: matcher(pattern) });
        });
        return function (url, init) {
            for (var i = 0; i < tests.length; i++) {
                var value = tests[i].key === 'method'
                    ? String((init && init.method) || 'GET')
                    : String(url || '');
                if (!tests[i].test(value)) return false;
            }
            return true;
        };
    }

    function readPath(root, path) {
        var parts = String(path).split('.');
        var node = root;
        for (var i = 0; i < parts.length; i++) {
            if (!node || typeof node !== 'object') return undefined;
            node = node[parts[i]];
        }
        return node;
    }

    // Delete a dotted path, honouring the `[]` wildcard segment the lists use
    // for "every element of this array".
    function deletePath(root, path) {
        var parts = String(path).split('.');
        var leaf = parts.pop();
        var nodes = [root];
        for (var i = 0; i < parts.length; i++) {
            var next = [];
            for (var j = 0; j < nodes.length; j++) {
                var node = nodes[j];
                if (!node || typeof node !== 'object') continue;
                if (parts[i] === '[]' || parts[i] === '*') {
                    for (var k in node) {
                        if (node[k] && typeof node[k] === 'object') next.push(node[k]);
                    }
                } else if (node[parts[i]]) {
                    next.push(node[parts[i]]);
                }
            }
            nodes = next;
        }
        for (var n = 0; n < nodes.length; n++) {
            if (nodes[n] && typeof nodes[n] === 'object') {
                try { delete nodes[n][leaf]; } catch (e) { /* frozen */ }
            }
        }
    }

    // One coalesced observer for every DOM-editing scriptlet on the page, not
    // one per rule. The same contract cosmetic-filters holds: a pass per
    // animation frame at most, and nothing at all on a page with no rules.
    var sweeps = [];
    var sweepQueued = false;
    function domSweep(selector, edit) {
        if (!doc || !selector) return false;
        sweeps.push({ selector: selector, edit: edit });
        scheduleSweep();
        return true;
    }
    function runSweeps() {
        sweepQueued = false;
        for (var i = 0; i < sweeps.length; i++) {
            var nodes;
            try { nodes = doc.querySelectorAll(sweeps[i].selector); } catch (e) { continue; }
            for (var j = 0; j < nodes.length; j++) {
                try { if (sweeps[i].edit(nodes[j])) state.applied += 1; } catch (e) {}
            }
        }
    }
    function scheduleSweep() {
        if (sweepQueued || !sweeps.length) return;
        sweepQueued = true;
        var onFrame = W.requestAnimationFrame
            ? function (fn) { W.requestAnimationFrame(fn); }
            : function (fn) { W.setTimeout(fn, 16); };
        onFrame(runSweeps);
    }
    if (doc && W.MutationObserver) {
        var observer = new W.MutationObserver(scheduleSweep);
        var attach = function () {
            if (doc.documentElement) {
                observer.observe(doc.documentElement, { childList: true, subtree: true });
            }
            scheduleSweep();
        };
        if (doc.readyState === 'loading' && doc.addEventListener) {
            doc.addEventListener('DOMContentLoaded', attach, { once: true });
            attach();
        } else {
            attach();
        }
    }

    // ---- dispatch --------------------------------------------------------
    // One rule at a time, each in its own try/catch. A scriptlet that throws
    // takes down its own filter and nothing else — a page must never lose the
    // other twenty rules because one of them met a sealed global.
    for (var r = 0; r < mine.length; r++) {
        var rule = mine[r];
        var name = rule[0];
        var impl = SCRIPTLETS[name];
        if (typeof impl !== 'function') { state.failed += 1; continue; }
        try {
            if (impl.apply(null, rule.slice(1))) {
                state.applied += 1;
                state.by[name] = (state.by[name] || 0) + 1;
            } else {
                state.refused += 1;
            }
        } catch (e) {
            state.failed += 1;
        }
    }
    return state;
}
