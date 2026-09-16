<script lang="ts">
  import { api, type CoinSettings, type CoinStatus, type NodeProbe } from '../../api';
  import { formatDuration, formatInteger } from '../../format';

  /** One coin's node: what the config says, how the pool sees it, and a probe button. */
  let { coin, live }: { coin: CoinSettings; live: CoinStatus | undefined } = $props();

  let probing = $state(false);
  let probe = $state<NodeProbe | null>(null);
  let probeError = $state<string | null>(null);

  async function test() {
    probing = true;
    probe = null;
    probeError = null;
    try {
      probe = await api.testNode(coin.key);
    } catch (err) {
      probeError = err instanceof Error ? err.message : String(err);
    } finally {
      probing = false;
    }
  }

  const health = $derived(live?.node);
  const reach = $derived(
    !health ? 'waiting' : !health.connected ? (health.stale ? 'stale' : 'unreachable') : 'connected',
  );
  const reachClass = $derived(reach === 'connected' ? 'ok' : reach === 'waiting' ? '' : 'bad');
</script>

<div class="card">
  <div class="title">
    <strong>{coin.symbol}</strong>
    <span class="muted">{coin.chain}</span>
    {#if coin.merge_mined_with}
      <span class="badge">merge-mined with {coin.merge_mined_with.toUpperCase()}</span>
    {:else}
      <span class="badge">parent chain</span>
    {/if}
    <span class="badge {reachClass}">{reach}</span>
  </div>
  <dl>
    <dt>RPC</dt>
    <dd><code>{coin.rpc_url}</code> as <code>{coin.rpc_user}</code></dd>
    <dt>ZMQ</dt>
    <dd>
      {#if coin.zmq_hashblock}
        <code>{coin.zmq_hashblock}</code>
        {#if health?.zmq === true}<span class="badge ok">subscribed</span>{:else if health?.zmq === false}<span class="badge warn">down, polling</span>{/if}
      {:else}
        <span class="muted">not configured; polling every {coin.poll_interval_ms} ms</span>
      {/if}
    </dd>
    <dt>Timing</dt>
    <dd class="muted">
      poll {coin.poll_interval_ms} ms · refresh {coin.template_refresh_secs} s · withdraw after {coin.template_stale_secs} s unreachable
    </dd>
    {#if live}
      <dt>Height</dt>
      <dd class="num">{formatInteger(live.height)} <span class="muted">template {formatDuration(live.template_age_seconds)} old</span></dd>
    {/if}
    {#if health?.last_error}
      <dt>Last error</dt>
      <dd class="bad">{health.last_error}{#if health.last_ok_seconds != null}<span class="muted"> · last answered {formatDuration(health.last_ok_seconds)} ago</span>{/if}</dd>
    {/if}
  </dl>
  <div class="actions">
    <button class="ghost" onclick={test} disabled={probing}>{probing ? 'Testing…' : 'Test connection'}</button>
    {#if probe}
      <span class="result ok">
        {probe.subversion || 'answered'} · {probe.chain} · height {formatInteger(probe.height)} · {probe.connections} peers
        · {probe.latency_ms} ms{#if probe.initial_block_download} · <strong>still syncing</strong>{/if}
      </span>
    {:else if probeError}
      <span class="result bad">{probeError}</span>
    {/if}
  </div>
</div>

<style>
  .card {
    background: var(--panel-2);
    border-radius: 10px;
    padding: 0.8rem 1rem;
  }
  .title {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem;
    margin-bottom: 0.4rem;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.25rem 0.8rem;
    margin: 0;
    font-size: 0.88rem;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.9em;
  }
  .bad {
    color: var(--bad);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.7rem;
    font-size: 0.85rem;
  }
  .result.ok {
    color: var(--ok);
  }
  .result.bad {
    color: var(--bad);
  }
</style>
