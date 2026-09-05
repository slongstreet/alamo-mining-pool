<script lang="ts">
  import type { Status } from '../api';
  import type { Connection } from '../live.svelte';
  import type { ThemePreference } from '../theme.svelte';
  import { formatDuration } from '../format';

  let {
    status,
    connection,
    error,
    theme,
  }: { status: Status | null; connection: Connection; error: string | null; theme: ThemePreference } =
    $props();

  const labels: Record<Connection, string> = {
    connecting: 'connecting',
    live: 'live',
    polling: 'polling',
    offline: 'offline',
  };
  const themeLabel = $derived(
    theme.theme === 'system' ? 'Auto theme' : theme.theme === 'dark' ? 'Dark theme' : 'Light theme',
  );
</script>

<header>
  <div>
    <h1>{status?.pool_name ?? 'Alamo'}</h1>
    <p class="muted">
      {#if status}
        v{status.version} · up {formatDuration(status.uptime_seconds)}
        {#if status.coins.length}
          · {status.coins.map((c) => `${c.symbol} ${c.chain}`).join(' + ')}
        {/if}
      {:else if error}
        {error}
      {:else}
        connecting…
      {/if}
    </p>
  </div>
  <div class="controls">
    <span class="conn {connection}" title={error ?? ''}>
      <i></i>
      {labels[connection]}
    </span>
    <button class="ghost" onclick={() => theme.cycle()} title="Cycle theme">{themeLabel}</button>
  </div>
</header>

<style>
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 1rem;
    flex-wrap: wrap;
    margin-bottom: 1.25rem;
  }
  h1 {
    margin: 0;
    font-size: 1.8rem;
    letter-spacing: -0.02em;
  }
  p {
    margin: 0.1rem 0 0;
    font-size: 0.9rem;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding-top: 0.4rem;
  }
  .conn {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.8rem;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .conn i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }
  .conn.live i {
    background: var(--ok);
    box-shadow: 0 0 0 3px rgb(61 220 151 / 20%);
  }
  .conn.polling i {
    background: var(--warn);
  }
  .conn.offline i {
    background: var(--bad);
  }
</style>
