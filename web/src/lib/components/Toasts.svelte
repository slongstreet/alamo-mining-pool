<script lang="ts">
  import type { ToastQueue } from '../toasts.svelte';

  /**
   * The notice stack. The container is a permanent live region, so screen readers announce
   * each notice as it is added; Escape dismisses the notice that has focus.
   */
  let { queue }: { queue: ToastQueue } = $props();

  function onKey(e: KeyboardEvent) {
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    const el = (e.target as Element | null)?.closest?.('[data-toast]');
    if (!(el instanceof HTMLElement)) return;
    e.preventDefault();
    queue.dismiss(Number(el.dataset.toast));
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="toasts" role="status" aria-live="polite">
  {#each queue.items as t (t.id)}
    <div class="toast {t.tone}" data-toast={t.id}>
      <div class="text">
        <strong>{t.title}</strong>
        <span>{t.body}</span>
      </div>
      <button class="ghost close" onclick={() => queue.dismiss(t.id)} aria-label="Dismiss: {t.title}" title="Dismiss (Esc)"
        >×</button
      >
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 1rem;
    bottom: 1rem;
    z-index: 20;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    width: min(22rem, calc(100vw - 2rem));
    pointer-events: none;
  }
  .toast {
    pointer-events: auto;
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
    background: var(--panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-left: 4px solid var(--muted);
    border-radius: 10px;
    padding: 0.6rem 0.6rem 0.6rem 0.8rem;
    box-shadow: var(--shadow), 0 8px 24px rgb(0 0 0 / 25%);
    font-size: 0.88rem;
  }
  .toast.ok {
    border-left-color: var(--ok);
  }
  .toast.warn {
    border-left-color: var(--warn);
  }
  .toast.bad {
    border-left-color: var(--bad);
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow-wrap: anywhere;
  }
  .text span {
    color: var(--muted);
    font-size: 0.82rem;
  }
  .close {
    padding: 0 0.45rem;
    line-height: 1.4;
  }
</style>
