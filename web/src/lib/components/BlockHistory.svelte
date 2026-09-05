<script lang="ts">
  import type { BlockRow, CoinStatus } from '../api';
  import { formatAgo, formatCoins, formatDifficulty, shortHash } from '../format';

  let { blocks, coins, now }: { blocks: BlockRow[]; coins: CoinStatus[]; now: number } = $props();

  const classes: Record<BlockRow['status'], string> = {
    accepted: 'warn',
    confirmed: 'ok',
    orphaned: 'bad',
    rejected: 'bad',
  };
  const labels: Record<BlockRow['status'], string> = {
    accepted: 'confirming',
    confirmed: 'confirmed',
    orphaned: 'orphaned',
    rejected: 'rejected',
  };
</script>

<section class="panel">
  <h2>Blocks</h2>
  {#if blocks.length === 0}
    <div class="empty">
      No blocks yet. When a share beats the network target it lands here with its confirmations.
      {#if coins.length}
        The next {coins[0].symbol} block pays {formatCoins(coins[0].coinbase_value, coins[0].symbol)}.
      {/if}
    </div>
  {:else}
    <div class="scroll">
      <table>
        <thead>
          <tr>
            <th>Found</th>
            <th>Coin</th>
            <th class="r">Height</th>
            <th>Hash</th>
            <th>Worker</th>
            <th class="r">Reward</th>
            <th class="r">Share / net diff</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {#each blocks as b (b.id)}
            <tr>
              <td class="muted num">{formatAgo(b.found_at, now)}</td>
              <td><span class="badge accent">{b.coin}</span></td>
              <td class="r num">{b.height.toLocaleString()}</td>
              <td class="mono" title={b.hash}>{shortHash(b.hash)}</td>
              <td class="mono">{b.worker}</td>
              <td class="r num">{formatCoins(b.reward_sats, b.coin)}</td>
              <td class="r num">{formatDifficulty(b.share_diff)} / {formatDifficulty(b.difficulty)}</td>
              <td>
                <span class="badge {classes[b.status]}">{labels[b.status]}</span>
                {#if b.status === 'accepted' || b.status === 'confirmed'}
                  <span class="muted num"> {b.confirmations}</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>
