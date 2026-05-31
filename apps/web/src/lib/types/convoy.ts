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

// Write-side wire shapes (hq-fe-api-w.9). Mirrors `gt_web::dto`. `POST /api/convoys`
// creates + launches a convoy; the per-member fail route halts a stuck convoy with
// an operator-supplied reason. pause/resume are not exposed — the orchestration
// domain has no Pause/Resume commands yet.

/** Body of `POST /api/convoys`. Members are dispatched in order; the orchestrator
 *  launches the first member atomically as part of the create call. */
export interface ConvoyCreateRequest {
  convoy: string;
  members: string[];
}

/** Echo body of `POST /api/convoys`. `launched: true` is a fixed marker — a
 *  successful response always means the convoy is live and the first member
 *  already dispatched. */
export interface ConvoyCreateResponse {
  convoy: string;
  members: string[];
  launched: boolean;
}

/** Body of `POST /api/convoys/:convoy/members/:member/fail`. `reason` is required
 *  so the audit feed always carries operator context for the halt. */
export interface ConvoyMemberFailRequest {
  reason: string;
}
