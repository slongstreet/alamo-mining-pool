<script lang="ts">
  import { onMount } from 'svelte';
  import { live } from './lib/live.svelte';
  import { theme } from './lib/theme.svelte';
  import { formatCompact, formatDuration, formatHashrate, formatInt } from './lib/format';
  import StatTile from './components/StatTile.svelte';
  import Odds from './components/Odds.svelte';
  import HashrateChart from './components/HashrateChart.svelte';
  import Workers from './components/Workers.svelte';
  import ShareLog from './components/ShareLog.svelte';
  import Blocks from './components/Blocks.svelte';

  onMount(() => {
    live.start();
    return () => live.stop();
  });

  const status = $derived(live.status);
  const online = $derived(status?.workers.filter((w) => w.connections > 0).length ?? 0);
  const rejectRate = $derived.by(() => {
    if (!status) return 0;
    const total = status.shares_accepted + status.shares_rejected;
    return total === 0 ? 0 : status.shares_rejected / total;
  });
  const connectionLabel: Record<string, string> = {
    connecting: 'connecting…',
    live: 'live',
    polling: 'polling',
    offline: 'daemon unreachable',
  };
  const themeIcon = $derived(theme.current === 'dark' ? '🌙' : theme.current === 'light' ? '☀️' : '🖥️');
</script>

<main>
  <header>
    <div>
      <h1>{status?.pool_name ?? 'Alamo'}</h1>
      <p class="muted sub">
        {#if status}
          v{status.version} · up {formatDuration(status.uptime_seconds)} ·
          {#each status.coins as c, i (c.symbol)}{i > 0 ? ' + ' : ''}{c.symbol} {c.chain}{/each}
        {:else if live.error}
          {live.error}
        {:else}
          connecting…
        {/if}
      </p>
    </div>
    <div class="tools">
      <span class="badge" class:ok={live.connection === 'live'} class:bad={live.connection === 'offline'}>
        <span class="dot"></span>{connectionLabel[live.connection]}
      </span>
      <button class="plain" type="button" onclick={() => theme.cycle()} title="Theme: {theme.current}" aria-label="Cycle theme (now {theme.current})">
        {themeIcon}
      </button>
    </div>
  </header>

  {#if status}
    <div class="tiles">
      <StatTile label="Pool hashrate" value={formatHashrate(status.hashrate)} tone="accent" sub="10 minute window" />
      <StatTile label="Workers online" value={formatInt(online)} sub="{status.workers.length} known" />
      <StatTile label="Shares accepted" value={formatCompact(status.shares_accepted)} sub="{formatCompact(status.shares_rejected)} rejected ({(rejectRate * 100).toFixed(2)}%)" tone={rejectRate > 0.05 ? 'bad' : 'default'} />
      <StatTile label="Blocks found" value={formatInt(status.blocks.filter((b) => b.status !== 'rejected').length)} sub="{status.blocks.filter((b) => b.status === 'confirmed').length} confirmed" />
      <StatTile label="Best share ever" value={formatCompact(status.best_share_difficulty)} sub={status.coins[0] ? `network ${formatCompact(status.coins[0].network_difficulty)}` : ''} />
    </div>

    <div class="odds-grid" style:--cols={Math.min(status.coins.length, 2)}>
      {#each status.coins as coin (coin.symbol)}
        <Odds {coin} hashrate={status.hashrate} lifetimeBest={status.best_share_difficulty} now={status.now} />
      {/each}
      {#if status.coins.length === 0}
        <section class="panel"><p class="empty">Waiting for the first block template from the node.</p></section>
      {/if}
    </div>

    <HashrateChart live={status.hashrate} workers={status.workers.map((w) => w.name)} />
    <Workers workers={status.workers} />
    <div class="two">
      <ShareLog shares={live.shares} connection={live.connection} />
      <Blocks blocks={status.blocks} coins={status.coins} />
    </div>
  {:else}
    <section class="panel"><p class="empty">Waiting for the daemon…</p></section>
  {/if}
</main>

<style>
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 1rem;
    margin-bottom: 1.25rem;
  }
  h1 {
    margin: 0;
    font-size: 1.8rem;
    letter-spacing: -0.02em;
  }
  .sub {
    margin: 0.1rem 0 0;
    font-size: 0.9rem;
  }
  .tools {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    flex-shrink: 0;
  }
  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 0.75rem;
    margin-bottom: 1rem;
  }
  .odds-grid {
    display: grid;
    grid-template-columns: repeat(var(--cols, 1), minmax(0, 1fr));
    gap: 1rem;
    margin-bottom: 1rem;
  }
  .two {
    display: grid;
    grid-template-columns: 1fr;
    gap: 1rem;
  }
  main > :global(section.panel) {
    margin-bottom: 1rem;
  }
  @media (max-width: 960px) {
    .odds-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
