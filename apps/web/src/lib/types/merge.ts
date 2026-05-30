// Wire shape of `GET /api/merges` (hq-fe-api-r.4). Mirrors `gt_web::dto::MergeSlotDto`
// 1:1. `state` is the canonical lifecycle string the SSE stream already emits
// (`merge.*` records use the same vocabulary), so the dashboard can apply patches
// from `/api/stream` without re-shaping the wire snapshot.
export type MergeSlotState = 'ready' | 'merging' | 'merged' | 'failed';

export interface MergeSlot {
  bead: string;
  branch: string;
  state: string;
}
