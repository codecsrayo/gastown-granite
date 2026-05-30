// Map a `claim/<bead-id-dashed>` git branch back to the original bead id (hq-fe-view.14
// cross-link). Convention: worktrees off main use `claim/<bead-id>` for the branch ref, and
// git refs can't contain `.`, so the bead id is dash-encoded. The trailing `-N` is the .N
// child suffix; everything before is the parent epic id (which itself may carry dashes,
// e.g. `hq-fe-api-r`). Returns null when the branch doesn't follow the convention so the
// caller can render the raw branch without inventing an id.

export function beadIdFromBranch(branch: string | null): string | null {
  if (!branch) return null;
  const rest = branch.startsWith('claim/') ? branch.slice('claim/'.length) : null;
  if (!rest) return null;
  // Split on the *last* dash only if the tail is purely digits — that's the `.N` slot.
  // Otherwise treat the whole tail as the bead id (epic-level claim, no child).
  const lastDash = rest.lastIndexOf('-');
  if (lastDash <= 0 || lastDash === rest.length - 1) return rest;
  const tail = rest.slice(lastDash + 1);
  if (!/^\d+$/.test(tail)) return rest;
  return `${rest.slice(0, lastDash)}.${tail}`;
}
