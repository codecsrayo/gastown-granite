// Bearer-token persistence helper. Single source of truth so /login,
// lib/api/*, and ProfileMenu logout all touch the same key.

const KEY = 'gt-bearer';

export function readBearer(): string | null {
  if (typeof localStorage === 'undefined') return null;
  return localStorage.getItem(KEY);
}

export function writeBearer(token: string) {
  if (typeof localStorage === 'undefined') return;
  localStorage.setItem(KEY, token);
}

export function clearBearer() {
  if (typeof localStorage === 'undefined') return;
  localStorage.removeItem(KEY);
}
