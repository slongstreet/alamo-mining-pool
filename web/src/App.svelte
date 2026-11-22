<script lang="ts">
  import { onMount } from 'svelte';
  import { LiveStatus } from './lib/live.svelte';
  import { ThemePreference } from './lib/theme.svelte';
  import { parseHash, pickLane, rememberLane, rememberedLane } from './lib/lanes';
  import Header from './lib/components/Header.svelte';
  import LaneCards from './lib/components/LaneCards.svelte';
  import LaneNotice from './lib/components/LaneNotice.svelte';
  import StatTiles from './lib/components/StatTiles.svelte';
  import OddsPanel from './lib/components/OddsPanel.svelte';
  import HashrateChart from './lib/components/HashrateChart.svelte';
  import WorkerTable from './lib/components/WorkerTable.svelte';
  import ShareLog from './lib/components/ShareLog.svelte';
  import BlockHistory from './lib/components/BlockHistory.svelte';
  import SettingsPage from './lib/components/settings/SettingsPage.svelte';

  const live = new LiveStatus();
  const theme = new ThemePreference();
  theme.apply();

  // Pages and lanes are told apart by the hash, so the daemon serves one document for all
  // of them and back/forward move between lanes.
  let hash = $state(location.hash);
  const route = $derived(parseHash(hash));
  const page = $derived(route.page);

  onMount(() => {
    live.start();
    const onHash = () => (hash = location.hash);
    window.addEventListener('hashchange', onHash);
    return () => {
      live.stop();
      window.removeEventListener('hashchange', onHash);
    };
  });

  const status = $derived(live.status);
  const now = $derived(status?.now ?? Math.floor(Date.now() / 1000));
  const lanes = $derived(status?.lanes ?? []);
  const multiLane = $derived(lanes.length > 1);
  const lane = $derived(
    pickLane(lanes, route.page === 'dashboard' ? route.lane : null, rememberedLane()),
  );
  const workerNames = $derived(lane?.workers.map((w) => w.name) ?? []);

  // A lane named in the hash is the one to come back to.
  $effect(() => {
    if (route.page === 'dashboard' && route.lane && lane?.key === route.lane) rememberLane(lane.key);
  });

  function select(key: string) {
    location.hash = `#/${encodeURIComponent(key)}`;
  }
</script>

<main>
  <Header {status} connection={live.connection} error={live.error} {theme} {page} />

  {#if page === 'settings'}
    <SettingsPage {status} />
  {:else if status && lane}
    <div class="grid">
      {#if multiLane}
        <LaneCards {lanes} selected={lane.key} onselect={select} />
      {/if}
      {#if lane.state === 'failed'}
        <LaneNotice {lane} />
      {/if}

      <StatTiles {lane} {now} {multiLane} />

      {#if lane.state === 'connecting'}
        <LaneNotice {lane} />
      {:else if lane.state === 'running'}
        <div class="grid two">
          {#each lane.coins as coin (coin.symbol)}
            <OddsPanel {coin} bestShare={lane.best_share_difficulty} {now} scoringSince={lane.scoring_since} />
          {/each}
          {#if lane.coins.length === 0}
            <section class="panel">
              <h2>Block odds</h2>
              <div class="empty">Waiting for the first block template from the node.</div>
            </section>
          {/if}
        </div>
      {/if}

      <HashrateChart
        live={lane.hashrate}
        workers={workerNames}
        lane={lane.key}
        allLabel={multiLane ? 'Lane' : 'Pool'}
      />
      <WorkerTable {lane} />

      <div class="grid two">
        <ShareLog {now} lane={lane.key} shareMultiplier={lane.share_multiplier} />
        <BlockHistory blocks={status.blocks} {lanes} {now} />
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
