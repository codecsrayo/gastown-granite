// @vitest-environment node

import { describe, expect, it } from 'vitest';
import { beadIdFromBranch } from './claim-branch';

describe('beadIdFromBranch', () => {
  it('returns null for non-claim branches', () => {
    expect(beadIdFromBranch('main')).toBeNull();
    expect(beadIdFromBranch('feat/foo')).toBeNull();
    expect(beadIdFromBranch(null)).toBeNull();
  });

  it('converts trailing -N into .N child suffix', () => {
    expect(beadIdFromBranch('claim/hq-fe-view-14')).toBe('hq-fe-view.14');
    expect(beadIdFromBranch('claim/hq-mcp-onboard-8')).toBe('hq-mcp-onboard.8');
    expect(beadIdFromBranch('claim/hq-fe-api-r-8')).toBe('hq-fe-api-r.8');
  });

  it('leaves epic-level claims (no trailing number) intact', () => {
    expect(beadIdFromBranch('claim/hq-fe-view')).toBe('hq-fe-view');
    expect(beadIdFromBranch('claim/hq-taxon')).toBe('hq-taxon');
  });

  it('handles non-numeric tail by returning the raw remainder', () => {
    expect(beadIdFromBranch('claim/hq-fe-view-foo')).toBe('hq-fe-view-foo');
  });
});
