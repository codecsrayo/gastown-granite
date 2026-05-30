import { describe, expect, it, beforeEach } from 'vitest';
import { readBearer, writeBearer, clearBearer } from './bearer';

describe('bearer helper', () => {
  beforeEach(() => {
    localStorage.clear();
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
});
