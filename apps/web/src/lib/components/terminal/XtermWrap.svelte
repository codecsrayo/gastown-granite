<script lang="ts">
  // XtermWrap — single terminal pane (hq-fe-view.11).
  //
  // Mounts an xterm.js instance, opens a binary WebSocket to
  // `GET /api/sessions/:id/term` (hq-fe-term.2), wires bytes both directions,
  // and emits a JSON text frame `{"resize":{"cols":N,"rows":M}}` on the xterm
  // resize callback so the gt-web handler can pass it through.
  //
  // Auth note: browser `WebSocket` cannot set arbitrary headers, so JWT bearer
  // delivery over WS is not wired here. Open posture (dev) works as-is; prod
  // auth over WS is a follow-up bead (cookie / `?token=` query, decided
  // alongside RBAC config).
  //
  // Lazy-loaded by `Dock.svelte` — pulling `@xterm/xterm` (~150kb gz) only
  // when the dock is expanded keeps the SPA bundle lean.

  import { onMount, onDestroy } from 'svelte';
  import { Terminal } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import '@xterm/xterm/css/xterm.css';

  let { sessionId }: { sessionId: string } = $props();

  let host: HTMLDivElement | undefined = $state();
  let status = $state<'connecting' | 'open' | 'closed' | 'error'>('connecting');
  let lastError = $state<string | null>(null);

  // Held outside `$state` — xterm + WebSocket are mutable handles, not reactive
  // values. Wrapping them in runes would log a Svelte warning on every byte.
  let term: Terminal | null = null;
  let fit: FitAddon | null = null;
  let socket: WebSocket | null = null;
  let resizeObs: ResizeObserver | null = null;

  function wsUrl(id: string): string {
    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    return `${proto}//${window.location.host}/api/sessions/${encodeURIComponent(id)}/term`;
  }

  function sendResize(cols: number, rows: number): void {
    if (!socket || socket.readyState !== WebSocket.OPEN) return;
    socket.send(JSON.stringify({ resize: { cols, rows } }));
  }

  onMount(() => {
    if (!host) return;
    const t = new Terminal({
      cursorBlink: true,
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
      fontSize: 13,
      theme: { background: '#0b0b0d', foreground: '#e6e6e6' },
      convertEol: true,
    });
    const f = new FitAddon();
    t.loadAddon(f);
    t.open(host);
    f.fit();
    term = t;
    fit = f;

    // Refit on container resize so the dock open/close + window resize keep the
    // pty viewport matching what xterm displays.
    resizeObs = new ResizeObserver(() => {
      try {
        f.fit();
        sendResize(t.cols, t.rows);
      } catch {
        // fit() throws when the host has zero size (dock collapsed mid-tick).
      }
    });
    resizeObs.observe(host);

    const ws = new WebSocket(wsUrl(sessionId));
    ws.binaryType = 'arraybuffer';
    socket = ws;

    ws.onopen = () => {
      status = 'open';
      sendResize(t.cols, t.rows);
    };
    ws.onmessage = (msg) => {
      if (msg.data instanceof ArrayBuffer) {
        t.write(new Uint8Array(msg.data));
      } else if (typeof msg.data === 'string') {
        // Server only emits binary today; keep a path for control text just in case.
        t.write(msg.data);
      }
    };
    ws.onerror = () => {
      status = 'error';
      lastError = 'websocket error';
    };
    ws.onclose = (ev) => {
      status = 'closed';
      if (ev.code && ev.code !== 1000 && ev.code !== 1005) {
        lastError = `closed (${ev.code}${ev.reason ? `: ${ev.reason}` : ''})`;
      }
    };

    t.onData((data) => {
      if (socket && socket.readyState === WebSocket.OPEN) {
        socket.send(new TextEncoder().encode(data));
      }
    });
  });

  onDestroy(() => {
    resizeObs?.disconnect();
    resizeObs = null;
    if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) {
      socket.close(1000, 'tab closed');
    }
    socket = null;
    term?.dispose();
    term = null;
    fit = null;
  });
</script>

<div class="flex h-full min-h-0 flex-col">
  <div
    class="flex shrink-0 items-center gap-2 border-b px-2 py-1 font-mono text-[10px]"
    style="border-color: var(--border-soft); color: var(--ink-faint); background: var(--paper-2)"
  >
    <span style="color: var(--ink-soft)">{sessionId}</span>
    <span>·</span>
    <span
      class="rounded px-1"
      style="background: var(--paper); color: {status === 'open'
        ? 'var(--accent)'
        : status === 'error'
          ? 'var(--danger)'
          : 'var(--ink-faint)'}"
    >
      {status}
    </span>
    {#if lastError}
      <span style="color: var(--ink-faint)">· {lastError}</span>
    {/if}
  </div>
  <div bind:this={host} class="min-h-0 flex-1" style="background: #0b0b0d"></div>
</div>
