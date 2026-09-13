<script lang="ts">
  import type { CoinStatus, WorkerStatus } from '../api';
  import { formatDifficulty, formatDuration, formatHashrate, formatInteger, shortAddress } from '../format';
  import MinerSetup from './MinerSetup.svelte';

  let {
    workers,
    stratumPort,
    coins,
  }: { workers: WorkerStatus[]; stratumPort: number; coins: CoinStatus[] } = $props();
</script>

<section class="panel">
  <h2>Workers</h2>
  {#if workers.length === 0}
    <p class="empty">No workers yet. Point a miner at the pool like this:</p>
    <MinerSetup port={stratumPort} {coins} />
  {:else}
    <div class="scroll">
      <table>
        <thead>
          <tr>
            <th>Worker</th>
            <th>Pays</th>
            <th class="r">Hashrate</th>
            <th class="r">Diff</th>
            <th class="r">Shares</th>
            <th class="r">Best</th>
            <th class="r">Last share</th>
          </tr>
        </thead>
        <tbody>
          {#each workers as w (w.name)}
            <tr class:offline={w.connections === 0}>
              <td>
                <span class="dot" class:on={w.connections > 0}></span>
                <span class="mono">{w.name}</span>
                {#if w.connections > 1}<span class="badge">{w.connections}×</span>{/if}
              </td>
              <td class="pays">
                <span class="mono" title={w.address}>{shortAddress(w.address)}</span>
                {#if w.fallback}<span class="badge warn" title="Username was not a valid address; the configured fallback is paid">fallback</span>{/if}
                {#each w.aux_payouts as aux (aux.coin)}
                  <span class="mono muted" title={aux.address}> · {aux.coin} {shortAddress(aux.address)}</span>
                  {#if aux.fallback}<span class="badge warn">fallback</span>{/if}
                {/each}
              </td>
              <td class="r num">{formatHashrate(w.hashrate)}</td>
              <td class="r num">{formatDifficulty(w.difficulty)}</td>
              <td class="r num">
                {formatInteger(w.shares_accepted)}
                {#if w.shares_rejected}<span class="bad"> / {formatInteger(w.shares_rejected)}</span>{/if}
              </td>
              <td class="r num">{formatDifficulty(w.best_difficulty)}</td>
              <td class="r num muted">{w.last_share_seconds == null ? '—' : `${formatDuration(w.last_share_seconds)} ago`}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <details class="connect">
      <summary>Connect another miner</summary>
      <MinerSetup port={stratumPort} {coins} />
    </details>
  {/if}
</section>

<style>
  .dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
    margin-right: 0.5rem;
    vertical-align: middle;
    opacity: 0.5;
  }
  .dot.on {
    background: var(--ok);
    opacity: 1;
  }
  tr.offline td {
    color: var(--muted);
  }
  .pays {
    max-width: 24rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .bad {
    color: var(--bad);
  }
  .badge {
    margin-left: 0.35rem;
  }
  .connect {
    margin-top: 0.9rem;
  }
  .connect summary {
    cursor: pointer;
    color: var(--muted);
    font-size: 0.85rem;
  }
  .connect summary:hover {
    color: var(--text);
  }
</style>
