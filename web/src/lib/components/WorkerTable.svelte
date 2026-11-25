<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { api, type LaneStatus, type WorkerStatus } from '../api';
  import { formatClock, formatDifficulty, formatDuration, formatHashrate, formatInteger, shortAddress, shortWorker } from '../format';
  import { unitsHelp } from '../lanes';
  import { dashboardReadOnly } from '../readonly';
  import {
    REJECT_RATE_WARN_PERCENT,
    aliasMap,
    isQuiet,
    nextSort,
    parseSort,
    quietHelp,
    quietThreshold,
    rejectRate,
    rejectsHelp,
    sortWorkers,
    type WorkerSort,
    type WorkerSortKey,
  } from '../workers';
  import Info from './Info.svelte';
  import MinerSetup from './MinerSetup.svelte';

  /** The selected lane's workers, with removal, aliases, sorting, and connection instructions. */
  let { lane }: { lane: LaneStatus } = $props();

  const workers = $derived(lane.workers);
  const aliases = $derived(aliasMap(workers));
  const target = $derived(lane.target_share_seconds);
  const quietAfter = $derived(quietThreshold(target));
  const diffHelp = $derived(
    'The share difficulty the pool currently asks this worker for, in stratum units: the number ' +
      `the miner itself reports. Best is in network units, comparable to the network difficulty. ${unitsHelp(lane)}`,
  );

  // Sorting: null is the daemon's order (connected first, then natural name order).
  const SORT_STORAGE = 'alamo.workers.sort';
  function loadSort(): WorkerSort | null {
    try {
      return parseSort(localStorage.getItem(SORT_STORAGE));
    } catch {
      return null;
    }
  }
  let sort = $state<WorkerSort | null>(loadSort());
  function sortBy(key: WorkerSortKey) {
    sort = nextSort(sort, key);
    try {
      if (sort) localStorage.setItem(SORT_STORAGE, JSON.stringify(sort));
      else localStorage.removeItem(SORT_STORAGE);
    } catch {
      // Not remembered; the sort still applies.
    }
  }
  const rows = $derived(sort ? sortWorkers(workers, sort, aliases) : workers);
  function ariaSort(key: WorkerSortKey): 'ascending' | 'descending' | undefined {
    if (sort?.key !== key) return undefined;
    return sort.dir === 'asc' ? 'ascending' : 'descending';
  }
  function arrow(key: WorkerSortKey): string {
    if (sort?.key !== key) return '';
    return sort.dir === 'asc' ? '▲' : '▼';
  }

  // Edit and remove controls are hidden until the settings say the dashboard is writable,
  // so a read-only one never flashes a control that would only get a 403.
  let readOnly = $state(true);
  onMount(() => {
    void dashboardReadOnly().then((ro) => (readOnly = ro));
  });

  /** Workers whose removal is in flight; the row disappears with the next snapshot. */
  let removing = $state<Set<string>>(new Set());
  let actionError = $state<string | null>(null);

  async function remove(w: WorkerStatus) {
    if (!confirm(`Remove ${w.name} from the workers table?\n\nIts share counts, share log, and hashrate history are dropped. Blocks it found and the pool's lifetime work are kept. If it connects again it starts fresh.`)) return;
    removing = new Set(removing).add(w.name);
    actionError = null;
    try {
      await api.removeWorker(w.name, lane.key);
    } catch (err) {
      actionError = err instanceof Error ? err.message : String(err);
      const next = new Set(removing);
      next.delete(w.name);
      removing = next;
    }
  }

  // Alias editing: one row at a time.
  let editing = $state<string | null>(null);
  let draft = $state('');
  let saving = $state(false);
  let input = $state<HTMLInputElement | null>(null);

  async function startEdit(w: WorkerStatus) {
    editing = w.name;
    draft = w.alias ?? '';
    actionError = null;
    await tick();
    input?.focus();
    input?.select();
  }

  function cancelEdit() {
    if (!saving) editing = null;
  }

  async function saveEdit(name: string) {
    if (saving) return;
    const alias = draft.trim();
    saving = true;
    try {
      // The daemon applies it at once and publishes a snapshot carrying it.
      await api.setWorkerAlias(name, alias === '' ? null : alias, lane.key);
      editing = null;
    } catch (err) {
      actionError = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }

  function onKey(e: KeyboardEvent, name: string) {
    if (e.key === 'Enter') {
      e.preventDefault();
      void saveEdit(name);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelEdit();
    }
  }
</script>

{#snippet header(key: WorkerSortKey, label: string)}
  <button class="sort" onclick={() => sortBy(key)}>
    {label}{#if arrow(key)}<span class="arrow" aria-hidden="true">{arrow(key)}</span>{/if}
  </button>
{/snippet}

<section class="panel">
  <h2>Workers</h2>
  {#if workers.length === 0}
    <p class="empty">No workers yet. Point a miner at the pool like this:</p>
    <MinerSetup {lane} />
  {:else}
    {#if actionError}<div class="bad small">{actionError}</div>{/if}
    <div class="scroll">
      <table>
        <thead>
          <tr>
            <th aria-sort={ariaSort('name')}>{@render header('name', 'Worker')}</th>
            <th>Pays</th>
            <th class="r" aria-sort={ariaSort('hashrate')}>{@render header('hashrate', 'Hashrate')}</th>
            <th class="r" aria-sort={ariaSort('difficulty')}>
              {@render header('difficulty', 'Diff')} <Info text={diffHelp} align="right" />
            </th>
            <th class="r" aria-sort={ariaSort('shares')}>{@render header('shares', 'Shares')}</th>
            <th class="r" aria-sort={ariaSort('best')}>{@render header('best', 'Best')}</th>
            <th class="r" aria-sort={ariaSort('last_share')}>{@render header('last_share', 'Last share')}</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each rows as w (w.name)}
            {@const quiet = isQuiet(w, quietAfter)}
            {@const rate = rejectRate(w)}
            <tr class:offline={w.connections === 0} class:quiet>
              <td class="name">
                <span class="dot" class:on={w.connections > 0} class:warn={quiet}></span>
                {#if editing === w.name}
                  <input
                    bind:this={input}
                    bind:value={draft}
                    class="alias-input"
                    maxlength="32"
                    placeholder={shortWorker(w.name)}
                    aria-label="Alias for {w.name} (empty clears it)"
                    title="Enter saves, Escape cancels; empty clears the alias"
                    disabled={saving}
                    onkeydown={(e) => onKey(e, w.name)}
                    onblur={cancelEdit}
                  />
                {:else if w.alias}
                  <span class="label" title={w.name}>{w.alias}</span>
                  <span class="mono muted secondary" title={w.name}>{shortWorker(w.name)}</span>
                {:else}
                  <span class="mono label" title={w.name}>{shortWorker(w.name)}</span>
                {/if}
                {#if w.connections > 1}<span class="badge">{w.connections}×</span>{/if}
                {#if quiet}<span class="badge warn" title={quietHelp(w, target)}>quiet</span>{/if}
                {#if !readOnly && editing !== w.name}
                  <button
                    class="ghost edit"
                    onclick={() => startEdit(w)}
                    title={w.alias ? 'Rename this worker' : 'Give this worker a name'}
                    aria-label="Set alias for {w.name}"
                  >✎</button>
                {/if}
              </td>
              <td class="pays">
                <span class="mono" title={w.address}>{shortAddress(w.address)}</span>
                {#if w.fallback}<span class="badge warn" title="Username was not a valid address; the configured fallback is paid">fallback</span>{/if}
                {#each w.aux_payouts as aux (aux.coin)}
                  <span class="mono muted" title={aux.address}> · {aux.coin} {shortAddress(aux.address)}</span>
                  {#if aux.fallback}<span class="badge warn">fallback</span>{/if}
                {/each}
              </td>
              <td class="r num">{formatHashrate(w.hashrate)}</td>
              <td class="r num">{formatDifficulty(w.difficulty)}</td>
              <td class="r num">
                {formatInteger(w.shares_accepted)}
                {#if w.shares_rejected}
                  <span class="bad" title={rejectsHelp(w)}> / {formatInteger(w.shares_rejected)}</span>
                  {#if rate}
                    <span
                      class="rate"
                      class:warn={rate > REJECT_RATE_WARN_PERCENT}
                      class:muted={rate <= REJECT_RATE_WARN_PERCENT}
                      title="Rejected as a share of all shares, lifetime">({rate.toFixed(rate < 1 ? 2 : 1)}%)</span
                    >
                  {/if}
                {/if}
              </td>
              <td class="r num" title={w.best_at == null ? undefined : formatClock(w.best_at)}>{formatDifficulty(w.best_difficulty)}</td>
              <td class="r num muted">{w.last_share_seconds == null ? '—' : `${formatDuration(w.last_share_seconds)} ago`}</td>
              <td class="r">
                {#if w.connections === 0 && !readOnly}
                  <button
                    class="ghost remove"
                    onclick={() => remove(w)}
                    disabled={removing.has(w.name)}
                    title="Remove this worker from the table"
                    aria-label="Remove {w.name}"
                  >
                    {removing.has(w.name) ? '…' : '×'}
                  </button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <details class="connect">
      <summary>Connect another miner</summary>
      <MinerSetup {lane} />
    </details>
  {/if}
</section>

<style>
  .dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
    margin-right: 0.5rem;
    vertical-align: middle;
    opacity: 0.5;
  }
  .dot.on {
    background: var(--ok);
    opacity: 1;
  }
  .dot.on.warn {
    background: var(--warn);
  }
  tr.offline td {
    color: var(--muted);
  }
  /* Names are already shortened to `ltc1q3…x7k2.rig1`; the ellipsis is only a backstop
     for very long worker parts. */
  .name {
    max-width: 20rem;
    white-space: nowrap;
  }
  .name .label {
    display: inline-block;
    max-width: calc(100% - 1.5rem);
    overflow: hidden;
    text-overflow: ellipsis;
    vertical-align: bottom;
  }
  .name .secondary {
    font-size: 0.8em;
    margin-left: 0.35rem;
  }
  @media (max-width: 640px) {
    .name {
      max-width: 14rem;
    }
    .name .secondary {
      display: none;
    }
  }
  .alias-input {
    font: inherit;
    width: 12rem;
    max-width: calc(100% - 1.5rem);
    padding: 0 0.3rem;
  }
  .remove,
  .edit {
    padding: 0 0.45rem;
    line-height: 1.4;
    font-size: 0.9rem;
  }
  .edit {
    margin-left: 0.35rem;
    font-size: 0.8rem;
  }
  .small {
    font-size: 0.8rem;
    margin-bottom: 0.5rem;
  }
  .pays {
    max-width: 18rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .bad {
    color: var(--bad);
  }
  .warn {
    color: var(--warn);
  }
  .rate {
    margin-left: 0.25rem;
    font-size: 0.85em;
  }
  .badge {
    margin-left: 0.35rem;
  }
  /* Column headers are buttons for keyboard sorting but look like plain headers. */
  .sort {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: inherit;
    text-transform: inherit;
    letter-spacing: inherit;
    cursor: pointer;
  }
  .sort:hover,
  .sort:focus-visible {
    color: var(--text);
  }
  .arrow {
    margin-left: 0.25rem;
    font-size: 0.75em;
  }
  .connect {
    margin-top: 0.9rem;
  }
  .connect summary {
    cursor: pointer;
    color: var(--muted);
    font-size: 0.85rem;
  }
  .connect summary:hover {
    color: var(--text);
  }
</style>
