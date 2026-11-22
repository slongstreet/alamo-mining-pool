<script lang="ts">
  import type { LaneStatus } from '../api';
  import { laneName, laneSymbols } from '../lanes';

  /** What a lane that is not running says in place of its block odds. */
  let { lane }: { lane: LaneStatus } = $props();

  const nodes = $derived(laneSymbols(lane).join(' and '));
</script>

{#if lane.state === 'failed'}
  <section class="panel notice bad" role="alert">
    <h2>{laneName(lane)} lane failed <span class="badge bad">failed</span></h2>
    <p class="error">{lane.error ?? 'The lane could not start.'}</p>
    <p class="muted">
      Fix the <code>[coins.{lane.key}]</code> section of the config file (and
      <code>[coins.{lane.key}.stratum]</code>, if it has one), then restart the daemon. Until then this
      lane mines nothing; other lanes keep running.
    </p>
  </section>
{:else if lane.state === 'connecting'}
  <section class="panel notice">
    <h2>Block odds <span class="badge warn">waiting for node</span></h2>
    <p>
      The {laneName(lane)} lane is waiting for its {nodes} node{laneSymbols(lane).length === 1 ? '' : 's'} to
      answer. It keeps retrying; block odds appear once the node sends its first block template.
    </p>
    <p class="muted">The figures shown are restored from the pool's database.</p>
  </section>
{/if}

<style>
  .notice {
    border-left: 3px solid var(--warn);
  }
  .notice.bad {
    border-left-color: var(--bad);
  }
  h2 {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem;
  }
  p {
    margin: 0 0 0.5rem;
    font-size: 0.9rem;
  }
  p:last-child {
    margin-bottom: 0;
  }
  .error {
    color: var(--bad);
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.85rem;
    overflow-wrap: anywhere;
  }
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.9em;
  }
</style>
