/** Dark/light theme: follow the system by default, remember an explicit choice. */

export type Theme = 'system' | 'dark' | 'light';

const KEY = 'alamo.theme';
const ORDER: Theme[] = ['system', 'dark', 'light'];

function load(): Theme {
  try {
    const saved = localStorage.getItem(KEY);
    return saved === 'dark' || saved === 'light' ? saved : 'system';
  } catch {
    return 'system';
  }
}

export class ThemePreference {
  theme = $state<Theme>(load());

  apply() {
    const root = document.documentElement;
    if (this.theme === 'system') {
      root.removeAttribute('data-theme');
    } else {
      root.setAttribute('data-theme', this.theme);
    }
    try {
      if (this.theme === 'system') localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, this.theme);
    } catch {
      // Storage may be unavailable; the choice still applies for this page view.
    }
  }

  cycle() {
    this.theme = ORDER[(ORDER.indexOf(this.theme) + 1) % ORDER.length];
    this.apply();
  }
}
