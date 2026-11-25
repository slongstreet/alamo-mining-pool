<script lang="ts">
  import { workerLabel, workerTitle, type WorkerAliases } from '../workers';

  /** A worker's alias when it has one (full name in the tooltip), else its shortened name. */
  let { name, aliases }: { name: string; aliases: WorkerAliases } = $props();

  const aliased = $derived(aliases.has(name));
</script>

<span class:mono={!aliased} title={workerTitle(name, aliases)}>{workerLabel(name, aliases)}</span>
