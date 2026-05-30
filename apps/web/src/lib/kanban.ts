// Kanban state machine. Mirrors `gt_web::routes::is_operator_transition_allowed`
// (hq-fe-api-w.4) so the frontend can pre-validate a drag-drop and short-circuit
// disallowed moves with an optimistic revert message instead of waiting for a 400.

export type BeadStatus = 'pending' | 'dispatched' | 'working' | 'done' | 'failed';

export const KANBAN_COLUMNS: readonly BeadStatus[] = [
  'pending',
  'dispatched',
  'working',
  'done',
  'failed'
];

const ALLOWED: Record<BeadStatus, readonly BeadStatus[]> = {
  pending: ['working', 'done', 'failed'],
  dispatched: ['pending', 'failed'],
  working: ['pending', 'done', 'failed'],
  done: ['pending'],
  failed: ['pending']
};

export function isTransitionAllowed(from: BeadStatus, to: BeadStatus): boolean {
  if (from === to) return false;
  return ALLOWED[from].includes(to);
}

export function isBeadStatus(s: string): s is BeadStatus {
  return (KANBAN_COLUMNS as readonly string[]).includes(s);
}
