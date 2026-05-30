import { describe, expect, it } from 'vitest';
import { createDangerMachine, DEFAULT_ARM_MS } from './danger-button';

describe('createDangerMachine', () => {
  it('starts idle', () => {
    const m = createDangerMachine();
    expect(m.state).toBe('idle');
    expect(m.armedAt).toBeNull();
  });

  it('arm() moves idle → armed and records timestamp', () => {
    const m = createDangerMachine();
    m.arm(1_000);
    expect(m.state).toBe('armed');
    expect(m.armedAt).toBe(1_000);
  });

  it('fire() from armed → firing; from idle is a no-op', () => {
    const m = createDangerMachine();
    expect(m.fire()).toBe('idle');
    m.arm(0);
    expect(m.fire()).toBe('firing');
  });

  it('settle() returns to idle after firing', () => {
    const m = createDangerMachine();
    m.arm(0);
    m.fire();
    expect(m.settle()).toBe('idle');
    expect(m.armedAt).toBeNull();
  });

  it('expireIfStale auto-disarms after the window', () => {
    const m = createDangerMachine();
    m.arm(0);
    expect(m.expireIfStale(DEFAULT_ARM_MS - 1)).toBe('armed');
    expect(m.expireIfStale(DEFAULT_ARM_MS)).toBe('idle');
    expect(m.armedAt).toBeNull();
  });

  it('expireIfStale is a no-op while firing', () => {
    const m = createDangerMachine();
    m.arm(0);
    m.fire();
    expect(m.expireIfStale(10_000)).toBe('firing');
  });

  it('cancel() returns to idle from any state', () => {
    const m = createDangerMachine();
    m.arm(0);
    expect(m.cancel()).toBe('idle');
    expect(m.armedAt).toBeNull();
  });
});
