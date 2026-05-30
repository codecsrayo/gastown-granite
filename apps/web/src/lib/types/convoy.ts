// Wire shape of `GET /api/convoys` (hq-fe-api-r.3). Mirrors `gt_web::dto::ConvoyDto` 1:1.
// `state` is the canonical lifecycle string (`staged|launched|closed|failed`) from
// `gt_orchestration::state::ConvoyState`; `member.state` is the per-slot string
// (`pending|active|done|failed`). Members are ordered the way the actor stores them, so
// the dashboard preserves convoy ordering.
export interface ConvoyMember {
  bead: string;
  state: string;
}

export interface Convoy {
  id: string;
  state: string;
  members: ConvoyMember[];
}
