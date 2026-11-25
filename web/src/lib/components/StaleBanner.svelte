<script lang="ts">
  import { formatDuration } from '../format';

  /**
   * Shown while the dashboard is not receiving updates. Fixed over the page, so it does
   * not push the panels around as it comes and goes.
   */
  let { seconds, error }: { seconds: number | null; error: string | null } = $props();
</script>

<!-- Only the heading is a live region: the counter changes every second and should not be
     read out each time. -->
<div class="stale">
  <strong role="alert">Not updating</strong>
  {#if seconds != null}— last update {formatDuration(seconds)} ago{/if}
  {#if error}<span class="err">· {error}</span>{/if}
</div>

<style>
  .stale {
    position: fixed;
    top: 0.75rem;
    left: 50%;
    transform: translateX(-50%);
    z-index: 20;
    max-width: calc(100vw - 2rem);
    background: var(--panel);
    color: var(--text);
    border: 1px solid var(--bad);
    border-radius: 999px;
    padding: 0.35rem 1rem;
    font-size: 0.88rem;
    font-variant-numeric: tabular-nums;
    box-shadow: var(--shadow), 0 6px 18px rgb(0 0 0 / 25%);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  strong {
    color: var(--bad);
  }
  .err {
    color: var(--muted);
  }
</style>
