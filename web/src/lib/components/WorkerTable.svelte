<script lang="ts">
  import { api, type CoinStatus, type WorkerStatus } from '../api';
  import { formatDifficulty, formatDuration, formatHashrate, formatInteger, shortAddress, shortWorker } from '../format';
  import MinerSetup from './MinerSetup.svelte';

  let {
    workers,
    stratumPort,
    coins,
  }: { workers: WorkerStatus[]; stratumPort: number; coins: CoinStatus[] } = $props();

  /** Workers whose removal is in flight; the row disappears with the next snapshot. */
  let removing = $state<Set<string>>(new Set());
  let removeError = $state<string | null>(null);
  let resetting = $state(false);

  async function resetStats() {
    if (!confirm('Reset accepted/rejected share counts and best share for every worker?')) return;
    resetting = true;
    removeError = null;
    try {
      await api.resetStats();
    } catch (err) {
      removeError = err instanceof Error ? err.message : String(err);
    } finally {
      resetting = false;
    }
  }

  async function remove(w: WorkerStatus) {
    if (!confirm(`Remove ${w.name} from the workers table?\n\nIts share counts, share log, and hashrate history are dropped. Blocks it found and the pool's lifetime work are kept. If it connects again it starts fresh.`)) return;
    removing = new Set(removing).add(w.name);
    removeError = null;
    try {
      await api.removeWorker(w.name);
    } catch (err) {
      removeError = err instanceof Error ? err.message : String(err);
      const next = new Set(removing);
      next.delete(w.name);
      removing = next;
    }
  }
</script>

<section class="panel">
  <div class="head">
    <h2>Workers</h2>
    {#if workers.length > 0}
      <button
        class="ghost"
        onclick={resetStats}
        disabled={resetting}
        title="Zero share counts and best share for every worker. Hashrate, accepted work, and blocks are kept."
      >
        {resetting ? 'Resetting…' : 'Reset share counts'}
      </button>
    {/if}
  </div>
  {#if workers.length === 0}
    <p class="empty">No workers yet. Point a miner at the pool like this:</p>
    <MinerSetup port={stratumPort} {coins} />
  {:else}
    {#if removeError}<div class="bad small">{removeError}</div>{/if}
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
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each workers as w (w.name)}
            <tr class:offline={w.connections === 0}>
              <td class="name">
                <span class="dot" class:on={w.connections > 0}></span>
                <span class="mono" title={w.name}>{shortWorker(w.name)}</span>
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
              <td class="r">
                {#if w.connections === 0}
                  <button
                    class="ghost remove"
                    onclick={() => remove(w)}
                    disabled={removing.has(w.name)}
                    title="Remove this worker from the table"
                    aria-label="Remove {w.name}"
                  >
                    {removing.has(w.name) ? '…' : '×'}
                  </button>
                {/if}
              </td>
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
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1rem;
    margin-bottom: 0.5rem;
  }
  .head h2 {
    margin: 0;
  }
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
  /* Names are already shortened to `ltc1q3…x7k2.rig1`; the ellipsis is only a backstop
     for very long worker parts. */
  .name {
    max-width: 20rem;
    white-space: nowrap;
  }
  .name .mono {
    display: inline-block;
    max-width: calc(100% - 1.5rem);
    overflow: hidden;
    text-overflow: ellipsis;
    vertical-align: bottom;
  }
  @media (max-width: 640px) {
    .name {
      max-width: 14rem;
    }
  }
  .remove {
    padding: 0 0.45rem;
    line-height: 1.4;
    font-size: 0.9rem;
  }
  .small {
    font-size: 0.8rem;
    margin-bottom: 0.5rem;
  }
  .pays {
    max-width: 18rem;
    white-space: nowrap;
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
