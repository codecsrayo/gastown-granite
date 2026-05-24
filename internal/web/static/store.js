// store.js — a tiny observable state store (Observer) with optional
// localStorage persistence (Memento).
//
// One store owns a slice of state; callers mutate it through set() and react
// to every change through subscribe(). When a `key` is given the persisted
// projection of the state is written to localStorage on each change and
// re-hydrated on construction — so reloads (and, for us, HTMX morphs that
// blow away client-only DOM state) can be restored from a single source of
// truth instead of scattered globals + ad-hoc save/load calls.
//
//   var store = GTStore.create({
//       key:     'my-feature',                 // omit for in-memory only
//       initial: { collapsed: {}, open: {} },
//       persist: function (s) { return { collapsed: s.collapsed }; },
//   });
//   store.subscribe(function (s) { render(s); });   // fires immediately
//   store.set(function (s) { s.collapsed['x'] = true; });
//   store.get();
(function (global) {
    'use strict';

    // Recursive merge of plain-object maps (one or two levels, which is all
    // our UI state needs). Arrays and primitives from `b` win outright.
    function mergeDeep(a, b) {
        var out = {}, k;
        for (k in a) out[k] = a[k];
        for (k in b) {
            if (b[k] && typeof b[k] === 'object' && !Array.isArray(b[k])) {
                out[k] = mergeDeep(a[k] || {}, b[k]);
            } else {
                out[k] = b[k];
            }
        }
        return out;
    }

    function create(opts) {
        opts = opts || {};
        var key     = opts.key || null;
        var persist = opts.persist || function (s) { return s; };
        var state   = opts.initial || {};
        var subs    = [];

        // Memento: re-hydrate the persisted projection over the initial state.
        if (key) {
            try {
                var saved = JSON.parse(localStorage.getItem(key));
                if (saved) state = mergeDeep(state, saved);
            } catch (e) { /* corrupt/absent — start from initial */ }
        }

        function save() {
            if (!key) return;
            try { localStorage.setItem(key, JSON.stringify(persist(state))); }
            catch (e) { /* storage full/unavailable — ignore */ }
        }

        function emit() {
            for (var i = 0; i < subs.length; i++) subs[i](state);
        }

        return {
            get: function () { return state; },

            // Mutate state via an updater (mutates the draft) or a shallow
            // patch object. Persists, then notifies every subscriber.
            set: function (updater) {
                if (typeof updater === 'function') updater(state);
                else if (updater) { for (var k in updater) state[k] = updater[k]; }
                save();
                emit();
            },

            // Register an observer. Fires immediately with current state
            // unless immediate === false. Returns an unsubscribe fn.
            subscribe: function (fn, immediate) {
                subs.push(fn);
                if (immediate !== false) fn(state);
                return function () {
                    var i = subs.indexOf(fn);
                    if (i >= 0) subs.splice(i, 1);
                };
            },
        };
    }

    global.GTStore = { create: create };
})(window);
