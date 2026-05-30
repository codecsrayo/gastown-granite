// Single shared `EventSource` against `/api/stream` with a kind-routed fan-out
// (hq-fe-build.3). The doc rule from `frontend-architecture.md` pins this as the canonical
// transport: one connection per browser tab, multiplexed in client; the alternative (one
// `EventSource` per consumer) burns sockets and breaks once we add bearer auth headers,
// which the standard `EventSource` constructor doesn't let us set.
//
// Subscribers register `(kind, handler)` pairs. `kind` may be the exact wire string
// (`'agent.spawned'`), a domain prefix with `.*` (`'agent.*'`), or `'*'` for all frames.
// Subscribe returns the unsubscribe fn, matching the Svelte 5 `$effect` teardown contract:
//
//     $effect(() => subscribe('agent.*', handler))
//
// Reconnect: the browser auto-reconnects on its own; we surface the `error` event so a UI
// can show a transient banner. `Last-Event-ID` rehydration is implicit — `EventSource`
// stamps it for free when the server emits an `id:` line per frame (gt-web's SSE bridge
// does, per `stream.rs::into_sse_stream`).

import type { EventRecord } from '$lib/types/event';

export type SseHandler = (record: EventRecord) => void;
export type SseStatus = 'connecting' | 'open' | 'closed' | 'error';
export type SseStatusHandler = (status: SseStatus) => void;

/** Pattern accepted by `subscribe(kind, ...)`. Either an exact match, a domain prefix
 *  (`'agent.*'`), or the wildcard `'*'` (every frame). */
export type KindPattern = string;

interface Sub {
  pattern: KindPattern;
  handler: SseHandler;
}

interface StatusSub {
  handler: SseStatusHandler;
}

function matches(pattern: KindPattern, kind: string): boolean {
  if (pattern === '*') return true;
  if (pattern.endsWith('.*')) return kind.startsWith(pattern.slice(0, -1));
  return pattern === kind;
}

class SseRouter {
  /** Endpoint path. Pinned to `/api/stream` for the canonical bus; overridable for tests. */
  path: string = '/api/stream';
  /** Factory hook so vitest can inject a fake `EventSource` without polyfilling jsdom. */
  factory: (url: string) => EventSource = (url) => new EventSource(url);

  private source: EventSource | null = null;
  private subs = new Set<Sub>();
  private statusSubs = new Set<StatusSub>();
  status: SseStatus = 'closed';

  /** Subscribe a handler. Returns the unsubscribe fn. Connects lazily on first sub. */
  subscribe(pattern: KindPattern, handler: SseHandler): () => void {
    const sub: Sub = { pattern, handler };
    this.subs.add(sub);
    this.ensureOpen();
    return () => {
      this.subs.delete(sub);
      if (this.subs.size === 0 && this.statusSubs.size === 0) {
        this.close();
      }
    };
  }

  /** Subscribe to connection-status changes. Fires the current status synchronously so the
   *  consumer doesn't have to mirror it themselves. */
  subscribeStatus(handler: SseStatusHandler): () => void {
    const sub: StatusSub = { handler };
    this.statusSubs.add(sub);
    handler(this.status);
    this.ensureOpen();
    return () => {
      this.statusSubs.delete(sub);
      if (this.subs.size === 0 && this.statusSubs.size === 0) {
        this.close();
      }
    };
  }

  /** Force-close + reset subscribers. Useful for tests and for the logout path. */
  reset(): void {
    this.close();
    this.subs.clear();
    this.statusSubs.clear();
  }

  private ensureOpen(): void {
    if (this.source) return;
    this.setStatus('connecting');
    const src = this.factory(this.path);
    this.source = src;
    src.onopen = () => this.setStatus('open');
    src.onerror = () => this.setStatus('error');
    src.onmessage = (msg) => this.dispatch(msg);
  }

  private close(): void {
    if (this.source) {
      this.source.close();
      this.source = null;
    }
    this.setStatus('closed');
  }

  private setStatus(next: SseStatus): void {
    if (this.status === next) return;
    this.status = next;
    for (const s of this.statusSubs) s.handler(next);
  }

  private dispatch(msg: MessageEvent): void {
    let rec: EventRecord;
    try {
      rec = JSON.parse(msg.data) as EventRecord;
    } catch {
      return;
    }
    for (const sub of this.subs) {
      if (matches(sub.pattern, rec.type)) {
        try {
          sub.handler(rec);
        } catch {
          // Subscriber errors must not poison the dispatch loop. The console.error keeps the
          // failure visible during dev without taking the connection down.
          console.error('sse handler threw for kind', rec.type);
        }
      }
    }
  }
}

/** Process-wide singleton. Tests reach into `sse.factory` + `sse.reset()` to inject fakes. */
export const sse = new SseRouter();

/** Convenience wrapper: subscribe + return unsubscribe. Equivalent to `sse.subscribe`. */
export function subscribe(pattern: KindPattern, handler: SseHandler): () => void {
  return sse.subscribe(pattern, handler);
}

/** Convenience wrapper for status. Equivalent to `sse.subscribeStatus`. */
export function subscribeStatus(handler: SseStatusHandler): () => void {
  return sse.subscribeStatus(handler);
}
