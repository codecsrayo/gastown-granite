// Auth store · runes singleton holding actor + roles + scopes + read-only flag.
//
// Real hydration lands with hq-fe-rbac.4 (`/api/whoami`); until then the store
// boots in `dev` mode where every scope check passes and read-only is off, so
// the dashboard can render full UI against an unauthenticated dev backend.
// Once whoami hydrates, dev=false and checks are honored.

type Mode = 'dev' | 'live';

class Auth {
  actor = $state<string | null>(null);
  roles = $state<readonly string[]>([]);
  scopes = $state<ReadonlySet<string>>(new Set());
  readOnly = $state<boolean>(false);
  mode = $state<Mode>('dev');

  hydrate(input: { actor: string; roles: string[]; scopes: string[]; readOnly?: boolean }) {
    this.actor = input.actor;
    this.roles = [...input.roles];
    this.scopes = new Set(input.scopes);
    this.readOnly = input.readOnly ?? false;
    this.mode = 'live';
  }

  reset() {
    this.actor = null;
    this.roles = [];
    this.scopes = new Set();
    this.readOnly = false;
    this.mode = 'dev';
  }

  setReadOnly(value: boolean) {
    this.readOnly = value;
  }

  hasScope(scope: string): boolean {
    if (this.mode === 'dev') return true;
    if (this.readOnly) return scope.endsWith('.read');
    return this.scopes.has(scope);
  }

  hasRole(role: string): boolean {
    if (this.mode === 'dev') return true;
    return this.roles.includes(role);
  }
}

export const auth = new Auth();
