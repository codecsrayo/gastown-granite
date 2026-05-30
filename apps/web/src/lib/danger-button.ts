// State machine for DangerButton's 1-step armable interaction. Extracted
// from the .svelte component so the timing logic is unit-testable in pure
// TypeScript without a DOM.
//
// Lifecycle:
//   idle  ── arm() ──▶ armed (window=ARM_MS) ── fire() ──▶ firing
//   armed ── timeout ──▶ idle (auto-disarm so a stale arm can't be
//                             accidentally fired after the user walked away)
//   firing ── settle(ok|err) ──▶ idle (ready to re-arm)

export type DangerState = 'idle' | 'armed' | 'firing';

export const DEFAULT_ARM_MS = 3000;

export interface DangerMachine {
  state: DangerState;
  armedAt: number | null;
  // Returns the new state plus the timer id to clear on next transition.
  arm(now: number): DangerState;
  expireIfStale(now: number, windowMs?: number): DangerState;
  fire(): DangerState;
  settle(): DangerState;
  cancel(): DangerState;
}

export function createDangerMachine(): DangerMachine {
  let state: DangerState = 'idle';
  let armedAt: number | null = null;

  return {
    get state() {
      return state;
    },
    get armedAt() {
      return armedAt;
    },
    arm(now) {
      state = 'armed';
      armedAt = now;
      return state;
    },
    expireIfStale(now, windowMs = DEFAULT_ARM_MS) {
      if (state === 'armed' && armedAt !== null && now - armedAt >= windowMs) {
        state = 'idle';
        armedAt = null;
      }
      return state;
    },
    fire() {
      if (state !== 'armed') return state;
      state = 'firing';
      armedAt = null;
      return state;
    },
    settle() {
      state = 'idle';
      armedAt = null;
      return state;
    },
    cancel() {
      state = 'idle';
      armedAt = null;
      return state;
    }
  };
}
