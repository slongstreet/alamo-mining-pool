<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Status } from './lib/api';
  import { formatDuration } from './lib/format';

  let status = $state<Status | null>(null);
  let error = $state<string | null>(null);

  async function refresh() {
    try {
      status = await api.status();
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(() => {
    refresh();
    const timer = setInterval(refresh, 5000);
    return () => clearInterval(timer);
  });
</script>

<main>
  <header>
    <h1>{status?.pool_name ?? 'Alamo'}</h1>
    <p class="muted">
      {#if status}
        v{status.version} · up {formatDuration(status.uptime_seconds)}
      {:else if error}
        {error}
      {:else}
        connecting…
      {/if}
    </p>
  </header>

  <section class="panel">
    <h2>Next block odds</h2>
    <p class="muted">The odds visualizer lands in Wave 4. This shell proves the dashboard builds and embeds.</p>
  </section>
</main>

<style>
  header {
    margin-bottom: 1.5rem;
  }
  h1 {
    margin: 0;
    font-size: 2rem;
    letter-spacing: -0.02em;
  }
  h2 {
    margin-top: 0;
    font-size: 1.1rem;
    color: var(--accent);
  }
</style>
