// help-popover.js — reusable "?" info popover component.
//
// Any box (panel, card, drawer …) inherits the behavior by:
//   1. Putting a trigger in its header:
//        <button class="help-btn" aria-expanded="false">?</button>
//      (optionally <button class="help-btn" data-help="my-key">?</button>
//       to use an explicit content key)
//   2. Registering the content once at startup:
//        GTHelpPopover.register('convoy-panel', {
//            title: '🚚 Convoys',
//            html:  '<p>…</p><ul><li>…</li></ul>',
//        });
//   3. Installing the component once per page:
//        GTHelpPopover.install({ footer: '<code>docs/...</code>' });
//
// One popover is open at a time. ESC / outside-click / ✕ all dismiss it.
// When no `data-help` is set, the key is the id of the closest `[id]`
// ancestor — so panels automatically resolve to their own id.
(function (global) {
    'use strict';

    var content = {};         // key -> { title, html }
    var footer  = '';         // shared footer markup (e.g. doc pointer)
    var active  = null;       // { btn, popover } | null
    var installed = false;

    function buildPopover(c) {
        var p = document.createElement('div');
        p.className = 'help-popover';
        p.innerHTML =
            '<div class="help-popover-header">' +
              '<span>' + c.title + '</span>' +
              '<button class="help-popover-close" type="button" aria-label="Cerrar">✕</button>' +
            '</div>' +
            '<div class="help-popover-body">' + c.html + '</div>' +
            (footer ? '<div class="help-popover-footer">' + footer + '</div>' : '');
        return p;
    }

    function close() {
        if (!active) return;
        active.popover.remove();
        active.btn.setAttribute('aria-expanded', 'false');
        active = null;
    }

    function open(btn, key) {
        close();
        var c = content[key];
        if (!c) return;
        var p = buildPopover(c);
        // Append to body so the host container's overflow:hidden can't clip
        // the popover. Position is computed in viewport coords + scroll.
        document.body.appendChild(p);
        var r = btn.getBoundingClientRect();
        p.style.top = (r.bottom + global.scrollY + 8) + 'px';
        var raw = r.left + global.scrollX - 8;
        var maxLeft = global.scrollX + global.innerWidth - p.offsetWidth - 8;
        p.style.left = Math.max(8, Math.min(raw, maxLeft)) + 'px';
        btn.setAttribute('aria-expanded', 'true');
        active = { btn: btn, popover: p };
    }

    function resolveKey(btn) {
        var k = btn.getAttribute('data-help');
        if (k) return k;
        var host = btn.closest('[id]');
        return host ? host.id : null;
    }

    function install(opts) {
        if (installed) return;
        installed = true;
        opts = opts || {};
        footer = opts.footer || '';

        document.addEventListener('click', function (e) {
            var btn = e.target.closest('.help-btn');
            if (btn) {
                e.preventDefault();
                e.stopPropagation();
                var key = resolveKey(btn);
                if (!key) return;
                if (active && active.btn === btn) close();
                else open(btn, key);
                return;
            }
            if (e.target.closest('.help-popover-close')) { close(); return; }
            if (active && !e.target.closest('.help-popover')) close();
        });

        document.addEventListener('keydown', function (e) {
            if (e.key === 'Escape' && active) close();
        });
    }

    global.GTHelpPopover = {
        register:    function (key, c) { content[key] = c; },
        registerAll: function (map) { for (var k in map) content[k] = map[k]; },
        install:     install,
        close:       close,
    };
})(window);
