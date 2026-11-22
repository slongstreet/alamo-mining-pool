<script lang="ts">
  import { onMount } from 'svelte';
  import {
    api,
    ApiError,
    downloads,
    type CoinSettings,
    type LaneSettings,
    type SettingsDoc,
    type SettingsPatch,
    type Status,
  } from '../../api';
  import Field from './Field.svelte';
  import NodeCard from './NodeCard.svelte';
  import VardiffCard from './VardiffCard.svelte';
  import { algorithmLabel, laneName } from '../../lanes';
  import { formatInteger } from '../../format';

  /**
   * Operator settings. Every change goes to the daemon as a patch; the daemon validates
   * the whole patch, stores it, applies it live, and returns the new document, which
   * replaces the drafts here.
   */
  let { status }: { status: Status | null } = $props();

  let doc = $state<SettingsDoc | null>(null);
  let loadError = $state<string | null>(null);
  let waiting = $state(false);

  // Drafts, as the inputs hold them.
  let poolName = $state('');
  let tag = $state('');
  let fallback = $state<Record<string, string>>({});

  const logLevels = ['error', 'warn', 'info', 'debug', 'trace'];

  function adopt(d: SettingsDoc) {
    doc = d;
    poolName = d.pool_name.value;
    tag = d.coinbase_tag.value;
    fallback = Object.fromEntries(d.coins.map((c) => [c.key, c.fallback_address.value]));
  }

  async function load() {
    try {
      adopt(await api.settings());
      loadError = null;
      waiting = false;
    } catch (err) {
      if (err instanceof ApiError && err.status === 503) {
        waiting = true;
        loadError = null;
      } else {
        loadError = err instanceof Error ? err.message : String(err);
      }
    }
  }

  onMount(() => {
    load();
    // Until the pool has connected to its nodes there is nothing to show; keep asking.
    const timer = setInterval(() => {
      if (!doc) load();
    }, 5000);
    return () => clearInterval(timer);
  });

  /** 'pool', 'payouts', 'logs', 'data', or `mining:<lane>`. */
  type Section = string;
  let busy = $state<Section | null>(null);
  let notice = $state<Record<Section, { kind: 'ok' | 'bad'; text: string } | undefined>>({});

  async function apply(section: Section, patch: SettingsPatch, okText = 'Saved') {
    busy = section;
    notice[section] = undefined;
    try {
      adopt(await api.updateSettings(patch));
      notice[section] = { kind: 'ok', text: okText };
    } catch (err) {
      notice[section] = { kind: 'bad', text: err instanceof Error ? err.message : String(err) };
    } finally {
      busy = null;
    }
  }

  const readOnly = $derived(doc?.read_only ?? false);
  const locked = $derived(readOnly || busy !== null);
  const tagBytes = $derived(new TextEncoder().encode(tag).length);
  const multiLane = $derived((doc?.lanes.length ?? 0) > 1);

  // Lane names and algorithms come from the live status; the settings document has the
  // listener and vardiff.
  function liveLane(key: string) {
    return status?.lanes.find((l) => l.key === key);
  }
  function laneTitle(key: string): string {
    const l = liveLane(key);
    return l ? laneName(l) : key.toUpperCase();
  }
  function portOf(listen: string): string {
    return listen.slice(listen.lastIndexOf(':') + 1);
  }
  function liveCoin(lane: string, symbol: string) {
    return liveLane(lane)?.coins.find((c) => c.symbol === symbol);
  }
  const groups = $derived(
    (doc?.lanes ?? []).map((l) => ({ lane: l, coins: doc!.coins.filter((c) => c.lane === l.key) })),
  );

  const poolDirty = $derived(!!doc && (poolName !== doc.pool_name.value || tag !== doc.coinbase_tag.value));
  function savePool() {
    if (!doc) return;
    const patch: SettingsPatch = {};
    if (poolName !== doc.pool_name.value) patch.pool_name = poolName;
    if (tag !== doc.coinbase_tag.value) patch.coinbase_tag = tag;
    apply('pool', patch);
  }

  // A coin whose lane has not connected cannot take a fallback change: the daemon checks
  // the address against the node's network, which is not known yet.
  const payoutsDirty = $derived(
    !!doc && doc.coins.some((c) => c.connected && fallback[c.key] !== c.fallback_address.value),
  );
  function savePayouts() {
    if (!doc) return;
    const changed = doc.coins.filter((c) => c.connected && fallback[c.key] !== c.fallback_address.value);
    apply('payouts', { fallback_addresses: Object.fromEntries(changed.map((c) => [c.key, fallback[c.key]])) });
  }
  function fallbackHint(c: CoinSettings): string {
    const role = c.merge_mined_with
      ? `Paid when the stratum password carries no valid ${c.symbol} address.`
      : `Paid when the stratum username is not a valid ${c.symbol} address.`;
    if (!c.connected && liveLane(c.lane)?.state === 'failed') {
      return `${role} The ${laneTitle(c.lane)} lane failed to start, so this cannot be changed here (the address is checked against the node's network); fix the config file and restart.`;
    }
    if (!c.connected) {
      return `${role} It can be changed once the ${laneTitle(c.lane)} lane has connected: the address is checked against the node's network, which is not known yet.`;
    }
    return `${role} Must be a ${c.chain} address.`;
  }

  /** Lanes on the default [stratum] section share its vardiff overrides. */
  const sharedLanes = $derived((doc?.lanes ?? []).filter((l) => !l.own_stratum_section));
  function vardiffTitle(l: LaneSettings): string {
    const live = liveLane(l.key);
    const who = live ? `${algorithmLabel(live.algorithm)} miners` : `${l.key.toUpperCase()} miners`;
    return `Vardiff — ${who}, port ${portOf(l.stratum_listen)}`;
  }
  function vardiffLead(l: LaneSettings): string {
    const live = liveLane(l.key);
    const m = live?.share_multiplier ?? 0;
    const units =
      m === 1
        ? `Variable difficulty, in the units ${algorithmLabel(live!.algorithm)} miners display: the same as network difficulty.`
        : m > 1
          ? `Variable difficulty, in the units ${algorithmLabel(live!.algorithm)} miners display: difficulty ${formatInteger(m)} is one unit of network difficulty.`
          : 'Variable difficulty, in the units miners display.';
    let text = `${units} Changes apply to miners that connect from now on.`;
    if (!l.own_stratum_section && sharedLanes.length > 1) {
      const others = sharedLanes.filter((o) => o.key !== l.key).map((o) => `${laneTitle(o.key)} lane`);
      text += ` This lane uses the default [stratum] section together with the ${others.join(' and ')}: a change here changes ${others.length === 1 ? 'both' : 'all of them'}.`;
    }
    return text;
  }

  let resetting = $state<string | null>(null);
  async function resetStats(lane: string) {
    const which = multiLane ? ` in the ${laneTitle(lane)} lane` : '';
    if (!confirm(`Reset accepted/rejected share counts and best share for every worker${which}?\n\nHashrate history, accepted work, rounds, and blocks are kept.`)) return;
    resetting = lane;
    notice.data = undefined;
    try {
      await api.resetStats(lane);
      notice.data = { kind: 'ok', text: multiLane ? `${laneTitle(lane)} lane: share counts and best share reset` : 'Share counts and best share reset' };
    } catch (err) {
      notice.data = { kind: 'bad', text: err instanceof Error ? err.message : String(err) };
    } finally {
      resetting = null;
    }
  }

  const btcConfigured = $derived(doc?.coins.some((c) => c.key === 'btc') ?? true);
  const btcSnippet = [
    '[coins.btc]',
    'enabled = true',
    'rpc_url = "http://127.0.0.1:8332"',
    'rpc_user = "alamo"',
    'rpc_password = "${BTC_RPC_PASSWORD}"',
    'zmq_hashblock = "tcp://127.0.0.1:28334"',
    'fallback_address = "bc1q..."',
    '',
    '[coins.btc.stratum]',
    'listen = "0.0.0.0:3334"',
  ].join('\n');
</script>

{#snippet noteFor(section: Section)}
  {#if notice[section]}
    <span class="notice {notice[section]!.kind}" role="status">{notice[section]!.text}</span>
  {/if}
{/snippet}

<div class="settings">
  <p class="intro muted">
    Changes are stored in the pool's database and override <code>alamo.toml</code>; each one can be reverted
    to the file's value. Node endpoints and credentials stay in the file.
  </p>

  {#if readOnly}
    <div class="banner">
      This dashboard is read-only (<code>[web] read_only = true</code>). Settings can be viewed but not changed.
    </div>
  {/if}

  {#if loadError}
    <section class="panel"><div class="empty">Cannot load settings: {loadError}</div></section>
  {:else if !doc}
    <section class="panel">
      <div class="empty">
        {#if waiting}
          The pool is still connecting to its nodes. Settings appear once it has.
        {:else}
          Loading settings…
        {/if}
      </div>
    </section>
  {:else}
    <section class="panel">
      <h2>Pool</h2>
      <Field
        label="Pool name"
        hint="Shown in the dashboard header."
        setting={doc.pool_name}
        disabled={locked}
        onrevert={() => apply('pool', { pool_name: null }, 'Reverted')}
      >
        <input class="field" type="text" maxlength="64" bind:value={poolName} disabled={locked} />
      </Field>
      <Field
        label="Coinbase tag"
        hint="Written into the coinbase transaction of every block this pool finds, so it is permanent once a block is mined. Takes effect on the next work template, within seconds."
        setting={doc.coinbase_tag}
        disabled={locked}
        onrevert={() => apply('pool', { coinbase_tag: null }, 'Reverted')}
      >
        <div class="with-counter">
          <input class="field mono" type="text" bind:value={tag} disabled={locked} spellcheck="false" />
          <span class="counter num" class:bad={tagBytes > doc.coinbase_tag_max_bytes}>
            {tagBytes} / {doc.coinbase_tag_max_bytes} bytes
          </span>
        </div>
      </Field>
      <div class="actions">
        <button class="primary" onclick={savePool} disabled={locked || !poolDirty || tagBytes > doc.coinbase_tag_max_bytes}>
          {busy === 'pool' ? 'Saving…' : 'Save'}
        </button>
        {@render noteFor('pool')}
      </div>
    </section>

    <section class="panel">
      <h2>Payouts</h2>
      <p class="lead muted">
        Miners are paid at the address they mine with. These addresses are paid instead when a miner gives
        none that is valid for the chain. Set them to addresses you control.
      </p>
      {#each groups as g (g.lane.key)}
        {#if multiLane}<h3>{laneTitle(g.lane.key)} lane</h3>{/if}
        {#each g.coins as coin (coin.key)}
          <Field
            label="{coin.symbol} fallback address"
            hint={fallbackHint(coin)}
            setting={coin.fallback_address}
            disabled={locked || !coin.connected}
            onrevert={() => apply('payouts', { fallback_addresses: { [coin.key]: null } }, 'Reverted')}
          >
            <input
              class="field mono"
              type="text"
              bind:value={fallback[coin.key]}
              disabled={locked || !coin.connected}
              spellcheck="false"
            />
          </Field>
        {/each}
      {/each}
      <div class="actions">
        <button class="primary" onclick={savePayouts} disabled={locked || !payoutsDirty}>
          {busy === 'payouts' ? 'Saving…' : 'Save'}
        </button>
        {@render noteFor('payouts')}
      </div>
    </section>

    <section class="panel">
      <h2>Nodes</h2>
      <p class="lead muted">
        Endpoints and credentials come from <code>alamo.toml</code>; on Umbrel the node apps supply them.
        They are not editable here because the dashboard has no login of its own.
      </p>
      {#each groups as g (g.lane.key)}
        {#if multiLane}
          {@const live = liveLane(g.lane.key)}
          <h3>
            {laneTitle(g.lane.key)} lane
            <span class="muted">
              {live ? `${algorithmLabel(live.algorithm)} · ` : ''}port {portOf(g.lane.stratum_listen)}
            </span>
          </h3>
        {/if}
        <div class="cards">
          {#each g.coins as coin (coin.key)}
            <NodeCard {coin} live={liveCoin(coin.lane, coin.symbol)} laneState={liveLane(coin.lane)?.state} />
          {/each}
        </div>
      {/each}
    </section>

    {#if !btcConfigured}
      <section class="panel">
        <h2>Bitcoin is not configured</h2>
        <p class="lead muted">
          To solo-mine Bitcoin on its own SHA-256 port, add this to <code>alamo.toml</code> (see
          <code>config/alamo.example.toml</code> for the optional vardiff table) and restart the pool. Node
          endpoints and credentials can only be set in the file, because the dashboard's API has no
          authentication.
        </p>
        <pre class="config">{btcSnippet}</pre>
      </section>
    {/if}

    {#each doc.lanes as l (l.key)}
      {@const section = `mining:${l.key}`}
      <VardiffCard
        title={vardiffTitle(l)}
        lead={vardiffLead(l)}
        vardiff={l.vardiff}
        disabled={locked}
        busy={busy === section}
        onsave={(changes) =>
          apply(section, { lanes: { [l.key]: { vardiff: changes } } }, 'Saved; applies to miners that connect from now on')}
        onrevert={(field) => apply(section, { lanes: { [l.key]: { vardiff: { [field]: null } } } }, 'Reverted')}
        oninvalid={(text) => (notice[section] = { kind: 'bad', text })}
      >
        {#snippet note()}
          {@render noteFor(section)}
        {/snippet}
      </VardiffCard>
    {/each}

    <section class="panel">
      <h2>Data</h2>
      {#each doc.lanes as l (l.key)}
        <div class="row">
          <div>
            <strong>Share counts and best share{multiLane ? ` — ${laneTitle(l.key)} lane` : ''}</strong>
            <p class="hint">
              {#if multiLane}
                Zero the accepted and rejected counts of every worker in this lane, and the lane's best share; other
                lanes are untouched. Hashrate history, accepted work, rounds, and blocks are kept.
              {:else}
                Zero every worker's accepted and rejected counts and the best share. Hashrate history, accepted work,
                rounds, and blocks are kept.
              {/if}
            </p>
          </div>
          <button
            class="danger"
            onclick={() => resetStats(l.key)}
            disabled={readOnly || resetting !== null}
            aria-label={multiLane ? `Reset share counts of the ${laneTitle(l.key)} lane` : undefined}
          >
            {resetting === l.key ? 'Resetting…' : 'Reset'}
          </button>
        </div>
      {/each}
      <div class="row">
        <div>
          <strong>Database backup</strong>
          <p class="hint">A consistent copy of <code>alamo.db</code>: workers, shares, hashrate history, blocks, and these settings. Restore by stopping the pool and copying it over the original.</p>
        </div>
        <a class="ghost" href={downloads.backup} download>Download</a>
      </div>
      {@render noteFor('data')}
    </section>

    <section class="panel">
      <h2>Diagnostics</h2>
      <Field
        label="Log level"
        hint="Applies immediately. Debug and trace show the pool's own detail without flooding from HTTP and database libraries. The file value is RUST_LOG, or info."
        setting={doc.log_level}
        disabled={locked}
        onrevert={() => apply('logs', { log_level: null }, 'Reverted')}
      >
        <select
          class="field"
          value={doc.log_level.value}
          disabled={locked}
          onchange={(e) => apply('logs', { log_level: (e.currentTarget as HTMLSelectElement).value }, 'Log level set')}
        >
          {#if !logLevels.includes(doc.log_level.value)}
            <option value={doc.log_level.value}>{doc.log_level.value} (from the config file)</option>
          {/if}
          {#each logLevels as level (level)}
            <option value={level}>{level}</option>
          {/each}
        </select>
      </Field>
      {@render noteFor('logs')}
      <div class="row">
        <div>
          <strong>Log</strong>
          <p class="hint">The most recent log lines the daemon holds in memory (about 2 MB). Node credentials are never logged.</p>
        </div>
        <a class="ghost" href={downloads.logs} download>Download</a>
      </div>
      <details>
        <summary>Effective configuration</summary>
        <pre class="config">{doc.config_toml}</pre>
      </details>
    </section>
  {/if}
</div>

<style>
  .settings {
    max-width: 820px;
    display: grid;
    gap: 1rem;
  }
  .intro,
  .lead {
    margin: 0 0 0.9rem;
    font-size: 0.88rem;
  }
  .intro {
    margin-bottom: 0;
  }
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.9em;
  }
  .banner {
    background: var(--panel-2);
    border-left: 3px solid var(--warn);
    border-radius: 8px;
    padding: 0.6rem 0.9rem;
    font-size: 0.88rem;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem;
  }
  .notice {
    font-size: 0.85rem;
  }
  .notice.ok {
    color: var(--ok);
  }
  .notice.bad {
    color: var(--bad);
  }
  .with-counter {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  .counter {
    flex: none;
    font-size: 0.8rem;
    color: var(--muted);
  }
  .counter.bad {
    color: var(--bad);
  }
  .cards {
    display: grid;
    gap: 0.75rem;
  }
  h3 {
    margin: 1.1rem 0 0.6rem;
    font-size: 0.9rem;
    font-weight: 600;
  }
  h3:first-of-type {
    margin-top: 0;
  }
  h3 .muted {
    font-weight: 400;
    font-size: 0.82rem;
    margin-left: 0.35rem;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1rem;
    padding: 0.6rem 0;
    border-top: 1px solid var(--border);
  }
  .row:first-of-type {
    border-top: 0;
    padding-top: 0;
  }
  .row strong {
    font-size: 0.9rem;
  }
  .row .hint,
  .hint {
    margin: 0.15rem 0 0;
    font-size: 0.8rem;
    color: var(--muted);
  }
  .row > a,
  .row > button {
    flex: none;
  }
  details {
    margin-top: 0.75rem;
  }
  summary {
    cursor: pointer;
    color: var(--muted);
    font-size: 0.85rem;
  }
  summary:hover {
    color: var(--text);
  }
  .config {
    margin: 0.5rem 0 0;
    padding: 0.75rem 0.9rem;
    background: var(--bg);
    border-radius: 8px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.8rem;
    line-height: 1.45;
    overflow-x: auto;
  }
</style>
