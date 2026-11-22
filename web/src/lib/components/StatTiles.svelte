<script lang="ts">
  import type { LaneStatus } from '../api';
  import { formatAgo, formatClock, formatDifficulty, formatHashrate, formatInteger, shortWorker } from '../format';
  import Info from './Info.svelte';

  /** The selected lane's headline figures; `multiLane` names hashrate per lane. */
  let { lane, now, multiLane }: { lane: LaneStatus; now: number; multiLane: boolean } = $props();

  const online = $derived(lane.workers.filter((w) => w.connections > 0).length);
  const total = $derived(lane.shares_accepted + lane.shares_rejected);
  const rejectRate = $derived(total > 0 ? (lane.shares_rejected / total) * 100 : 0);
  const blocks = $derived(lane.coins.reduce((n, c) => n + c.round.blocks_found, 0));
</script>

<div class="tiles">
  <div class="tile">
    <div class="label">{multiLane ? 'Lane hashrate' : 'Pool hashrate'}</div>
    <div class="value num">{formatHashrate(lane.hashrate)}</div>
    <div class="sub">10 minute estimate</div>
  </div>
  <div class="tile">
    <div class="label">Workers</div>
    <div class="value num">{online}</div>
    <div class="sub">{lane.workers.length} seen</div>
  </div>
  <div class="tile">
    <div class="label">Shares</div>
    <div class="value num">{formatInteger(lane.shares_accepted)}</div>
    <div class="sub">
      {formatInteger(lane.shares_rejected)} rejected
      {#if total > 0}({rejectRate.toFixed(rejectRate < 1 ? 2 : 1)}%){/if}
    </div>
  </div>
  <div class="tile">
    <div class="label">
      Best share
      <Info
        text="The highest-difficulty share any worker has submitted since the last reset. A share whose difficulty reaches the network difficulty is a block. It is a record, not a predictor. Reset it from the Settings page."
      />
    </div>
    <div class="value num">{formatDifficulty(lane.best_share_difficulty)}</div>
    {#if lane.best_share_worker}
      <div class="sub">
        <span class="mono" title={lane.best_share_worker}>{shortWorker(lane.best_share_worker)}</span>
        {#if lane.best_share_at != null}
          · <span title={formatClock(lane.best_share_at)}>{formatAgo(lane.best_share_at, now)}</span>
        {/if}
      </div>
    {:else}
      <div class="sub">difficulty, since last reset</div>
    {/if}
  </div>
  <div class="tile">
    <div class="label">Blocks found</div>
    <div class="value num">{blocks}</div>
    <div class="sub">
      {lane.coins.map((c) => `${c.round.blocks_found} ${c.symbol}`).join(' · ') || 'none yet'}
    </div>
  </div>
</div>
