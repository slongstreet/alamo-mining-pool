<script lang="ts">
  import { api, type Status } from '../api';
  import { formatDifficulty, formatHashrate, formatInteger } from '../format';

  let { status }: { status: Status } = $props();

  let resetting = $state(false);
  let resetError = $state<string | null>(null);

  async function resetStats() {
    if (!confirm('Reset accepted/rejected share counts and best share for every worker?')) return;
    resetting = true;
    resetError = null;
    try {
      await api.resetStats();
    } catch (err) {
      resetError = err instanceof Error ? err.message : String(err);
    } finally {
      resetting = false;
    }
  }

  const online = $derived(status.workers.filter((w) => w.connections > 0).length);
  const total = $derived(status.shares_accepted + status.shares_rejected);
  const rejectRate = $derived(total > 0 ? (status.shares_rejected / total) * 100 : 0);
  const blocks = $derived(status.coins.reduce((n, c) => n + c.round.blocks_found, 0));
</script>

<div class="tiles">
  <div class="tile">
    <div class="label">Pool hashrate</div>
    <div class="value num">{formatHashrate(status.hashrate)}</div>
    <div class="sub">10 minute estimate</div>
  </div>
  <div class="tile">
    <div class="label">Workers</div>
    <div class="value num">{online}</div>
    <div class="sub">{status.workers.length} seen</div>
  </div>
  <div class="tile">
    <div class="label">Shares</div>
    <div class="value num">{formatInteger(status.shares_accepted)}</div>
    <div class="sub">
      {formatInteger(status.shares_rejected)} rejected
      {#if total > 0}({rejectRate.toFixed(rejectRate < 1 ? 2 : 1)}%){/if}
    </div>
  </div>
  <div class="tile">
    <div class="label">
      Best share
      <button class="ghost reset" onclick={resetStats} disabled={resetting} title="Zero share counts and best share for every worker">
        {resetting ? 'resetting…' : 'reset'}
      </button>
    </div>
    <div class="value num">{formatDifficulty(status.best_share_difficulty)}</div>
    <div class="sub">
      {#if resetError}<span class="bad">{resetError}</span>{:else}difficulty, since last reset{/if}
    </div>
  </div>
  <div class="tile">
    <div class="label">Blocks found</div>
    <div class="value num">{blocks}</div>
    <div class="sub">
      {status.coins.map((c) => `${c.round.blocks_found} ${c.symbol}`).join(' · ') || 'none yet'}
    </div>
  </div>
</div>
