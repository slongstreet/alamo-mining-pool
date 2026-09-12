<script lang="ts">
  import type { CoinStatus } from '../lib/api';
  import {
    clamp01,
    formatAgo,
    formatCoin,
    formatCompact,
    formatDurationLong,
    formatHashrate,
    formatInt,
    formatPercent,
  } from '../lib/format';

  interface Props {
    coin: CoinStatus;
    hashrate: number;
    lifetimeBest: number;
    now: number;
  }
  let { coin, hashrate, lifetimeBest, now }: Props = $props();

  const horizons = $derived(
    coin.odds
      ? [
          { label: 'Next hour', p: coin.odds.p_hour },
          { label: 'Next day', p: coin.odds.p_day },
          { label: 'Next week', p: coin.odds.p_week },
          { label: 'Next 30 days', p: coin.odds.p_month },
          { label: 'Next year', p: coin.odds.p_year },
        ]
      : [],
  );

  const expected = $derived(coin.odds?.expected_seconds ?? null);
  const round = $derived(coin.round);
  const progress = $derived(round ? clamp01(round.work / round.expected_work) : 0);
  const luck = $derived(round?.luck_percent ?? 100);
  const luckTone = $derived(!round || round.work === 0 ? '' : luck >= 100 ? 'ok' : luck < 50 ? 'bad' : 'warn');

  /** Position of a difficulty on a log scale that ends at the network difficulty. */
  function logShare(diff: number): number {
    if (diff <= 1 || coin.network_difficulty <= 1) return 0;
    return clamp01(Math.log(diff) / Math.log(coin.network_difficulty));
  }
</script>

<section class="panel odds">
  <h2>
    <span>{coin.name} · next block odds</span>
    <span class="badge">{coin.chain}</span>
  </h2>

  <div class="hero">
    <div class="hero-value num">
      {#if expected == null}
        no hashrate
      {:else}
        {formatDurationLong(expected)}
      {/if}
    </div>
    <div class="hero-sub">
      expected time to a {coin.symbol} block at {formatHashrate(hashrate)} against difficulty
      {formatCompact(coin.network_difficulty)}
    </div>
  </div>

  <div class="grid">
    <div>
      <h3>Probability of at least one block</h3>
      <ul class="bars" aria-label="Block probability by horizon">
        {#each horizons as h (h.label)}
          <li>
            <span class="bar-label">{h.label}</span>
            <span class="track" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow={h.p * 100} aria-label={h.label}>
              <span class="fill" style:width="{clamp01(h.p) * 100}%"></span>
            </span>
            <span class="bar-value num">{formatPercent(h.p)}</span>
          </li>
        {/each}
      </ul>
    </div>

    <div>
      <h3>This round</h3>
      {#if round}
        <div class="row">
          <span class="muted">Work vs expected</span>
          <span class="num">{formatPercent(round.work / round.expected_work)}</span>
        </div>
        <span class="track wide" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow={progress * 100} aria-label="Round progress">
          <span class="fill" style:width="{progress * 100}%"></span>
        </span>
        <div class="row">
          <span class="muted">Luck</span>
          <span class="badge {luckTone}">
            {#if round.work === 0}no shares yet{:else}{luck >= 1000 ? formatCompact(luck) : luck.toFixed(0)}%{/if}
          </span>
        </div>
        <div class="row">
          <span class="muted">Shares this round</span>
          <span class="num">{formatInt(round.shares)}</span>
        </div>
        <div class="row">
          <span class="muted">Round started</span>
          <span>{formatAgo(now - round.started_at)}</span>
        </div>
      {:else}
        <p class="muted">Round accounting starts on the first share.</p>
      {/if}

      <h3>Best share vs network target</h3>
      <div class="row">
        <span class="muted">Best this round</span>
        <span class="num">{formatCompact(round?.best_share ?? 0)}</span>
      </div>
      <span class="track wide" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow={logShare(round?.best_share ?? 0) * 100} aria-label="Best share this round, log scale">
        <span class="fill" style:width="{logShare(round?.best_share ?? 0) * 100}%"></span>
      </span>
      <div class="row">
        <span class="muted">Best ever</span>
        <span class="num">{formatCompact(lifetimeBest)}</span>
      </div>
      <span class="track wide" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow={logShare(lifetimeBest) * 100} aria-label="Best share ever, log scale">
        <span class="fill" style:width="{logShare(lifetimeBest) * 100}%"></span>
      </span>
      <div class="row">
        <span class="muted">Network target</span>
        <span class="num">{formatCompact(coin.network_difficulty)}</span>
      </div>
      <p class="hint muted">Bars use a log scale: half way is the square root of the network difficulty.</p>
    </div>
  </div>

  <footer class="facts">
    <span>height <b class="num">{formatInt(coin.height)}</b></span>
    <span>reward <b class="num">{formatCoin(coin.coinbase_value, coin.symbol)}</b></span>
    <span>template <b>{formatAgo(coin.template_age_seconds)}</b></span>
    <span>maturity <b class="num">{coin.coinbase_maturity}</b> confirmations</span>
  </footer>
</section>

<style>
  h3 {
    margin: 0.75rem 0 0.4rem;
    font-size: 0.85rem;
    color: var(--text-2);
  }
  .hero {
    margin: 0.25rem 0 0.75rem;
  }
  .hero-value {
    font-size: 2.4rem;
    font-weight: 600;
    letter-spacing: -0.03em;
    line-height: 1.1;
    color: var(--accent);
  }
  .hero-sub {
    color: var(--muted);
    font-size: 0.88rem;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1.5rem;
  }
  @media (max-width: 720px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
  .bars {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.5rem;
  }
  .bars li {
    display: grid;
    grid-template-columns: 7.5rem 1fr 4.5rem;
    align-items: center;
    gap: 0.6rem;
    font-size: 0.9rem;
  }
  .bar-label {
    color: var(--text-2);
  }
  .bar-value {
    text-align: right;
  }
  .track {
    display: block;
    height: 10px;
    background: var(--track);
    border-radius: 5px;
    overflow: hidden;
  }
  .track.wide {
    margin: 0.2rem 0 0.5rem;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--series);
    border-radius: 0 5px 5px 0;
    transition: width 0.4s ease;
    min-width: 0;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
    font-size: 0.9rem;
    padding: 0.15rem 0;
  }
  .hint {
    font-size: 0.78rem;
    margin: 0.25rem 0 0;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem 1.5rem;
    margin-top: 1rem;
    padding-top: 0.75rem;
    border-top: 1px solid var(--line);
    font-size: 0.85rem;
    color: var(--muted);
  }
  .facts b {
    font-weight: 500;
    color: var(--text);
  }
</style>
