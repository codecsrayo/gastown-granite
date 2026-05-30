// Wire shape of `GET /api/issues`. Mirrors `gt_web::dto::IssueDto`. `status` is from
// `hq.issues` (open|working|closed) — distinct from the dispatcher-table `beads` status
// (pending|dispatched|done|closed). The /worktrees cross-link reads from this resource
// because the agent's claim branch maps to a row in hq.issues, not the dispatcher scratch.
export interface Issue {
  id: string;
  title: string;
  status: string;
  priority: number;
  issue_type: string;
  assignee: string | null;
  owner: string | null;
  external_ref: string | null;
  created_at: string | null;
  updated_at: string | null;
  closed_at: string | null;
}
