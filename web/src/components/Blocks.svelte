<script lang="ts">
  import type { Block, CoinStatus } from '../lib/api';
  import { clamp01, formatCoin, formatCompact, formatDateTime, formatInt, shortHash } from '../lib/format';

  interface Props {
    blocks: Block[];
    coins: CoinStatus[];
  }
  let { blocks, coins }: Props = $props();

  function maturity(coin: string): number {
    return coins.find((c) => c.symbol === coin)?.coinbase_maturity ?? 100;
  }

  const LABEL: Record<Block['status'], { text: string; tone: string }> = {
    accepted: { text: 'confirming', tone: 'warn' },
    confirmed: { text: 'confirmed', tone: 'ok' },
    rejected: { text: 'rejected', tone: 'bad' },
    orphaned: { text: 'orphaned', tone: 'bad' },
  };

  let copied = $state<string | null>(null);
  async function copy(hash: string) {
    try {
      await navigator.clipboard.writeText(hash);
      copied = hash;
      setTimeout(() => (copied = null), 1200);
    } catch {
      /* clipboard may be unavailable */
    }
  }
</script>

<section class="panel">
  <h2>
    <span>Blocks found</span>
    <span class="badge">{blocks.filter((b) => b.status === 'confirmed' || b.status === 'accepted').length} on chain</span>
  </h2>
  {#if blocks.length === 0}
    <p class="empty">No blocks yet. When a share beats a network target it lands here with its confirmations.</p>
  {:else}
    <div class="scroll-x">
      <table>
        <thead>
          <tr>
            <th>Coin</th>
            <th class="r">Height</th>
            <th>Hash</th>
            <th>Worker</th>
            <th class="r">Reward</th>
            <th class="r">Share / net diff</th>
            <th>Found</th>
            <th>Status</th>
            <th>Confirmations</th>
          </tr>
        </thead>
        <tbody>
          {#each blocks as b (b.id)}
            {@const need = maturity(b.coin)}
            {@const label = LABEL[b.status]}
            <tr>
              <td><b>{b.coin}</b></td>
              <td class="r">{formatInt(b.height)}</td>
              <td>
                <button class="hash mono" type="button" title="Copy {b.hash}" onclick={() => copy(b.hash)}>
                  {copied === b.hash ? 'copied' : shortHash(b.hash, 12, 8)}
                </button>
              </td>
              <td class="mono" title={b.worker}>{shortHash(b.worker, 12, 10)}</td>
              <td class="r">{formatCoin(b.reward_sats, b.coin)}</td>
              <td class="r">{formatCompact(b.share_diff)} / {formatCompact(b.difficulty)}</td>
              <td>{formatDateTime(b.found_at)}</td>
              <td><span class="badge {label.tone}">{label.text}</span></td>
              <td>
                {#if b.status === 'accepted' || b.status === 'confirmed'}
                  <span class="conf">
                    <span class="track" role="meter" aria-valuemin="0" aria-valuemax={need} aria-valuenow={Math.min(b.confirmations, need)} aria-label="Confirmations">
                      <span class="fill" class:done={b.status === 'confirmed'} style:width="{clamp01(b.confirmations / need) * 100}%"></span>
                    </span>
                    <span class="num">{b.status === 'confirmed' ? `${formatInt(b.confirmations)}` : `${b.confirmations} / ${need}`}</span>
                  </span>
                {:else}
                  <span class="muted">—</span>
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
  .hash {
    background: none;
    border: 0;
    padding: 0;
    color: var(--text);
    cursor: pointer;
    font: inherit;
  }
  .hash:hover {
    color: var(--accent);
  }
  .conf {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.85rem;
  }
  .track {
    display: inline-block;
    width: 80px;
    height: 8px;
    border-radius: 4px;
    background: var(--track);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--warn);
    border-radius: 0 4px 4px 0;
  }
  .fill.done {
    background: var(--ok);
  }
</style>
