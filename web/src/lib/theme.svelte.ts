/** Dark / light / system theme, remembered per browser. */

export type Theme = 'system' | 'dark' | 'light';
const KEY = 'alamo.theme';

function load(): Theme {
  try {
    const v = localStorage.getItem(KEY);
    if (v === 'dark' || v === 'light') return v;
  } catch {
    /* storage may be unavailable */
  }
  return 'system';
}

class ThemeStore {
  current = $state<Theme>(load());

  constructor() {
    this.apply();
  }

  set(theme: Theme) {
    this.current = theme;
    try {
      if (theme === 'system') localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, theme);
    } catch {
      /* ignore */
    }
    this.apply();
  }

  cycle() {
    const order: Theme[] = ['system', 'dark', 'light'];
    this.set(order[(order.indexOf(this.current) + 1) % order.length]);
  }

  private apply() {
    const root = document.documentElement;
    if (this.current === 'system') root.removeAttribute('data-theme');
    else root.setAttribute('data-theme', this.current);
  }
}

export const theme = new ThemeStore();
