<script lang="ts">
  import type { LaneStatus } from '../api';
  import { addressExample } from '../lanes';

  /** Stratum connection instructions for one lane: URL, username, and password. */
  let { lane }: { lane: LaneStatus } = $props();

  // The parent pays from the username; aux chains take their address from the password.
  // Configured coins are known before the lane's nodes answer.
  const parent = $derived(lane.configured_coins[0]?.symbol ?? lane.key.toUpperCase());
  const aux = $derived(lane.configured_coins.slice(1).map((c) => c.symbol));
  const auxLabel = $derived(aux.join(' / '));
  const auxTag = $derived((aux[0] ?? '').toLowerCase());
  const auxExample = $derived(addressExample(aux[0] ?? '', lane.chain));
  const host = $derived(location.hostname || 'pool-host');
  const port = $derived(lane.stratum_port);
  const url = $derived(`stratum+tcp://${host}:${port}`);
  const sha256 = $derived(lane.algorithm === 'sha256d');
</script>

<div class="setup">
  <dl>
    <dt>URL</dt>
    <dd><code>{url}</code></dd>
    <dt>Username</dt>
    <dd>
      <code>&lt;{parent} address&gt;</code>
      <span class="hint">
        Add a worker name after a dot, e.g. <code>{addressExample(parent, lane.chain)}.rig1</code>. Blocks pay this
        address.
      </span>
    </dd>
    <dt>Password</dt>
    <dd>
      {#if aux.length}
        <code>&lt;{auxLabel} address&gt;</code>
        <span class="hint">
          Bare, or tagged as <code>{auxTag}={auxExample}</code> next to other options such as <code>d=1024</code>.
          Leave it out and {auxLabel} goes to the configured fallback address.
        </span>
      {:else}
        <code>anything</code>
        <span class="hint">Any password works; this lane has no merge-mined coins.</span>
      {/if}
      <span class="hint">
        Optional: <code>d=&lt;difficulty&gt;</code> sets the starting share difficulty, within the lane's vardiff
        limits.
      </span>
    </dd>
    {#if sha256}
      <dt>Version rolling</dt>
      <dd>
        Supported
        <span class="hint">ASICBoost miners can roll version bits (BIP310 <code>mining.configure</code>).</span>
      </dd>
    {/if}
  </dl>
  <p class="hint">
    Shows the host this page was loaded from. If miners reach the pool by another name or IP, use that
    with port {port}.
  </p>
</div>

<style>
  .setup {
    max-width: 44rem;
    margin: 0 auto;
    font-size: 0.9rem;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.5rem 1rem;
    margin: 0;
  }
  dt {
    color: var(--muted);
    font-weight: 600;
    padding-top: 0.15rem;
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.92em;
    background: var(--panel-2);
    border-radius: 6px;
    padding: 0.1rem 0.4rem;
    overflow-wrap: anywhere;
  }
  .hint {
    display: block;
    color: var(--muted);
    margin-top: 0.3rem;
  }
  p.hint {
    margin: 0.9rem 0 0;
    text-align: center;
  }
  @media (max-width: 480px) {
    dl {
      grid-template-columns: 1fr;
      gap: 0.25rem;
    }
    dt {
      margin-top: 0.5rem;
    }
  }
</style>
