<script lang="ts">
  import type { Share } from '../lib/api';
  import { formatCompact, formatTime, shortHash } from '../lib/format';

  interface Props {
    shares: Share[];
    connection: string;
  }
  let { shares, connection }: Props = $props();
</script>

<section class="panel">
  <h2>
    <span>Share log</span>
    <span class="badge" class:ok={connection === 'live'}>{connection === 'live' ? 'live' : 'last 100'}</span>
  </h2>
  {#if shares.length === 0}
    <p class="empty">Shares appear here as workers submit them.</p>
  {:else}
    <div class="scroll-x log">
      <table>
        <thead>
          <tr>
            <th>Time</th>
            <th>Worker</th>
            <th class="r">Job diff</th>
            <th class="r">Share diff</th>
            <th>Result</th>
          </tr>
        </thead>
        <tbody>
          {#each shares as s, i (`${s.ts}-${s.worker}-${i}`)}
            {@const ok = s.accepted === true || s.accepted === 1}
            <tr>
              <td class="num">{formatTime(s.ts)}</td>
              <td class="mono" title={s.worker}>{shortHash(s.worker, 12, 10)}</td>
              <td class="r">{formatCompact(s.difficulty)}</td>
              <td class="r" class:hot={ok && s.share_diff >= s.difficulty * 64}>{ok ? formatCompact(s.share_diff) : '—'}</td>
              <td>
                {#if ok}
                  <span class="badge ok"><span class="dot"></span>accepted</span>
                {:else}
                  <span class="badge bad"><span class="dot"></span>{s.reject_reason ?? 'rejected'}</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

<style>
  .log {
    max-height: 380px;
    overflow-y: auto;
  }
  thead th {
    position: sticky;
    top: 0;
    background: var(--panel);
  }
  td.hot {
    color: var(--accent);
    font-weight: 600;
  }
</style>
