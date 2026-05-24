// tear-off.js — Abstract Factory for "tear off into a standalone window,
// merge back later" behavior.
//
// Two features in the dashboard want the same shape of interaction:
//   * console tabs  → pop a live tmux terminal into its own browser window
//   * grid panels   → pop a dashboard panel into its own browser window
//
// They differ only in *what* gets opened, how the item is removed from the
// dashboard, and how it is re-attached on merge. Everything else (open the
// popup, refuse gracefully when blocked, coordinate merge-back over a
// same-origin BroadcastChannel) is identical.
//
// So we model it as an Abstract Factory: a concrete factory supplies the
// family of products for one item kind, and createController() assembles
// them into a working tear-off controller. Adding a third tear-off-able
// thing later means writing one more factory, not more wiring.
//
//   var ctrl = GTTearOff.createController(factory);
//   ctrl.popOut(item);   // open window + detach from dashboard
//
// Abstract factory contract (all methods required):
//   factory.channelName()      -> string : BroadcastChannel name (per family)
//   factory.windowFeatures()   -> string : window.open() feature string
//   factory.urlFor(item)       -> string : popup URL for this item
//   factory.onDetach(item)     -> void   : remove the item from the dashboard
//   factory.onMerge(msg)       -> void   : re-attach the item (merge message)
//   factory.matches(msg)       -> bool   : is this merge message ours?
(function (global) {
    'use strict';

    // Assemble a factory's product family into a controller. The controller
    // owns the BroadcastChannel and listens for merge-back messages for the
    // lifetime of the page.
    function createController(factory) {
        var channel = null;
        try {
            channel = new BroadcastChannel(factory.channelName());
        } catch (e) {
            // BroadcastChannel unsupported — pop-out still works, merge-back
            // is just unavailable. Silent fallback.
        }

        if (channel) {
            channel.addEventListener('message', function (ev) {
                var msg = ev.data || {};
                if (msg.type === 'merge' && factory.matches(msg)) {
                    factory.onMerge(msg);
                }
            });
        }

        return {
            // Open the item in a standalone window and detach it from the
            // dashboard. Returns the window handle, or null if the popup was
            // blocked (in which case the item is left attached untouched).
            popOut: function (item) {
                var win = global.open(
                    factory.urlFor(item),
                    '_blank',
                    factory.windowFeatures()
                );
                if (!win) return null;
                factory.onDetach(item);
                return win;
            },
            channel: channel,
        };
    }

    // Popup-side helper: a thin wrapper over the same BroadcastChannel for
    // the standalone window to announce that it is merging back. `merge`
    // is idempotent on the dashboard side, so calling it from both an
    // explicit button and `beforeunload` is safe.
    function createMergeChannel(channelName) {
        var channel = null;
        try {
            channel = new BroadcastChannel(channelName);
        } catch (e) { /* silent fallback */ }
        return {
            channel: channel,
            merge: function (payload) {
                if (!channel) return;
                payload = payload || {};
                payload.type = 'merge';
                channel.postMessage(payload);
            },
        };
    }

    global.GTTearOff = {
        createController: createController,
        createMergeChannel: createMergeChannel,
    };
})(window);
