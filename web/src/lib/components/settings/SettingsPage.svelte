<script lang="ts">
  import { onMount } from 'svelte';
  import {
    api,
    ApiError,
    downloads,
    type SettingsDoc,
    type SettingsPatch,
    type Status,
    type VardiffField,
  } from '../../api';
  import Field from './Field.svelte';
  import NodeCard from './NodeCard.svelte';

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
  // Number inputs hand back a number (or null when cleared); compare and send as text.
  let vardiff = $state<Record<VardiffField, string | number | null>>({
    initial_difficulty: '',
    min_difficulty: '',
    max_difficulty: '',
    target_share_seconds: '',
    retarget_seconds: '',
    variance_percent: '',
  });

  const vardiffFields: { key: VardiffField; label: string; hint: string }[] = [
    { key: 'initial_difficulty', label: 'Starting difficulty', hint: 'Share difficulty a new connection begins at.' },
    { key: 'min_difficulty', label: 'Minimum difficulty', hint: 'Vardiff never goes below this.' },
    { key: 'max_difficulty', label: 'Maximum difficulty', hint: 'Vardiff never goes above this.' },
    { key: 'target_share_seconds', label: 'Target seconds per share', hint: 'Difficulty is tuned so each worker submits about one share per this many seconds.' },
    { key: 'retarget_seconds', label: 'Retarget interval (s)', hint: 'Minimum time between difficulty changes; each change is based on the last four intervals.' },
    { key: 'variance_percent', label: 'Variance (%)', hint: 'Only retarget when the observed rate is off by more than this.' },
  ];
  const logLevels = ['error', 'warn', 'info', 'debug', 'trace'];

  function adopt(d: SettingsDoc) {
    doc = d;
    poolName = d.pool_name.value;
    tag = d.coinbase_tag.value;
    fallback = Object.fromEntries(d.coins.map((c) => [c.key, c.fallback_address.value]));
    for (const f of vardiffFields) {
      vardiff[f.key] = String(d.vardiff[f.key].value);
    }
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

  type Section = 'pool' | 'payouts' | 'mining' | 'logs' | 'data';
  let busy = $state<Section | null>(null);
  let notice = $state<Partial<Record<Section, { kind: 'ok' | 'bad'; text: string }>>>({});

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

  const poolDirty = $derived(!!doc && (poolName !== doc.pool_name.value || tag !== doc.coinbase_tag.value));
  function savePool() {
    if (!doc) return;
    const patch: SettingsPatch = {};
    if (poolName !== doc.pool_name.value) patch.pool_name = poolName;
    if (tag !== doc.coinbase_tag.value) patch.coinbase_tag = tag;
    apply('pool', patch);
  }

  const payoutsDirty = $derived(!!doc && doc.coins.some((c) => fallback[c.key] !== c.fallback_address.value));
  function savePayouts() {
    if (!doc) return;
    const changed = doc.coins.filter((c) => fallback[c.key] !== c.fallback_address.value);
    apply('payouts', { fallback_addresses: Object.fromEntries(changed.map((c) => [c.key, fallback[c.key]])) });
  }

  const draftText = (f: VardiffField) => String(vardiff[f] ?? '').trim();
  const miningDirty = $derived(!!doc && vardiffFields.some((f) => draftText(f.key) !== String(doc!.vardiff[f.key].value)));
  function saveMining() {
    if (!doc) return;
    const patch: SettingsPatch = { vardiff: {} };
    for (const f of vardiffFields) {
      const text = draftText(f.key);
      if (text === String(doc.vardiff[f.key].value)) continue;
      const n = Number(text);
      if (text === '' || !Number.isFinite(n)) {
        notice.mining = { kind: 'bad', text: `${f.label} must be a number` };
        return;
      }
      patch.vardiff![f.key] = n;
    }
    apply('mining', patch, 'Saved; applies to miners that connect from now on');
  }

  let resetting = $state(false);
  async function resetStats() {
    if (!confirm('Reset accepted/rejected share counts and best share for every worker?\n\nHashrate history, accepted work, rounds, and blocks are kept.')) return;
    resetting = true;
    notice.data = undefined;
    try {
      await api.resetStats();
      notice.data = { kind: 'ok', text: 'Share counts and best share reset' };
    } catch (err) {
      notice.data = { kind: 'bad', text: err instanceof Error ? err.message : String(err) };
    } finally {
      resetting = false;
    }
  }

  function liveCoin(symbol: string) {
    return status?.coins.find((c) => c.symbol === symbol);
  }
</script>

{#snippet note(section: Section)}
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
        {@render note('pool')}
      </div>
    </section>

    <section class="panel">
      <h2>Payouts</h2>
      <p class="lead muted">
        Miners are paid at the address they mine with. These addresses are paid instead when a miner gives
        none that is valid for the chain. Set them to addresses you control.
      </p>
      {#each doc.coins as coin (coin.key)}
        <Field
          label="{coin.symbol} fallback address"
          hint={coin.merge_mined_with
            ? `Paid when the stratum password carries no valid ${coin.symbol} address. Must be a ${coin.chain} address.`
            : `Paid when the stratum username is not a valid ${coin.symbol} address. Must be a ${coin.chain} address.`}
          setting={coin.fallback_address}
          disabled={locked}
          onrevert={() => apply('payouts', { fallback_addresses: { [coin.key]: null } }, 'Reverted')}
        >
          <input class="field mono" type="text" bind:value={fallback[coin.key]} disabled={locked} spellcheck="false" />
        </Field>
      {/each}
      <div class="actions">
        <button class="primary" onclick={savePayouts} disabled={locked || !payoutsDirty}>
          {busy === 'payouts' ? 'Saving…' : 'Save'}
        </button>
        {@render note('payouts')}
      </div>
    </section>

    <section class="panel">
      <h2>Nodes</h2>
      <p class="lead muted">
        Endpoints and credentials come from <code>alamo.toml</code>; on Umbrel the Litecoin and Dogecoin
        apps supply them. They are not editable here because the dashboard has no login of its own.
      </p>
      <div class="cards">
        {#each doc.coins as coin (coin.key)}
          <NodeCard {coin} live={liveCoin(coin.symbol)} />
        {/each}
      </div>
    </section>

    <section class="panel">
      <h2>Mining</h2>
      <p class="lead muted">
        Variable difficulty, in the units miners display (for scrypt, difficulty 65536 is one unit of
        network difficulty). Changes apply to miners that connect from now on.
      </p>
      <div class="two-col">
        {#each vardiffFields as f (f.key)}
          <Field
            label={f.label}
            hint={f.hint}
            setting={doc.vardiff[f.key]}
            disabled={locked}
            onrevert={() => apply('mining', { vardiff: { [f.key]: null } }, 'Reverted')}
          >
            <input class="field num" type="number" inputmode="decimal" step="any" bind:value={vardiff[f.key]} disabled={locked} />
          </Field>
        {/each}
      </div>
      <div class="actions">
        <button class="primary" onclick={saveMining} disabled={locked || !miningDirty}>
          {busy === 'mining' ? 'Saving…' : 'Save'}
        </button>
        {@render note('mining')}
      </div>
    </section>

    <section class="panel">
      <h2>Data</h2>
      <div class="row">
        <div>
          <strong>Share counts and best share</strong>
          <p class="hint">Zero every worker's accepted and rejected counts and the best share. Hashrate history, accepted work, rounds, and blocks are kept.</p>
        </div>
        <button class="danger" onclick={resetStats} disabled={readOnly || resetting}>
          {resetting ? 'Resetting…' : 'Reset'}
        </button>
      </div>
      <div class="row">
        <div>
          <strong>Database backup</strong>
          <p class="hint">A consistent copy of <code>alamo.db</code>: workers, shares, hashrate history, blocks, and these settings. Restore by stopping the pool and copying it over the original.</p>
        </div>
        <a class="ghost" href={downloads.backup} download>Download</a>
      </div>
      {@render note('data')}
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
      {@render note('logs')}
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
  .two-col {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
    column-gap: 1.25rem;
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
