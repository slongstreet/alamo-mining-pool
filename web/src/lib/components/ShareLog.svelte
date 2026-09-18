<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type ShareRow } from '../api';
  import { formatAgo, formatClock, formatDifficulty, shortWorker } from '../format';
  import Info from './Info.svelte';

  /** `shareMultiplier` converts the job difficulty (stratum units) to network units. */
  let { now, shareMultiplier }: { now: number; shareMultiplier: number } = $props();

  const jobHelp =
    'The difficulty the pool asked the miner for, in stratum units: the number the miner itself ' +
    'reports. Share diff, Best share, and the network difficulty are in network units, which are ' +
    'this divided by the share multiplier (65,536 for scrypt).';

  let shares = $state<ShareRow[]>([]);
  let paused = $state(false);

  async function load() {
    if (paused) return;
    try {
      shares = await api.shares(40);
    } catch {
      // Keep the last list.
    }
  }

  onMount(() => {
    load();
    const timer = setInterval(load, 3_000);
    return () => clearInterval(timer);
  });

  const reasons: Record<string, string> = {
    stale_job: 'stale',
    unknown_job: 'unknown job',
    low_difficulty: 'low diff',
    duplicate: 'duplicate',
    invalid_ntime: 'bad ntime',
    invalid_extranonce2: 'bad extranonce',
    unauthorized: 'unauthorized',
  };
</script>

<section class="panel">
  <div class="head">
    <h2>Share log</h2>
    <button class="ghost" aria-pressed={paused} onclick={() => (paused = !paused)}>{paused ? 'Resume' : 'Pause'}</button>
  </div>
  {#if shares.length === 0}
    <div class="empty">No shares yet.</div>
  {:else}
    <div class="scroll log">
      <table>
        <thead>
          <tr>
            <th>When</th>
            <th>Worker</th>
            <th class="r">Job diff <Info text={jobHelp} align="right" /></th>
            <th class="r">Share diff</th>
            <th>Result</th>
          </tr>
        </thead>
        <tbody>
          {#each shares as s (s.id)}
            <tr>
              <td class="muted num" title={formatClock(s.ts)}>{formatAgo(s.ts, now)}</td>
              <td class="mono worker" title={s.worker}>{shortWorker(s.worker)}</td>
              <td class="r num">{formatDifficulty(s.difficulty)}</td>
              <td class="r num" class:hot={s.accepted && s.share_diff * shareMultiplier >= s.difficulty * 100}>
                {s.accepted ? formatDifficulty(s.share_diff) : '—'}
              </td>
              <td>
                {#if s.accepted}
                  <span class="badge ok">accepted</span>
                {:else}
                  <span class="badge bad">{reasons[s.reject_reason ?? ''] ?? s.reject_reason ?? 'rejected'}</span>
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
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.5rem;
  }
  .head h2 {
    margin: 0;
  }
  .log {
    max-height: 420px;
    overflow-y: auto;
  }
  .worker {
    max-width: 18rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  @media (max-width: 640px) {
    .worker {
      max-width: 12rem;
    }
  }
  .hot {
    color: var(--accent);
    font-weight: 600;
  }
</style>
