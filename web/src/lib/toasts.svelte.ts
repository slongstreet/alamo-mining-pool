/** In-page notices: a short, bounded stack that hides itself after a while. */
import type { AlertTone } from './alerts';

export interface Toast {
  id: number;
  title: string;
  body: string;
  tone: AlertTone;
}

/** Notices shown at once; the oldest goes when another arrives. */
const MAX_TOASTS = 6;
export const TOAST_MS = 60_000;

export class ToastQueue {
  items = $state<Toast[]>([]);

  #next = 1;
  #timers = new Map<number, ReturnType<typeof setTimeout>>();

  push(toast: Omit<Toast, 'id'>, ms = TOAST_MS) {
    const id = this.#next++;
    const items = [...this.items, { ...toast, id }];
    for (const old of items.splice(0, Math.max(0, items.length - MAX_TOASTS))) this.#clearTimer(old.id);
    this.items = items;
    this.#timers.set(
      id,
      setTimeout(() => this.dismiss(id), ms),
    );
  }

  dismiss(id: number) {
    this.#clearTimer(id);
    if (this.items.some((t) => t.id === id)) this.items = this.items.filter((t) => t.id !== id);
  }

  clear() {
    for (const id of this.#timers.keys()) this.#clearTimer(id);
    this.items = [];
  }

  #clearTimer(id: number) {
    const timer = this.#timers.get(id);
    if (timer) clearTimeout(timer);
    this.#timers.delete(id);
  }
}
