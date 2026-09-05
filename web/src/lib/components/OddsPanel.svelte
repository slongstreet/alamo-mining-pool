<script lang="ts">
  import type { CoinStatus } from '../api';
  import {
    formatAgo,
    formatCoins,
    formatDifficulty,
    formatDuration,
    formatPercent,
    formatPercentValue,
    formatProgress,
  } from '../format';

  let { coin, bestShare, now }: { coin: CoinStatus; bestShare: number; now: number } = $props();

  const horizons = $derived([
    { label: 'Next hour', p: coin.odds.p_hour },
    { label: 'Next day', p: coin.odds.p_day },
    { label: 'Next week', p: coin.odds.p_week },
    { label: 'Next 30 days', p: coin.odds.p_month },
    { label: 'Next year', p: coin.odds.p_year },
  ]);
  const roundPct = $derived(Math.min(100, coin.round.progress * 100));
  /** Best share as a fraction of a block, on a log scale so small shares still register. */
  const bestRatio = $derived(
    coin.network_difficulty > 0 ? bestShare / coin.network_difficulty : 0,
  );
  const bestBar = $derived(
    bestRatio <= 0 ? 0 : Math.min(100, Math.max(2, ((Math.log10(bestRatio) + 6) / 6) * 100)),
  );
  const luck = $derived(coin.round.luck_percent);
  const nodeTitle = $derived(
    coin.node.connected
      ? coin.node.zmq === true
        ? 'Node answering; block notifications over ZMQ'
        : coin.node.zmq === false
          ? 'Node answering; ZMQ subscription down, polling instead'
          : 'Node answering; polling for new blocks'
      : `${coin.node.failures} failed polls${coin.node.last_error ? `: ${coin.node.last_error}` : ''}`,
  );
  const luckClass = $derived(luck == null ? '' : luck >= 100 ? 'ok' : luck >= 70 ? 'warn' : 'bad');
</script>

<section class="panel">
  <div class="head">
    <h2>
      {coin.symbol} block odds
      {#if !coin.node.connected}
        <span class="badge bad" title={nodeTitle}>node {coin.node.stale ? 'stale' : 'unreachable'}</span>
      {:else if coin.node.zmq === false}
        <span class="badge warn" title={nodeTitle}>zmq down</span>
      {:else if coin.node.zmq}
        <span class="badge ok" title={nodeTitle}>zmq</span>
      {/if}
    </h2>
    <span class="muted num">
      height {coin.height.toLocaleString()} · diff {formatDifficulty(coin.network_difficulty)} ·
      {formatCoins(coin.coinbase_value, coin.symbol)} reward
    </span>
  </div>

  <div class="eta">
    <div>
      <div class="big num">{formatDuration(coin.odds.expected_seconds)}</div>
      <div class="muted">expected time to block at {coin.odds.hashrate > 0 ? 'current' : 'zero'} hashrate</div>
    </div>
    <div class="luck">
      <div class="big num {luckClass}">{formatPercentValue(luck)}</div>
      <div class="muted">lifetime luck{coin.round.blocks_found ? ` over ${coin.round.blocks_found} block${coin.round.blocks_found === 1 ? '' : 's'}` : ''}</div>
    </div>
  </div>

  <ul class="bars">
    {#each horizons as h (h.label)}
      <li>
        <span class="label">{h.label}</span>
        <span class="meter"><span style="width: {Math.max(h.p > 0 ? 0.6 : 0, h.p * 100)}%"></span></span>
        <span class="pct num">{formatPercent(h.p)}</span>
      </li>
    {/each}
  </ul>

  <div class="round">
    <div class="row">
      <span>Current round</span>
      <span class="muted num">
        {formatDifficulty(coin.round.work)} of {formatDifficulty(coin.round.expected_work)} expected
        · {formatProgress(coin.round.progress)}
      </span>
    </div>
    <div class="meter tall">
      <span class:over={coin.round.progress > 1} style="width: {roundPct}%"></span>
    </div>
    <div class="muted small">
      {#if coin.round.started_at != null}
        since the last block, {formatAgo(coin.round.started_at, now)}
      {:else}
        since the pool started keeping score; no {coin.symbol} block yet
      {/if}
    </div>

    <div class="row">
      <span>Best share vs network target</span>
      <span class="muted num">
        {formatDifficulty(bestShare)} / {formatDifficulty(coin.network_difficulty)}
        {#if bestRatio > 0}· {formatPercent(bestRatio)} of a block{/if}
      </span>
    </div>
    <div class="meter tall">
      <span class="alt" style="width: {bestBar}%"></span>
    </div>
    <div class="muted small">log scale · a share at 100% is a block</div>
  </div>
</section>

<style>
  .head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .head span {
    font-size: 0.8rem;
  }
  .eta {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
    margin: 0.75rem 0 1rem;
  }
  .luck {
    text-align: right;
  }
  .big {
    font-size: 2rem;
    font-weight: 600;
    letter-spacing: -0.02em;
    line-height: 1.1;
  }
  .big.ok {
    color: var(--ok);
  }
  .big.warn {
    color: var(--warn);
  }
  .big.bad {
    color: var(--bad);
  }
  .eta .muted {
    font-size: 0.82rem;
  }
  .bars {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.45rem;
  }
  .bars li {
    display: grid;
    grid-template-columns: 7.5rem 1fr 4.5rem;
    align-items: center;
    gap: 0.75rem;
    font-size: 0.9rem;
  }
  .bars .pct {
    text-align: right;
  }
  .round {
    margin-top: 1.1rem;
    padding-top: 0.9rem;
    border-top: 1px solid var(--border);
    display: grid;
    gap: 0.35rem;
  }
  .row {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
    font-size: 0.9rem;
  }
  .row .muted {
    font-size: 0.8rem;
  }
  .meter.tall {
    height: 10px;
  }
  .meter .over {
    background: var(--warn);
  }
  .meter .alt {
    background: var(--accent-2);
  }
  .small {
    font-size: 0.78rem;
    margin-bottom: 0.5rem;
  }
</style>
