import { describe, expect, it, beforeEach } from 'vitest';
import { readBearer, writeBearer, clearBearer } from './bearer';

function getCookie(name: string): string | null {
  for (const pair of document.cookie.split(';')) {
    const [k, v] = pair.trim().split('=');
    if (k === name) return decodeURIComponent(v ?? '');
  }
  return null;
}

describe('bearer helper', () => {
  beforeEach(() => {
    localStorage.clear();
    // jsdom keeps cookies between tests; clear the one we set.
    document.cookie = 'gt_web_token=; path=/; max-age=0; SameSite=Strict';
  });

  it('round-trips a token', () => {
    expect(readBearer()).toBeNull();
    writeBearer('abc.def.ghi');
    expect(readBearer()).toBe('abc.def.ghi');
  });

  it('clearBearer removes the key', () => {
    writeBearer('t');
    clearBearer();
    expect(readBearer()).toBeNull();
  });

  it('overwrites on second write', () => {
    writeBearer('one');
    writeBearer('two');
    expect(readBearer()).toBe('two');
  });

  // hq-fe-rbac.6 — cookie mirror so browser WS / SSE can authenticate against
  // gt-web's auth_middleware cookie fallback (those constructors cannot set
  // the Authorization header).
  it('writeBearer mirrors token to gt_web_token cookie', () => {
    writeBearer('jwt.token.value');
    expect(getCookie('gt_web_token')).toBe('jwt.token.value');
  });

  it('clearBearer clears gt_web_token cookie', () => {
    writeBearer('to-be-cleared');
    expect(getCookie('gt_web_token')).toBe('to-be-cleared');
    clearBearer();
    expect(getCookie('gt_web_token')).toBeNull();
  });

  it('overwrites cookie on second write', () => {
    writeBearer('one');
    writeBearer('two');
    expect(getCookie('gt_web_token')).toBe('two');
  });
});
