// ychrome bundled userscript: re-enable text selection and right-click.
// Many sites disable copy/select/context-menu to stop you saving text or
// images. This restores them: it strips the CSS `user-select:none`, removes the
// inline handlers that swallow selectstart/copy/contextmenu, and stops those
// events being cancelled at the document level. Small, safe, reversible (disable
// = rename away from .js). Injected at document-start into the top frame.
(function () {
    'use strict';
    if (window.__yunblock_loaded) return;
    window.__yunblock_loaded = true;

    // Let selection, copy, context menu and drag through even when a page (or a
    // library) tries to cancel them. Capture phase, so we run before the page's
    // own listeners and undo a preventDefault they would otherwise apply.
    var EVENTS = ['selectstart', 'copy', 'cut', 'contextmenu', 'dragstart', 'mousedown'];
    EVENTS.forEach(function (name) {
        document.addEventListener(name, function (event) {
            event.stopPropagation();
        }, true);
    });

    // Clear the common inline blockers a page ships in its HTML.
    function clearInlineBlockers(root) {
        var props = ['onselectstart', 'oncontextmenu', 'oncopy', 'oncut', 'ondragstart'];
        var nodes = [root];
        if (root.querySelectorAll) {
            nodes = nodes.concat(Array.prototype.slice.call(root.querySelectorAll('*')));
        }
        nodes.forEach(function (el) {
            props.forEach(function (prop) {
                if (el && el[prop]) { try { el[prop] = null; } catch (e) { /* ignore */ } }
            });
        });
    }

    // Undo `user-select:none` globally with a late, high-specificity rule.
    function injectCss() {
        var style = document.createElement('style');
        style.textContent =
            '*, *::before, *::after { -webkit-user-select: text !important; user-select: text !important; }';
        (document.head || document.documentElement).appendChild(style);
    }

    function run() {
        injectCss();
        clearInlineBlockers(document.documentElement);
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', run, { once: true });
    } else {
        run();
    }
})();
