// terminal-attach.js
//
// Shared xterm.js + /api/session/attach WebSocket factory used by both the
// dashboard (dashboard.js: console tabs in #output-panel-terminal) and the
// standalone pop-out window (console.html: full-page terminal).
//
// Centralizes the ttyd-compatible wire protocol, key remapping, exponential
// WS reconnect, resize plumbing, and xterm setup — every caller only has to
// supply the session name, a mount container, and optional callbacks for
// status / title updates.
//
// Wire protocol (single-byte command prefix on each frame):
//   Server → Client: '0'+bytes = OUTPUT, '1'+text = WINDOW_TITLE,
//                    '2'+json  = SET_PREFERENCES (ignored)
//   Client → Server: '0'+bytes = INPUT, '1'+json  = RESIZE {cols,rows},
//                    '2'       = PAUSE, '3' = RESUME
//
// Public API:
//   var attach = window.GTTerminalAttach.create({sessionName, onStatus, onTitle});
//   attach.mount(containerEl);
//   attach.refit();          // re-fit + push new size to PTY
//   attach.focus();
//   attach.hasTerm();        // bool — useful for "is something live?" checks
//   attach.getFit();          // FitAddon instance (or null)
//   attach.sessionName;      // accessor field for caller convenience
//   attach.close();          // tear down xterm + WS + listeners
(function() {
    var DEFAULT_SCROLLBACK = 50000;
    var MAX_RECONNECT_MS = 30000;

    function noop() {}

    function create(opts) {
        opts = opts || {};
        var sessionName = opts.sessionName || '';
        var onStatus = typeof opts.onStatus === 'function' ? opts.onStatus : noop;
        var onTitle  = typeof opts.onTitle  === 'function' ? opts.onTitle  : noop;
        var scrollback = opts.scrollback || DEFAULT_SCROLLBACK;

        var term = null;
        var fit = null;
        var ws = null;
        var userClosed = false;
        var retryDelay = 500;
        var retryTimer = null;
        var resizeHandler = null;
        var observer = null;
        var container = null;

        function wsSendCmd(c, payload) {
            if (!ws || ws.readyState !== 1) return;
            var enc;
            if (typeof payload === 'string') enc = new TextEncoder().encode(payload);
            else if (payload) enc = payload;
            else enc = new Uint8Array(0);
            var out = new Uint8Array(enc.length + 1);
            out[0] = c.charCodeAt(0);
            out.set(enc, 1);
            ws.send(out);
        }

        function sendResize() {
            if (!term || !ws || ws.readyState !== 1) return;
            wsSendCmd('1', JSON.stringify({ cols: term.cols, rows: term.rows }));
        }

        function scheduleReconnect(reason) {
            if (userClosed) return;
            onStatus('reconnecting', 'warn');
            retryDelay = Math.min(MAX_RECONNECT_MS, retryDelay * 2);
            clearTimeout(retryTimer);
            retryTimer = setTimeout(connect, retryDelay);
            void reason; // reserved for future status messages
        }

        function connect() {
            var proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
            var url = proto + '//' + window.location.host
                    + '/api/session/attach?session=' + encodeURIComponent(sessionName);
            ws = new WebSocket(url);
            ws.binaryType = 'arraybuffer';

            ws.onopen = function() {
                retryDelay = 500;
                onStatus('live', 'ok');
                sendResize();
            };
            ws.onmessage = function(ev) {
                var bytes = typeof ev.data === 'string'
                    ? new TextEncoder().encode(ev.data)
                    : new Uint8Array(ev.data);
                if (bytes.length === 0) return;
                var cmd = String.fromCharCode(bytes[0]);
                var payload = bytes.subarray(1);
                if (cmd === '0') {
                    if (term) term.write(payload);
                } else if (cmd === '1') {
                    try { onTitle(new TextDecoder().decode(payload)); } catch (e) {}
                }
                // '2' (SET_PREFERENCES) silently dropped — forward-compatible.
            };
            ws.onclose = function(ev) {
                if (userClosed) { onStatus('closed', 'muted'); return; }
                scheduleReconnect((ev && ev.reason) || (ev && 'code ' + ev.code) || 'closed');
            };
            ws.onerror = function() {
                if (!userClosed) onStatus('ws error', 'warn');
            };
        }

        function bindKeyRouting() {
            // Force-route Ctrl+C / Ctrl+Z / Ctrl+\ / Ctrl+D into the PTY.
            // xterm would otherwise bubble Ctrl+C to the browser (Copy);
            // preserve Copy only when there's an active selection.
            term.attachCustomKeyEventHandler(function(ev) {
                if (ev.type !== 'keydown') return true;
                if (!(ev.ctrlKey || ev.metaKey) || ev.shiftKey || ev.altKey) return true;
                var byte = null;
                switch (ev.key) {
                    case 'c': case 'C': byte = '\x03'; break;
                    case 'z': case 'Z': byte = '\x1a'; break;
                    case '\\':          byte = '\x1c'; break;
                    case 'd': case 'D': byte = '\x04'; break;
                }
                if (!byte) return true;
                if (byte === '\x03' && term.hasSelection && term.hasSelection()) return true;
                wsSendCmd('0', byte);
                ev.preventDefault();
                ev.stopPropagation();
                return false;
            });
            term.onData(function(data) { wsSendCmd('0', data); });
        }

        function bindResize() {
            resizeHandler = function() {
                if (fit) { try { fit.fit(); } catch (e) {} }
                sendResize();
            };
            window.addEventListener('resize', resizeHandler);
            if (typeof ResizeObserver !== 'undefined' && container) {
                observer = new ResizeObserver(resizeHandler);
                observer.observe(container);
            }
        }

        function mount(el) {
            if (!sessionName) return;
            container = el;
            term = new Terminal({
                fontFamily: 'monospace',
                fontSize: 13,
                cursorBlink: true,
                convertEol: false,
                allowProposedApi: true,
                scrollback: scrollback,
                scrollOnUserInput: true,
            });
            if (window.FitAddon && window.FitAddon.FitAddon) {
                fit = new window.FitAddon.FitAddon();
                term.loadAddon(fit);
            }
            if (window.WebLinksAddon && window.WebLinksAddon.WebLinksAddon) {
                term.loadAddon(new window.WebLinksAddon.WebLinksAddon());
            }
            term.open(el);
            if (fit) { try { fit.fit(); } catch (e) {} }
            bindKeyRouting();
            bindResize();
            connect();
            setTimeout(function() { if (term) term.focus(); }, 50);
        }

        function close() {
            userClosed = true;
            if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; }
            if (observer)   { try { observer.disconnect(); } catch (e) {} observer = null; }
            if (ws)         { try { ws.close(); } catch (e) {} ws = null; }
            if (term) {
                try { term.dispose(); } catch (e) {}
                term = null;
            }
            fit = null;
            if (resizeHandler) {
                window.removeEventListener('resize', resizeHandler);
                resizeHandler = null;
            }
        }

        function refit() {
            if (resizeHandler) resizeHandler();
        }

        function focus() {
            if (term) term.focus();
        }

        function hasTerm() {
            return !!term;
        }

        function getFit() {
            return fit;
        }

        return {
            mount:       mount,
            close:       close,
            refit:       refit,
            focus:       focus,
            hasTerm:     hasTerm,
            getFit:      getFit,
            sessionName: sessionName,
        };
    }

    window.GTTerminalAttach = { create: create };
})();
