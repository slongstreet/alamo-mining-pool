<script lang="ts">
  import type { LaneStatus } from '../api';
  import { formatDuration, formatHashrate } from '../format';
  import { laneHealth, laneName, laneTagline, onlineWorkers, parentExpectedSeconds } from '../lanes';

  /** One selectable card per lane; the page below follows the selected one. */
  let {
    lanes,
    selected,
    onselect,
  }: { lanes: LaneStatus[]; selected: string; onselect: (key: string) => void } = $props();
</script>

<div class="lanes" role="group" aria-label="Lanes">
  {#each lanes as lane (lane.key)}
    {@const health = laneHealth(lane)}
    {@const online = onlineWorkers(lane)}
    {@const eta = parentExpectedSeconds(lane)}
    <button
      type="button"
      class="lane"
      class:failed={lane.state === 'failed'}
      aria-pressed={lane.key === selected}
      aria-describedby="lane-health-{lane.key}"
      onclick={() => onselect(lane.key)}
    >
      <span class="top">
        <strong class="name">{laneName(lane)}</strong>
        <span class="badge {health.kind}" title={health.detail}>{health.label}</span>
      </span>
      <span class="muted tagline">{laneTagline(lane)}</span>
      {#if lane.state === 'failed'}
        <span class="figures bad" title={health.detail}>{health.detail}</span>
      {:else}
        <span class="figures num">
          {formatHashrate(lane.hashrate)} · {online} worker{online === 1 ? '' : 's'} ·
          {eta === undefined ? 'no template yet' : `block in ${formatDuration(eta)}`}
        </span>
      {/if}
      <span class="sr-only" id="lane-health-{lane.key}">{health.label}: {health.detail}</span>
    </button>
  {/each}
</div>

<style>
  .lanes {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 260px), 1fr));
    gap: 1rem;
  }
  .lane {
    all: unset;
    box-sizing: border-box;
    position: relative;
    display: grid;
    gap: 0.15rem;
    min-width: 0;
    background: var(--panel);
    border: 1px solid transparent;
    border-radius: var(--radius);
    padding: 0.8rem 1.1rem;
    box-shadow: var(--shadow);
    color: var(--text);
    cursor: pointer;
    text-align: left;
  }
  .lane:hover {
    border-color: var(--border);
  }
  .lane:focus-visible {
    outline: 2px solid var(--accent-2);
    outline-offset: 2px;
  }
  .lane[aria-pressed='true'] {
    border-color: var(--accent);
    box-shadow: var(--shadow), inset 3px 0 0 var(--accent);
  }
  .top {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.6rem;
    min-width: 0;
  }
  .name {
    font-size: 1rem;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .top .badge {
    flex: none;
  }
  .tagline {
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .figures {
    font-size: 0.88rem;
    margin-top: 0.2rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .figures.bad {
    color: var(--bad);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
