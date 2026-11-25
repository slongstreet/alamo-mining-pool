<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { blockFlash, describeAlert, observe, tabTitle, type AlertBaseline } from './lib/alerts';
  import { DesktopNotifications } from './lib/desktop.svelte';
  import { LiveStatus } from './lib/live.svelte';
  import { ToastQueue } from './lib/toasts.svelte';
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
  import StaleBanner from './lib/components/StaleBanner.svelte';
  import Toasts from './lib/components/Toasts.svelte';

  const live = new LiveStatus();
  const theme = new ThemePreference();
  theme.apply();
  const toasts = new ToastQueue();
  const desktop = new DesktopNotifications();

  // Pages and lanes are told apart by the hash, so the daemon serves one document for all
  // of them and back/forward move between lanes.
  let hash = $state(location.hash);
  const route = $derived(parseHash(hash));
  const page = $derived(route.page);

  // A one-second wall clock for the staleness check and its counter, nothing else.
  let clockMs = $state(Date.now());

  onMount(() => {
    live.start();
    const onHash = () => (hash = location.hash);
    window.addEventListener('hashchange', onHash);
    const clock = setInterval(() => (clockMs = Date.now()), 1_000);
    window.addEventListener('focus', onAttention);
    document.addEventListener('visibilitychange', onAttention);
    return () => {
      live.stop();
      clearInterval(clock);
      toasts.clear();
      window.removeEventListener('hashchange', onHash);
      window.removeEventListener('focus', onAttention);
      document.removeEventListener('visibilitychange', onAttention);
    };
  });

  // Stale: no status for STALE_MS by the client clock, whatever the connection says, or
  // the daemon unreachable. Cleared by the next status (which moves `receivedAt`).
  const STALE_MS = 10_000;
  const stale = $derived(
    live.receivedAt != null && (live.connection === 'offline' || clockMs - live.receivedAt > STALE_MS),
  );
  const staleSeconds = $derived(
    stale && live.receivedAt != null ? Math.max(0, Math.floor((clockMs - live.receivedAt) / 1000)) : null,
  );

  const status = $derived(live.status);
  const now = $derived(status?.now ?? Math.floor(Date.now() / 1000));
  const lanes = $derived(status?.lanes ?? []);
  const multiLane = $derived(lanes.length > 1);
  const lane = $derived(
    pickLane(lanes, route.page === 'dashboard' ? route.lane : null, rememberedLane()),
  );
  // `lane` is a new object with every snapshot; its key only changes on a real lane
  // switch. Effects that care about which lane, not its figures, read this (children that
  // take `lane={lane.key}` derive their own copy for the same reason).
  const laneKey = $derived(lane?.key);

  // A lane named in the hash is the one to come back to.
  $effect(() => {
    if (route.page === 'dashboard' && route.lane && laneKey === route.lane) rememberLane(laneKey);
  });

  // "★ BTC block found — …" in the title until the tab is looked at again.
  let flash = $state<string | null>(null);

  // Notices. The first snapshot after load, or after a gap in updates, is a baseline:
  // what it already holds is not news (see `observe`).
  let baseline: AlertBaseline | null = null;
  let lastReceived: number | null = null;
  let staleSince = false;
  $effect(() => {
    if (stale) staleSince = true;
  });
  $effect(() => {
    const next = status;
    if (!next) return;
    untrack(() => {
      const received = live.receivedAt;
      const gap = staleSince || (lastReceived != null && received != null && received - lastReceived > STALE_MS);
      staleSince = false;
      lastReceived = received;
      const { baseline: b, alerts } = observe(gap ? null : baseline, next);
      baseline = b;
      const several = next.lanes.length > 1;
      for (const alert of alerts) {
        const text = describeAlert(alert, several);
        toasts.push(text);
        if (alert.kind === 'block') {
          desktop.notify(`★ ${text.title}`, text.body, alert.key);
          // A tab in view already shows the notice; one in the background or behind
          // another window carries it in its title until it is looked at.
          if (document.visibilityState !== 'visible' || !document.hasFocus()) flash = blockFlash(alert.block);
        }
      }
    });
  });

  function onAttention() {
    desktop.refresh();
    if (document.visibilityState === 'visible' && document.hasFocus()) flash = null;
  }

  // The tab title: a derived string, so the effect only writes when the text changes.
  const docTitle = $derived(tabTitle(status, { stale, flash }));
  $effect(() => {
    if (document.title !== docTitle) document.title = docTitle;
  });

  function select(key: string) {
    location.hash = `#/${encodeURIComponent(key)}`;
  }
</script>

<main>
  <Header {status} connection={live.connection} error={live.error} {theme} {page} {desktop} />
  {#if stale}
    <StaleBanner seconds={staleSeconds} error={live.error} />
  {/if}
  <Toasts queue={toasts} />

  {#if page === 'settings'}
    <SettingsPage {status} />
  {:else if status && lane}
    <div class="grid dash" class:stale>
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
        workers={lane.workers}
        lane={lane.key}
        allLabel={multiLane ? 'Lane' : 'Pool'}
      />
      <WorkerTable {lane} />

      <div class="grid two">
        <ShareLog {now} lane={lane.key} shareMultiplier={lane.share_multiplier} workers={lane.workers} />
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

<style>
  /* Stale: the figures fade and lose their colour, so old numbers do not pass for live
     ones. Opacity and filter only: nothing moves. */
  .dash {
    transition:
      opacity 0.3s ease,
      filter 0.3s ease;
  }
  .dash.stale {
    opacity: 0.5;
    filter: grayscale(0.85);
  }
  @media (prefers-reduced-motion: reduce) {
    .dash {
      transition: none;
    }
  }
</style>
