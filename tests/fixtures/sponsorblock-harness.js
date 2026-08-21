// Harness for the `sponsorblock` userscript, run by the Rust test in
// src/sponsorblock.rs. It loads the body the CATALOG serves — not a copy —
// beside a preamble this repo's own encoder produced, and reads back what the
// script RESOLVED.
//
//   node sponsorblock-harness.js <path-to-userscript.js> <path-to-preamble.js> <expectations.json>
//
// ⛔ WHY IT EXISTS. Until v2.1.0 the script read `window.__ysbConfig[id]` while
// ychrome had always written `{categories:{…}}`, so every per-category choice
// made in the settings pane was silently discarded and the script's own default
// table was what ran. The test that was supposed to cover this asserted only
// that each category id and colour APPEARED in the preamble text — true of the
// broken shape too. A substring can prove a value was WRITTEN. Only running
// both halves together proves it was READ.
'use strict';

const fs = require('fs');
const vm = require('vm');

const scriptPath = process.argv[2];
const preamblePath = process.argv[3];
const expected = JSON.parse(fs.readFileSync(process.argv[4], 'utf8'));

function check(name, condition, detail) {
    if (!condition) throw new Error(`FAILED ${name}${detail ? `: ${detail}` : ''}`);
    console.log(`ok ${name}`);
}

// ---- a stub page, thin on purpose ----------------------------------------
// The script binds to a player, watches the SPA and schedules timers. None of
// that is under test here, so every one of them answers "nothing found" and the
// script settles immediately. `pathname` is deliberately NOT `/watch`: with no
// video id there is no lookup, so the harness never reaches for the network.
const listeners = [];
const documentElement = {
    attributes: {},
    setAttribute(name, value) { this.attributes[name] = value; },
    getAttribute(name) { return this.attributes[name]; },
};
const context = {
    console,
    setTimeout: () => 0,
    clearTimeout: () => {},
    setInterval: () => 0,
    URLSearchParams,
    TextEncoder,
    Date,
    Math,
    JSON,
    Object,
    Promise,
    isFinite,
    fetch: () => Promise.reject(new Error('the harness makes no network calls')),
    location: { hostname: 'www.youtube.com', pathname: '/', search: '', href: 'https://www.youtube.com/' },
    document: {
        documentElement,
        body: { appendChild() {} },
        addEventListener(...args) { listeners.push(args); },
        querySelector: () => null,
        querySelectorAll: () => [],
        getElementById: () => null,
        createElement: () => ({
            style: { cssText: '' },
            dataset: {},
            appendChild() {},
            remove() {},
            addEventListener() {},
            get isConnected() { return false; },
        }),
    },
};
context.window = context;
context.globalThis = context;
vm.createContext(context);

// The two halves, in the order `webpolicy::policy()` injects them.
vm.runInContext(fs.readFileSync(preamblePath, 'utf8'), context, { filename: 'preamble.js' });
vm.runInContext(fs.readFileSync(scriptPath, 'utf8'), context, { filename: 'sponsorblock.js' });

check('the script loaded', !!context.window.__ysb, 'window.__ysb is absent');

// ---- what the script RESOLVED, not what the preamble said ----------------
const resolved = context.window.__ysb.config();
for (const [id, want] of Object.entries(expected.categories || {})) {
    check(
        `category ${id} resolves to ${want}`,
        resolved[id] && resolved[id].behaviour === want,
        `got ${resolved[id] && resolved[id].behaviour}`,
    );
}
for (const [id, want] of Object.entries(expected.colors || {})) {
    check(`category ${id} keeps its colour`, resolved[id] && resolved[id].color === want,
        `got ${resolved[id] && resolved[id].color}`);
}

const prefs = context.window.__ysb.prefs();
for (const [key, want] of Object.entries(expected.prefs || {})) {
    check(`preference ${key} resolves to ${JSON.stringify(want)}`, prefs[key] === want,
        `got ${JSON.stringify(prefs[key])}`);
}

// ⛔ The privacy default is a PROPERTY OF THE SCRIPT, not of the preamble: a
// copy of this file deployed by hand with nothing to configure it must not
// contribute to a shared public database on its own.
if (expected.identified === false) {
    check('no write credential reached the page', prefs.userId === null,
        `got ${JSON.stringify(prefs.userId)}`);
    check('voting is off without one', prefs.voting === false);
    check('submission is off without one', prefs.submission === false);
}
if (expected.identified === true) {
    check('the write credential reached the page', typeof prefs.userId === 'string' && prefs.userId.length > 0);
}

console.log('ALL OK');
