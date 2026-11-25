/**
 * Desktop notifications for found blocks, as an opt-in. Browsers only allow them in a
 * secure context (HTTPS or localhost), so on a plain-HTTP install, like Umbrel's, this is
 * unsupported and the dashboard offers no control for it. Permission is only ever asked
 * for from a click.
 */
const KEY = 'alamo.notify.desktop';

function readOptIn(): boolean {
  try {
    return localStorage.getItem(KEY) === '1';
  } catch {
    return false;
  }
}

function writeOptIn(on: boolean) {
  try {
    if (on) localStorage.setItem(KEY, '1');
    else localStorage.removeItem(KEY);
  } catch {
    // Not remembered past this page load.
  }
}

export class DesktopNotifications {
  readonly supported: boolean =
    typeof window !== 'undefined' && window.isSecureContext && 'Notification' in window;
  permission = $state<NotificationPermission>('default');
  optedIn = $state(false);

  constructor() {
    if (this.supported) {
      this.permission = Notification.permission;
      this.optedIn = readOptIn();
    }
  }

  /** Opted in and allowed by the browser. */
  get active(): boolean {
    return this.supported && this.optedIn && this.permission === 'granted';
  }

  /** Permission can be changed in the browser's site settings while the page is open. */
  refresh() {
    if (this.supported) this.permission = Notification.permission;
  }

  /** From a click only: turns notifications off, or on (asking the browser if it has not been asked). */
  async toggle() {
    if (!this.supported) return;
    if (this.active) {
      this.optedIn = false;
      writeOptIn(false);
      return;
    }
    let permission = Notification.permission;
    if (permission === 'default') {
      try {
        permission = await Notification.requestPermission();
      } catch {
        permission = Notification.permission;
      }
    }
    this.permission = permission;
    this.optedIn = permission === 'granted';
    writeOptIn(this.optedIn);
  }

  notify(title: string, body: string, tag: string) {
    this.refresh();
    if (!this.active) return;
    try {
      new Notification(title, { body, tag, icon: '/favicon.svg' });
    } catch {
      // Some browsers (Android Chrome) only allow notifications from a service worker.
    }
  }
}
