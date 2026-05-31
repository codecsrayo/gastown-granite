// Wire shapes for `POST /api/nudge` (hq-fe-api-w.1). Mirrors
// `gt_web::dto::{NudgeRequest, NudgeResponse}`. A nudge is a write-side command
// that maps to one `AgentEvent::Heartbeat` on the agent relay; the reactor records
// it and SSE subscribers see it as `agent.heartbeat`. The dashboard uses this when
// the operator manually pings a session row to assert liveness.

export interface NudgeRequest {
  session: string;
}

export interface NudgeResponse {
  accepted: boolean;
}
