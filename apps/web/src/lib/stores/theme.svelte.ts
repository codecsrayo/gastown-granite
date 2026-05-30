// Theme store · dark canonical (mirrors apps/web/docs/pagina.png).
// Toggles [data-theme] on <html>; persists in localStorage.

type Mode = 'dark' | 'light';
const KEY = 'gt-theme';

function readInitial(): Mode {
  if (typeof localStorage === 'undefined') return 'dark';
  const raw = localStorage.getItem(KEY);
  return raw === 'light' ? 'light' : 'dark';
}

function applyToDom(mode: Mode) {
  if (typeof document === 'undefined') return;
  document.documentElement.setAttribute('data-theme', mode);
}

class Theme {
  current = $state<Mode>(readInitial());

  hydrate() {
    applyToDom(this.current);
  }

  set(mode: Mode) {
    this.current = mode;
    if (typeof localStorage !== 'undefined') localStorage.setItem(KEY, mode);
    applyToDom(mode);
  }

  toggle() {
    this.set(this.current === 'dark' ? 'light' : 'dark');
  }
}

export const theme = new Theme();
