// Wire shapes for `GET /api/worktrees`. Mirrors `gt_web::dto::WorktreeDto` /
// `gt_web::dto::DirtyFileDto` 1:1 — keep field names in lockstep with the Rust source so the
// dashboard contract is the same string on both sides (docs/07-frontend.md).
//
// xy: two-char porcelain v2 code: index slot + worktree slot. "??" = untracked,
// ".M" = unstaged modify, "M." = staged modify, "A." = staged add, "UU" = unmerged.
export interface DirtyFile {
  path: string;
  xy: string;
}

export interface Worktree {
  path: string;
  branch: string | null;
  head: string;
  is_main: boolean;
  ahead: number;
  behind: number;
  dirty: DirtyFile[];
  // hq-fe-api-r.10: HEAD commit context — surfaced under the branch line in the panel so the
  // operator can read "what was last done here" without opening the repo. Both fields are
  // populated from the same `git log -1 --format=%s%n%an` call; null when HEAD is unreadable.
  head_subject: string | null;
  head_author: string | null;
}
