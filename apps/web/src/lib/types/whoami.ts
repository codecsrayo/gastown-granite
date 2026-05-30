// Wire shape of `GET /api/whoami` (hq-fe-rbac.4). Mirrors
// `gt_web::dto::WhoamiDto` 1:1. `mode` is the frontier auth posture:
//   open   — dev fallback, every request passes (actor=`web:open`)
//   bearer — token enforced, actor=`web:<sha-prefix>`
// `roles`/`scopes` come back empty until hq-fe-rbac.{1,2,3} land. The fields are
// already on the wire so the auth store can hydrate against the final shape today.
export type WhoamiMode = 'open' | 'bearer';

export interface Whoami {
  actor: string;
  mode: string;
  roles: string[];
  scopes: string[];
}
