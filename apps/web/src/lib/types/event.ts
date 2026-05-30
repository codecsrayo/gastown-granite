// Wire shape of one `EventRecord` (mirrors `gt_audit::record::EventRecord`). Every record
// in `events.jsonl` and every frame on `/api/stream` ships this exact JSON — the doc rule
// from `apps/api/docs/07-frontend.md` ("shared `EventRecord`") means the browser and the
// log readers see byte-identical payloads.
//
// `kind` is the routing key (`agent.spawned`, `merge.complete`, `quota.rotated`, etc.); the
// router in `lib/sse.ts` fans frames out to subscribers keyed on this string. `payload` is
// type-erased on the wire, so consumers cast per-kind.
export interface EventRecord {
  event_id: string;
  correlation_id: string;
  causation_id: string | null;
  ts: string;
  /** Routing key. Serialized as `type` over the wire (`#[serde(rename = "type")]`). */
  type: string;
  payload: unknown;
}
