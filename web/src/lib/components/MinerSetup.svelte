<script lang="ts">
  import type { CoinStatus } from '../api';

  /** Stratum connection instructions: URL, username, and password for this pool. */
  let { port, coins }: { port: number; coins: CoinStatus[] } = $props();

  // The parent chain comes first; the aux chains take their address from the password.
  // Before the first template the coin list is empty, so fall back to the pool's defaults.
  const parent = $derived(coins[0]?.symbol ?? 'LTC');
  const aux = $derived(coins.slice(1).map((c) => c.symbol));
  const auxLabel = $derived(aux.length > 0 ? aux.join(' / ') : 'DOGE');
  const auxTag = $derived((aux[0] ?? 'DOGE').toLowerCase());
  const host = $derived(location.hostname || 'pool-host');
  const url = $derived(`stratum+tcp://${host}:${port}`);
</script>

<div class="setup">
  <dl>
    <dt>URL</dt>
    <dd><code>{url}</code></dd>
    <dt>Username</dt>
    <dd>
      <code>&lt;{parent} address&gt;</code>
      <span class="hint">Add a worker name after a dot, e.g. <code>ltc1q….rig1</code>. Blocks pay this address.</span>
    </dd>
    <dt>Password</dt>
    <dd>
      <code>&lt;{auxLabel} address&gt;</code>
      <span class="hint">
        Bare, or tagged as <code>{auxTag}=D…</code> next to other options such as <code>d=1024</code>.
        Leave it out and {auxLabel} goes to the configured fallback address.
      </span>
    </dd>
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
