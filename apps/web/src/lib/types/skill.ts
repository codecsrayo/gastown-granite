// Wire shapes for the skills + roles surface (hq-fe-skills.{2,3}). Mirrors
// `gt_web::dto::{SkillDto, RoleSkillsDto, SkillToggleRequest, SkillToggleResponse}`
// 1:1. The dashboard hydrates the catalog with `GET /api/skills`, joins it against
// `GET /api/roles` for the SkillToggle grid, and posts toggles through
// `POST /api/roles/:role/skills`. Live deltas land on `/api/stream` as
// `EventRecord` kinds `skills.registered | skills.retired |
// skills.enabled_for_role | skills.disabled_for_role` (hq-fe-skills.5).

/** One catalog entry. `default_scopes` is the union the resolver hands out when the
 *  skill is enabled on a role; the dashboard renders them as scope chips. */
export interface Skill {
  id: string;
  label: string;
  description: string;
  default_scopes: string[];
}

/** Per-role enabled-skill list. `skills` is alphabetically sorted (the actor stores
 *  bindings in a `BTreeSet`) so the wire shape is deterministic. An empty `skills`
 *  array is a distinct state from "no binding row" — it means the role was bound
 *  and stripped, not that it was never touched. */
export interface RoleSkills {
  role: string;
  skills: string[];
}

/** Body of `POST /api/roles/:role/skills`. `enabled=true` enables the binding,
 *  `false` disables it. Idempotent: re-asserting the existing state succeeds with
 *  200 without emitting a `SkillEvent`. */
export interface SkillToggleRequest {
  skill: string;
  enabled: boolean;
}

/** Echo body of `POST /api/roles/:role/skills`. */
export interface SkillToggleResponse {
  role: string;
  skill: string;
  enabled: boolean;
}

/** SSE record kinds the skills actor emits on `/api/stream` (hq-fe-skills.5). The
 *  dashboard treats any of these as a signal to invalidate its skills + roles
 *  caches and refetch. */
export type SkillEventKind =
  | 'skills.registered'
  | 'skills.retired'
  | 'skills.enabled_for_role'
  | 'skills.disabled_for_role';
