// @vitest-environment node

import { describe, expect, it } from 'vitest';
import { CATEGORIES, categoryOf } from './event-category';

describe('categoryOf', () => {
  it('routes agent.* to agent', () => {
    expect(categoryOf('agent.spawned')).toBe('agent');
    expect(categoryOf('agent.killed')).toBe('agent');
    expect(categoryOf('agent.heartbeat')).toBe('agent');
  });

  it('routes the bead/convoy lifecycle prefixes to work', () => {
    expect(categoryOf('merge.complete')).toBe('work');
    expect(categoryOf('patrol.lease_expired')).toBe('work');
    expect(categoryOf('orch.member_dispatched')).toBe('work');
    expect(categoryOf('scheduling.dispatched')).toBe('work');
  });

  it('routes quota.* to quota', () => {
    expect(categoryOf('quota.tokens_sampled')).toBe('quota');
    expect(categoryOf('quota.rotated')).toBe('quota');
    expect(categoryOf('quota.account_limited')).toBe('quota');
  });

  it('routes web.* + mcp.* (frontier audit) to audit', () => {
    expect(categoryOf('web.invoked')).toBe('audit');
    expect(categoryOf('web.unauthorized')).toBe('audit');
    expect(categoryOf('mcp.invoked')).toBe('audit');
  });

  it('falls through to system for any unknown prefix', () => {
    expect(categoryOf('rig.added')).toBe('system');
    expect(categoryOf('platform.feed')).toBe('system');
    expect(categoryOf('something.new')).toBe('system');
  });

  it('CATEGORIES holds every category emitted by categoryOf', () => {
    expect(new Set(CATEGORIES)).toEqual(new Set(['agent', 'work', 'quota', 'system', 'audit']));
  });
});
