// Wire shape of `GET /api/beads?status=<status>`. Mirrors `gt_web::dto::BeadDto`.
// `status` here is the dispatcher-table value (pending|dispatched|working|done|failed)
// — distinct from `hq.issues.status` (open|working|closed) consumed by `lib/types/issue.ts`.
export interface Bead {
  id: string;
  title: string;
  status: string;
  priority: number;
  assignee: string | null;
}

// Write-side wire shapes (hq-fe-api-w.{3,4,5,11}). Mirrors `gt_web::dto`. The
// dashboard's kanban dispatches these via `lib/api/beads.ts`; the gateway is the
// only path that mints a row in `beads`, patches editable columns, drives
// operator transitions, or appends a free-text comment.

/** Body of `POST /api/beads` — mints a `pending` row. `priority` defaults to 2
 *  (P2) when omitted. `assignee` empty/absent leaves the bead unassigned. */
export interface BeadCreateRequest {
  id: string;
  title: string;
  priority?: number;
  assignee?: string | null;
}

/** Body of `POST /api/beads/bulk` — atomic create-N (hq-fe-api-w.11). Capped at
 *  100 items per request by the gateway; rate-limited per actor. */
export interface BulkBeadCreateRequest {
  beads: BeadCreateRequest[];
}

/** Response of `POST /api/beads/bulk`. Echoes every persisted row in request
 *  order so the kanban can append without a follow-up snapshot fetch. */
export interface BulkBeadCreateResponse {
  created: Bead[];
}

/** Body of `PATCH /api/beads/:id` — partial update. Every field optional;
 *  omitted = leave alone; `assignee: ""` clears to unassigned. Status changes
 *  go through `POST /api/beads/:id/transition`, not here. */
export interface BeadUpdateRequest {
  title?: string;
  priority?: number;
  assignee?: string | null;
}

/** Body of `POST /api/beads/:id/transition` — operator override. `to` must be
 *  one of the dispatcher status strings (pending|dispatched|working|done|failed);
 *  the gateway rejects illegal transitions (see `gt_web::routes::transition_bead`
 *  for the matrix). */
export interface BeadTransitionRequest {
  to: string;
}

/** Body of `POST /api/beads/:id/comments` — append-only operator note. `author`
 *  empty/absent records as `@anon`. Capped at 4096 chars. */
export interface BeadCommentRequest {
  body: string;
  author?: string | null;
}

/** Echo body of `POST /api/beads/:id/comments`. `appended` is the canonical
 *  fragment the gateway formatted (timestamp + author tag + body); the
 *  dashboard renders it inline without re-fetching the row. `ts` is the same
 *  RFC3339 stamp embedded in `appended`. */
export interface BeadCommentResponse {
  id: string;
  appended: string;
  ts: string;
}
