// Harness for the GENERATED `cosmetic-filters` userscript, run by the Rust test
// in src/abp.rs. It executes the body the CATALOG serves — not a copy — against
// a stub DOM, so a generator that starts emitting a payload the runtime cannot
// read fails here rather than on a user's screen.
//
//   node cosmetic-filters-harness.js <path-to-userscript.js> <hostname>
//
// A source needle can prove `:has-text` is mentioned. Only this can prove the
// element gets hidden.
'use strict';

const fs = require('fs');
const vm = require('vm');

const scriptPath = process.argv[2];
const hostname = process.argv[3] || 'example.com';
const source = fs.readFileSync(scriptPath, 'utf8');

const checks = [];
function check(name, condition, detail) {
    if (!condition) {
        throw new Error(`FAILED ${name}${detail ? `: ${detail}` : ''}`);
    }
    checks.push(name);
    console.log(`ok ${name}`);
}

// The rules the generated script carries for this host are read back out of its
// own payload, so the harness never has to guess what the corpus contained.
const payloadMatch = source.match(/var RULES = (\{.*?\});\n/s);
if (!payloadMatch) throw new Error('the generated script has no RULES payload');
const RULES = JSON.parse(payloadMatch[1]);

function rulesFor(host) {
    let mine = [];
    for (const key of Object.keys(RULES)) {
        if (host === key || (host.length > key.length && host.slice(-(key.length + 1)) === `.${key}`)) {
            mine = mine.concat(RULES[key]);
        }
    }
    return mine;
}

// ---- a stub DOM, thin on purpose -----------------------------------------
// Elements answer only what the script asks: a selector match, textContent, and
// a style bag that records what was set.
function makeElement(selector, text) {
    const properties = {};
    return {
        selector,
        textContent: text,
        style: {
            setProperty(name, value, priority) {
                properties[name] = { value, priority };
            },
        },
        properties,
    };
}

const elements = [];
const sandbox = {
    console,
    Object,
    Array,
    JSON,
    String,
    setTimeout: (fn) => { fn(); return 0; },
};
sandbox.window = sandbox;
sandbox.globalThis = sandbox;
sandbox.location = { hostname, href: `https://${hostname}/` };
sandbox.requestAnimationFrame = (fn) => { fn(); return 0; };
sandbox.MutationObserver = function MutationObserver(callback) {
    this.callback = callback;
    this.observe = () => {};
    this.disconnect = () => {};
};
// `documentElement` carries real attribute storage: the script publishes its
// state there, because the DOM is the one thing the isolated world and the page
// world share. A stub without these would make the publish silently a no-op and
// the harness would prove the opposite of what it claims.
const rootAttributes = Object.create(null);
sandbox.document = {
    readyState: 'complete',
    documentElement: {
        setAttribute(name, value) {
            rootAttributes[name] = String(value);
        },
        getAttribute(name) {
            return name in rootAttributes ? rootAttributes[name] : null;
        },
    },
    addEventListener() {},
    querySelectorAll(selector) {
        return elements.filter((el) => el.selector === selector);
    },
};

const mine = rulesFor(hostname);
// Seed one element per rule this host has, each shaped so the rule MUST fire:
// a has-text element whose text carries the phrase, plus a decoy whose text
// does not, and a style element.
const decoys = [];
for (const rule of mine) {
    if (rule[0] === 't') {
        elements.push(makeElement(rule[1], `... ${rule[2]} ...`));
        const decoy = makeElement(rule[1], 'nothing of interest here');
        decoys.push(decoy);
        elements.push(decoy);
    } else {
        elements.push(makeElement(rule[1], ''));
    }
}

vm.createContext(sandbox);
vm.runInContext(source, sandbox, { filename: scriptPath });

if (mine.length === 0) {
    // An unlisted host: the script must bail before it does any WORK. This is
    // the performance contract, and @match only enforces it in the engine — the
    // script must hold it too, for a GUI too old to apply @match.
    //
    // ⛔ BUT IT MUST STILL SAY IT RAN. This check used to require
    // `__yggCosmeticState === undefined`, which made "ran, nothing to do here"
    // and "never loaded at all" the same reading — and the second is the
    // failure the whole provisioning lane exists to detect. So the state is
    // published BEFORE the early return: `rules: 0` with no passes.
    const idle = sandbox.__yggCosmeticState;
    check('an unlisted host still reports that it ran', idle !== undefined);
    check('an unlisted host has no rules', idle && idle.rules === 0);
    check('an unlisted host does no work', idle && idle.passes === 0);
    check(
        'an unlisted host publishes to the DOM, which both worlds share',
        sandbox.document.documentElement.getAttribute('data-ycf') !== null,
    );
    console.log(`ALL OK (${checks.length} checks, host=${hostname}, unlisted)`);
    process.exit(0);
}

const state = sandbox.__yggCosmeticState;
check('a listed host runs', state !== undefined);

const hasText = mine.filter((rule) => rule[0] === 't');
const styled = mine.filter((rule) => rule[0] === 's');

if (hasText.length) {
    check('every :has-text element whose text matches is hidden', state.hidden === hasText.length,
        `hidden=${state.hidden} expected=${hasText.length}`);
    const hiddenOnes = elements.filter((el) => el.properties.display);
    check('…and it is hidden with !important, which a page cannot outrank',
        hiddenOnes.every((el) => el.properties.display.priority === 'important'));
    check('…and an element whose text does NOT match is left alone',
        decoys.every((el) => el.properties.display === undefined),
        `${decoys.filter((el) => el.properties.display).length} decoys were hidden`);
}

if (styled.length) {
    check('every :style rule applied its declarations', state.styled === styled.length,
        `styled=${state.styled} expected=${styled.length}`);
    // `overflow: auto !important` is the consent-banner scroll unlock, and the
    // `!important` has to survive the split or the banner's own rule wins.
    const important = styled.find((rule) => rule[2].toLowerCase().includes('!important'));
    if (important) {
        const el = elements.find((candidate) => candidate.selector === important[1]);
        const name = important[2].split(':')[0].trim();
        check('…and !important survives the declaration split',
            el.properties[name] && el.properties[name].priority === 'important',
            JSON.stringify(el.properties));
        check('…and the value has the !important token stripped off it',
            !String(el.properties[name].value).toLowerCase().includes('important'),
            JSON.stringify(el.properties[name]));
    }
}

console.log(`ALL OK (${checks.length} checks, host=${hostname}, rules=${mine.length})`);
process.exit(0);
