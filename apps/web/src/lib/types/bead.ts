// Wire shape of `GET /api/beads?status=<status>`. Mirrors `gt_web::dto::BeadDto`.
// `status` here is the dispatcher-table value (pending|dispatched|working|done|failed)
// — distinct from `hq.issues.status` (open|working|closed) consumed by `lib/types/issue.ts`.
export interface Bead {
  id: string;
  title: string;
  status: string;
  priority: number;
  assignee: string | null;
}
