import { describe, expect, it, beforeEach } from 'vitest';
import { auth } from './auth.svelte';

describe('auth store', () => {
  beforeEach(() => {
    auth.reset();
  });

  it('boots in dev mode with permissive checks', () => {
    expect(auth.mode).toBe('dev');
    expect(auth.hasScope('session.kill')).toBe(true);
    expect(auth.hasRole('sheriff')).toBe(true);
  });

  it('hydrate() switches to live mode and enforces scopes', () => {
    auth.hydrate({
      actor: 'alice',
      roles: ['operator'],
      scopes: ['session.read', 'bead.read']
    });
    expect(auth.mode).toBe('live');
    expect(auth.actor).toBe('alice');
    expect(auth.hasScope('session.read')).toBe(true);
    expect(auth.hasScope('session.kill')).toBe(false);
    expect(auth.hasRole('operator')).toBe(true);
    expect(auth.hasRole('admin')).toBe(false);
  });

  it('readOnly mode allows only *.read scopes', () => {
    auth.hydrate({
      actor: 'bob',
      roles: ['admin'],
      scopes: ['session.read', 'session.kill', 'bead.update'],
      readOnly: true
    });
    expect(auth.hasScope('session.read')).toBe(true);
    expect(auth.hasScope('session.kill')).toBe(false);
    expect(auth.hasScope('bead.update')).toBe(false);
  });

  it('setReadOnly toggles without losing scopes', () => {
    auth.hydrate({
      actor: 'bob',
      roles: ['admin'],
      scopes: ['session.kill']
    });
    expect(auth.hasScope('session.kill')).toBe(true);
    auth.setReadOnly(true);
    expect(auth.hasScope('session.kill')).toBe(false);
    auth.setReadOnly(false);
    expect(auth.hasScope('session.kill')).toBe(true);
  });

  it('reset() returns to dev-mode permissive defaults', () => {
    auth.hydrate({ actor: 'a', roles: [], scopes: [] });
    auth.reset();
    expect(auth.mode).toBe('dev');
    expect(auth.actor).toBeNull();
    expect(auth.hasScope('whatever')).toBe(true);
  });
});
