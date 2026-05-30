// Wire shape of `GET /api/mayor/status` (hq-fe-api-r.7). Mirrors
// `gt_web::dto::MayorStatusDto` 1:1. `attached` is the only field that's
// always present; the rest are surfaced only when the registry currently
// holds a mayor row. Heartbeat freshness is deferred — the field is not on
// the wire today but the backend dto note flags where it would be added.
export interface MayorStatus {
  attached: boolean;
  session_id: string | null;
  rig: string | null;
  state: string | null;
}
