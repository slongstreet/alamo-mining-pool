<script lang="ts">
  import { onMount } from 'svelte';
  import { LiveStatus } from './lib/live.svelte';
  import { ThemePreference } from './lib/theme.svelte';
  import Header from './lib/components/Header.svelte';
  import StatTiles from './lib/components/StatTiles.svelte';
  import OddsPanel from './lib/components/OddsPanel.svelte';
  import HashrateChart from './lib/components/HashrateChart.svelte';
  import WorkerTable from './lib/components/WorkerTable.svelte';
  import ShareLog from './lib/components/ShareLog.svelte';
  import BlockHistory from './lib/components/BlockHistory.svelte';

  const live = new LiveStatus();
  const theme = new ThemePreference();
  theme.apply();

  onMount(() => {
    live.start();
    return () => live.stop();
  });

  const status = $derived(live.status);
  const now = $derived(status?.now ?? Math.floor(Date.now() / 1000));
  const workerNames = $derived(status?.workers.map((w) => w.name) ?? []);
</script>

<main>
  <Header {status} connection={live.connection} error={live.error} {theme} />

  {#if status}
    <div class="grid">
      <StatTiles {status} />

      <div class="grid two">
        {#each status.coins as coin (coin.symbol)}
          <OddsPanel {coin} bestShare={status.best_share_difficulty} {now} />
        {/each}
        {#if status.coins.length === 0}
          <section class="panel">
            <h2>Block odds</h2>
            <div class="empty">Waiting for the first block template from the node.</div>
          </section>
        {/if}
      </div>

      <HashrateChart live={status.hashrate} workers={workerNames} />
      <WorkerTable workers={status.workers} />

      <div class="grid two">
        <ShareLog {now} />
        <BlockHistory blocks={status.blocks} coins={status.coins} {now} />
      </div>
    </div>
  {:else}
    <section class="panel">
      <div class="empty">
        {#if live.error}
          Cannot reach the daemon: {live.error}
        {:else}
          Connecting to the pool…
        {/if}
      </div>
    </section>
  {/if}
</main>
