(function() {
    'use strict';

    // ============================================
    // CSRF PROTECTION
    // ============================================
    // Inject dashboard token into all POST requests to prevent cross-site request forgery.
    var _origFetch = window.fetch;
    var _csrfMeta = document.querySelector('meta[name="dashboard-token"]');
    var _csrfToken = _csrfMeta ? _csrfMeta.getAttribute('content') : '';
    window.fetch = function(url, opts) {
        opts = opts || {};
        if (opts.method && opts.method.toUpperCase() === 'POST' && _csrfToken) {
            opts.headers = opts.headers || {};
            opts.headers['X-Dashboard-Token'] = _csrfToken;
        }
        return _origFetch.call(this, url, opts);
    };

    // ============================================
    // ICON HELPER
    // ============================================
    // Build a Tabler SVG sprite reference. Sprite lives in #gt-icon-sprite (inlined in body).
    function icon(name, extra) {
        var cls = 'icon icon-' + name + (extra ? ' ' + extra : '');
        return '<svg class="' + cls + '" aria-hidden="true"><use href="#icon-' + name + '"/></svg>';
    }
    window.gtIcon = icon;

    // ============================================
    // SSE (Server-Sent Events) CONNECTION
    // ============================================
    window.sseConnected = false;
    var evtSource = null;
    var sseReconnectDelay = 1000;
    var sseMaxReconnectDelay = 30000;

    function connectSSE() {
        if (evtSource) {
            evtSource.close();
        }

        evtSource = new EventSource('/api/events');

        evtSource.addEventListener('connected', function() {
            window.sseConnected = true;
            sseReconnectDelay = 1000;
            updateConnectionStatus('live');
        });

        evtSource.addEventListener('dashboard-update', function(e) {
            if (window.pauseRefresh) return;
            // Trigger HTMX to re-fetch the dashboard
            var dashboard = document.getElementById('dashboard-main');
            if (dashboard && typeof htmx !== 'undefined') {
                htmx.trigger(dashboard, 'sse:dashboard-update');
            }
        });

        // Surgical panel dispatch.
        // Each typed event maps to the panel(s) that depend on it. The
        // panels re-fetch / and morph in just that fragment via hx-select,
        // avoiding a full-page re-render while still keeping the SSE handler
        // dumb (server doesn't need per-panel endpoints).
        //
        // Event types come from internal/events/events.go.
        var EVENT_TO_PANELS = {
            // Escalation lifecycle
            escalation_sent:        ['#escalations-panel'],
            escalation_acked:       ['#escalations-panel'],
            escalation_closed:      ['#escalations-panel'],
            escalation_reassigned:  ['#escalations-panel'],
            // Mail flow
            mail_sent:              ['#mail-panel'],
            mail_received:          ['#mail-panel'],
            // Session / agent lifecycle
            session_start:          ['#convoy-panel', '#polecats-panel', '#sessions-panel', '#activity-panel'],
            session_end:            ['#convoy-panel', '#polecats-panel', '#sessions-panel', '#activity-panel'],
            boot:                   ['#convoy-panel', '#polecats-panel', '#sessions-panel'],
            nudge:                  ['#activity-panel'],
            handoff:                ['#convoy-panel', '#activity-panel'],
            // Work assignment
            sling:                  ['#convoy-panel', '#hooks-panel', '#work-panel'],
            done:                   ['#convoy-panel', '#hooks-panel', '#merge-queue-panel'],
            hook_attached:          ['#hooks-panel'],
            hook_detached:          ['#hooks-panel'],
            // Merge queue
            merge_started:          ['#merge-queue-panel'],
            merged:                 ['#merge-queue-panel', '#convoy-panel'],
            merge_failed:           ['#merge-queue-panel', '#escalations-panel'],
            // Polecat health
            polecat_checked:        ['#polecats-panel'],
            polecat_nudged:         ['#polecats-panel', '#activity-panel'],
            mass_death:             ['#polecats-panel', '#escalations-panel'],
            // Account quota rotation — hydrated by the /api/quota/stream SSE
            // subscriber, not by re-fetching the full dashboard page.
            // refreshPanel special-cases '#quota-drawer' to call
            // window.refreshQuotaDrawer (re-paints from the cached snapshot)
            // instead of htmx.ajax so the JSON-driven mosaic isn't blown away.
            quota_scanned:          ['#quota-drawer'],
            quota_rotated:          ['#quota-drawer'],
            quota_swap_failed:      ['#quota-drawer'],
            quota_limited:          ['#quota-drawer'],
            quota_near_limit:       ['#quota-drawer'],
            quota_cleared:          ['#quota-drawer'],
            quota_token_expired:    ['#quota-drawer'],
            quota_blocked:          ['#quota-drawer'],
            quota_assigned:         ['#quota-drawer'],
        };

        function refreshPanel(selector) {
            var el = document.querySelector(selector);
            if (!el || typeof htmx === 'undefined' || window.pauseRefresh) return;
            // Quota drawer is JSON-hydrated — defer to the dedicated loader so
            // the page-fetch swap doesn't replace the mosaic with the empty
            // server-side placeholder.
            if (selector === '#quota-drawer') {
                if (window.refreshQuotaDrawer) window.refreshQuotaDrawer();
                return;
            }
            // Re-fetch the full page but only swap in this panel's outerHTML.
            htmx.ajax('GET', '/', {
                target: selector,
                select: selector,
                swap: 'outerHTML',
            });
        }

        // Login URL detected in a session pane — show a persistent toast
        // with a one-click copy button so the user doesn't have to fight
        // tmux selection to grab the OAuth URL.
        evtSource.addEventListener('login_required', function(e) {
            try {
                var data = JSON.parse(e.data || '{}');
                var payload = data.payload || {};
                var session = payload.session || 'unknown session';
                var url = payload.url || '';
                if (!url) return;
                showActionToast({
                    tag: 'login:' + session,
                    type: 'warning',
                    icon: '<svg class="icon icon-lock" aria-hidden="true"><use href="#icon-lock"/></svg>',
                    title: 'Account needs login',
                    message: session + ' is waiting on OAuth — open the URL in a browser to authenticate.',
                    actionValue: url,
                    actionLabel: 'Copy URL',
                });
            } catch (err) {
                console.warn('login_required: bad payload', err);
            }
        });

        Object.keys(EVENT_TO_PANELS).forEach(function(evtType) {
            evtSource.addEventListener(evtType, function(e) {
                if (window.pauseRefresh) return;
                var panels = EVENT_TO_PANELS[evtType] || [];
                // Debounce per-panel: collapse rapid bursts into one fetch.
                panels.forEach(function(sel) {
                    if (refreshPanel._timers && refreshPanel._timers[sel]) {
                        clearTimeout(refreshPanel._timers[sel]);
                    }
                    refreshPanel._timers = refreshPanel._timers || {};
                    refreshPanel._timers[sel] = setTimeout(function() {
                        refreshPanel(sel);
                    }, 250);
                });
            });
        });

        evtSource.onerror = function() {
            window.sseConnected = false;
            updateConnectionStatus('reconnecting');
            evtSource.close();
            // Exponential backoff reconnect
            setTimeout(function() {
                sseReconnectDelay = Math.min(sseReconnectDelay * 2, sseMaxReconnectDelay);
                connectSSE();
            }, sseReconnectDelay);
        };
    }

    function updateConnectionStatus(state) {
        var el = document.getElementById('connection-status');
        if (!el) return;
        switch (state) {
            case 'live':
                el.textContent = 'Live';
                el.className = 'connection-live';
                break;
            case 'reconnecting':
                el.textContent = 'Reconnecting...';
                el.className = 'connection-reconnecting';
                break;
            default:
                el.textContent = 'Connecting...';
                el.className = '';
        }
    }

    // Start SSE connection
    connectSSE();

    // ============================================
    // GIT ACTIVITY FEED
    // ============================================
    // Subscribes to /api/git/events (separate SSE stream from the main
    // dashboard channel) and renders each ref change in the #git-feed panel.
    // Backend buffers the last ~200 events so newly-opened tabs see history.
    (function gitFeedInit() {
        var feed = document.getElementById('git-feed');
        var emptyEl = document.getElementById('git-empty');
        var countEl = document.getElementById('git-count');
        var statusEl = document.getElementById('git-status');
        var repoFilterEl = document.getElementById('git-repo-filter');
        var clearBtn = document.getElementById('git-clear-btn');
        var kindBtns = document.querySelectorAll('.git-kind-btn');
        if (!feed) return;

        var MAX_ENTRIES = 200;
        var entries = [];
        var gitSource = null;
        var reconnectDelay = 1000;
        var reconnectMax = 30000;

        // Filters
        var repos = new Set();
        var activeKindGroups = new Set();
        kindBtns.forEach(function(b) { activeKindGroups.add(b.dataset.kind); });

        function kindGroup(kind) {
            if (kind === 'commit') return 'commit';
            if (kind === 'branch_create' || kind === 'branch_delete') return 'branch';
            if (kind === 'remote_update' || kind === 'remote_create' || kind === 'remote_delete') return 'remote';
            if (kind === 'head_change') return 'head_change';
            return 'other';
        }

        function eventMatches(ev) {
            var repoSel = repoFilterEl ? repoFilterEl.value : 'all';
            if (repoSel !== 'all' && ev.repo_label !== repoSel) return false;
            if (!activeKindGroups.has(kindGroup(ev.kind))) return false;
            return true;
        }

        function ensureRepoOption(label) {
            if (!repoFilterEl || !label || repos.has(label)) return;
            repos.add(label);
            var opt = document.createElement('option');
            opt.value = label;
            opt.textContent = label;
            repoFilterEl.appendChild(opt);
        }

        function applyFilters() {
            var rows = feed.querySelectorAll('.git-event');
            var visible = 0;
            rows.forEach(function(row) {
                var match = activeKindGroups.has(kindGroup(row.dataset.kind)) &&
                    ((repoFilterEl && repoFilterEl.value === 'all') || row.dataset.repo === repoFilterEl.value);
                row.classList.toggle('git-hidden', !match);
                if (match) visible++;
            });
            if (countEl) countEl.textContent = String(visible);
        }

        function setStatus(state) {
            if (!statusEl) return;
            if (state === 'live') {
                statusEl.textContent = 'live';
                statusEl.className = 'git-status connected';
            } else if (state === 'reconnect') {
                statusEl.textContent = 'reconnecting…';
                statusEl.className = 'git-status disconnected';
            } else {
                statusEl.textContent = 'connecting…';
                statusEl.className = 'git-status';
            }
        }

        function fmtTime(iso) {
            var d = new Date(iso);
            if (isNaN(d.getTime())) return '';
            var hh = String(d.getHours()).padStart(2, '0');
            var mm = String(d.getMinutes()).padStart(2, '0');
            var ss = String(d.getSeconds()).padStart(2, '0');
            return hh + ':' + mm + ':' + ss;
        }

        function esc(s) {
            if (s == null) return '';
            return String(s)
                .replace(/&/g, '&amp;')
                .replace(/</g, '&lt;')
                .replace(/>/g, '&gt;')
                .replace(/"/g, '&quot;');
        }

        function render(ev) {
            var row = document.createElement('div');
            row.className = 'git-event kind-' + esc(ev.kind || 'unknown');
            row.dataset.kind = ev.kind || '';
            row.dataset.repo = ev.repo_label || '';
            var bodyHTML = '';
            if (ev.kind === 'commit' || ev.kind === 'remote_update') {
                var prefix = ev.kind === 'remote_update' ? '<svg class="icon icon-arrow-up" aria-hidden="true"><use href="#icon-arrow-up"/></svg>' + ' ' : '';
                bodyHTML =
                    '<span class="git-sha">' + esc(ev.short_sha || '') + '</span>' +
                    '<span class="git-branch">' + esc(prefix + (ev.branch || '')) + '</span>' +
                    '<span class="git-subject" title="' + esc(ev.subject || '') + '">' +
                        esc(ev.subject || '(no subject)') +
                    '</span>' +
                    (ev.author ? '<span class="git-author">' + esc(ev.author) + '</span>' : '');
            } else if (ev.kind === 'branch_create' || ev.kind === 'remote_create') {
                bodyHTML =
                    '<span class="git-branch">+ ' + esc(ev.branch || '') + '</span>' +
                    '<span class="git-sha">' + esc(ev.short_sha || '') + '</span>' +
                    (ev.subject ? '<span class="git-subject">' + esc(ev.subject) + '</span>' : '');
            } else if (ev.kind === 'branch_delete' || ev.kind === 'remote_delete') {
                bodyHTML = '<span class="git-branch">' + '<svg class="icon icon-minus" aria-hidden="true"><use href="#icon-minus"/></svg>' + ' ' + esc(ev.branch || '') + '</span>';
            } else if (ev.kind === 'head_change') {
                bodyHTML = '<span class="git-branch">' + '<svg class="icon icon-arrow-narrow-right" aria-hidden="true"><use href="#icon-arrow-narrow-right"/></svg>' + ' ' + esc(ev.branch || '') + '</span>';
            } else {
                bodyHTML = '<span class="git-subject">' + esc(JSON.stringify(ev)) + '</span>';
            }
            row.innerHTML =
                '<span class="git-time">' + esc(fmtTime(ev.ts)) + '</span>' +
                '<span class="git-kind">' + esc(ev.kind || '') + '</span>' +
                '<span class="git-repo" title="' + esc(ev.repo || '') + '">' + esc(ev.repo_label || '') + '</span>' +
                '<span class="git-body">' + bodyHTML + '</span>';
            if (!eventMatches(ev)) row.classList.add('git-hidden');
            return row;
        }

        function pushEvent(ev) {
            if (!ev || !ev.kind) return;
            if (emptyEl && emptyEl.parentNode) emptyEl.remove();
            ensureRepoOption(ev.repo_label);
            entries.push(ev);
            var atBottom = (feed.scrollHeight - feed.scrollTop - feed.clientHeight) < 24;
            feed.appendChild(render(ev));
            while (feed.children.length > MAX_ENTRIES) {
                feed.removeChild(feed.firstChild);
            }
            while (entries.length > MAX_ENTRIES) entries.shift();
            var visible = feed.querySelectorAll('.git-event:not(.git-hidden)').length;
            if (countEl) countEl.textContent = String(visible);
            if (atBottom) feed.scrollTop = feed.scrollHeight;
        }

        function clearAll() {
            entries = [];
            feed.querySelectorAll('.git-event').forEach(function(el) { el.remove(); });
            if (countEl) countEl.textContent = '0';
        }

        kindBtns.forEach(function(btn) {
            btn.addEventListener('click', function() {
                var k = btn.dataset.kind;
                if (activeKindGroups.has(k)) {
                    activeKindGroups.delete(k);
                    btn.classList.remove('active');
                } else {
                    activeKindGroups.add(k);
                    btn.classList.add('active');
                }
                applyFilters();
            });
        });
        if (repoFilterEl) repoFilterEl.addEventListener('change', applyFilters);
        if (clearBtn) clearBtn.addEventListener('click', clearAll);

        function connect() {
            if (gitSource) {
                try { gitSource.close(); } catch (e) {}
            }
            setStatus('connect');
            gitSource = new EventSource('/api/git/events');
            gitSource.addEventListener('connected', function() {
                reconnectDelay = 1000;
                setStatus('live');
            });
            gitSource.addEventListener('git-event', function(e) {
                try {
                    var data = JSON.parse(e.data);
                    pushEvent(data);
                } catch (err) {
                    console.warn('git-event parse failed', err);
                }
            });
            gitSource.onerror = function() {
                setStatus('reconnect');
                try { gitSource.close(); } catch (e) {}
                setTimeout(function() {
                    reconnectDelay = Math.min(reconnectDelay * 2, reconnectMax);
                    connect();
                }, reconnectDelay);
            };
        }

        connect();
    })();

    // ============================================
    // PANEL POP-OUT (tear-off into standalone window) + SOLO MODE
    // ============================================
    // The dashboard runs in two modes:
    //   * normal  — the full grid; the ⇱ button on a panel pops it into its
    //               own window, leaving a "merge back" placeholder behind.
    //   * solo    — the same page opened as /?solo=<panelId>; everything but
    //               the target panel is hidden (CSS), so the popup *is* a
    //               live, self-refreshing copy of that one panel.
    // Merge-back is coordinated over the 'gastown-panel' BroadcastChannel.
    var soloPanelId = new URLSearchParams(window.location.search).get('solo');

    // Single source of truth for per-panel UI state (Observer + Memento via
    // GTStore). `collapsed` is persisted across reloads; `poppedOut` is
    // session-only — a popped-out panel's window dies on reload, so there'd
    // be nothing to merge back. Every mutation re-renders through the
    // subscriber below, which is also what we re-run after an HTMX morph.
    var panelStore = GTStore.create({
        key:     'gastown-panel-ui',
        initial: { collapsed: {}, poppedOut: {} },
        persist: function (s) { return { collapsed: s.collapsed }; },
    });

    function applyPoppedOut(panel) {
        panel.classList.add('popped-out');
        if (!panel.querySelector('.panel-popped-note')) {
            var note = document.createElement('div');
            note.className = 'panel-popped-note';
            note.innerHTML =
                '<span class="panel-popped-label">Abierto en ventana aparte</span>' +
                '<button class="panel-merge-back-btn" type="button">' + '<svg class="icon icon-arrows-minimize" aria-hidden="true"><use href="#icon-arrows-minimize"/></svg>' + ' Reintegrar</button>';
            panel.appendChild(note);
        }
    }

    function clearPoppedOut(panel) {
        panel.classList.remove('popped-out');
        var note = panel.querySelector('.panel-popped-note');
        if (note) note.remove();
    }

    // Render the whole UI state onto the DOM. Idempotent — safe to call on
    // every store change and after every morph.
    function applyPanelState(s) {
        document.querySelectorAll('.panel[id]').forEach(function (panel) {
            var id = panel.id;
            // Never collapse the solo-mode target — it owns the whole window.
            panel.classList.toggle('collapsed', !!s.collapsed[id] && id !== soloPanelId);
            if (s.poppedOut[id]) applyPoppedOut(panel);
            else clearPoppedOut(panel);
        });
    }
    panelStore.subscribe(applyPanelState); // fires now → restores on load

    function markPanelPoppedOut(panelId) {
        panelStore.set(function (s) { s.poppedOut[panelId] = true; });
    }

    function mergePanelBack(panelId) {
        panelStore.set(function (s) { delete s.poppedOut[panelId]; });
    }

    // Dim "0" count badges so the eye lands on panels that actually have
    // something. CSS can't match text content, so we tag them here and
    // re-run after every morph.
    function decorateCounts() {
        document.querySelectorAll('.panel-header .count').forEach(function (el) {
            var n = el.textContent.trim();
            el.classList.toggle('count-zero', n === '' || n === '0');
        });
    }
    decorateCounts();

    // ============================================
    // HELP POPOVER — per-panel "?" explainer
    // ============================================
    // The popover itself is the reusable GTHelpPopover component (any box
    // with a .help-btn inherits the behavior). Here we only register the
    // panel-specific content sourced from docs/glossary.md and install it.
    GTHelpPopover.registerAll({
        'convoy-panel': {
            title: '<svg class="icon icon-truck" aria-hidden="true"><use href="#icon-truck"/></svg>' + ' Convoys',
            html:
                '<p>Órdenes de trabajo principales que envuelven Beads relacionados.</p>' +
                '<ul>' +
                  '<li>Agrupan tareas y pueden asignarse a varios workers.</li>' +
                  '<li>Crear con <code>gt convoy create</code> o el botón <code>+ New Convoy</code>.</li>' +
                  '<li>Click en una fila para detalle (progreso, work, actividad).</li>' +
                '</ul>',
        },
        'crew-panel': {
            title: '<svg class="icon icon-users" aria-hidden="true"><use href="#icon-users"/></svg>' + ' Crew',
            html:
                '<p>Agentes nombrados de larga vida para colaboración persistente.</p>' +
                '<ul>' +
                  '<li>Mantienen contexto entre sesiones (a diferencia de Polecats efímeros).</li>' +
                  '<li>Ideales para trabajo en curso con un humano o tema concreto.</li>' +
                '</ul>',
        },
        'polecats-panel': {
            title: '<svg class="icon icon-paw" aria-hidden="true"><use href="#icon-paw"/></svg>' + ' Polecats',
            html:
                '<p>Workers con identidad persistente pero sesiones efímeras.</p>' +
                '<ul>' +
                  '<li>Cada uno tiene un bead de agente, cadena CV e historial.</li>' +
                  '<li>Trabajan en worktrees git aislados para evitar conflictos.</li>' +
                  '<li>Sesiones y sandboxes se crean por tarea, se limpian al terminar.</li>' +
                '</ul>',
        },
        'sessions-panel': {
            title: '<svg class="icon icon-device-desktop" aria-hidden="true"><use href="#icon-device-desktop"/></svg>' + ' Sessions',
            html:
                '<p>Sesiones tmux vivas de todos los agentes.</p>' +
                '<ul>' +
                  '<li>Click en una fila → abre terminal xterm.js en el panel inferior.</li>' +
                  '<li>Cerrar la tab solo desconecta el navegador; la sesión sigue en tmux.</li>' +
                '</ul>',
        },
        'activity-panel': {
            title: '<svg class="icon icon-history" aria-hidden="true"><use href="#icon-history"/></svg>' + ' Activity',
            html:
                '<p>Timeline de eventos del sistema (boot, sling, done, merges, escalaciones…).</p>' +
                '<ul>' +
                  '<li>Filtros por categoría (Agent / Work / Comms / System), Rig y Agente.</li>' +
                  '<li>Stream en vivo vía SSE.</li>' +
                '</ul>',
        },
        'git-panel': {
            title: '<svg class="icon icon-git-branch" aria-hidden="true"><use href="#icon-git-branch"/></svg>' + ' Git',
            html:
                '<p>Cambios de refs en vivo a través de Rigs y worktrees de Polecats.</p>' +
                '<ul>' +
                  '<li>Branches creados, push y delete — stream en tiempo real.</li>' +
                  '<li>Filtros por repo y tipo de evento.</li>' +
                '</ul>',
        },
        'mail-panel': {
            title: '<svg class="icon icon-mail" aria-hidden="true"><use href="#icon-mail"/></svg>' + ' Mail',
            html:
                '<p>Mensajería persistente entre agentes — cartas con thread.</p>' +
                '<ul>' +
                  '<li>Conversaciones agrupadas; compose desde aquí.</li>' +
                  '<li>Distinto de <code>gt nudge</code> (mensajería real-time, no persistida).</li>' +
                '</ul>',
        },
        'merge-queue-panel': {
            title: '<svg class="icon icon-git-pull-request" aria-hidden="true"><use href="#icon-git-pull-request"/></svg>' + ' Merge Queue',
            html:
                '<p>Cola del <strong>Refinery</strong> — branches de Polecats esperando merge.</p>' +
                '<ul>' +
                  '<li>El Refinery mergea inteligentemente y maneja conflictos.</li>' +
                  '<li>Garantiza calidad antes de tocar <code>main</code>.</li>' +
                '</ul>',
        },
        'escalations-panel': {
            title: '<svg class="icon icon-alert-octagon" aria-hidden="true"><use href="#icon-alert-octagon"/></svg>' + ' Escalations',
            html:
                '<p>Alertas de Polecats o Refinery que requieren atención humana.</p>' +
                '<ul>' +
                  '<li>Ack, cerrar o reasignar desde aquí.</li>' +
                  '<li>Disparadas por merges fallidos, polecats stuck, etc.</li>' +
                '</ul>',
        },
        'rigs-panel': {
            title: '<svg class="icon icon-building-factory-2" aria-hidden="true"><use href="#icon-building-factory-2"/></svg>' + ' Rigs',
            html:
                '<p>Repos git bajo manejo de Gas Town — donde ocurre el trabajo real.</p>' +
                '<ul>' +
                  '<li>Cada Rig tiene sus Polecats, Refinery, Witness y Crew.</li>' +
                  '<li>El nivel <em>Town</em> (<code>~/gt/</code>) coordina todos los Rigs.</li>' +
                '</ul>',
        },
        'dogs-panel': {
            title: '<svg class="icon icon-dog" aria-hidden="true"><use href="#icon-dog"/></svg>' + ' Dogs',
            html:
                '<p>Crew de mantenimiento del <strong>Deacon</strong>: cleanup, health checks, tareas background.</p>' +
                '<ul>' +
                  '<li><strong>Boot</strong> (el Dog) revisa al Deacon cada 5 min — watchdog del watchdog.</li>' +
                '</ul>',
        },
        'queues-panel': {
            title: '<svg class="icon icon-clipboard-list" aria-hidden="true"><use href="#icon-clipboard-list"/></svg>' + ' Queues',
            html:
                '<p>Colas de trabajo por agente (basadas en Hook).</p>' +
                '<ul>' +
                  '<li>Muestra qué hay encolado para cada worker.</li>' +
                '</ul>',
        },
        'work-panel': {
            title: '<svg class="icon icon-clipboard-list" aria-hidden="true"><use href="#icon-clipboard-list"/></svg>' + ' Work',
            html:
                '<p><strong>Beads</strong> — unidades atómicas de trabajo en Dolt (issues, tasks, epics).</p>' +
                '<ul>' +
                  '<li>Filtrar por prioridad y estado.</li>' +
                  '<li>Asignar a un agente con <code>gt sling</code>.</li>' +
                '</ul>',
        },
        'hooks-panel': {
            title: '<svg class="icon icon-anchor" aria-hidden="true"><use href="#icon-anchor"/></svg>' + ' Hooks',
            html:
                '<p>Bead pinneado por agente — su cola principal de trabajo.</p>' +
                '<ul>' +
                  '<li><strong>GUPP</strong>: «If there is work on your Hook, YOU MUST RUN IT.»</li>' +
                  '<li>Attach / detach desde <code>+ Attach</code> o <code>Clear All</code>.</li>' +
                '</ul>',
        },
    });
    GTHelpPopover.install({ footer: 'Más info: <code>docs/glossary.md</code>' });

    // Concrete tear-off factory for grid panels: opens the dashboard in
    // solo mode in a standalone window and hides the panel behind a
    // placeholder; merge-back restores it in place.
    var panelTearOff = GTTearOff.createController({
        channelName:    function () { return 'gastown-panel'; },
        windowFeatures: function () { return 'width=1100,height=820,resizable=yes,scrollbars=yes'; },
        urlFor:   function (item) { return '/?solo=' + encodeURIComponent(item.panelId); },
        onDetach: function (item) { markPanelPoppedOut(item.panelId); },
        onMerge:  function (msg)  { mergePanelBack(msg.panel); },
        matches:  function (msg)  { return !!msg.panel; },
    });

    if (soloPanelId) {
        initSoloMode(soloPanelId);
    } else {
        // ---- Pop-out (⇱): tear the panel into its own window -----------
        document.addEventListener('click', function (e) {
            var btn = e.target.closest('.popout-btn');
            if (!btn) return;
            e.preventDefault();
            var panel = btn.closest('.panel');
            if (!panel || !panel.id) return;
            if (panelStore.get().poppedOut[panel.id]) return; // already out
            var win = panelTearOff.popOut({ panelId: panel.id });
            if (!win) showToast('error', 'Pop-out blocked', 'Allow popups for this site');
        });

        // ---- Merge back (placeholder button): ask the popup to close ----
        document.addEventListener('click', function (e) {
            var btn = e.target.closest('.panel-merge-back-btn');
            if (!btn) return;
            e.preventDefault();
            var panel = btn.closest('.panel');
            if (!panel) return;
            // Tell the popup to close itself; re-show locally right away so
            // the grid recovers even if the popup window is already gone.
            if (panelTearOff.channel) {
                panelTearOff.channel.postMessage({ type: 'merge-request', panel: panel.id });
            }
            mergePanelBack(panel.id);
        });
    }

    // Solo mode: hide everything but the target panel (CSS via body class)
    // and add a floating "Reintegrar" control that merges back and closes.
    function initSoloMode(panelId) {
        document.body.classList.add('solo-mode');
        var panel = document.getElementById(panelId);
        if (panel) {
            panel.classList.add('solo-target');
            var h = panel.querySelector('.panel-header h2');
            if (h) document.title = 'gt: ' + h.textContent.trim();
        }
        var bar = document.createElement('div');
        bar.className = 'solo-bar';
        bar.innerHTML = '<button class="solo-merge-btn" type="button" ' +
            'title="Reintegrar al dashboard">' + '<svg class="icon icon-arrows-minimize" aria-hidden="true"><use href="#icon-arrows-minimize"/></svg>' + ' Reintegrar</button>';
        document.body.appendChild(bar);

        var merge = GTTearOff.createMergeChannel('gastown-panel');
        var done = false;
        function mergeBack() {
            if (done) return;
            done = true;
            merge.merge({ panel: panelId });
        }
        bar.querySelector('.solo-merge-btn').addEventListener('click', function () {
            mergeBack();
            window.close();
        });
        // The dashboard can also request the merge (placeholder button).
        if (merge.channel) {
            merge.channel.addEventListener('message', function (ev) {
                var m = ev.data || {};
                if (m.type === 'merge-request' && m.panel === panelId) window.close();
            });
        }
        // Closing the window for any reason re-integrates the panel — a
        // panel popup owns no killable resource, so it must never vanish.
        window.addEventListener('beforeunload', mergeBack);
    }

    // ============================================
    // COLLAPSE BUTTON HANDLER
    // ============================================
    // Toggle the panel's collapse bit in panelStore; the store's subscriber
    // (applyPanelState) renders it and persists to localStorage. Panels
    // without an id can't be tracked — toggle them directly (there are none).
    document.addEventListener('click', function(e) {
        var btn = e.target.closest('.collapse-btn');
        if (!btn) return;

        e.preventDefault();
        var panel = btn.closest('.panel');
        if (!panel) return;

        if (!panel.id) { panel.classList.toggle('collapsed'); return; }
        panelStore.set(function (s) {
            if (s.collapsed[panel.id]) delete s.collapsed[panel.id];
            else s.collapsed[panel.id] = true;
        });
    });

    // After HTMX swap - morph preserves most state, but we need to re-init some things
    document.body.addEventListener('htmx:afterSwap', function() {
        // Morph re-renders panels from the server, wiping client-only state.
        // Re-render the whole UI state from the single store + re-tag chrome.
        if (soloPanelId) {
            var sp = document.getElementById(soloPanelId);
            if (sp) sp.classList.add('solo-target');
        }
        applyPanelState(panelStore.get());
        decorateCounts();
        var mailDetail = document.getElementById('mail-detail');
        var mailCompose = document.getElementById('mail-compose');
        var issueDetail = document.getElementById('issue-detail');
        var prDetail = document.getElementById('pr-detail');
        var convoyDetailView = document.getElementById('convoy-detail');
        var convoyCreateView = document.getElementById('convoy-create-form');
        // A live terminal in the unified output panel counts as a detail
        // view — we don't want the 30s polling refresh to morph through
        // the active xterm canvas.
        var outputPanelOpen = outputPanel && outputPanel.classList.contains('open');
        var inDetailView = (mailDetail && mailDetail.style.display !== 'none') ||
                          (mailCompose && mailCompose.style.display !== 'none') ||
                          (issueDetail && issueDetail.style.display !== 'none') ||
                          (prDetail && prDetail.style.display !== 'none') ||
                          (convoyDetailView && convoyDetailView.style.display !== 'none') ||
                          (convoyCreateView && convoyCreateView.style.display !== 'none') ||
                          outputPanelOpen;
        if (!inDetailView) {
            window.pauseRefresh = false;
        }
        // Reload dynamic panels after swap (handled via window functions)
        if (window.refreshCrewPanel) window.refreshCrewPanel();
        if (window.refreshReadyPanel) window.refreshReadyPanel();
        if (window.refreshQuotaDrawer) window.refreshQuotaDrawer();
        if (window.restoreQuotaDrawerState) window.restoreQuotaDrawerState();
        // Update connection status indicator after morph
        updateConnectionStatus(window.sseConnected ? 'live' : 'reconnecting');
    });

    // ============================================
    // COMMAND PALETTE
    // ============================================
    var allCommands = [];
    var visibleCommands = [];
    var selectedIdx = 0;
    var isPaletteOpen = false;
    var executionLock = false;
    var pendingCommand = null; // Command waiting for args
    var cachedOptions = null;  // Cached options from /api/options
    var recentCommands = [];   // Recently executed commands (from localStorage)
    var MAX_RECENT = 10;
    var RECENT_STORAGE_KEY = 'gt-palette-recent';

    // Load recent commands from localStorage
    function loadRecentCommands() {
        try {
            var stored = localStorage.getItem(RECENT_STORAGE_KEY);
            if (stored) {
                recentCommands = JSON.parse(stored);
                if (!Array.isArray(recentCommands)) recentCommands = [];
                // Cap at MAX_RECENT
                recentCommands = recentCommands.slice(0, MAX_RECENT);
            }
        } catch (e) {
            recentCommands = [];
        }
    }

    // Save a command to recent history
    function saveRecentCommand(cmdName) {
        // Remove duplicate if exists
        recentCommands = recentCommands.filter(function(c) { return c !== cmdName; });
        // Add to front
        recentCommands.unshift(cmdName);
        // Cap at MAX_RECENT
        recentCommands = recentCommands.slice(0, MAX_RECENT);
        try {
            localStorage.setItem(RECENT_STORAGE_KEY, JSON.stringify(recentCommands));
        } catch (e) {
            // localStorage full or unavailable, ignore
        }
    }

    // Detect active context based on expanded panel or visible detail view
    function detectActiveContext() {
        var expandedPanel = document.querySelector('.panel.expanded');
        if (expandedPanel) {
            var panelId = expandedPanel.id || '';
            if (panelId.indexOf('mail') !== -1) return 'Mail';
            if (panelId.indexOf('crew') !== -1) return 'Crew';
            if (panelId.indexOf('issue') !== -1 || panelId.indexOf('work') !== -1) return 'Work';
            if (panelId.indexOf('ready') !== -1) return 'Work';
            if (panelId.indexOf('pr') !== -1 || panelId.indexOf('merge') !== -1) return 'Status';
        }
        // Check detail views
        var mailDetail = document.getElementById('mail-detail');
        var mailCompose = document.getElementById('mail-compose');
        if ((mailDetail && mailDetail.style.display !== 'none') ||
            (mailCompose && mailCompose.style.display !== 'none')) return 'Mail';
        var issueDetail = document.getElementById('issue-detail');
        if (issueDetail && issueDetail.style.display !== 'none') return 'Work';
        var prDetail = document.getElementById('pr-detail');
        if (prDetail && prDetail.style.display !== 'none') return 'Status';
        return null;
    }

    // Score a command for fuzzy matching. Returns -1 for no match, higher is better.
    function scoreCommand(cmd, query) {
        var name = cmd.name.toLowerCase();
        var desc = cmd.desc.toLowerCase();
        var cat = cmd.category.toLowerCase();
        var q = query.toLowerCase();

        // Exact prefix match on name is best
        if (name.indexOf(q) === 0) return 100 + (50 - name.length);
        // Prefix match on a word within the name
        var nameParts = name.split(' ');
        for (var i = 0; i < nameParts.length; i++) {
            if (nameParts[i].indexOf(q) === 0) return 80 + (50 - name.length);
        }
        // Substring match in name
        if (name.indexOf(q) !== -1) return 60 + (50 - name.length);
        // Match in description
        if (desc.indexOf(q) !== -1) return 40;
        // Match in category
        if (cat.indexOf(q) !== -1) return 20;
        // Fuzzy: all query chars appear in order in name
        var ni = 0;
        for (var qi = 0; qi < q.length; qi++) {
            ni = name.indexOf(q[qi], ni);
            if (ni === -1) return -1;
            ni++;
        }
        return 10;
    }

    // Highlight matching portions in text for display
    function highlightMatch(text, query) {
        if (!query) return escapeHtml(text);
        var lowerText = text.toLowerCase();
        var lowerQuery = query.toLowerCase();
        var idx = lowerText.indexOf(lowerQuery);
        if (idx !== -1) {
            return escapeHtml(text.substring(0, idx)) +
                '<mark>' + escapeHtml(text.substring(idx, idx + query.length)) + '</mark>' +
                escapeHtml(text.substring(idx + query.length));
        }
        return escapeHtml(text);
    }

    loadRecentCommands();

    var overlay = document.getElementById('command-palette-overlay');
    var searchInput = document.getElementById('command-palette-input');
    var resultsDiv = document.getElementById('command-palette-results');
    var toastContainer = document.getElementById('toast-container');
    var outputPanel = document.getElementById('output-panel');
    var outputContent = document.getElementById('output-panel-content');
    var outputCmd = document.getElementById('output-panel-cmd');

    // Output panel
    function showOutput(cmd, output) {
        outputCmd.textContent = 'gt ' + cmd;
        outputContent.textContent = output;
        outputContent.style.display = '';
        // If a live terminal was previously mounted here, tear it down so the
        // pane reverts to its plain-text personality.
        var termWrap = document.getElementById('output-panel-terminal-wrap');
        if (termWrap && termWrap.style.display !== 'none') {
            closeSessionAttachInner();
        }
        outputPanel.classList.add('open');
    }

    // ---- Multi-console tab state ---------------------------------------
    // Each tab tracks a live gt-console-* tmux session. Only one terminal
    // is mounted at a time (the active tab); switching tabs tears the
    // browser-side attach down and re-attaches to the new session. The
    // tmux session keeps running headless in the background while inactive,
    // so the user can flip back and the scrollback survives.
    var consoleTabs = [];           // [{sessionName, cmdName}]
    var activeConsoleSession = null;
    var tabsEl = document.getElementById('output-panel-tabs');

    function renderConsoleTabs() {
        if (!tabsEl) return;
        if (consoleTabs.length === 0) {
            tabsEl.setAttribute('hidden', '');
            tabsEl.innerHTML = '';
            return;
        }
        tabsEl.removeAttribute('hidden');
        var html = '';
        for (var i = 0; i < consoleTabs.length; i++) {
            var t = consoleTabs[i];
            var cls = 'output-tab' + (t.sessionName === activeConsoleSession ? ' active' : '');
            // draggable=true lets the user tear a tab off — drag it
            // outside the strip and we open it in a standalone window.
            html += '<span class="' + cls + '" data-sess="' + escapeHtml(t.sessionName) + '" draggable="true">';
            html += '<span class="output-tab-label" title="' + escapeHtml(t.sessionName) + '">' + escapeHtml(t.cmdName || t.sessionName) + '</span>';
            html += '<span class="output-tab-popout" data-sess="' + escapeHtml(t.sessionName) + '" title="Open in standalone window">' + '<svg class="icon icon-arrows-maximize" aria-hidden="true"><use href="#icon-arrows-maximize"/></svg>' + '</span>';
            html += '<span class="output-tab-close" data-sess="' + escapeHtml(t.sessionName) + '" title="Close tab">' + '<svg class="icon icon-x" aria-hidden="true"><use href="#icon-x"/></svg>' + '</span>';
            html += '</span>';
        }
        tabsEl.innerHTML = html;
    }

    function findTab(sessionName) {
        for (var i = 0; i < consoleTabs.length; i++) {
            if (consoleTabs[i].sessionName === sessionName) return i;
        }
        return -1;
    }

    function mountConsoleSession(sessionName, cmdName) {
        outputCmd.textContent = cmdName ? ('gt ' + cmdName) : sessionName;
        outputContent.style.display = 'none';
        outputContent.textContent = '';
        var termWrap = document.getElementById('output-panel-terminal-wrap');
        if (termWrap) termWrap.style.display = 'flex';
        activeConsoleSession = sessionName;
        renderConsoleTabs();
        outputPanel.classList.add('open');
        // New / switched tab → expand if the panel was minimized.
        outputPanel.classList.remove('minimized');
        openSessionAttach(sessionName, {
            wrapId:   'output-panel-terminal-wrap',
            termId:   'output-panel-terminal',
            statusId: 'output-panel-status',
        });
    }

    // opts.ephemeral (default true) controls whether the close (✕ on the
    // tab, or the panel Close button) calls /api/session/kill server-side.
    // gt-console-* sessions are ephemeral and should be reaped on close;
    // rig/crew/polecat sessions opened via session-row click are
    // persistent — we only detach the browser-side attach, the tmux
    // session keeps running owned by gt.
    function addConsoleTab(cmdName, sessionName, opts) {
        opts = opts || {};
        var ephemeral = opts.ephemeral !== false;
        if (findTab(sessionName) === -1) {
            consoleTabs.push({ sessionName: sessionName, cmdName: cmdName, ephemeral: ephemeral });
        }
        mountConsoleSession(sessionName, cmdName);
    }

    function switchConsoleTab(sessionName) {
        if (sessionName === activeConsoleSession) return;
        var idx = findTab(sessionName);
        if (idx === -1) return;
        mountConsoleSession(sessionName, consoleTabs[idx].cmdName);
    }

    function killConsoleSession(sessionName) {
        if (sessionName && sessionName.indexOf('gt-console-') === 0) {
            fetch('/api/session/kill?session=' + encodeURIComponent(sessionName), { method: 'POST' })
                .catch(function(err) { console.warn('kill console session failed:', err); });
        }
    }

    function closeConsoleTab(sessionName) {
        var idx = findTab(sessionName);
        if (idx === -1) return;
        var ephemeral = consoleTabs[idx].ephemeral;
        consoleTabs.splice(idx, 1);
        if (ephemeral) {
            killConsoleSession(sessionName);
            // The user might have just finished `gt account login` or
            // another flow that mutated state we render — kick a fresh
            // quota fetch so the mosaic reflects the new token expiry
            // without waiting for the 30s polling refresh.
            if (window.refreshQuotaDrawer) window.refreshQuotaDrawer();
        }
        if (activeConsoleSession === sessionName) {
            closeSessionAttachInner();
            if (consoleTabs.length > 0) {
                // Switch to the tab nearest where we just removed.
                var next = consoleTabs[Math.min(idx, consoleTabs.length - 1)];
                mountConsoleSession(next.sessionName, next.cmdName);
            } else {
                activeConsoleSession = null;
                renderConsoleTabs();
                outputPanel.classList.remove('open');
            }
        } else {
            renderConsoleTabs();
        }
    }

    // Public entry called by /api/run console_session responses.
    function showOutputTerminal(cmd, sessionName) {
        addConsoleTab(cmd, sessionName);
    }

    // Detach a tab from the dashboard without killing its tmux session:
    // tear down the local attach if it was the active tab, splice the tab,
    // and switch to the next sibling (or close the panel if empty). Used
    // by the tear-off factory below — the popup owns the attach afterwards.
    function detachConsoleTab(sessionName) {
        var idx = findTab(sessionName);
        if (idx === -1) return;
        consoleTabs.splice(idx, 1);
        if (activeConsoleSession === sessionName) {
            closeSessionAttachInner();
            if (consoleTabs.length > 0) {
                var next = consoleTabs[Math.min(idx, consoleTabs.length - 1)];
                mountConsoleSession(next.sessionName, next.cmdName);
            } else {
                activeConsoleSession = null;
                renderConsoleTabs();
                outputPanel.classList.remove('open');
            }
        } else {
            renderConsoleTabs();
        }
    }

    // Concrete tear-off factory for console tabs: opens console.html in a
    // standalone window, detaches the tab, and re-adds it on merge-back.
    var consoleTearOff = GTTearOff.createController({
        channelName:    function () { return 'gastown-console'; },
        windowFeatures: function () { return 'width=960,height=600,resizable=yes'; },
        urlFor: function (item) {
            return '/static/console.html'
                + '?session=' + encodeURIComponent(item.sessionName)
                + '&label='   + encodeURIComponent(item.cmdName || item.sessionName);
        },
        onDetach: function (item) { detachConsoleTab(item.sessionName); },
        onMerge:  function (msg) {
            addConsoleTab(msg.label || msg.session, msg.session, {
                ephemeral: msg.ephemeral !== false,
            });
            showToast('info', 'Merged back', msg.label || msg.session);
        },
        matches: function (msg) { return !!msg.session; },
    });

    function popoutConsoleTab(sessionName) {
        var idx = findTab(sessionName);
        if (idx === -1) return;
        var win = consoleTearOff.popOut(consoleTabs[idx]);
        if (!win) showToast('error', 'Pop-out blocked', 'Allow popups for this site');
    }

    // Tab clicks: pop-out (⇱), close (✕), or switch (label area).
    if (tabsEl) {
        tabsEl.addEventListener('click', function(e) {
            var pop = e.target.closest('.output-tab-popout');
            if (pop) {
                e.stopPropagation();
                popoutConsoleTab(pop.getAttribute('data-sess'));
                return;
            }
            var close = e.target.closest('.output-tab-close');
            if (close) {
                e.stopPropagation();
                closeConsoleTab(close.getAttribute('data-sess'));
                return;
            }
            var tab = e.target.closest('.output-tab');
            if (tab) switchConsoleTab(tab.getAttribute('data-sess'));
        });

        // Drag-tear-off: if the user drags a tab and releases outside
        // the tab strip's bounding rect, pop it out into its own window.
        tabsEl.addEventListener('dragstart', function(e) {
            var tab = e.target.closest('.output-tab');
            if (!tab) return;
            var sess = tab.getAttribute('data-sess');
            e.dataTransfer.setData('text/plain', sess);
            e.dataTransfer.effectAllowed = 'move';
            tab.classList.add('dragging');
        });
        tabsEl.addEventListener('dragend', function(e) {
            var tab = e.target.closest('.output-tab');
            if (tab) tab.classList.remove('dragging');
            var sess = tab && tab.getAttribute('data-sess');
            if (!sess) return;
            // If the pointer ended outside the tab strip's rect (with a
            // small fudge factor) we treat it as a tear-off intent.
            var rect = tabsEl.getBoundingClientRect();
            var pad = 24;
            var inside = e.clientX > rect.left - pad && e.clientX < rect.right + pad
                      && e.clientY > rect.top  - pad && e.clientY < rect.bottom + pad;
            if (!inside) popoutConsoleTab(sess);
        });
    }

    // Merge-back for popped-out consoles is handled by consoleTearOff's
    // BroadcastChannel listener (see GTTearOff.createController above).

    document.getElementById('output-close-btn').onclick = function() {
        // Close button reaps every EPHEMERAL session in the tab list (the
        // gt-console-* ones spawned by palette commands) and detaches
        // from persistent ones (rig/crew/polecat opened via session-row
        // click) without killing them — those have their own lifecycle
        // owned by gt.
        var termWrap = document.getElementById('output-panel-terminal-wrap');
        if (termWrap && termWrap.style.display !== 'none') {
            closeSessionAttachInner();
        }
        // Only reap ephemeral (gt-console-*) sessions. Persistent rig /
        // crew / polecat sessions stay alive in tmux — they have their
        // own lifecycle owned by gt.
        var hadEphemeral = false;
        for (var i = 0; i < consoleTabs.length; i++) {
            if (consoleTabs[i].ephemeral) {
                killConsoleSession(consoleTabs[i].sessionName);
                hadEphemeral = true;
            }
        }
        if (hadEphemeral && window.refreshQuotaDrawer) {
            window.refreshQuotaDrawer();
        }
        consoleTabs = [];
        activeConsoleSession = null;
        renderConsoleTabs();
        outputPanel.classList.remove('open');
    };

    // Minimize collapses the panel to its header strip without killing any
    // tabs or attaches — the user can re-expand by clicking the header,
    // and switching tabs / running a new command auto-restores.
    var minBtn = document.getElementById('output-min-btn');
    function setOutputMinimized(min) {
        if (min) outputPanel.classList.add('minimized');
        else     outputPanel.classList.remove('minimized');
        // Refit the active terminal after the height changes so xterm
        // matches the new visible area.
        refitActiveAttach();
    }
    if (minBtn) {
        minBtn.addEventListener('click', function(e) {
            e.stopPropagation();
            setOutputMinimized(!outputPanel.classList.contains('minimized'));
        });
    }
    var outHeader = document.querySelector('#output-panel .output-panel-header');
    if (outHeader) {
        outHeader.addEventListener('click', function(e) {
            // Only restore when minimized AND click wasn't on an action
            // button (otherwise close/minimize would also trigger restore).
            if (!outputPanel.classList.contains('minimized')) return;
            if (e.target.closest('.output-panel-btn')) return;
            setOutputMinimized(false);
        });
    }

    // Drag-to-resize on the top edge of the output panel. Persists the chosen
    // height in localStorage so the layout survives page refresh. When a live
    // terminal is mounted, fits the xterm on every move so the PTY tracks the
    // new viewport size in real time.
    (function() {
        var handle = document.getElementById('output-panel-resize');
        if (!handle) return;
        var STORAGE_KEY = 'gastown.outputpanel.height';
        var MIN_PX = 220;
        function maxPx() { return Math.max(MIN_PX, window.innerHeight - 80); }
        function applyHeight(px) {
            px = Math.max(MIN_PX, Math.min(maxPx(), px));
            outputPanel.style.height = px + 'px';
            try { localStorage.setItem(STORAGE_KEY, String(px)); } catch (e) {}
            // Refit any live attach so xterm + PTY agree on cols/rows.
            refitActiveAttach();
        }
        var saved = NaN;
        try { saved = parseInt(localStorage.getItem(STORAGE_KEY), 10); } catch (e) {}
        if (!isNaN(saved)) applyHeight(saved);

        var dragging = false, startY = 0, startHeight = 0;
        handle.addEventListener('mousedown', function(e) {
            dragging = true;
            startY = e.clientY;
            startHeight = outputPanel.getBoundingClientRect().height;
            document.body.style.userSelect = 'none';
            handle.classList.add('dragging');
            e.preventDefault();
        });
        window.addEventListener('mousemove', function(e) {
            if (!dragging) return;
            // Panel grows upward, so mouse moving up = larger height.
            applyHeight(startHeight + (startY - e.clientY));
        });
        window.addEventListener('mouseup', function() {
            if (!dragging) return;
            dragging = false;
            document.body.style.userSelect = '';
            handle.classList.remove('dragging');
        });
    })();

    // Load commands once
    fetch('/api/commands')
        .then(function(r) { return r.json(); })
        .then(function(data) {
            allCommands = data.commands || [];
        })
        .catch(function() {
            console.error('Failed to load commands');
        });

    // Fetch dynamic options (rigs, polecats, convoys, agents, hooks)
    function fetchOptions() {
        return fetch('/api/options')
            .then(function(r) { return r.json(); })
            .then(function(data) {
                cachedOptions = data;
                return data;
            })
            .catch(function() {
                console.error('Failed to load options');
                return null;
            });
    }

    // Get options for a specific argType
    // Returns array of {value, label, disabled} objects
    function getOptionsForType(argType) {
        if (!cachedOptions) return [];

        var rawOptions;
        switch (argType) {
            case 'rigs': rawOptions = cachedOptions.rigs || []; break;
            case 'polecats': rawOptions = cachedOptions.polecats || []; break;
            case 'convoys': rawOptions = cachedOptions.convoys || []; break;
            case 'agents': rawOptions = cachedOptions.agents || []; break;
            case 'hooks': rawOptions = cachedOptions.hooks || []; break;
            case 'messages': rawOptions = cachedOptions.messages || []; break;
            case 'crew': rawOptions = cachedOptions.crew || []; break;
            case 'escalations': rawOptions = cachedOptions.escalations || []; break;
            default: return [];
        }

        // Normalize to {value, label, disabled} format
        return rawOptions.map(function(opt) {
            if (typeof opt === 'string') {
                return { value: opt, label: opt, disabled: false };
            }
            // Agent format: {name, status, running}
            var statusText = opt.running ? '<svg class="icon icon-point-filled icon-green" aria-hidden="true"><use href="#icon-point-filled"/></svg>' + ' running' : '<svg class="icon icon-point" aria-hidden="true"><use href="#icon-point"/></svg>' + ' stopped';
            return {
                value: opt.name,
                label: opt.name + ' (' + statusText + ')',
                disabled: !opt.running,
                running: opt.running
            };
        });
    }

    function escapeHtml(str) {
        if (!str) return '';
        var div = document.createElement('div');
        div.textContent = str;
        return div.innerHTML;
    }

    // Parse args template like "<address> -s <subject> -m <message>" into field definitions
    // Returns [{name: "address", flag: null}, {name: "subject", flag: "-s"}, {name: "message", flag: "-m"}]
    function parseArgsTemplate(argsStr) {
        if (!argsStr) return [];
        var args = [];
        // Match patterns like "<name>" or "-f <name>"
        var regex = /(?:(-\w+)\s+)?<([^>]+)>/g;
        var match;
        while ((match = regex.exec(argsStr)) !== null) {
            args.push({ name: match[2], flag: match[1] || null });
        }
        return args;
    }

    function renderResults() {
        // If waiting for args, show the args input with options
        if (pendingCommand) {
            var options = pendingCommand.argType ? getOptionsForType(pendingCommand.argType) : [];
            var argFields = parseArgsTemplate(pendingCommand.args);

            var formHtml = '<div class="command-args-prompt">' +
                '<div class="command-args-header">gt ' + escapeHtml(pendingCommand.name) + '</div>';

            // Build form fields for each argument
            for (var i = 0; i < argFields.length; i++) {
                var field = argFields[i];
                var fieldId = 'arg-field-' + i;
                var isFirstField = (i === 0) && !field.flag; // First positional arg
                var hasOptions = isFirstField && pendingCommand.argType && options.length > 0;
                var noOptions = isFirstField && pendingCommand.argType && options.length === 0;
                var isMessageField = field.name === 'message' || field.name === 'body';

                formHtml += '<div class="command-field">';
                formHtml += '<label class="command-field-label" for="' + fieldId + '">' + escapeHtml(field.name) + '</label>';

                if (hasOptions) {
                    // Dropdown for first arg when options exist
                    formHtml += '<select id="' + fieldId + '" class="command-field-select" data-flag="' + (field.flag || '') + '">';
                    formHtml += '<option value="">Select ' + escapeHtml(field.name) + '...</option>';
                    for (var j = 0; j < options.length; j++) {
                        var opt = options[j];
                        var disabledAttr = opt.disabled ? ' disabled' : '';
                        var optClass = opt.disabled ? ' class="option-disabled"' : (opt.running ? ' class="option-running"' : '');
                        formHtml += '<option value="' + escapeHtml(opt.value) + '"' + disabledAttr + optClass + '>' + escapeHtml(opt.label) + '</option>';
                    }
                    formHtml += '</select>';
                } else if (noOptions) {
                    formHtml += '<input type="text" id="' + fieldId + '" class="command-field-input" data-flag="' + (field.flag || '') + '" placeholder="No ' + escapeHtml(pendingCommand.argType) + ' available">';
                } else if (isMessageField) {
                    formHtml += '<textarea id="' + fieldId + '" class="command-field-textarea" data-flag="' + (field.flag || '') + '" placeholder="Enter ' + escapeHtml(field.name) + '..." rows="3"></textarea>';
                } else {
                    formHtml += '<input type="text" id="' + fieldId + '" class="command-field-input" data-flag="' + (field.flag || '') + '" placeholder="Enter ' + escapeHtml(field.name) + '...">';
                }
                formHtml += '</div>';
            }

            // If no arg fields parsed, show generic input
            if (argFields.length === 0 && pendingCommand.args) {
                formHtml += '<div class="command-field">';
                formHtml += '<input type="text" id="arg-field-0" class="command-field-input" placeholder="' + escapeHtml(pendingCommand.args) + '">';
                formHtml += '</div>';
            }

            formHtml += '<div class="command-args-actions">' +
                '<button id="command-args-run" class="command-args-btn run">Run</button>' +
                '<button id="command-args-cancel" class="command-args-btn cancel">Cancel</button>' +
                '</div></div>';

            resultsDiv.innerHTML = formHtml;

            // Focus first field
            var firstField = resultsDiv.querySelector('#arg-field-0');
            if (firstField) firstField.focus();

            // Wire up run/cancel buttons
            var runBtn = document.getElementById('command-args-run');
            var cancelBtn = document.getElementById('command-args-cancel');

            if (runBtn) {
                runBtn.onclick = function() {
                    runBtn.classList.add('loading');
                    runBtn.textContent = 'Running';
                    runWithArgsFromForm(argFields.length || 1);
                };
            }
            if (cancelBtn) {
                cancelBtn.onclick = cancelArgs;
            }

            // Enter key submits
            resultsDiv.querySelectorAll('input, select').forEach(function(el) {
                el.onkeydown = function(e) {
                    if (e.key === 'Enter') {
                        e.preventDefault();
                        runWithArgsFromForm(argFields.length || 1);
                    } else if (e.key === 'Escape') {
                        e.preventDefault();
                        cancelArgs();
                    }
                };
            });
            return;
        }

        if (visibleCommands.length === 0) {
            resultsDiv.innerHTML = '<div class="command-palette-empty">No matching commands</div>';
            return;
        }
        var currentQuery = searchInput ? searchInput.value.trim() : '';
        var html = '';

        if (currentQuery) {
            // Search mode: flat list with highlights
            for (var i = 0; i < visibleCommands.length; i++) {
                var cmd = visibleCommands[i];
                var cls = 'command-item' + (i === selectedIdx ? ' selected' : '');
                var argsHint = cmd.args ? ' <span class="command-args">' + escapeHtml(cmd.args) + '</span>' : '';
                var nameHtml = highlightMatch('gt ' + cmd.name, currentQuery);
                html += '<div class="' + cls + '" data-cmd-name="' + escapeHtml(cmd.name) + '" data-cmd-args="' + escapeHtml(cmd.args || '') + '">' +
                    '<span class="command-name">' + nameHtml + argsHint + '</span>' +
                    '<span class="command-desc">' + escapeHtml(cmd.desc) + '</span>' +
                    '<span class="command-category">' + escapeHtml(cmd.category) + '</span>' +
                    '</div>';
            }
        } else {
            // Browse mode: show Recent, Contextual, then All Commands
            // visibleCommands was rebuilt by filterCommands with sections baked in
            for (var j = 0; j < visibleCommands.length; j++) {
                var item = visibleCommands[j];
                if (item._section) {
                    // Section header
                    html += '<div class="command-section-header">' + escapeHtml(item._section) + '</div>';
                    continue;
                }
                var cls2 = 'command-item' + (j === selectedIdx ? ' selected' : '');
                var argsHint2 = item.args ? ' <span class="command-args">' + escapeHtml(item.args) + '</span>' : '';
                var icon = item._recent ? '<span class="command-recent-icon">&#8635;</span>' : '';
                html += '<div class="' + cls2 + '" data-cmd-name="' + escapeHtml(item.name) + '" data-cmd-args="' + escapeHtml(item.args || '') + '">' +
                    icon +
                    '<span class="command-name">gt ' + escapeHtml(item.name) + argsHint2 + '</span>' +
                    '<span class="command-desc">' + escapeHtml(item.desc) + '</span>' +
                    '<span class="command-category">' + escapeHtml(item.category) + '</span>' +
                    '</div>';
            }
        }
        resultsDiv.innerHTML = html;

        // Scroll selected item into view
        var selectedEl = resultsDiv.querySelector('.command-item.selected');
        if (selectedEl) {
            selectedEl.scrollIntoView({ block: 'nearest' });
        }
    }

    function runWithArgsFromForm(fieldCount) {
        var args = [];
        for (var i = 0; i < fieldCount; i++) {
            var field = document.getElementById('arg-field-' + i);
            if (field) {
                var val = field.value.trim();
                var flag = field.getAttribute('data-flag');
                if (val) {
                    if (flag) {
                        // Flag-based arg: -s "value"
                        args.push(flag);
                        args.push('"' + val.replace(/"/g, '\\"') + '"');
                    } else {
                        // Positional arg
                        args.push(val);
                    }
                }
            }
        }
        if (pendingCommand) {
            var fullCmd = pendingCommand.name + (args.length ? ' ' + args.join(' ') : '');
            pendingCommand = null;
            runCommand(fullCmd);
        }
    }

    function runWithArgs() {
        runWithArgsFromForm(10); // fallback
    }

    function cancelArgs() {
        pendingCommand = null;
        filterCommands(searchInput ? searchInput.value : '');
    }

    function filterCommands(query) {
        query = (query || '').trim();
        if (!query) {
            // Build sectioned list: Recent, Contextual, All Commands
            visibleCommands = [];
            var shownNames = {};

            // Recent section
            var recentItems = [];
            for (var ri = 0; ri < recentCommands.length; ri++) {
                var recentCmd = allCommands.find(function(c) { return c.name === recentCommands[ri]; });
                if (recentCmd) recentItems.push(recentCmd);
            }
            if (recentItems.length > 0) {
                visibleCommands.push({ _section: 'Recent' });
                for (var ri2 = 0; ri2 < recentItems.length; ri2++) {
                    var rcmd = Object.assign({}, recentItems[ri2], { _recent: true });
                    visibleCommands.push(rcmd);
                    shownNames[rcmd.name] = true;
                }
            }

            // Contextual section
            var context = detectActiveContext();
            if (context) {
                var contextItems = allCommands.filter(function(c) {
                    return c.category === context && !shownNames[c.name];
                });
                if (contextItems.length > 0) {
                    visibleCommands.push({ _section: 'Suggested \u2014 ' + context });
                    for (var ci = 0; ci < contextItems.length; ci++) {
                        visibleCommands.push(contextItems[ci]);
                        shownNames[contextItems[ci].name] = true;
                    }
                }
            }

            // All commands section (remaining)
            var remaining = allCommands.filter(function(c) { return !shownNames[c.name]; });
            remaining.sort(function(a, b) { return a.name.localeCompare(b.name); });
            if (remaining.length > 0) {
                visibleCommands.push({ _section: 'All Commands' });
                for (var ai = 0; ai < remaining.length; ai++) {
                    visibleCommands.push(remaining[ai]);
                }
            }
        } else {
            // Score and sort by relevance
            var scored = [];
            for (var i = 0; i < allCommands.length; i++) {
                var s = scoreCommand(allCommands[i], query);
                if (s > 0) {
                    scored.push({ cmd: allCommands[i], score: s });
                }
            }
            scored.sort(function(a, b) { return b.score - a.score; });
            visibleCommands = scored.map(function(item) { return item.cmd; });
        }
        selectedIdx = 0;
        // In browse mode, skip section headers for initial selection
        while (selectedIdx < visibleCommands.length && visibleCommands[selectedIdx]._section) {
            selectedIdx++;
        }
        renderResults();
    }

    function openPalette() {
        isPaletteOpen = true;
        pendingCommand = null;
        if (overlay) {
            overlay.style.display = 'flex';
            overlay.classList.add('open');
        }
        if (searchInput) {
            searchInput.value = '';
            searchInput.focus();
        }
        filterCommands('');
        // Fetch fresh options in background
        fetchOptions();
    }

    function closePalette() {
        isPaletteOpen = false;
        pendingCommand = null;
        if (overlay) {
            overlay.classList.remove('open');
            overlay.style.display = 'none';
        }
        if (searchInput) {
            searchInput.value = '';
        }
        visibleCommands = [];
        if (resultsDiv) {
            resultsDiv.innerHTML = '';
        }
    }

    function selectCommand(cmdName, cmdArgs) {
        // If command needs args, show args input
        if (cmdArgs) {
            var cmd = allCommands.find(function(c) { return c.name === cmdName; });
            if (cmd) {
                pendingCommand = cmd;
                // Make sure options are loaded before rendering
                if (cmd.argType && !cachedOptions) {
                    fetchOptions().then(function() {
                        renderResults();
                    });
                } else {
                    renderResults();
                }
                return;
            }
        }
        // No args needed, run directly
        runCommand(cmdName);
    }

    function runCommand(cmdName) {
        if (executionLock) {
            console.log('Execution locked, ignoring');
            return;
        }
        if (!cmdName) {
            console.log('No command name');
            return;
        }

        // Close palette FIRST before anything else
        closePalette();

        // Save to recent commands history
        // Extract base command name (without args) for history
        var baseName = cmdName.split(' ').slice(0, 3).join(' ');
        var matchedCmd = allCommands.find(function(c) { return cmdName.indexOf(c.name) === 0; });
        saveRecentCommand(matchedCmd ? matchedCmd.name : baseName);

        executionLock = true;
        console.log('Running command:', cmdName);

        showToast('info', 'Running...', 'gt ' + cmdName);

        var payload = { command: cmdName };
        // Include confirmed flag if the command requires server-side confirmation
        if (matchedCmd && matchedCmd.confirm) {
            payload.confirmed = true;
        }

        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            // Interactive commands (e.g. `account login`, `console`) are
            // dispatched into a fresh tmux session server-side; the response
            // carries the session name. Mount the xterm.js attach in the
            // output panel — the same surface that hosts headless command
            // output — so the user doesn't have to look elsewhere for the
            // live terminal.
            if (data.success && data.console_session) {
                showToast('info', 'Console opened', 'gt ' + cmdName + ' → ' + data.console_session);
                showOutputTerminal(cmdName, data.console_session);
                return;
            }
            if (data.success) {
                showToast('success', 'Success', 'gt ' + cmdName);
                if (data.output && data.output.trim()) {
                    showOutput(cmdName, data.output);
                }
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
                if (data.output) {
                    showOutput(cmdName, data.output);
                }
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message || 'Request failed');
        })
        .finally(function() {
            // Unlock after 1 second to prevent double-clicks
            setTimeout(function() {
                executionLock = false;
            }, 1000);
        });
    }

    function showToast(type, title, message) {
        var toast = document.createElement('div');
        toast.className = 'toast ' + type;
        var icon = type === 'success' ? '<svg class="icon icon-check" aria-hidden="true"><use href="#icon-check"/></svg>' : type === 'error' ? '<svg class="icon icon-x" aria-hidden="true"><use href="#icon-x"/></svg>' : '<svg class="icon icon-info-circle" aria-hidden="true"><use href="#icon-info-circle"/></svg>';
        toast.innerHTML = '<span class="toast-icon">' + icon + '</span>' +
            '<div class="toast-content">' +
            '<div class="toast-title">' + escapeHtml(title) + '</div>' +
            '<div class="toast-message">' + escapeHtml(message) + '</div>' +
            '</div>' +
            '<button class="toast-close">' + '<svg class="icon icon-x" aria-hidden="true"><use href="#icon-x"/></svg>' + '</button>';
        toastContainer.appendChild(toast);

        setTimeout(function() {
            if (toast.parentNode) toast.parentNode.removeChild(toast);
        }, 4000);

        toast.querySelector('.toast-close').onclick = function() {
            if (toast.parentNode) toast.parentNode.removeChild(toast);
        };
    }

    // Persistent toast for actions the user must take (login URL, etc).
    // Stays until the user dismisses it or clicks the action button.
    // De-dup by tag — repeated calls with the same tag replace the existing
    // toast rather than stacking new ones.
    var _persistentToastsByTag = {};
    function showActionToast(opts) {
        opts = opts || {};
        var tag = opts.tag || ('action-' + Date.now());
        if (_persistentToastsByTag[tag] && _persistentToastsByTag[tag].parentNode) {
            _persistentToastsByTag[tag].parentNode.removeChild(_persistentToastsByTag[tag]);
        }
        var toast = document.createElement('div');
        toast.className = 'toast ' + (opts.type || 'info') + ' toast-persistent';
        var icon = opts.icon || '<svg class="icon icon-info-circle" aria-hidden="true"><use href="#icon-info-circle"/></svg>';
        var actionLabel = opts.actionLabel || 'Copy';
        var actionValue = opts.actionValue || '';
        toast.innerHTML = '<span class="toast-icon">' + icon + '</span>' +
            '<div class="toast-content">' +
            '<div class="toast-title">' + escapeHtml(opts.title || '') + '</div>' +
            '<div class="toast-message">' + escapeHtml(opts.message || '') + '</div>' +
            (actionValue ? '<div class="toast-action-value">' + escapeHtml(actionValue) + '</div>' : '') +
            '</div>' +
            (actionValue ? '<button class="toast-action-btn">' + escapeHtml(actionLabel) + '</button>' : '') +
            '<button class="toast-close">' + '<svg class="icon icon-x" aria-hidden="true"><use href="#icon-x"/></svg>' + '</button>';
        toastContainer.appendChild(toast);
        _persistentToastsByTag[tag] = toast;

        var actionBtn = toast.querySelector('.toast-action-btn');
        if (actionBtn) {
            actionBtn.onclick = function() {
                if (navigator.clipboard && actionValue) {
                    navigator.clipboard.writeText(actionValue).then(function() {
                        actionBtn.innerHTML = '<svg class="icon icon-check" aria-hidden="true"><use href="#icon-check"/></svg>' + ' Copied';
                        actionBtn.classList.add('copied');
                    }).catch(function() {
                        actionBtn.textContent = 'Copy failed';
                    });
                }
            };
        }

        toast.querySelector('.toast-close').onclick = function() {
            if (toast.parentNode) toast.parentNode.removeChild(toast);
            delete _persistentToastsByTag[tag];
        };
    }

    // SINGLE click handler for command palette
    resultsDiv.addEventListener('click', function(e) {
        var item = e.target.closest('.command-item');
        if (!item) return;

        e.preventDefault();
        e.stopPropagation();

        var cmdName = item.getAttribute('data-cmd-name');
        var cmdArgs = item.getAttribute('data-cmd-args');
        if (cmdName) {
            selectCommand(cmdName, cmdArgs);
        }
    });

    // Open palette button
    document.addEventListener('click', function(e) {
        if (e.target.closest('#open-palette-btn')) {
            e.preventDefault();
            openPalette();
            return;
        }
        var launchBtn = e.target.closest('#launch-agents-btn');
        if (launchBtn) {
            e.preventDefault();
            launchAgents(launchBtn);
            return;
        }
        // Click on overlay background closes palette
        if (e.target === overlay) {
            closePalette();
        }
    });

    function launchAgents(btn) {
        if (btn.disabled) return;
        if (!window.confirm('Run `gt up --restore`?\n\nStarts daemon, deacon, mayor, all witnesses, refineries, crew, and pinned polecats.\nIdempotent — running services are not touched.')) {
            return;
        }
        var origText = btn.innerHTML;
        btn.disabled = true;
        btn.innerHTML = '<svg class="icon icon-hourglass-empty" aria-hidden="true"><use href="#icon-hourglass-empty"/></svg>' + ' Launching...';
        showToast('info', 'Running...', 'gt up --restore');

        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: 'up --restore', confirmed: true })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Launched', 'Agents starting — reloading dashboard in 3s');
                // Give sessions a moment to register before reloading so the
                // refreshed page shows the new running state, not the
                // pre-launch snapshot.
                setTimeout(function() { window.location.reload(); }, 3000);
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
                btn.disabled = false;
                btn.innerHTML = origText;
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message || 'Request failed');
            btn.disabled = false;
            btn.innerHTML = origText;
        });
    }

    // Keyboard handling
    document.addEventListener('keydown', function(e) {
        // Cmd+K or Ctrl+K toggles palette
        if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
            e.preventDefault();
            if (isPaletteOpen) {
                closePalette();
            } else {
                openPalette();
            }
            return;
        }

        // Rest only when palette is open
        if (!isPaletteOpen) return;

        // If in args mode, let the args input handle keys
        if (pendingCommand) return;

        if (e.key === 'Escape') {
            e.preventDefault();
            closePalette();
            return;
        }

        if (e.key === 'ArrowDown') {
            e.preventDefault();
            if (visibleCommands.length > 0) {
                var next = selectedIdx + 1;
                // Skip section headers
                while (next < visibleCommands.length && visibleCommands[next]._section) next++;
                if (next < visibleCommands.length) selectedIdx = next;
                renderResults();
            }
            return;
        }

        if (e.key === 'ArrowUp') {
            e.preventDefault();
            var prev = selectedIdx - 1;
            // Skip section headers
            while (prev >= 0 && visibleCommands[prev]._section) prev--;
            if (prev >= 0) selectedIdx = prev;
            renderResults();
            return;
        }

        if (e.key === 'Enter') {
            e.preventDefault();
            var selected = visibleCommands[selectedIdx];
            if (selected && !selected._section) {
                selectCommand(selected.name, selected.args);
            }
            return;
        }
    });

    // Input filtering
    searchInput.addEventListener('input', function() {
        filterCommands(searchInput.value);
    });

    // ============================================
    // MAIL PANEL INTERACTIONS
    // ============================================
    var mailList = document.getElementById('mail-list');
    var mailAll = document.getElementById('mail-all');
    var mailDetail = document.getElementById('mail-detail');
    var mailCompose = document.getElementById('mail-compose');
    var currentMessageId = null;
    var currentMessageFrom = null;
    var currentMailTab = 'inbox';

    // Mail tab switching
    document.querySelectorAll('.mail-tab').forEach(function(tab) {
        tab.addEventListener('click', function() {
            var targetTab = tab.getAttribute('data-tab');
            if (targetTab === currentMailTab) return;

            // Update active tab
            document.querySelectorAll('.mail-tab').forEach(function(t) {
                t.classList.remove('active');
            });
            tab.classList.add('active');
            currentMailTab = targetTab;

            // Show/hide views
            if (targetTab === 'inbox') {
                mailList.style.display = 'block';
                mailAll.style.display = 'none';
            } else {
                mailList.style.display = 'none';
                mailAll.style.display = 'block';
            }

            // Hide detail/compose views
            mailDetail.style.display = 'none';
            mailCompose.style.display = 'none';
        });
    });

    // Load mail inbox as threaded conversations
    function loadMailInbox() {
        var loading = document.getElementById('mail-loading');
        var threadsContainer = document.getElementById('mail-threads');
        var empty = document.getElementById('mail-empty');
        var count = document.getElementById('mail-count');

        if (!loading || !threadsContainer) return;

        fetch('/api/mail/threads')
            .then(function(r) { return r.json(); })
            .then(function(data) {
                loading.style.display = 'none';

                if (data.threads && data.threads.length > 0) {
                    threadsContainer.style.display = 'block';
                    empty.style.display = 'none';
                    threadsContainer.innerHTML = '';

                    data.threads.forEach(function(thread) {
                        var threadEl = document.createElement('div');
                        threadEl.className = 'mail-thread' + (thread.unread_count > 0 ? ' mail-thread-unread' : '');

                        var last = thread.last_message;
                        var hasMultiple = thread.count > 1;
                        var countBadge = hasMultiple ? '<span class="thread-count">' + thread.count + '</span>' : '';
                        var unreadDot = thread.unread_count > 0 ? '<span class="thread-unread-dot"></span>' : '';

                        var priorityIcon = '';
                        if (last.priority === 'urgent') priorityIcon = '<span class="priority-urgent">' + '<svg class="icon icon-bolt" aria-hidden="true"><use href="#icon-bolt"/></svg>' + '</span> ';
                        else if (last.priority === 'high') priorityIcon = '<span class="priority-high">!</span> ';

                        // Thread header (always visible)
                        var headerEl = document.createElement('div');
                        headerEl.className = 'mail-thread-header';
                        headerEl.setAttribute('data-thread-id', thread.thread_id);
                        headerEl.innerHTML =
                            '<div class="mail-thread-left">' +
                                unreadDot +
                                '<span class="mail-from">' + escapeHtml(last.from) + '</span>' +
                                countBadge +
                            '</div>' +
                            '<div class="mail-thread-center">' +
                                priorityIcon +
                                '<span class="mail-subject">' + escapeHtml(thread.subject) + '</span>' +
                                (hasMultiple ? '<span class="mail-thread-preview"> — ' + escapeHtml(last.body ? last.body.substring(0, 60) : '') + '</span>' : '') +
                            '</div>' +
                            '<div class="mail-thread-right">' +
                                '<span class="mail-time">' + formatMailTime(last.timestamp) + '</span>' +
                            '</div>';

                        threadEl.appendChild(headerEl);

                        // Thread messages (collapsed by default, only for multi-message threads)
                        if (hasMultiple) {
                            var msgsEl = document.createElement('div');
                            msgsEl.className = 'mail-thread-messages';
                            msgsEl.style.display = 'none';

                            thread.messages.forEach(function(msg) {
                                var msgEl = document.createElement('div');
                                msgEl.className = 'mail-thread-msg' + (msg.read ? '' : ' mail-unread');
                                msgEl.setAttribute('data-msg-id', msg.id);
                                msgEl.setAttribute('data-from', msg.from);
                                msgEl.innerHTML =
                                    '<div class="mail-thread-msg-header">' +
                                        '<span class="mail-from">' + escapeHtml(msg.from) + '</span>' +
                                        '<span class="mail-time">' + formatMailTime(msg.timestamp) + '</span>' +
                                    '</div>' +
                                    '<div class="mail-thread-msg-subject">' + escapeHtml(msg.subject) + '</div>';
                                msgsEl.appendChild(msgEl);
                            });

                            threadEl.appendChild(msgsEl);
                        } else {
                            // Single message thread - clicking opens the message directly
                            headerEl.setAttribute('data-msg-id', last.id);
                            headerEl.setAttribute('data-from', last.from);
                        }

                        threadsContainer.appendChild(threadEl);
                    });

                    // Update count
                    if (count) {
                        var unread = data.unread_count || 0;
                        count.textContent = unread > 0 ? unread + ' unread' : data.total;
                        if (unread > 0) count.classList.add('has-unread');
                        else count.classList.remove('has-unread');
                    }
                } else {
                    threadsContainer.style.display = 'none';
                    empty.style.display = 'block';
                    if (count) count.textContent = '0';
                }
            })
            .catch(function(err) {
                loading.textContent = 'Failed to load mail';
                console.error('Mail load error:', err);
            });
    }

    function formatMailTime(timestamp) {
        if (!timestamp) return '';
        var d = new Date(timestamp);
        var now = new Date();
        var diff = now - d;

        // Format: "Jan 26, 3:45 PM" or "Jan 26 2025, 3:45 PM" if different year
        var months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
        var month = months[d.getMonth()];
        var day = d.getDate();
        var hours = d.getHours();
        var minutes = d.getMinutes();
        var ampm = hours >= 12 ? 'PM' : 'AM';
        hours = hours % 12 || 12;
        var minStr = minutes < 10 ? '0' + minutes : minutes;
        var yearPart = d.getFullYear() !== now.getFullYear() ? ' ' + d.getFullYear() + ',' : '';
        var dateStr = month + ' ' + day + yearPart + ', ' + hours + ':' + minStr + ' ' + ampm;

        // Add relative time in parentheses for recent messages
        var relative = '';
        if (diff < 60000) relative = ' (just now)';
        else if (diff < 3600000) relative = ' (' + Math.floor(diff / 60000) + 'm ago)';
        else if (diff < 86400000) relative = ' (' + Math.floor(diff / 3600000) + 'h ago)';
        else if (diff < 604800000) relative = ' (' + Math.floor(diff / 86400000) + 'd ago)';

        return dateStr + relative;
    }

    // Load mail on page load
    loadMailInbox();

    // ============================================
    // CREW PANEL
    // ============================================
    function loadCrew() {
        var loading = document.getElementById('crew-loading');
        var table = document.getElementById('crew-table');
        var tbody = document.getElementById('crew-tbody');
        var empty = document.getElementById('crew-empty');
        var count = document.getElementById('crew-count');

        if (!loading || !table || !tbody) return;

        fetch('/api/crew')
            .then(function(r) { return r.json(); })
            .then(function(data) {
                loading.style.display = 'none';

                if (data.crew && data.crew.length > 0) {
                    table.style.display = 'table';
                    empty.style.display = 'none';
                    tbody.innerHTML = '';

                    // Check for state changes and notify
                    checkCrewNotifications(data.crew);

                    data.crew.forEach(function(member) {
                        var tr = document.createElement('tr');
                        var rowClass = 'crew-' + member.state;
                        tr.className = rowClass;

                        var stateClass = 'crew-state-' + member.state;
                        var stateText = member.state.charAt(0).toUpperCase() + member.state.slice(1);
                        var stateIcon = '';
                        if (member.state === 'spinning') stateIcon = '<svg class="icon icon-refresh" aria-hidden="true"><use href="#icon-refresh"/></svg>' + ' ';
                        else if (member.state === 'finished') stateIcon = '<svg class="icon icon-check icon-green" aria-hidden="true"><use href="#icon-check"/></svg>' + ' ';
                        else if (member.state === 'questions') stateIcon = '<svg class="icon icon-question-mark" aria-hidden="true"><use href="#icon-question-mark"/></svg>' + ' ';
                        else if (member.state === 'ready') stateIcon = '<svg class="icon icon-player-pause" aria-hidden="true"><use href="#icon-player-pause"/></svg>' + ' ';

                        var sessionBadge = '';
                        if (member.session === 'attached') {
                            sessionBadge = '<span class="badge badge-green">Attached</span>';
                        } else if (member.session === 'detached') {
                            sessionBadge = '<span class="badge badge-muted">Detached</span>';
                        } else {
                            sessionBadge = '<span class="badge badge-muted">None</span>';
                        }

                        // Build the attach command based on the crew member's role
                        var attachCmd = 'gt crew at ' + member.name;
                        if (member.name === 'mayor') {
                            attachCmd = 'gt mayor attach';
                        } else if (member.name === 'deacon') {
                            attachCmd = 'gt deacon attach';
                        } else if (member.name === 'witness' || member.name.startsWith('witness-')) {
                            attachCmd = 'gt witness attach';
                        }

                        var accountCell = member.account
                            ? '<span class="crew-account">' + escapeHtml(member.account) + '</span>'
                            : '<span class="crew-account crew-account-none">—</span>';

                        tr.innerHTML =
                            '<td><span class="crew-name">' + escapeHtml(member.name) + '</span></td>' +
                            '<td><span class="crew-rig">' + escapeHtml(member.rig) + '</span></td>' +
                            '<td><span class="' + stateClass + '">' + stateIcon + stateText + '</span></td>' +
                            '<td><span class="crew-hook">' + (member.hook ? escapeHtml(member.hook) : '—') + '</span></td>' +
                            '<td class="crew-activity">' + (member.last_active || '—') + '</td>' +
                            '<td>' + sessionBadge + '</td>' +
                            '<td>' + accountCell + '</td>' +
                            '<td><button class="attach-btn" data-cmd="' + escapeHtml(attachCmd) + '" title="Copy attach command">' + icon('paperclip') + ' Attach</button></td>';
                        tbody.appendChild(tr);
                    });

                    if (count) count.textContent = data.total;
                } else {
                    table.style.display = 'none';
                    empty.style.display = 'block';
                    if (count) count.textContent = '0';
                }
            })
            .catch(function(err) {
                loading.textContent = 'Failed to load crew';
                console.error('Crew load error:', err);
            });
    }

    // Track previous crew states for notifications
    var previousCrewStates = {};
    var crewNeedsAttention = 0;

    // Load crew on page load
    loadCrew();
    // Expose for refresh after HTMX swaps
    window.refreshCrewPanel = loadCrew;

    // Crew notification system - check for state changes
    function checkCrewNotifications(crewList) {
        var newNeedsAttention = 0;

        crewList.forEach(function(member) {
            var key = member.rig + '/' + member.name;
            var prevState = previousCrewStates[key];
            var newState = member.state;

            // Count crew needing attention
            if (newState === 'finished' || newState === 'questions') {
                newNeedsAttention++;
            }

            // Notify on state transitions to finished/questions
            if (prevState && prevState !== newState) {
                if (newState === 'finished') {
                    showToast('success', 'Crew Finished', member.name + ' finished their work!');
                    playNotificationSound();
                } else if (newState === 'questions') {
                    showToast('info', 'Needs Attention', member.name + ' has questions for you');
                    playNotificationSound();
                }
            }

            // Update stored state
            previousCrewStates[key] = newState;
        });

        // Update badge on crew panel
        crewNeedsAttention = newNeedsAttention;
        updateCrewBadge();
    }

    function updateCrewBadge() {
        var countEl = document.getElementById('crew-count');
        if (!countEl) return;

        // Add attention indicator if crew needs attention
        if (crewNeedsAttention > 0) {
            countEl.classList.add('needs-attention');
            countEl.setAttribute('data-attention', crewNeedsAttention);
        } else {
            countEl.classList.remove('needs-attention');
            countEl.removeAttribute('data-attention');
        }
    }

    function playNotificationSound() {
        // Simple beep using Web Audio API (optional, non-blocking)
        try {
            var ctx = new (window.AudioContext || window.webkitAudioContext)();
            var oscillator = ctx.createOscillator();
            var gain = ctx.createGain();
            oscillator.connect(gain);
            gain.connect(ctx.destination);
            oscillator.frequency.value = 800;
            gain.gain.value = 0.1;
            oscillator.start();
            oscillator.stop(ctx.currentTime + 0.1);
        } catch (e) {
            // Audio not available, ignore
        }
    }

    // Handle attach button clicks - copy command to clipboard
    document.addEventListener('click', function(e) {
        var btn = e.target.closest('.attach-btn');
        if (!btn) return;
        
        e.preventDefault();
        var cmd = btn.getAttribute('data-cmd');
        if (!cmd) return;

        navigator.clipboard.writeText(cmd).then(function() {
            showToast('success', 'Copied', cmd);
        }).catch(function() {
            // Fallback for older browsers
            showToast('info', 'Run in terminal', cmd);
        });
    });


    // ============================================
    // HOOK MANAGEMENT
    // ============================================

    function detachHook(btn) {
        var beadId = btn.getAttribute('data-hook-id');
        if (!beadId) return;

        if (!confirm('Detach hook ' + beadId + '?')) return;

        btn.disabled = true;
        btn.textContent = '...';

        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: 'hook detach ' + beadId, confirmed: true })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Detached', beadId + ' detached from hook');
                // Refresh the page to update the hooks panel
                if (typeof htmx !== 'undefined') {
                    htmx.trigger(document.body, 'htmx:load');
                }
            } else {
                showToast('error', 'Failed', data.error || 'Failed to detach hook');
                btn.disabled = false;
                btn.textContent = 'Detach';
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
            btn.disabled = false;
            btn.textContent = 'Detach';
        });
    }
    window.detachHook = detachHook;

    function openHookAttachForm() {
        var form = document.getElementById('hook-attach-form');
        if (form) {
            form.style.display = 'block';
            var input = document.getElementById('hook-attach-bead');
            if (input) {
                input.value = '';
                setTimeout(function() { input.focus(); }, 50);
            }
        }
    }
    window.openHookAttachForm = openHookAttachForm;

    function closeHookAttachForm() {
        var form = document.getElementById('hook-attach-form');
        if (form) {
            form.style.display = 'none';
        }
    }
    window.closeHookAttachForm = closeHookAttachForm;

    function submitHookAttach() {
        var input = document.getElementById('hook-attach-bead');
        var beadId = input ? input.value.trim() : '';

        if (!beadId) {
            showToast('error', 'Missing', 'Bead ID is required');
            return;
        }

        var submitBtn = document.querySelector('.hook-attach-submit');
        if (submitBtn) {
            submitBtn.disabled = true;
            submitBtn.textContent = '...';
        }

        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: 'hook attach ' + beadId, confirmed: true })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Attached', beadId + ' attached to hook');
                closeHookAttachForm();
                if (typeof htmx !== 'undefined') {
                    htmx.trigger(document.body, 'htmx:load');
                }
            } else {
                showToast('error', 'Failed', data.error || 'Failed to attach hook');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        })
        .finally(function() {
            if (submitBtn) {
                submitBtn.disabled = false;
                submitBtn.textContent = 'Attach';
            }
        });
    }
    window.submitHookAttach = submitHookAttach;

    function clearAllHooks() {
        if (!confirm('Clear ALL hooks? This will detach all hooked work.')) return;

        var rows = document.querySelectorAll('.hook-detach-btn');
        if (rows.length === 0) {
            showToast('info', 'Nothing', 'No hooks to clear');
            return;
        }

        var beadIds = [];
        for (var i = 0; i < rows.length; i++) {
            var id = rows[i].getAttribute('data-hook-id');
            if (id) beadIds.push(id);
        }

        var completed = 0;
        var errors = 0;

        beadIds.forEach(function(beadId) {
            fetch('/api/run', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ command: 'hook detach ' + beadId, confirmed: true })
            })
            .then(function(r) { return r.json(); })
            .then(function(data) {
                if (data.success) {
                    completed++;
                } else {
                    errors++;
                }
            })
            .catch(function() {
                errors++;
            })
            .finally(function() {
                if (completed + errors === beadIds.length) {
                    if (errors > 0) {
                        showToast('error', 'Partial', completed + ' detached, ' + errors + ' failed');
                    } else {
                        showToast('success', 'Cleared', completed + ' hook(s) cleared');
                    }
                    if (typeof htmx !== 'undefined') {
                        htmx.trigger(document.body, 'htmx:load');
                    }
                }
            });
        });
    }
    window.clearAllHooks = clearAllHooks;

    // Handle Enter key in hook attach input
    document.addEventListener('keydown', function(e) {
        if (e.key === 'Enter' && e.target.id === 'hook-attach-bead') {
            e.preventDefault();
            submitHookAttach();
        }
        if (e.key === 'Escape' && e.target.id === 'hook-attach-bead') {
            e.preventDefault();
            closeHookAttachForm();
        }
    });

    // ============================================
    // ISSUE CREATION MODAL
    // ============================================
    function openIssueModal() {
        var modal = document.getElementById('issue-modal');
        if (modal) {
            modal.style.display = 'flex';
            window.pauseRefresh = true;
            // Focus the title input
            var titleInput = document.getElementById('issue-title');
            if (titleInput) {
                setTimeout(function() { titleInput.focus(); }, 100);
            }
        }
    }
    window.openIssueModal = openIssueModal;

    function closeIssueModal() {
        var modal = document.getElementById('issue-modal');
        if (modal) {
            modal.style.display = 'none';
            window.pauseRefresh = false;
            // Reset form
            var form = document.getElementById('issue-form');
            if (form) form.reset();
        }
    }
    window.closeIssueModal = closeIssueModal;

    function submitIssue(e) {
        e.preventDefault();
        
        var title = document.getElementById('issue-title').value.trim();
        var priority = document.getElementById('issue-priority').value;
        var description = document.getElementById('issue-description').value.trim();
        var submitBtn = document.getElementById('issue-submit-btn');

        if (!title) {
            showToast('error', 'Missing', 'Title is required');
            return;
        }

        // Disable button while submitting
        submitBtn.disabled = true;
        submitBtn.textContent = 'Creating...';

        var payload = {
            title: title,
            priority: parseInt(priority, 10)
        };
        if (description) {
            payload.description = description;
        }

        fetch('/api/issues/create', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Created', 'Issue ' + (data.id || '') + ' created');
                closeIssueModal();
                // Trigger a page refresh to show the new issue
                if (typeof htmx !== 'undefined') {
                    htmx.trigger(document.body, 'htmx:load');
                }
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        })
        .finally(function() {
            submitBtn.disabled = false;
            submitBtn.textContent = 'Create Issue';
        });
    }
    window.submitIssue = submitIssue;

    // Close modal on Escape key
    document.addEventListener('keydown', function(e) {
        if (e.key === 'Escape') {
            var modal = document.getElementById('issue-modal');
            if (modal && modal.style.display !== 'none') {
                closeIssueModal();
            }
        }
    });

    // ============================================
    // WORK PANEL TABS
    // ============================================
    function switchWorkTab(tab) {
        // Update active tab button
        document.querySelectorAll('.panel-tabs .tab-btn').forEach(function(btn) {
            btn.classList.remove('active');
            if (btn.getAttribute('data-tab') === tab) {
                btn.classList.add('active');
            }
        });

        // Filter rows based on tab
        var rows = document.querySelectorAll('#work-table tbody tr');
        rows.forEach(function(row) {
            var status = row.getAttribute('data-status') || 'ready';
            if (tab === 'all') {
                row.style.display = '';
            } else if (tab === 'ready' && status === 'ready') {
                row.style.display = '';
            } else if (tab === 'progress' && status === 'progress') {
                row.style.display = '';
            } else {
                row.style.display = 'none';
            }
        });

        // Update count
        var visibleCount = 0;
        rows.forEach(function(row) {
            if (row.style.display !== 'none') visibleCount++;
        });
        var countEl = document.querySelector('#work-panel .count');
        if (countEl) countEl.textContent = visibleCount;
    }
    window.switchWorkTab = switchWorkTab;

    // Initialize work panel to "Ready" tab on load
    setTimeout(function() {
        switchWorkTab('ready');
    }, 100);

    // ============================================
    // READY WORK PANEL
    // ============================================
    function loadReady() {
        var loading = document.getElementById('ready-loading');
        var table = document.getElementById('ready-table');
        var tbody = document.getElementById('ready-tbody');
        var empty = document.getElementById('ready-empty');
        var count = document.getElementById('ready-count');

        if (!loading || !table || !tbody) return;

        fetch('/api/ready')
            .then(function(r) { return r.json(); })
            .then(function(data) {
                loading.style.display = 'none';

                if (data.items && data.items.length > 0) {
                    table.style.display = 'table';
                    empty.style.display = 'none';
                    tbody.innerHTML = '';

                    data.items.forEach(function(item) {
                        var tr = document.createElement('tr');
                        var rowClass = '';
                        if (item.priority === 1) rowClass = 'ready-p1';
                        else if (item.priority === 2) rowClass = 'ready-p2';
                        tr.className = rowClass;

                        var priBadge = '';
                        if (item.priority === 1) priBadge = '<span class="badge badge-red">P1</span>';
                        else if (item.priority === 2) priBadge = '<span class="badge badge-orange">P2</span>';
                        else if (item.priority === 3) priBadge = '<span class="badge badge-yellow">P3</span>';
                        else priBadge = '<span class="badge badge-muted">P4</span>';

                        var sourceClass = item.source === 'town' ? 'ready-source ready-source-town' : 'ready-source';

                        tr.innerHTML =
                            '<td>' + priBadge + '</td>' +
                            '<td><span class="ready-id">' + escapeHtml(item.id) + '</span></td>' +
                            '<td><span class="ready-title">' + escapeHtml(item.title || '') + '</span></td>' +
                            '<td><span class="' + sourceClass + '">' + escapeHtml(item.source) + '</span></td>' +
                            '<td><button class="sling-btn" data-bead-id="' + escapeHtml(item.id) + '" title="Sling to rig">Sling</button></td>';
                        tbody.appendChild(tr);
                    });

                    if (count) count.textContent = data.summary.total;
                } else {
                    table.style.display = 'none';
                    empty.style.display = 'block';
                    if (count) count.textContent = '0';
                }
            })
            .catch(function(err) {
                loading.textContent = 'Failed to load ready work';
                console.error('Ready work load error:', err);
            });
    }

    // Load ready work on page load
    loadReady();
    // Expose for refresh after HTMX swaps
    window.refreshReadyPanel = loadReady;

    // ============================================
    // CONVOY PANEL INTERACTIONS
    // ============================================
    var convoyList = document.getElementById('convoy-list');
    var convoyDetail = document.getElementById('convoy-detail');
    var convoyCreateForm = document.getElementById('convoy-create-form');
    var currentConvoyId = null;

    // Click on convoy row to view details
    document.addEventListener('click', function(e) {
        var convoyRow = e.target.closest('.convoy-row');
        if (convoyRow && convoyRow.hasAttribute('data-convoy-id')) {
            e.preventDefault();
            var convoyId = convoyRow.getAttribute('data-convoy-id');
            if (convoyId) {
                openConvoyDetail(convoyId);
            }
        }
    });

    function openConvoyDetail(convoyId) {
        currentConvoyId = convoyId;
        window.pauseRefresh = true;

        // Reset views
        document.getElementById('convoy-detail-id').textContent = convoyId;
        document.getElementById('convoy-detail-title').textContent = 'Convoy: ' + convoyId;
        document.getElementById('convoy-detail-status').textContent = '';
        document.getElementById('convoy-detail-progress').textContent = '';
        document.getElementById('convoy-issues-loading').style.display = 'block';
        document.getElementById('convoy-issues-table').style.display = 'none';
        document.getElementById('convoy-issues-empty').style.display = 'none';
        document.getElementById('convoy-add-issue-form').style.display = 'none';

        // Show detail, hide list and create form
        convoyList.style.display = 'none';
        convoyCreateForm.style.display = 'none';
        convoyDetail.style.display = 'block';

        // Fetch convoy status via /api/run
        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: 'convoy status ' + convoyId })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            document.getElementById('convoy-issues-loading').style.display = 'none';

            if (!data.success) {
                document.getElementById('convoy-issues-empty').style.display = 'block';
                document.getElementById('convoy-issues-empty').querySelector('p').textContent = data.error || 'Failed to load convoy';
                return;
            }

            var issues = parseConvoyStatusOutput(data.output || '');
            if (issues.length === 0) {
                document.getElementById('convoy-issues-empty').style.display = 'block';
                return;
            }

            var tbody = document.getElementById('convoy-issues-tbody');
            tbody.innerHTML = '';
            issues.forEach(function(issue) {
                var tr = document.createElement('tr');
                var statusBadge = '';
                var statusLower = (issue.status || '').toLowerCase();
                if (statusLower === 'closed' || statusLower === 'complete' || statusLower === 'done') {
                    statusBadge = '<span class="badge badge-green">Done</span>';
                } else if (statusLower === 'in_progress' || statusLower === 'in progress' || statusLower === 'working') {
                    statusBadge = '<span class="badge badge-yellow">In Progress</span>';
                } else if (statusLower === 'open' || statusLower === 'ready') {
                    statusBadge = '<span class="badge badge-blue">Open</span>';
                } else if (statusLower === 'blocked') {
                    statusBadge = '<span class="badge badge-red">Blocked</span>';
                } else {
                    statusBadge = '<span class="badge badge-muted">' + escapeHtml(issue.status || 'Unknown') + '</span>';
                }

                tr.innerHTML =
                    '<td class="convoy-issue-status">' + statusBadge + '</td>' +
                    '<td><span class="issue-id">' + escapeHtml(issue.id) + '</span></td>' +
                    '<td class="issue-title">' + escapeHtml(issue.title || '') + '</td>' +
                    '<td>' + (issue.assignee ? '<span class="badge badge-blue">' + escapeHtml(issue.assignee) + '</span>' : '<span class="badge badge-muted">Unassigned</span>') + '</td>' +
                    '<td>' + escapeHtml(issue.progress || '') + '</td>';
                tbody.appendChild(tr);
            });
            document.getElementById('convoy-issues-table').style.display = 'table';
        })
        .catch(function(err) {
            document.getElementById('convoy-issues-loading').style.display = 'none';
            document.getElementById('convoy-issues-empty').style.display = 'block';
            document.getElementById('convoy-issues-empty').querySelector('p').textContent = 'Error: ' + err.message;
        });
    }

    // Parse convoy status text output into issue objects
    function parseConvoyStatusOutput(output) {
        var issues = [];
        var lines = output.split('\n');
        for (var i = 0; i < lines.length; i++) {
            var line = lines[i].trim();
            if (!line) continue;
            // Skip header lines and convoy summary lines
            if (line.startsWith('Convoy') || line.startsWith('===') || line.startsWith('---') ||
                line.startsWith('Status:') || line.startsWith('Progress:') || line.startsWith('Created:') ||
                line.startsWith('Title:') || line.startsWith('Issues:') || line.startsWith('Name:')) {
                // Extract convoy-level status/progress for the detail header
                if (line.startsWith('Status:')) {
                    var statusEl = document.getElementById('convoy-detail-status');
                    var statusVal = line.replace('Status:', '').trim().toLowerCase();
                    statusEl.textContent = statusVal;
                    statusEl.className = 'badge';
                    if (statusVal === 'active') statusEl.classList.add('badge-green');
                    else if (statusVal === 'stale') statusEl.classList.add('badge-yellow');
                    else if (statusVal === 'stuck') statusEl.classList.add('badge-red');
                    else if (statusVal === 'complete') statusEl.classList.add('badge-green');
                    else statusEl.classList.add('badge-muted');
                }
                if (line.startsWith('Progress:')) {
                    document.getElementById('convoy-detail-progress').textContent = line.replace('Progress:', '').trim();
                }
                continue;
            }
            // Look for issue lines - typically formatted as:
            // "○ id · title [● P2 · STATUS]" or similar bead-style output
            // Or tabular: "id   title   status   assignee"
            var issue = parseConvoyIssueLine(line);
            if (issue) {
                issues.push(issue);
            }
        }
        return issues;
    }

    // Parse a single issue line from convoy status output
    function parseConvoyIssueLine(line) {
        // Try bead-style format: "○ id · title   [● P2 · OPEN]"
        // or "◐ id · title   [● P2 · IN_PROGRESS]"
        var beadMatch = line.match(/^[○◐●✓]\s+(\S+)\s+[·:]\s+(.+?)(?:\s+\[.*?([A-Z_]+)\])?$/);
        if (beadMatch) {
            var statusFromBracket = '';
            if (beadMatch[3]) {
                statusFromBracket = beadMatch[3].toLowerCase().replace('_', ' ');
            } else {
                // Infer from icon
                if (line.startsWith('✓')) statusFromBracket = 'closed';
                else if (line.startsWith('◐')) statusFromBracket = 'in progress';
                else statusFromBracket = 'open';
            }
            return {
                id: beadMatch[1],
                title: beadMatch[2].trim(),
                status: statusFromBracket,
                assignee: '',
                progress: ''
            };
        }

        // Try simple "id title" format (at least an ID-like token)
        var parts = line.split(/\s{2,}/);
        if (parts.length >= 2 && parts[0].match(/^[a-zA-Z0-9_-]+$/)) {
            return {
                id: parts[0],
                title: parts[1] || '',
                status: parts[2] || '',
                assignee: parts[3] || '',
                progress: parts[4] || ''
            };
        }

        return null;
    }

    // Back button from convoy detail
    document.getElementById('convoy-back-btn').addEventListener('click', function() {
        convoyDetail.style.display = 'none';
        convoyList.style.display = 'block';
        currentConvoyId = null;
        window.pauseRefresh = false;
    });

    // New Convoy button
    document.getElementById('new-convoy-btn').addEventListener('click', function() {
        window.pauseRefresh = true;
        convoyList.style.display = 'none';
        convoyDetail.style.display = 'none';
        convoyCreateForm.style.display = 'block';
        document.getElementById('convoy-create-name').value = '';
        document.getElementById('convoy-create-issues').value = '';
        document.getElementById('convoy-create-name').focus();
    });

    // Cancel create convoy
    document.getElementById('convoy-create-back-btn').addEventListener('click', cancelConvoyCreate);
    document.getElementById('convoy-create-cancel-btn').addEventListener('click', cancelConvoyCreate);

    function cancelConvoyCreate() {
        convoyCreateForm.style.display = 'none';
        convoyList.style.display = 'block';
        window.pauseRefresh = false;
    }

    // Submit create convoy
    document.getElementById('convoy-create-submit-btn').addEventListener('click', function() {
        var name = document.getElementById('convoy-create-name').value.trim();
        var issuesStr = document.getElementById('convoy-create-issues').value.trim();

        if (!name) {
            showToast('error', 'Missing', 'Convoy name is required');
            return;
        }

        var btn = document.getElementById('convoy-create-submit-btn');
        btn.disabled = true;
        btn.textContent = 'Creating...';

        // Build command: convoy create <name> [issue1 issue2 ...]
        var cmd = 'convoy create ' + name;
        if (issuesStr) {
            cmd += ' ' + issuesStr;
        }

        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: cmd, confirmed: true, timeout: 120 })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Created', 'Convoy "' + name + '" created');
                cancelConvoyCreate();
                if (data.output && data.output.trim()) {
                    showOutput(cmd, data.output);
                }
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        })
        .finally(function() {
            btn.disabled = false;
            btn.textContent = 'Create Convoy';
        });
    });

    // Add Issue button in convoy detail
    document.getElementById('convoy-add-issue-btn').addEventListener('click', function() {
        var form = document.getElementById('convoy-add-issue-form');
        form.style.display = form.style.display === 'none' ? 'flex' : 'none';
        if (form.style.display !== 'none') {
            document.getElementById('convoy-add-issue-input').value = '';
            document.getElementById('convoy-add-issue-input').focus();
        }
    });

    // Cancel add issue
    document.getElementById('convoy-add-issue-cancel').addEventListener('click', function() {
        document.getElementById('convoy-add-issue-form').style.display = 'none';
    });

    // Submit add issue to convoy
    document.getElementById('convoy-add-issue-submit').addEventListener('click', submitAddIssueToConvoy);

    // Enter key in add issue input
    document.getElementById('convoy-add-issue-input').addEventListener('keydown', function(e) {
        if (e.key === 'Enter') {
            e.preventDefault();
            submitAddIssueToConvoy();
        } else if (e.key === 'Escape') {
            e.preventDefault();
            document.getElementById('convoy-add-issue-form').style.display = 'none';
        }
    });

    function submitAddIssueToConvoy() {
        var issueId = document.getElementById('convoy-add-issue-input').value.trim();
        if (!issueId || !currentConvoyId) {
            showToast('error', 'Missing', 'Issue ID is required');
            return;
        }

        var btn = document.getElementById('convoy-add-issue-submit');
        btn.disabled = true;
        btn.textContent = 'Adding...';

        var cmd = 'convoy add ' + currentConvoyId + ' ' + issueId;

        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: cmd, confirmed: true })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Added', 'Issue ' + issueId + ' added to convoy');
                document.getElementById('convoy-add-issue-form').style.display = 'none';
                // Refresh the convoy detail view
                openConvoyDetail(currentConvoyId);
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        })
        .finally(function() {
            btn.disabled = false;
            btn.textContent = 'Add';
        });
    }

    // Click on mail thread header - toggle expand or open single message
    document.addEventListener('click', function(e) {
        // Handle click on individual message within expanded thread
        var threadMsg = e.target.closest('.mail-thread-msg');
        if (threadMsg) {
            e.preventDefault();
            var msgId = threadMsg.getAttribute('data-msg-id');
            var from = threadMsg.getAttribute('data-from');
            if (msgId) {
                openMailDetail(msgId, from);
            }
            return;
        }

        // Handle click on thread header
        var threadHeader = e.target.closest('.mail-thread-header');
        if (threadHeader) {
            e.preventDefault();
            var msgId = threadHeader.getAttribute('data-msg-id');
            if (msgId) {
                // Single message thread - open directly
                var from = threadHeader.getAttribute('data-from');
                openMailDetail(msgId, from);
            } else {
                // Multi-message thread - toggle expand/collapse
                var threadEl = threadHeader.closest('.mail-thread');
                var msgsEl = threadEl ? threadEl.querySelector('.mail-thread-messages') : null;
                if (msgsEl) {
                    var isExpanded = msgsEl.style.display !== 'none';
                    msgsEl.style.display = isExpanded ? 'none' : 'block';
                    threadEl.classList.toggle('mail-thread-expanded', !isExpanded);
                }
            }
            return;
        }

        // Legacy: handle click on mail-row (All Traffic tab)
        var mailRow = e.target.closest('.mail-row');
        if (mailRow) {
            e.preventDefault();
            var msgId = mailRow.getAttribute('data-msg-id');
            var from = mailRow.getAttribute('data-from');
            if (msgId) {
                openMailDetail(msgId, from);
            }
        }
    });

    function openMailDetail(msgId, from) {
        currentMessageId = msgId;
        currentMessageFrom = from;

        // Pause HTMX refresh while viewing/composing mail
        window.pauseRefresh = true;

        // Show loading state
        document.getElementById('mail-detail-subject').textContent = 'Loading...';
        document.getElementById('mail-detail-from').textContent = from || '';
        document.getElementById('mail-detail-body').textContent = '';
        document.getElementById('mail-detail-time').textContent = '';

        // Hide both list views and compose, show detail
        mailList.style.display = 'none';
        if (mailAll) mailAll.style.display = 'none';
        mailCompose.style.display = 'none';
        mailDetail.style.display = 'block';

        // Fetch message content
        fetch('/api/mail/read?id=' + encodeURIComponent(msgId))
            .then(function(r) { return r.json(); })
            .then(function(msg) {
                document.getElementById('mail-detail-subject').textContent = msg.subject || '(no subject)';
                document.getElementById('mail-detail-from').textContent = msg.from || from;
                document.getElementById('mail-detail-body').textContent = msg.body || '(no content)';
                document.getElementById('mail-detail-time').textContent = msg.timestamp || '';
            })
            .catch(function(err) {
                document.getElementById('mail-detail-body').textContent = 'Error loading message: ' + err.message;
            });
    }

    // Back button from detail view - return to correct tab
    document.getElementById('mail-back-btn').addEventListener('click', function() {
        mailDetail.style.display = 'none';
        mailCompose.style.display = 'none';

        // Return to the correct view based on current tab
        if (currentMailTab === 'all' && mailAll) {
            mailAll.style.display = 'block';
            mailList.style.display = 'none';
        } else {
            mailList.style.display = 'block';
            if (mailAll) mailAll.style.display = 'none';
        }

        currentMessageId = null;
        currentMessageFrom = null;
        // Resume HTMX refresh
        window.pauseRefresh = false;
    });

    // Reply button
    document.getElementById('mail-reply-btn').addEventListener('click', function() {
        var subject = document.getElementById('mail-detail-subject').textContent;
        var replySubject = subject.startsWith('Re: ') ? subject : 'Re: ' + subject;

        document.getElementById('mail-compose-title').textContent = 'Reply';
        document.getElementById('compose-subject').value = replySubject;
        document.getElementById('compose-reply-to').value = currentMessageId || '';
        document.getElementById('compose-body').value = '';

        // Populate To dropdown and select the sender
        populateToDropdown(currentMessageFrom);

        mailDetail.style.display = 'none';
        mailCompose.style.display = 'block';
        document.getElementById('compose-body').focus();
    });

    // Compose new message button
    document.getElementById('compose-mail-btn').addEventListener('click', function() {
        // Pause HTMX refresh while composing
        window.pauseRefresh = true;

        document.getElementById('mail-compose-title').textContent = 'New Message';
        document.getElementById('compose-subject').value = '';
        document.getElementById('compose-body').value = '';
        document.getElementById('compose-reply-to').value = '';

        // Populate To dropdown
        populateToDropdown(null);

        // Hide all mail views, show compose
        mailList.style.display = 'none';
        if (mailAll) mailAll.style.display = 'none';
        mailDetail.style.display = 'none';
        mailCompose.style.display = 'block';
        document.getElementById('compose-to').focus();
    });

    // Back button from compose view
    document.getElementById('compose-back-btn').addEventListener('click', function() {
        mailCompose.style.display = 'none';
        if (currentMessageId) {
            mailDetail.style.display = 'block';
        } else if (currentMailTab === 'all' && mailAll) {
            mailAll.style.display = 'block';
        } else {
            mailList.style.display = 'block';
        }
    });

    // Cancel compose
    document.getElementById('compose-cancel-btn').addEventListener('click', function() {
        mailCompose.style.display = 'none';
        mailList.style.display = 'block';
        currentMessageId = null;
        currentMessageFrom = null;
        // Resume HTMX refresh
        window.pauseRefresh = false;
    });

    // Send message
    document.getElementById('mail-send-btn').addEventListener('click', function() {
        var to = document.getElementById('compose-to').value;
        var subject = document.getElementById('compose-subject').value;
        var body = document.getElementById('compose-body').value;
        var replyTo = document.getElementById('compose-reply-to').value;

        if (!to || !subject) {
            showToast('error', 'Missing fields', 'Please fill in To and Subject');
            return;
        }

        var btn = document.getElementById('mail-send-btn');
        btn.textContent = 'Sending...';
        btn.disabled = true;

        fetch('/api/mail/send', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                to: to,
                subject: subject,
                body: body,
                reply_to: replyTo || undefined
            })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Sent', 'Message sent to ' + to);
                mailCompose.style.display = 'none';
                mailList.style.display = 'block';
                currentMessageId = null;
                currentMessageFrom = null;
                // Resume HTMX refresh and reload inbox
                window.pauseRefresh = false;
                loadMailInbox();
            } else {
                showToast('error', 'Failed', data.error || 'Failed to send message');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        })
        .finally(function() {
            btn.textContent = 'Send';
            btn.disabled = false;
        });
    });

    // Populate To dropdown with agents
    // Returns a Promise so callers can wait for it
    function populateToDropdown(selectedValue) {
        var select = document.getElementById('compose-to');
        
        // Show loading state
        select.innerHTML = '<option value="">Loading recipients...</option>';
        select.disabled = true;

        // If we have a selected value for reply, add it immediately so it's available
        if (selectedValue) {
            var cleanValue = selectedValue.replace(/\/$/, '').trim();
            var opt = document.createElement('option');
            opt.value = cleanValue;
            opt.textContent = cleanValue + ' (replying to)';
            opt.selected = true;
            select.appendChild(opt);
            select.disabled = false;
        }

        // Fetch agents from options API
        return fetch('/api/options')
            .then(function(r) { return r.json(); })
            .then(function(data) {
                // Clear loading state, rebuild options
                select.innerHTML = '<option value="">Select recipient...</option>';
                
                // Re-add reply-to if present
                if (selectedValue) {
                    var cleanVal = selectedValue.replace(/\/$/, '').trim();
                    var replyOpt = document.createElement('option');
                    replyOpt.value = cleanVal;
                    replyOpt.textContent = cleanVal + ' (replying to)';
                    replyOpt.selected = true;
                    select.appendChild(replyOpt);
                }
                
                var agents = data.agents || [];
                var addedValues = selectedValue ? [selectedValue.replace(/\/$/, '').toLowerCase()] : [];

                agents.forEach(function(agent) {
                    var name = typeof agent === 'string' ? agent : agent.name;
                    var running = typeof agent === 'object' ? agent.running : true;

                    // Skip if already added as reply-to
                    if (addedValues.indexOf(name.toLowerCase()) !== -1) {
                        return;
                    }

                    var opt = document.createElement('option');
                    opt.value = name;
                    opt.innerHTML = escapeHtml(name) + (running ? ' (' + '<svg class="icon icon-point-filled icon-green" aria-hidden="true"><use href="#icon-point-filled"/></svg>' + ' running)' : ' (' + '<svg class="icon icon-point" aria-hidden="true"><use href="#icon-point"/></svg>' + ' stopped)');
                    if (!running) opt.disabled = true;
                    select.appendChild(opt);
                });
                
                select.disabled = false;
            })
            .catch(function(err) {
                console.error('Failed to load agents for To dropdown:', err);
                select.innerHTML = '<option value="">Failed to load recipients</option>';
                select.disabled = false;
            });
    }

    // ============================================
    // ISSUE PANEL INTERACTIONS
    // ============================================
    var issuesList = document.getElementById('issues-list');
    var issueDetail = document.getElementById('issue-detail');
    var currentIssueId = null;

    // Click on issue row to view details
    document.addEventListener('click', function(e) {
        var issueRow = e.target.closest('.issue-row');
        if (issueRow && issueRow.hasAttribute('data-issue-id')) {
            e.preventDefault();
            var issueId = issueRow.getAttribute('data-issue-id');
            if (issueId) {
                openIssueDetail(issueId);
            }
        }

        // Click on dependency links
        var depItem = e.target.closest('.issue-dep-item');
        if (depItem) {
            e.preventDefault();
            var depId = depItem.getAttribute('data-issue-id');
            if (depId) {
                openIssueDetail(depId);
            }
        }
    });

    function openIssueDetail(issueId) {
        currentIssueId = issueId;

        // Pause HTMX refresh while viewing issue
        window.pauseRefresh = true;

        // Show loading state
        document.getElementById('issue-detail-id').textContent = issueId;
        document.getElementById('issue-detail-title-text').textContent = 'Loading...';
        document.getElementById('issue-detail-description').textContent = '';
        document.getElementById('issue-detail-priority').textContent = '';
        document.getElementById('issue-detail-status').textContent = '';
        document.getElementById('issue-detail-type').textContent = '';
        document.getElementById('issue-detail-created').textContent = '';
        document.getElementById('issue-detail-owner').textContent = '';
        document.getElementById('issue-detail-actions').innerHTML = '';
        document.getElementById('issue-detail-depends-on').innerHTML = '';
        document.getElementById('issue-detail-blocks').innerHTML = '';
        document.getElementById('issue-detail-deps').style.display = 'none';
        document.getElementById('issue-detail-blocks-section').style.display = 'none';

        // Show detail view
        issuesList.style.display = 'none';
        issueDetail.style.display = 'block';

        // Fetch issue details
        fetch('/api/issues/show?id=' + encodeURIComponent(issueId))
            .then(function(r) { return r.json(); })
            .then(function(data) {
                if (data.error) {
                    document.getElementById('issue-detail-title-text').textContent = 'Error loading issue';
                    document.getElementById('issue-detail-description').textContent = data.error;
                    return;
                }

                document.getElementById('issue-detail-id').textContent = data.id || issueId;
                document.getElementById('issue-detail-title-text').textContent = data.title || '(no title)';
                document.getElementById('issue-detail-description').textContent = data.description || data.raw_output || '(no description)';

                // Priority badge
                var priorityEl = document.getElementById('issue-detail-priority');
                if (data.priority) {
                    priorityEl.textContent = data.priority;
                    priorityEl.className = 'badge';
                    if (data.priority === 'P1') priorityEl.classList.add('badge-red');
                    else if (data.priority === 'P2') priorityEl.classList.add('badge-orange');
                    else if (data.priority === 'P3') priorityEl.classList.add('badge-yellow');
                    else priorityEl.classList.add('badge-muted');
                }

                // Status
                var statusEl = document.getElementById('issue-detail-status');
                if (data.status) {
                    statusEl.textContent = data.status;
                    statusEl.className = 'issue-status ' + data.status.toLowerCase().replace(' ', '_');
                }

                // Meta info
                if (data.type) {
                    document.getElementById('issue-detail-type').textContent = 'Type: ' + data.type;
                }
                if (data.owner) {
                    document.getElementById('issue-detail-owner').textContent = 'Owner: ' + data.owner;
                }
                if (data.created) {
                    document.getElementById('issue-detail-created').textContent = 'Created: ' + data.created;
                }

                // Render action buttons
                renderIssueActions(issueId, data);

                // Dependencies
                if (data.depends_on && data.depends_on.length > 0) {
                    document.getElementById('issue-detail-deps').style.display = 'block';
                    var depsHtml = data.depends_on.map(function(dep) {
                        return '<span class="issue-dep-item" data-issue-id="' + escapeHtml(dep) + '">→ ' + escapeHtml(dep) + '</span>';
                    }).join(' ');
                    document.getElementById('issue-detail-depends-on').innerHTML = depsHtml;
                }

                // Blocks
                if (data.blocks && data.blocks.length > 0) {
                    document.getElementById('issue-detail-blocks-section').style.display = 'block';
                    var blocksHtml = data.blocks.map(function(dep) {
                        return '<span class="issue-dep-item" data-issue-id="' + escapeHtml(dep) + '">← ' + escapeHtml(dep) + '</span>';
                    }).join(' ');
                    document.getElementById('issue-detail-blocks').innerHTML = blocksHtml;
                }
            })
            .catch(function(err) {
                document.getElementById('issue-detail-title-text').textContent = 'Error';
                document.getElementById('issue-detail-description').textContent = 'Failed to load issue: ' + err.message;
            });
    }

    // Back button from issue detail
    var issueBackBtn = document.getElementById('issue-back-btn');
    if (issueBackBtn) {
        issueBackBtn.addEventListener('click', function() {
            issueDetail.style.display = 'none';
            issuesList.style.display = 'block';
            currentIssueId = null;
            // Resume HTMX refresh
            window.pauseRefresh = false;
        });
    }

    // ============================================
    // ISSUE ACTION BUTTONS
    // ============================================

    // Render action buttons based on current issue state
    function renderIssueActions(issueId, data) {
        var actionsEl = document.getElementById('issue-detail-actions');
        if (!actionsEl) return;

        var status = (data.status || '').toUpperCase();
        var isClosed = status === 'CLOSED';
        var currentPriority = data.priority || 'P2';
        // Extract numeric priority (P1 -> 1, P2 -> 2, etc.)
        var priNum = currentPriority.length === 2 ? parseInt(currentPriority[1], 10) : 2;

        var html = '<div class="issue-actions-bar">';

        // Close / Reopen button
        if (isClosed) {
            html += '<button class="issue-action-btn reopen" onclick="reopenIssue(\'' + escapeHtml(issueId) + '\')">' + '<svg class="icon icon-rotate" aria-hidden="true"><use href="#icon-rotate"/></svg>' + ' Reopen</button>';
        } else {
            html += '<button class="issue-action-btn close" onclick="closeIssue(\'' + escapeHtml(issueId) + '\')">' + '<svg class="icon icon-check" aria-hidden="true"><use href="#icon-check"/></svg>' + ' Close</button>';
        }

        // Priority dropdown
        html += '<div class="issue-action-group">';
        html += '<label class="issue-action-label">Priority</label>';
        html += '<select class="issue-action-select" id="issue-action-priority" onchange="updateIssuePriority(\'' + escapeHtml(issueId) + '\', this.value)">';
        for (var p = 1; p <= 4; p++) {
            var sel = p === priNum ? ' selected' : '';
            var pLabel = p === 1 ? 'P1 - Critical' : p === 2 ? 'P2 - High' : p === 3 ? 'P3 - Medium' : 'P4 - Low';
            html += '<option value="' + p + '"' + sel + '>' + pLabel + '</option>';
        }
        html += '</select>';
        html += '</div>';

        // Assignee dropdown
        html += '<div class="issue-action-group">';
        html += '<label class="issue-action-label">Assign</label>';
        html += '<select class="issue-action-select" id="issue-action-assignee" onchange="assignIssue(\'' + escapeHtml(issueId) + '\', this.value)">';
        html += '<option value="">Unassigned</option>';
        html += '<option value="" disabled>Loading agents...</option>';
        html += '</select>';
        html += '</div>';

        html += '</div>';
        actionsEl.innerHTML = html;

        // Load agents for assignee dropdown
        loadAssigneeOptions(data.owner || '');
    }

    // Load agent options into the assignee dropdown
    function loadAssigneeOptions(currentOwner) {
        var select = document.getElementById('issue-action-assignee');
        if (!select) return;

        fetch('/api/options')
            .then(function(r) { return r.json(); })
            .then(function(data) {
                // Rebuild dropdown
                var html = '<option value="">Unassigned</option>';
                var agents = data.agents || [];
                var polecats = data.polecats || [];

                // Combine agents and polecats for assignee options
                var seen = {};
                var allOptions = [];

                agents.forEach(function(agent) {
                    var name = typeof agent === 'string' ? agent : agent.name;
                    if (!seen[name]) {
                        seen[name] = true;
                        allOptions.push(name);
                    }
                });

                polecats.forEach(function(polecat) {
                    if (!seen[polecat]) {
                        seen[polecat] = true;
                        allOptions.push(polecat);
                    }
                });

                allOptions.forEach(function(name) {
                    var sel = name === currentOwner ? ' selected' : '';
                    html += '<option value="' + escapeHtml(name) + '"' + sel + '>' + escapeHtml(name) + '</option>';
                });

                select.innerHTML = html;
            })
            .catch(function() {
                select.innerHTML = '<option value="">Unassigned</option>';
            });
    }

    // Close an issue
    function closeIssue(issueId) {
        if (!confirm('Close issue ' + issueId + '?')) return;

        showToast('info', 'Closing...', issueId);

        fetch('/api/issues/close', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ id: issueId })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Closed', issueId + ' closed');
                // Re-fetch to update the detail view
                openIssueDetail(issueId);
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        });
    }
    window.closeIssue = closeIssue;

    // Reopen an issue
    function reopenIssue(issueId) {
        showToast('info', 'Reopening...', issueId);

        fetch('/api/issues/update', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ id: issueId, status: 'open' })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Reopened', issueId + ' reopened');
                openIssueDetail(issueId);
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        });
    }
    window.reopenIssue = reopenIssue;

    // Update issue priority
    function updateIssuePriority(issueId, priority) {
        var priNum = parseInt(priority, 10);
        if (priNum < 1 || priNum > 4) return;

        showToast('info', 'Updating...', 'Setting priority to P' + priNum);

        fetch('/api/issues/update', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ id: issueId, priority: priNum })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Updated', 'Priority set to P' + priNum);
                openIssueDetail(issueId);
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        });
    }
    window.updateIssuePriority = updateIssuePriority;

    // Assign issue to agent
    function assignIssue(issueId, assignee) {
        if (!assignee) return; // Unassigned selected, no-op for now

        showToast('info', 'Assigning...', 'Assigning to ' + assignee);

        fetch('/api/issues/update', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ id: issueId, assignee: assignee })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Assigned', 'Assigned to ' + assignee);
                openIssueDetail(issueId);
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message);
        });
    }
    window.assignIssue = assignIssue;

    // ============================================
    // PR/MERGE QUEUE PANEL INTERACTIONS
    // ============================================
    var prList = document.getElementById('pr-list');
    var prDetail = document.getElementById('pr-detail');
    var currentPrUrl = null;

    // Click on PR row to view details
    document.addEventListener('click', function(e) {
        var prRow = e.target.closest('.pr-row');
        if (prRow && prRow.hasAttribute('data-pr-url')) {
            e.preventDefault();
            var prUrl = prRow.getAttribute('data-pr-url');
            if (prUrl) {
                openPrDetail(prUrl);
            }
        }
    });

    function openPrDetail(prUrl) {
        currentPrUrl = prUrl;

        // Pause HTMX refresh while viewing PR
        window.pauseRefresh = true;

        // Show loading state
        document.getElementById('pr-detail-number').textContent = 'Loading...';
        document.getElementById('pr-detail-title-text').textContent = '';
        document.getElementById('pr-detail-body').textContent = '';
        document.getElementById('pr-detail-state').textContent = '';
        document.getElementById('pr-detail-author').textContent = '';
        document.getElementById('pr-detail-branches').textContent = '';
        document.getElementById('pr-detail-created').textContent = '';
        document.getElementById('pr-detail-additions').textContent = '';
        document.getElementById('pr-detail-deletions').textContent = '';
        document.getElementById('pr-detail-files').textContent = '';
        document.getElementById('pr-detail-labels').innerHTML = '';
        document.getElementById('pr-detail-checks').innerHTML = '';
        document.getElementById('pr-detail-labels-section').style.display = 'none';
        document.getElementById('pr-detail-checks-section').style.display = 'none';
        document.getElementById('pr-detail-link').href = prUrl;

        // Show detail view
        prList.style.display = 'none';
        prDetail.style.display = 'block';

        // Fetch PR details
        fetch('/api/pr/show?url=' + encodeURIComponent(prUrl))
            .then(function(r) { return r.json(); })
            .then(function(data) {
                if (data.error) {
                    document.getElementById('pr-detail-title-text').textContent = 'Error loading PR';
                    document.getElementById('pr-detail-body').textContent = data.error;
                    return;
                }

                document.getElementById('pr-detail-number').textContent = '#' + data.number;
                document.getElementById('pr-detail-title-text').textContent = data.title || '(no title)';
                document.getElementById('pr-detail-body').textContent = data.body || '(no description)';

                // State badge
                var stateEl = document.getElementById('pr-detail-state');
                if (data.state) {
                    stateEl.textContent = data.state;
                    stateEl.className = 'pr-state ' + data.state.toLowerCase();
                }

                // Meta info
                if (data.author) {
                    document.getElementById('pr-detail-author').textContent = 'by ' + data.author;
                }
                if (data.base_ref && data.head_ref) {
                    document.getElementById('pr-detail-branches').textContent = data.head_ref + ' → ' + data.base_ref;
                }
                if (data.created_at) {
                    var created = new Date(data.created_at);
                    document.getElementById('pr-detail-created').textContent = 'Created ' + created.toLocaleDateString();
                }

                // Stats
                if (data.additions !== undefined) {
                    document.getElementById('pr-detail-additions').textContent = '+' + data.additions;
                }
                if (data.deletions !== undefined) {
                    document.getElementById('pr-detail-deletions').textContent = '-' + data.deletions;
                }
                if (data.changed_files !== undefined) {
                    document.getElementById('pr-detail-files').textContent = data.changed_files + ' files';
                }

                // Labels
                if (data.labels && data.labels.length > 0) {
                    document.getElementById('pr-detail-labels-section').style.display = 'block';
                    var labelsHtml = data.labels.map(function(label) {
                        return '<span class="pr-label">' + escapeHtml(label) + '</span>';
                    }).join(' ');
                    document.getElementById('pr-detail-labels').innerHTML = labelsHtml;
                }

                // Checks
                if (data.checks && data.checks.length > 0) {
                    document.getElementById('pr-detail-checks-section').style.display = 'block';
                    var checksHtml = data.checks.map(function(check) {
                        var checkClass = 'pr-check';
                        if (check.toLowerCase().includes('success')) checkClass += ' success';
                        else if (check.toLowerCase().includes('failure')) checkClass += ' failure';
                        else if (check.toLowerCase().includes('pending') || check.toLowerCase().includes('in_progress')) checkClass += ' pending';
                        return '<span class="' + checkClass + '">' + escapeHtml(check) + '</span>';
                    }).join('');
                    document.getElementById('pr-detail-checks').innerHTML = checksHtml;
                }
            })
            .catch(function(err) {
                document.getElementById('pr-detail-title-text').textContent = 'Error';
                document.getElementById('pr-detail-body').textContent = 'Failed to load PR: ' + err.message;
            });
    }

    // Back button from PR detail
    var prBackBtn = document.getElementById('pr-back-btn');
    if (prBackBtn) {
        prBackBtn.addEventListener('click', function() {
            prDetail.style.display = 'none';
            prList.style.display = 'block';
            currentPrUrl = null;
            // Resume HTMX refresh
            window.pauseRefresh = false;
        });
    }

    // ============================================
    // SLING BUTTONS
    // ============================================
    var activeSlingDropdown = null;

    function closeSlingDropdown() {
        if (activeSlingDropdown) {
            activeSlingDropdown.remove();
            activeSlingDropdown = null;
        }
    }

    function openSlingDropdown(btn) {
        closeSlingDropdown();

        var beadId = btn.getAttribute('data-bead-id');
        if (!beadId) return;

        var dropdown = document.createElement('div');
        dropdown.className = 'sling-dropdown';
        dropdown.innerHTML = '<div class="sling-dropdown-loading">Loading rigs...</div>';

        // Position dropdown below the button
        var rect = btn.getBoundingClientRect();
        dropdown.style.position = 'fixed';
        dropdown.style.top = (rect.bottom + 4) + 'px';
        dropdown.style.left = rect.left + 'px';
        dropdown.style.zIndex = '10001';
        document.body.appendChild(dropdown);
        activeSlingDropdown = dropdown;

        // Fetch rig options
        fetch('/api/options?type=rigs')
            .then(function(r) { return r.json(); })
            .then(function(data) {
                var rigs = data.rigs || [];
                if (rigs.length === 0) {
                    dropdown.innerHTML = '<div class="sling-dropdown-empty">No rigs available</div>';
                    return;
                }
                var html = '<div class="sling-dropdown-header">Sling ' + escapeHtml(beadId) + ' to:</div>';
                for (var i = 0; i < rigs.length; i++) {
                    html += '<button class="sling-dropdown-item" data-rig="' + escapeHtml(rigs[i]) + '">' + escapeHtml(rigs[i]) + '</button>';
                }
                dropdown.innerHTML = html;

                // Handle rig selection
                dropdown.addEventListener('click', function(e) {
                    var item = e.target.closest('.sling-dropdown-item');
                    if (!item) return;
                    var rig = item.getAttribute('data-rig');
                    closeSlingDropdown();
                    executeSling(beadId, rig);
                });
            })
            .catch(function() {
                dropdown.innerHTML = '<div class="sling-dropdown-empty">Failed to load rigs</div>';
            });
    }

    function executeSling(beadId, rig) {
        var cmd = 'sling ' + beadId + ' ' + rig;
        showToast('info', 'Slinging...', beadId + ' → ' + rig);

        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: cmd, confirmed: true, timeout: 120 })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Slung', beadId + ' → ' + rig);
                if (data.output && data.output.trim()) {
                    showOutput(cmd, data.output);
                }
            } else {
                showToast('error', 'Sling failed', data.error || 'Unknown error');
                if (data.output) {
                    showOutput(cmd, data.output);
                }
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message || 'Request failed');
        });
    }

    // Click handler for sling buttons
    document.addEventListener('click', function(e) {
        var slingBtn = e.target.closest('.sling-btn');
        if (slingBtn) {
            e.preventDefault();
            e.stopPropagation();
            openSlingDropdown(slingBtn);
            return;
        }
        // Close dropdown when clicking outside
        if (activeSlingDropdown && !e.target.closest('.sling-dropdown')) {
            closeSlingDropdown();
        }
    });



    // ============================================
    // ESCALATION ACTIONS
    // ============================================
    document.addEventListener('click', function(e) {
        var btn = e.target.closest('.esc-btn');
        if (!btn) return;

        e.preventDefault();
        e.stopPropagation();

        var action = btn.getAttribute('data-action');
        var id = btn.getAttribute('data-id');
        if (!action || !id) return;

        if (action === 'reassign') {
            showReassignPicker(btn, id);
            return;
        }

        // Ack or Resolve - run directly
        var cmdName = 'escalate ' + action + ' ' + id;
        btn.disabled = true;
        btn.textContent = action === 'ack' ? 'Acking...' : 'Resolving...';

        runEscalationAction(cmdName, btn, action);
    });

    function runEscalationAction(cmdName, btn, action) {
        showToast('info', 'Running...', 'gt ' + cmdName);

        fetch('/api/run', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ command: cmdName, confirmed: true })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            if (data.success) {
                showToast('success', 'Success', 'gt ' + cmdName);
                // Remove ack button or fade row on resolve
                var row = btn.closest('.escalation-row');
                if (action === 'resolve' && row) {
                    row.style.opacity = '0.4';
                    row.style.pointerEvents = 'none';
                } else if (action === 'ack' && row) {
                    // Replace ack button with ACK badge
                    btn.outerHTML = '<span class="badge badge-cyan">ACK</span>';
                }
            } else {
                showToast('error', 'Failed', data.error || 'Unknown error');
                btn.disabled = false;
                btn.innerHTML = action === 'ack' ? ('<svg class="icon icon-thumb-up" aria-hidden="true"><use href="#icon-thumb-up"/></svg>' + ' Ack') : ('<svg class="icon icon-check" aria-hidden="true"><use href="#icon-check"/></svg>' + ' Resolve');
            }
        })
        .catch(function(err) {
            showToast('error', 'Error', err.message || 'Request failed');
            btn.disabled = false;
            btn.innerHTML = action === 'ack' ? ('<svg class="icon icon-thumb-up" aria-hidden="true"><use href="#icon-thumb-up"/></svg>' + ' Ack') : ('<svg class="icon icon-check" aria-hidden="true"><use href="#icon-check"/></svg>' + ' Resolve');
        });
    }

    function showReassignPicker(btn, escalationId) {
        // Check if picker already open
        var existing = btn.parentNode.querySelector('.reassign-picker');
        if (existing) {
            existing.remove();
            return;
        }

        var picker = document.createElement('div');
        picker.className = 'reassign-picker';
        picker.innerHTML = '<select class="reassign-select"><option value="">Loading...</option></select>' +
            '<button class="esc-btn esc-reassign-confirm">Go</button>' +
            '<button class="esc-btn esc-reassign-cancel">' + '<svg class="icon icon-x" aria-hidden="true"><use href="#icon-x"/></svg>' + '</button>';
        btn.parentNode.appendChild(picker);

        var select = picker.querySelector('.reassign-select');

        // Pause refresh while picker is open
        window.pauseRefresh = true;

        // Load agents
        fetch('/api/options')
            .then(function(r) { return r.json(); })
            .then(function(data) {
                select.innerHTML = '<option value="">Select agent...</option>';
                var agents = data.agents || [];
                agents.forEach(function(agent) {
                    var name = typeof agent === 'string' ? agent : agent.name;
                    var running = typeof agent === 'object' ? agent.running : true;
                    var opt = document.createElement('option');
                    opt.value = name;
                    opt.textContent = name + (running ? '' : ' (stopped)');
                    select.appendChild(opt);
                });
            })
            .catch(function() {
                select.innerHTML = '<option value="">Failed to load</option>';
            });

        // Confirm reassign
        picker.querySelector('.esc-reassign-confirm').addEventListener('click', function() {
            var agent = select.value;
            if (!agent) {
                showToast('error', 'Missing', 'Select an agent to reassign to');
                return;
            }
            picker.remove();
            window.pauseRefresh = false;

            var cmdName = 'escalate reassign ' + escalationId + ' ' + agent;
            btn.disabled = true;
            btn.textContent = 'Reassigning...';

            showToast('info', 'Running...', 'gt ' + cmdName);

            fetch('/api/run', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ command: cmdName, confirmed: true })
            })
            .then(function(r) { return r.json(); })
            .then(function(data) {
                if (data.success) {
                    showToast('success', 'Reassigned', 'Escalation reassigned to ' + agent);
                    var row = btn.closest('.escalation-row');
                    if (row) {
                        // Update the "From" cell to show new assignee
                        var fromCell = row.querySelectorAll('td')[2];
                        if (fromCell) fromCell.textContent = '→ ' + agent;
                    }
                } else {
                    showToast('error', 'Failed', data.error || 'Unknown error');
                }
                btn.disabled = false;
                btn.innerHTML = '<svg class="icon icon-rotate" aria-hidden="true"><use href="#icon-rotate"/></svg>' + ' Reassign';
            })
            .catch(function(err) {
                showToast('error', 'Error', err.message || 'Request failed');
                btn.disabled = false;
                btn.innerHTML = '<svg class="icon icon-rotate" aria-hidden="true"><use href="#icon-rotate"/></svg>' + ' Reassign';
            });
        });

        // Cancel
        picker.querySelector('.esc-reassign-cancel').addEventListener('click', function() {
            picker.remove();
            window.pauseRefresh = false;
        });
    }



    // ============================================
    // ACTIVITY TIMELINE FILTERS
    // ============================================

    function initTimelineFilters() {
        var timeline = document.getElementById('activity-timeline');
        if (!timeline) return;

        var entries = timeline.querySelectorAll('.tl-entry');
        var rigFilter = document.getElementById('tl-rig-filter');
        var agentFilter = document.getElementById('tl-agent-filter');
        var emptyMsg = document.getElementById('tl-empty-filtered');

        // Collect unique rigs and agents for dropdowns
        var rigs = {};
        var agents = {};
        entries.forEach(function(entry) {
            var rig = entry.getAttribute('data-rig');
            var agent = entry.getAttribute('data-agent');
            if (rig) rigs[rig] = true;
            if (agent) agents[agent] = true;
        });

        // Populate rig dropdown
        if (rigFilter) {
            Object.keys(rigs).sort().forEach(function(rig) {
                var opt = document.createElement('option');
                opt.value = rig;
                opt.textContent = rig;
                rigFilter.appendChild(opt);
            });
        }

        // Populate agent dropdown
        if (agentFilter) {
            Object.keys(agents).sort().forEach(function(agent) {
                var opt = document.createElement('option');
                opt.value = agent;
                opt.textContent = agent;
                agentFilter.appendChild(opt);
            });
        }

        // Current filter state
        var activeCategory = 'all';

        function applyFilters() {
            var selectedRig = rigFilter ? rigFilter.value : 'all';
            var selectedAgent = agentFilter ? agentFilter.value : 'all';
            var visibleCount = 0;

            entries.forEach(function(entry) {
                var show = true;

                if (activeCategory !== 'all' && entry.getAttribute('data-category') !== activeCategory) {
                    show = false;
                }
                if (selectedRig !== 'all' && entry.getAttribute('data-rig') !== selectedRig) {
                    show = false;
                }
                if (selectedAgent !== 'all' && entry.getAttribute('data-agent') !== selectedAgent) {
                    show = false;
                }

                if (show) {
                    entry.classList.remove('tl-hidden');
                    visibleCount++;
                } else {
                    entry.classList.add('tl-hidden');
                }
            });

            if (emptyMsg) {
                emptyMsg.style.display = visibleCount === 0 ? 'block' : 'none';
            }
        }

        // Category filter buttons
        document.addEventListener('click', function(e) {
            var btn = e.target.closest('.tl-filter-btn');
            if (!btn) return;
            if (btn.getAttribute('data-filter') !== 'category') return;

            // Update active state
            var group = btn.closest('.tl-filter-group');
            if (group) {
                group.querySelectorAll('.tl-filter-btn').forEach(function(b) {
                    b.classList.remove('active');
                });
            }
            btn.classList.add('active');
            activeCategory = btn.getAttribute('data-value');
            applyFilters();
        });

        // Dropdown filters
        if (rigFilter) {
            rigFilter.addEventListener('change', applyFilters);
        }
        if (agentFilter) {
            agentFilter.addEventListener('change', applyFilters);
        }
    }

    // Init on page load
    initTimelineFilters();

    // Re-init after HTMX swaps
    document.body.addEventListener('htmx:afterSwap', function() {
        initTimelineFilters();
    });

    // ============================================
    // SESSION TERMINAL PREVIEW
    // ============================================
    // Click a session row → open live xterm.js terminal over WebSocket.
    // Backend dumps tmux scrollback (last 10k lines) before live attach
    // so users can wheel-scroll up to see historical output.
    // Session-row click now opens the session as a persistent tab in the
    // unified output panel — same terminal surface as palette console
    // commands. The old #session-preview pane (a second xterm host inside
    // the Sessions panel) was removed; one terminal host is enough.
    document.addEventListener('click', function(e) {
        var sessionRow = e.target.closest('.session-row');
        if (sessionRow) {
            e.preventDefault();
            var sessionName = sessionRow.getAttribute('data-session-name');
            if (sessionName) {
                addConsoleTab(sessionName, sessionName, { ephemeral: false });
            }
        }
    });

    // ============================================
    // INTERACTIVE TMUX ATTACH (xterm.js + WebSocket)
    // ============================================
    // The xterm + WebSocket + ttyd-wire plumbing lives in the shared
    // factory (static/terminal-attach.js, also used by the pop-out
    // console window). Here we only manage one active attach at a time,
    // bound to the output-panel terminal host, plus the show/hide of its
    // wrap element and the dashboard's pause-refresh behavior.
    var currentAttach = null;
    // attachTargets controls which DOM nodes the attach binds to. Every
    // attach currently mounts into the output-panel terminal host; the
    // structure is retained so a future surface can re-bind via the
    // optional `targets` arg without reworking callers.
    var attachTargets = {
        wrapId:   'output-panel-terminal-wrap',
        termId:   'output-panel-terminal',
        statusId: 'output-panel-status',
    };

    // refitActiveAttach re-fits the live terminal to its container and
    // pushes the new size to the PTY. Safe to call when nothing is attached.
    function refitActiveAttach() {
        if (currentAttach) currentAttach.refit();
    }
    // Back-compat alias for the panel resize / minimize handlers.
    function sendAttachResize() { refitActiveAttach(); }

    function openSessionAttach(sessionName, targets) {
        if (typeof Terminal === 'undefined' || !window.GTTerminalAttach) {
            alert('xterm.js failed to load; terminal unavailable');
            return;
        }
        // Tear down any prior attach BEFORE swapping targets so the close
        // path hides the old wrap, not the one we're about to mount into.
        closeSessionAttachInner();

        attachTargets = targets || {
            wrapId:   'output-panel-terminal-wrap',
            termId:   'output-panel-terminal',
            statusId: 'output-panel-status',
        };
        var wrapEl = document.getElementById(attachTargets.wrapId);
        var termEl = document.getElementById(attachTargets.termId);
        if (!termEl || !wrapEl) return;
        wrapEl.style.display = 'flex';

        var statusEl = document.getElementById(attachTargets.statusId);
        currentAttach = window.GTTerminalAttach.create({
            sessionName: sessionName,
            onStatus: function(text, level) {
                if (!statusEl) return;
                statusEl.textContent = text;
                statusEl.className = 'output-panel-status ' + (level === 'ok' ? 'live' : level === 'warn' ? 'warn' : level === 'muted' ? 'muted' : '');
            },
            // Surface the tmux window title as the panel header tooltip.
            onTitle: function(title) { if (outputCmd) outputCmd.title = title; },
        });
        currentAttach.mount(termEl);

        // Pause dashboard re-renders while the terminal is live; a morph
        // would wipe the xterm canvas and drop focus. hx-preserve on
        // #output-panel-terminal-wrap is a second line of defense.
        window.pauseRefresh = true;
        setTimeout(function() {
            if (currentAttach) currentAttach.focus();
            if (termEl) termEl.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
        }, 50);
    }

    function closeSessionAttachInner() {
        if (currentAttach) {
            currentAttach.close();
            currentAttach = null;
        }
        // Hide whichever wrap we were bound to so its slot stops eating
        // layout space when the user reopens a non-terminal view there.
        var wrapEl = document.getElementById(attachTargets.wrapId);
        if (wrapEl) wrapEl.style.display = 'none';
    }


    // ============================================
    // CONVOY DRILL-DOWN (expand rows to show tracked issues)
    // ============================================
    var convoyCache = {}; // Cache fetched convoy data by ID

    document.addEventListener('click', function(e) {
        var row = e.target.closest('.convoy-row');
        if (!row) return;

        e.preventDefault();
        var convoyId = row.getAttribute('data-convoy-id');
        if (!convoyId) return;

        // Check if already expanded
        var existingDetail = row.nextElementSibling;
        if (existingDetail && existingDetail.classList.contains('convoy-detail-row')) {
            // Collapse: remove the detail row
            existingDetail.remove();
            row.classList.remove('convoy-expanded');
            var toggle = row.querySelector('.convoy-toggle');
            if (toggle) toggle.innerHTML = '<svg class="icon icon-chevron-right" aria-hidden="true"><use href="#icon-chevron-right"/></svg>';
            return;
        }

        // Collapse any other expanded convoy
        document.querySelectorAll('.convoy-detail-row').forEach(function(r) { r.remove(); });
        document.querySelectorAll('.convoy-row.convoy-expanded').forEach(function(r) {
            r.classList.remove('convoy-expanded');
            var t = r.querySelector('.convoy-toggle');
            if (t) t.innerHTML = '<svg class="icon icon-chevron-right" aria-hidden="true"><use href="#icon-chevron-right"/></svg>';
        });

        // Mark this row as expanded
        row.classList.add('convoy-expanded');
        var toggleEl = row.querySelector('.convoy-toggle');
        if (toggleEl) toggleEl.innerHTML = '<svg class="icon icon-chevron-down" aria-hidden="true"><use href="#icon-chevron-down"/></svg>';

        // Create detail row
        var detailRow = document.createElement('tr');
        detailRow.className = 'convoy-detail-row';
        var detailCell = document.createElement('td');
        detailCell.colSpan = 4;
        detailCell.innerHTML = '<div class="tracked-issues"><div class="tracked-issues-loading">Loading tracked issues...</div></div>';
        detailRow.appendChild(detailCell);
        row.parentNode.insertBefore(detailRow, row.nextSibling);

        // Check cache first
        if (convoyCache[convoyId]) {
            renderConvoyIssues(detailCell, convoyCache[convoyId]);
            return;
        }

        // Fetch via dedicated headless endpoint (/api/run now spawns a tmux
        // console and no longer returns JSON output, see api.go handleRun).
        fetch('/api/convoy/status?id=' + encodeURIComponent(convoyId))
        .then(function(r) {
            if (!r.ok) {
                return r.json().then(function(e) { throw new Error(e.error || ('HTTP ' + r.status)); });
            }
            return r.json();
        })
        .then(function(data) {
            convoyCache[convoyId] = data;
            renderConvoyIssues(detailCell, data);
        })
        .catch(function(err) {
            detailCell.innerHTML = '<div class="tracked-issues"><div class="tracked-issues-error">Failed to load: ' + escapeHtml(err.message) + '</div></div>';
        });
    });

    function renderConvoyIssues(cell, data) {
        var issues = data.tracked || [];
        if (issues.length === 0) {
            cell.innerHTML = '<div class="tracked-issues"><div class="tracked-issues-empty">No tracked issues</div></div>';
            return;
        }

        var html = '<div class="tracked-issues">';
        html += '<table class="tracked-issues-table">';
        html += '<thead><tr><th>Status</th><th>ID</th><th>Title</th><th>Assignee</th><th>Progress</th></tr></thead>';
        html += '<tbody>';

        for (var i = 0; i < issues.length; i++) {
            var issue = issues[i];

            // Status badge
            var statusBadge = '';
            switch (issue.status) {
                case 'closed':
                    statusBadge = '<span class="badge badge-green">Done</span>';
                    break;
                case 'in_progress':
                    statusBadge = '<span class="badge badge-yellow">In Progress</span>';
                    break;
                case 'hooked':
                    statusBadge = '<span class="badge badge-blue">Hooked</span>';
                    break;
                default:
                    statusBadge = '<span class="badge badge-muted">Open</span>';
            }

            // Assignee - extract short name
            var assignee = '—';
            if (issue.assignee) {
                var parts = issue.assignee.split('/');
                assignee = parts[parts.length - 1];
            }

            // Worker info as progress indicator
            var progress = '';
            if (issue.status === 'closed') {
                progress = '<span class="convoy-progress-done">' + '<svg class="icon icon-check" aria-hidden="true"><use href="#icon-check"/></svg>' + '</span>';
            } else if (issue.worker) {
                var workerName = issue.worker.split('/').pop();
                progress = '<span class="convoy-progress-active">@' + escapeHtml(workerName) + '</span>';
                if (issue.worker_age) {
                    progress += ' <span class="convoy-progress-age">' + escapeHtml(issue.worker_age) + '</span>';
                }
            }

            html += '<tr class="tracked-issue-row tracked-issue-' + escapeHtml(issue.status) + '">' +
                '<td>' + statusBadge + '</td>' +
                '<td><span class="issue-id">' + escapeHtml(issue.id) + '</span></td>' +
                '<td class="tracked-issue-title">' + escapeHtml(issue.title) + '</td>' +
                '<td class="tracked-issue-assignee">' + escapeHtml(assignee) + '</td>' +
                '<td class="tracked-issue-progress">' + progress + '</td>' +
                '</tr>';
        }

        html += '</tbody></table>';

        // Progress summary
        var completed = data.completed || 0;
        var total = data.total || issues.length;
        var pct = total > 0 ? Math.round((completed / total) * 100) : 0;
        html += '<div class="tracked-issues-summary">';
        html += '<div class="tracked-issues-progress-bar"><div class="tracked-issues-progress-fill" style="width: ' + pct + '%;"></div></div>';
        html += '<span class="tracked-issues-progress-text">' + completed + '/' + total + ' completed (' + pct + '%)</span>';
        html += '</div>';

        html += '</div>';
        cell.innerHTML = html;
    }

    // ==========================================================================
    // QUOTA DRAWER (Mosaic grid)
    //
    // Hydrated from /api/quota/stream (SSE, same shape as `gt quota status --json`).
    // The collapsed `🎫 Quota` stat in the summary banner shows live counters
    // (available | limited | expired); clicking it toggles the drawer with a
    // mosaic of per-account cards: status, token expiry, rotation count,
    // active sessions, and live token usage. Refreshed on quota_* SSE events
    // and after every htmx swap (the morph re-renders the empty placeholder).
    // Open/closed state persists across refreshes via localStorage so the
    // drawer doesn't snap closed under the 30s polling refresh.
    // ==========================================================================
    var QUOTA_DRAWER_STATE_KEY = 'gastown.quota.drawer';

    function quotaDrawerIsOpen() {
        try { return localStorage.getItem(QUOTA_DRAWER_STATE_KEY) === 'open'; }
        catch (e) { return false; }
    }
    function setQuotaDrawerState(open) {
        try { localStorage.setItem(QUOTA_DRAWER_STATE_KEY, open ? 'open' : 'closed'); }
        catch (e) { /* private mode — accept ephemeral state */ }
    }

    function applyQuotaDrawerVisibility() {
        var drawer = document.getElementById('quota-drawer');
        var trigger = document.getElementById('quota-stat-trigger');
        if (!drawer) return;
        var open = quotaDrawerIsOpen();
        if (open) {
            drawer.removeAttribute('hidden');
        } else {
            drawer.setAttribute('hidden', '');
        }
        if (trigger) trigger.setAttribute('aria-expanded', open ? 'true' : 'false');
    }

    function toggleQuotaDrawer() {
        setQuotaDrawerState(!quotaDrawerIsOpen());
        applyQuotaDrawerVisibility();
        // No fetch on open — the SSE stream already pushes snapshots whenever
        // they change, and the last one is cached in lastQuotaSnapshot. Render
        // it immediately so the drawer isn't empty until the next tick.
        if (quotaDrawerIsOpen() && lastQuotaSnapshot) {
            renderQuotaStatbar(lastQuotaSnapshot.counters);
            renderQuotaMosaic(lastQuotaSnapshot);
        }
    }

    function fmtRelative(rfc3339) {
        if (!rfc3339) return '';
        var t = Date.parse(rfc3339);
        if (isNaN(t)) return rfc3339;
        var delta = (t - Date.now()) / 1000;
        var abs = Math.abs(delta);
        var sign = delta >= 0 ? 'in ' : '';
        var suffix = delta >= 0 ? '' : ' ago';
        var unit;
        if (abs < 60) { unit = Math.round(abs) + 's'; }
        else if (abs < 3600) { unit = Math.round(abs / 60) + 'm'; }
        else if (abs < 86400) { unit = Math.round(abs / 3600) + 'h'; }
        else { unit = Math.round(abs / 86400) + 'd'; }
        return sign + unit + suffix;
    }

    function fmtTokens(n) {
        if (!n || n <= 0) return '0';
        if (n >= 1e9) return (n / 1e9).toFixed(1) + 'B';
        if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
        if (n >= 1e3) return (n / 1e3).toFixed(1) + 'k';
        return String(n);
    }

    function escapeHTML(s) {
        if (s == null) return '';
        return String(s)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    function renderQuotaStatbar(counters) {
        var el = document.getElementById('quota-stat-summary');
        if (!el) return;
        var c = counters || {};
        var parts = [];
        parts.push('<span class="qv-avail">' + (c.available || 0) + ' ' + '<svg class="icon icon-point-filled icon-green" aria-hidden="true"><use href="#icon-point-filled"/></svg>' + '</span>');
        if (c.limited)  { parts.push('<span class="qv-sep">|</span><span class="qv-lim">' + c.limited + ' ' + '<svg class="icon icon-alert-triangle icon-yellow" aria-hidden="true"><use href="#icon-alert-triangle"/></svg>' + '</span>'); }
        if (c.expired)  { parts.push('<span class="qv-sep">|</span><span class="qv-exp">' + c.expired + ' ' + '<svg class="icon icon-x icon-red" aria-hidden="true"><use href="#icon-x"/></svg>' + '</span>'); }
        if (c.cooldown) { parts.push('<span class="qv-sep">|</span><span class="qv-cool">' + c.cooldown + ' ' + '<svg class="icon icon-hourglass-empty" aria-hidden="true"><use href="#icon-hourglass-empty"/></svg>' + '</span>'); }
        el.innerHTML = parts.join('');
    }

    function renderQuotaCard(a, limitedSessions, ceilings) {
        var statusKey = a.status || 'available';
        var classes = 'quota-card status-' + statusKey;
        var html = '<article class="' + classes + '">';
        html += '<div class="quota-card-head">';
        html += '<span class="quota-card-handle">';
        if (a.is_default) html += '<span class="default-marker" title="Default account">' + '<svg class="icon icon-chevron-right" aria-hidden="true"><use href="#icon-chevron-right"/></svg>' + '</span>';
        html += escapeHTML(a.handle);
        html += '</span>';
        html += '<span class="quota-card-status s-' + statusKey + '">' + escapeHTML(statusKey) + '</span>';
        html += '</div>';

        if (a.email) {
            html += '<div class="quota-card-email">' + escapeHTML(a.email) + '</div>';
        }

        // This config dir is currently borrowing another account's token via a
        // quota rotation swap. Say so explicitly — otherwise the operator reads
        // the borrowed login as this account's own and assumes it's signed in.
        if (a.swapped_to) {
            html += '<div class="quota-card-swap" title="Quota rotation swapped this account&#39;s credentials">running as <code>' + escapeHTML(a.swapped_to) + '</code></div>';
        }

        html += '<dl class="quota-card-meta">';
        if (a.token_expires_at) {
            var exp = Date.parse(a.token_expires_at);
            var cls = '';
            var elapsed = !isNaN(exp) && exp < Date.now();
            if (!isNaN(exp)) {
                if (elapsed) cls = 'bad';
                else if (exp - Date.now() < 24 * 3600 * 1000) cls = 'warn';
            }
            // Distinguish "expires in 3h" from "expired 16h ago" so the user
            // never has to infer direction from the relative timestamp.
            var label = elapsed ? 'expired' : 'expires';
            html += '<dt>' + label + '</dt><dd class="' + cls + '">' + escapeHTML(fmtRelative(a.token_expires_at)) + '</dd>';
        }
        // Unlock countdown: prefer the parsed RFC3339 (unlocks_at) so the user
        // sees "unlocks in 2h 14m"; fall back to the raw "7pm" string when the
        // server couldn't parse a future time.
        if (a.unlocks_at) {
            var raw = a.resets_at ? ' title="resets ' + escapeHTML(a.resets_at) + '"' : '';
            html += '<dt>unlocks</dt><dd class="warn"' + raw + '>' + escapeHTML(fmtRelative(a.unlocks_at)) + '</dd>';
        } else if (a.resets_at) {
            html += '<dt>resets</dt><dd class="warn">' + escapeHTML(a.resets_at) + '</dd>';
        }
        if (a.rotation_count) {
            var lr = a.last_rotated_at ? ' · ' + fmtRelative(a.last_rotated_at) : '';
            html += '<dt>rotated</dt><dd>' + a.rotation_count + '×' + escapeHTML(lr) + '</dd>';
        }
        if (a.last_used && !a.rotation_count) {
            html += '<dt>last use</dt><dd>' + escapeHTML(fmtRelative(a.last_used)) + '</dd>';
        }
        html += '</dl>';

        // Expired tokens don't auto-unlock — they need a manual relogin. Show
        // the exact command so the operator can copy it without thinking.
        if (statusKey === 'expired') {
            html += '<div class="quota-card-action">needs relogin: <code>gt account login ' + escapeHTML(a.handle) + '</code></div>';
        }

        if (a.active_sessions && a.active_sessions.length) {
            html += '<div class="quota-card-sessions">';
            for (var i = 0; i < a.active_sessions.length; i++) {
                var sess = a.active_sessions[i];
                var sessInfo = limitedSessions && limitedSessions[sess];
                var sessCls = sessInfo && sessInfo.rate_limited ? 'quota-card-session rate-limited' : 'quota-card-session';
                html += '<span class="' + sessCls + '" title="' + escapeHTML(sess) + '">' + escapeHTML(sess) + '</span>';
            }
            html += '</div>';
        }

        // Two usage bars per account — session (5h) and week (7d) — each
        // showing "remaining until block": ceiling − tokens used in window.
        // The ceilings are operator-configured estimates (Anthropic's real
        // limit is opaque), passed in via resp.usage_ceilings. Always render
        // both bars even at 0 so the operator can tell "no transcript yet"
        // apart from "the card itself is broken". Counts come from the
        // assistant `usage` blocks in this account's Claude transcripts.
        var ceil = ceilings || {};
        var sessionUsed = a.usage ? quotaTokenTotal(a.usage.counts) : 0;
        var weekUsed = a.usage ? quotaTokenTotal(a.usage.week_counts) : 0;
        var sessCount = (a.usage && a.usage.sessions) ? a.usage.sessions.length : 0;
        html += renderUsageWindowBar('Session (5h)', sessionUsed, ceil.session_token_ceiling || 0, a.usage != null);
        html += renderUsageWindowBar('Week (7d)', weekUsed, ceil.weekly_token_ceiling || 0, a.usage != null);
        if (sessCount > 1) {
            html += '<div class="quota-card-usage-sess">' + sessCount + ' active sessions</div>';
        }

        html += '</article>';
        return html;
    }

    // Sum input+output+cache from a usage `counts`/`week_counts` block.
    function quotaTokenTotal(counts) {
        if (!counts) return 0;
        return (counts.input_tokens || 0) +
               (counts.output_tokens || 0) +
               (counts.cache_read_tokens || 0) +
               (counts.cache_creation_tokens || 0);
    }

    // Render one usage window bar showing tokens used / ceiling plus the
    // estimated remaining-until-block. `hasData` distinguishes an account with
    // a usage report (0 used → "no usage in window") from one without (—).
    function renderUsageWindowBar(label, used, ceiling, hasData) {
        var pct = ceiling > 0 ? Math.min(100, Math.round((used / ceiling) * 100)) : 0;
        var fillCls = 'quota-card-usage-fill';
        if (pct >= 90) fillCls += ' over';
        else if (pct >= 70) fillCls += ' near';

        var rightLabel;
        if (ceiling > 0) {
            rightLabel = fmtTokens(used) + ' / ' + fmtTokens(ceiling);
        } else if (used > 0) {
            rightLabel = fmtTokens(used) + ' tok';
        } else {
            rightLabel = hasData ? 'no usage in window' : '—';
        }

        var html = '<div class="quota-card-usage">';
        html += '<div class="quota-card-usage-label"><span>' + escapeHTML(label) + '</span><span>' + escapeHTML(rightLabel) + '</span></div>';
        html += '<div class="quota-card-usage-bar"><div class="' + fillCls + '" style="width:' + pct + '%"></div></div>';
        if (ceiling > 0) {
            var remaining = Math.max(0, ceiling - used);
            html += '<div class="quota-card-usage-remaining">~' + fmtTokens(remaining) + ' faltan</div>';
        }
        html += '</div>';
        return html;
    }

    function renderQuotaMosaic(resp) {
        var mosaic = document.getElementById('quota-mosaic');
        if (!mosaic) return;
        var accounts = resp.accounts || [];
        if (accounts.length === 0) {
            mosaic.innerHTML = '<div class="quota-card-empty" style="color:var(--text-muted);font-size:0.8rem;padding:8px">No accounts registered. Run <code>gt account add &lt;handle&gt;</code>.</div>';
            return;
        }
        var html = '';
        for (var i = 0; i < accounts.length; i++) {
            html += renderQuotaCard(accounts[i], resp.limited_sessions, resp.usage_ceilings);
        }
        mosaic.innerHTML = html;

        var meta = document.getElementById('quota-drawer-meta');
        if (meta) {
            var when = resp.generated_at ? fmtRelative(resp.generated_at) : '';
            meta.textContent = when ? 'snapshot ' + when : '';
        }

        renderQuotaWaiting(resp.limited_sessions);
        renderQuotaPlan(resp.last_plan);
        renderQuotaUsageNote(resp);
    }

    // Explain a column of empty bars: surfaces the server-side aggregation
    // error (claude config dir missing, tmux down, etc.) or the orphan-session
    // total when sessions exist but none resolve to a registered account.
    function renderQuotaUsageNote(resp) {
        var el = document.getElementById('quota-usage-note');
        if (!el) return;
        var msgs = [];
        if (resp.usage_error) {
            msgs.push('<span class="bad">usage aggregation failed:</span> ' + escapeHTML(resp.usage_error));
        }
        if (resp.orphan_sessions && resp.orphan_sessions.length > 0) {
            var orphanTokens = 0;
            for (var i = 0; i < resp.orphan_sessions.length; i++) {
                var c = resp.orphan_sessions[i].counts || {};
                orphanTokens += (c.input_tokens || 0) + (c.output_tokens || 0) +
                                (c.cache_read_tokens || 0) + (c.cache_creation_tokens || 0);
            }
            msgs.push(resp.orphan_sessions.length + ' orphan session(s) (' +
                      fmtTokens(orphanTokens) + ' tok) — set <code>GT_QUOTA_ACCOUNT</code> or align <code>CLAUDE_CONFIG_DIR</code> to attribute these.');
        }
        if (msgs.length === 0) {
            el.setAttribute('hidden', '');
            el.innerHTML = '';
            return;
        }
        el.innerHTML = msgs.join('<br>');
        el.removeAttribute('hidden');
    }

    function renderQuotaWaiting(limitedSessions) {
        var el = document.getElementById('quota-waiting');
        if (!el) return;
        var sessions = [];
        if (limitedSessions) {
            for (var sess in limitedSessions) {
                if (Object.prototype.hasOwnProperty.call(limitedSessions, sess)) {
                    sessions.push({ id: sess, info: limitedSessions[sess] });
                }
            }
        }
        if (sessions.length === 0) {
            el.setAttribute('hidden', '');
            el.innerHTML = '';
            return;
        }
        sessions.sort(function(a, b) { return a.id.localeCompare(b.id); });
        var html = '<h3>Waiting on unlock</h3><ul>';
        for (var i = 0; i < sessions.length; i++) {
            var s = sessions[i];
            var reset = s.info.resets_at ? ' (resets ' + escapeHTML(s.info.resets_at) + ')' : '';
            var acct = s.info.account ? ' → ' + escapeHTML(s.info.account) : '';
            html += '<li>' + escapeHTML(s.id) + acct + reset + '</li>';
        }
        html += '</ul>';
        el.innerHTML = html;
        el.removeAttribute('hidden');
    }

    function renderQuotaPlan(plan) {
        var el = document.getElementById('quota-plan');
        if (!el) return;
        if (!plan || !plan.assignments || Object.keys(plan.assignments).length === 0) {
            el.setAttribute('hidden', '');
            el.innerHTML = '';
            return;
        }
        var when = plan.timestamp ? fmtRelative(plan.timestamp) : '';
        var html = '<h3>Last rotation plan' + (when ? ' · ' + escapeHTML(when) : '') + '</h3><ul>';
        var keys = Object.keys(plan.assignments).sort();
        for (var i = 0; i < keys.length; i++) {
            html += '<li>' + escapeHTML(keys[i]) + ' → ' + escapeHTML(plan.assignments[keys[i]]) + '</li>';
        }
        html += '</ul>';
        el.innerHTML = html;
        el.removeAttribute('hidden');
    }

    // Last snapshot received over SSE — kept so the drawer can paint from
    // cache on toggle without re-fetching, and so the post-swap reload
    // pipeline (window.refreshQuotaDrawer) has something to render
    // synchronously before the next stream frame arrives.
    var lastQuotaSnapshot = null;

    function applyQuotaSnapshot(resp) {
        lastQuotaSnapshot = resp;
        renderQuotaStatbar(resp.counters);
        renderQuotaMosaic(resp);
    }

    var quotaStreamSource = null;
    var quotaStreamRetryMs = 1000;
    function openQuotaStream() {
        if (quotaStreamSource) return;
        try {
            quotaStreamSource = new EventSource('/api/quota/stream');
        } catch (e) {
            console.warn('quota stream: EventSource unavailable:', e);
            return;
        }
        quotaStreamSource.addEventListener('quota-snapshot', function(ev) {
            try {
                var resp = JSON.parse(ev.data);
                applyQuotaSnapshot(resp);
                quotaStreamRetryMs = 1000;
            } catch (e) {
                console.warn('quota stream: parse:', e);
            }
        });
        quotaStreamSource.addEventListener('error', function() {
            // EventSource retries automatically while readyState===CONNECTING.
            // If the server is genuinely gone (readyState===CLOSED), tear down
            // and back off — exponential up to 30s — so we don't hammer it.
            if (quotaStreamSource && quotaStreamSource.readyState === 2 /* CLOSED */) {
                quotaStreamSource = null;
                var el = document.getElementById('quota-stat-summary');
                if (el) el.textContent = '?';
                setTimeout(openQuotaStream, quotaStreamRetryMs);
                quotaStreamRetryMs = Math.min(quotaStreamRetryMs * 2, 30000);
            }
        });
    }

    function bindQuotaDrawerToggle() {
        var trigger = document.getElementById('quota-stat-trigger');
        if (trigger && !trigger._quotaBound) {
            trigger.addEventListener('click', toggleQuotaDrawer);
            trigger.addEventListener('keydown', function(e) {
                if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    toggleQuotaDrawer();
                }
            });
            trigger._quotaBound = true;
        }
        var close = document.getElementById('quota-drawer-close');
        if (close && !close._quotaBound) {
            close.addEventListener('click', function() {
                setQuotaDrawerState(false);
                applyQuotaDrawerVisibility();
            });
            close._quotaBound = true;
        }
    }

    // Post-swap reload pipeline expects this hook. Re-rendering from the
    // cached snapshot is enough: the SSE stream pushes a new frame on the
    // very next quota.json write (which a swap triggers), so we don't need
    // to force a fetch here.
    window.refreshQuotaDrawer = function() {
        if (lastQuotaSnapshot) applyQuotaSnapshot(lastQuotaSnapshot);
    };
    window.restoreQuotaDrawerState = function() {
        applyQuotaDrawerVisibility();
        bindQuotaDrawerToggle();
    };

    // Initial wire-up: bind controls, restore prior state, subscribe to the
    // SSE snapshot stream. The first 'quota-snapshot' frame the server emits
    // on connect hydrates the mosaic; subsequent frames push every change.
    bindQuotaDrawerToggle();
    applyQuotaDrawerVisibility();
    openQuotaStream();

    // When the tab regains focus after being hidden, EventSource keeps the
    // connection alive transparently — no manual refresh needed. We only
    // re-open if the server killed the stream while we were backgrounded.
    document.addEventListener('visibilitychange', function() {
        if (!document.hidden && !quotaStreamSource) openQuotaStream();
    });

})();
