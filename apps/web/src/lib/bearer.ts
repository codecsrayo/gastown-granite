// Bearer-token persistence helper. Single source of truth so /login,
// lib/api/*, and ProfileMenu logout all touch the same key.
//
// Cookie mirror (hq-fe-rbac.6): the SPA mirrors the bearer into a same-origin
// `gt_web_token` cookie alongside localStorage. Browser WebSocket / EventSource
// cannot set an `Authorization` header but auto-send same-origin cookies on
// upgrade requests, so the dock terminal (and any future WS / SSE consumer)
// authenticates against the gt-web `auth_middleware` cookie fallback.
// `SameSite=Strict` blocks cross-site rides; `Secure` is added on https so the
// cookie never leaks over plain http in prod. `HttpOnly` is intentionally NOT
// set — the SPA still reads the token via `readBearer` for the `Authorization`
// header on regular `fetch` calls.

const KEY = 'gt-bearer';
const COOKIE_NAME = 'gt_web_token';
const COOKIE_MAX_AGE = 60 * 60 * 24 * 30; // 30 days — matches typical session length

function isBrowser(): boolean {
  return typeof document !== 'undefined';
}

function writeCookie(token: string): void {
  if (!isBrowser()) return;
  const secure = window.location.protocol === 'https:' ? '; Secure' : '';
  document.cookie = `${COOKIE_NAME}=${encodeURIComponent(token)}; path=/; max-age=${COOKIE_MAX_AGE}; SameSite=Strict${secure}`;
}

function clearCookie(): void {
  if (!isBrowser()) return;
  const secure = window.location.protocol === 'https:' ? '; Secure' : '';
  // Max-Age=0 deletes immediately; SameSite must match the write so older browsers
  // overwrite the right cookie jar entry.
  document.cookie = `${COOKIE_NAME}=; path=/; max-age=0; SameSite=Strict${secure}`;
}

export function readBearer(): string | null {
  if (typeof localStorage === 'undefined') return null;
  return localStorage.getItem(KEY);
}

export function writeBearer(token: string) {
  if (typeof localStorage === 'undefined') return;
  localStorage.setItem(KEY, token);
  writeCookie(token);
}

export function clearBearer() {
  if (typeof localStorage === 'undefined') return;
  localStorage.removeItem(KEY);
  clearCookie();
}
