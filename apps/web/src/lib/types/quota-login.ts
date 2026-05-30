// Wire shapes for `quota.login_*` SSE kinds (hq-fe-auth.3). Mirrors
// `gt-web::dto::QuotaLogin{Started,UrlReady,Complete,Failed}` exactly — each variant
// is the JSON value of `EventRecord.payload` when `EventRecord.type` is the matching
// `quota.login_*` kind. The kind itself never appears inside the payload (no
// redundant discriminator).
//
// SSE routing: subscribe to `EventRecord`s on `/api/stream` and switch on
// `record.type === 'quota.login_*'` to narrow to the payload type below. `flow_id`
// demuxes concurrent flows; the UI keeps state keyed by `(account, flow_id)`.

/** Typed wire shape of `LoginFailure` (gt_login::events::LoginFailure). The CLI driver
 *  surfaces exactly these variants; each maps to a distinct rollback path in the UI. */
export type LoginFailure =
  | { kind: 'spawn'; message: string }
  | { kind: 'url_missing' }
  | { kind: 'token_rejected'; status: number }
  | { kind: 'cancelled' }
  | { kind: 'io'; message: string };

/** `EventRecord.payload` for `EventRecord.type === 'quota.login_started'`. */
export interface QuotaLoginStarted {
  account: string;
  flow_id: string;
}

/** `EventRecord.payload` for `EventRecord.type === 'quota.login_url_ready'`. The UI
 *  surfaces the URL so the operator can paste it into a browser. */
export interface QuotaLoginUrlReady {
  account: string;
  flow_id: string;
  url: string;
}

/** `EventRecord.payload` for `EventRecord.type === 'quota.login_complete'`. */
export interface QuotaLoginComplete {
  account: string;
  flow_id: string;
}

/** `EventRecord.payload` for `EventRecord.type === 'quota.login_failed'`. `reason`
 *  is the typed `LoginFailure`; `message` is its `Display` (`thiserror`) fallback so
 *  the UI can render without typing the union. */
export interface QuotaLoginFailed {
  account: string;
  flow_id: string;
  reason: LoginFailure;
  message: string;
}

/** Discriminated union over all four `quota.login_*` payloads. Use this when you
 *  fan SSE frames out by kind in a single switch. */
export type QuotaLoginEventPayload =
  | { type: 'quota.login_started'; payload: QuotaLoginStarted }
  | { type: 'quota.login_url_ready'; payload: QuotaLoginUrlReady }
  | { type: 'quota.login_complete'; payload: QuotaLoginComplete }
  | { type: 'quota.login_failed'; payload: QuotaLoginFailed };
