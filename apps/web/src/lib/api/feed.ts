// Thin client for `GET /api/feed?since=<rfc3339>&limit=<n>` (hq-fe-api-r.5).
// Historical replay of the same `events.jsonl` the SSE `/api/stream` ships, used
// by the Activity view to seed the in-memory ring buffer before subscribing to live
// frames so the first paint already carries the recent backlog.

import type { EventRecord } from '$lib/types/event';
import { apiGet, type ApiRequestOpts } from './client';

export interface FetchFeedOpts extends Omit<ApiRequestOpts, 'method' | 'body'> {
  /** RFC3339 timestamp; only records strictly newer than this are returned. */
  since?: string;
  /** Tail cap (server default 500, max 2000). */
  limit?: number;
}

export function fetchFeed(opts: FetchFeedOpts = {}): Promise<EventRecord[]> {
  const { since, limit, ...rest } = opts;
  const qs = new URLSearchParams();
  if (since) qs.set('since', since);
  if (limit !== undefined) qs.set('limit', String(limit));
  const suffix = qs.toString();
  const url = suffix ? `/api/feed?${suffix}` : '/api/feed';
  return apiGet<EventRecord[]>(url, rest);
}
