// Wire shape for `GET /api/sessions`. Mirrors `gt_web::dto::SessionDto` 1:1. The reader is
// the lifecycle port (`gt_agent::SessionQueries`); states map to:
//   spawned → tmux/polecat alive, no work
//   working → claimed a bead, agent active
//   done    → finished cleanly
//   killed  → terminated externally (gt-polecat kill / patrol expiry)
// `role` is the flat string (polecat / sheriff / deacon / refinery / witness / mayor);
// `crew` is the role/skill set running inside a polecat (claude-host, claude-host-onboard…).
export interface Session {
  id: string;
  rig: string;
  state: string;
  role: string;
  crew: string | null;
}
