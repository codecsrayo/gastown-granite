import { afterEach, describe, expect, it } from 'vitest';
import type { Issue } from '$lib/types/issue';
import { beads } from './beads.svelte';

function iss(id: string, status: string = 'working'): Issue {
  return {
    id,
    title: `bead ${id}`,
    status,
    priority: 1,
    issue_type: 'task',
    assignee: 'claude-host',
    owner: 'claude-host',
    external_ref: 'hq-fe-build',
    created_at: null,
    updated_at: null,
    closed_at: null,
  };
}

afterEach(() => beads.reset());

describe('beads store', () => {
  it('hydrate seeds rows + byId resolves', () => {
    beads.hydrate([iss('hq-fe-view.4'), iss('hq-fe-view.5')]);
    expect(beads.rows).toHaveLength(2);
    expect(beads.byId('hq-fe-view.5')?.status).toBe('working');
    expect(beads.byId('missing')).toBeUndefined();
  });

  it('patch mutates only the matching row', () => {
    beads.hydrate([iss('a'), iss('b')]);
    beads.patch('a', { status: 'closed' });
    expect(beads.byId('a')?.status).toBe('closed');
    expect(beads.byId('b')?.status).toBe('working');
  });

  it('replace overwrites + appends on miss', () => {
    beads.hydrate([iss('a')]);
    beads.replace({ ...iss('a'), title: 'updated' });
    expect(beads.byId('a')?.title).toBe('updated');
    beads.replace(iss('z'));
    expect(beads.rows.map((r) => r.id)).toEqual(['a', 'z']);
  });

  it('remove drops the matching row + leaves others alone', () => {
    beads.hydrate([iss('a'), iss('b'), iss('c')]);
    beads.remove('b');
    expect(beads.rows.map((r) => r.id)).toEqual(['a', 'c']);
  });
});
