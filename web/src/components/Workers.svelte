<script lang="ts">
  import type { Worker } from '../lib/api';
  import { formatAgo, formatCompact, formatHashrate, formatInt, shortHash } from '../lib/format';

  interface Props {
    workers: Worker[];
  }
  let { workers }: Props = $props();
</script>

<section class="panel">
  <h2>
    <span>Workers</span>
    <span class="badge">{workers.filter((w) => w.connections > 0).length} online</span>
  </h2>
  {#if workers.length === 0}
    <p class="empty">No workers yet. Point a miner at the stratum port with a payout address as the username.</p>
  {:else}
    <div class="scroll-x">
      <table>
        <thead>
          <tr>
            <th>Worker</th>
            <th>Pays</th>
            <th class="r">Hashrate</th>
            <th class="r">Diff</th>
            <th class="r">Accepted</th>
            <th class="r">Rejected</th>
            <th class="r">Best share</th>
            <th class="r">Last share</th>
          </tr>
        </thead>
        <tbody>
          {#each workers as w (w.name)}
            {@const online = w.connections > 0}
            <tr class:offline={!online}>
              <td>
                <span class="status" class:online title={online ? `${w.connections} connection(s)` : 'offline'}>
                  <span class="dot"></span>
                </span>
                <span class="mono" title={w.name}>{shortHash(w.name, 12, 10)}</span>
              </td>
              <td>
                <span class="mono" title={w.address}>{shortHash(w.address, 8, 6)}</span>
                {#if w.fallback}
                  <span class="badge warn" title="Username was not a valid address; the configured fallback is paid">fallback</span>
                {/if}
                {#each w.aux_payouts as a (a.coin)}
                  <span class="aux mono" title={a.address}>{a.coin} {shortHash(a.address, 6, 4)}</span>
                  {#if a.fallback}
                    <span class="badge warn">fallback</span>
                  {/if}
                {/each}
              </td>
              <td class="r">{online ? formatHashrate(w.hashrate) : '—'}</td>
              <td class="r">{online ? formatCompact(w.difficulty) : '—'}</td>
              <td class="r">{formatInt(w.shares_accepted)}</td>
              <td class="r" class:bad={w.shares_rejected > 0}>{formatInt(w.shares_rejected)}</td>
              <td class="r">{formatCompact(w.best_difficulty)}</td>
              <td class="r">{formatAgo(w.last_share_seconds)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

<style>
  .status {
    color: var(--muted);
    margin-right: 0.4rem;
  }
  .status.online {
    color: var(--ok);
  }
  .offline td {
    color: var(--muted);
  }
  .aux {
    margin-left: 0.5rem;
    color: var(--text-2);
  }
  td.bad {
    color: var(--bad);
  }
</style>
