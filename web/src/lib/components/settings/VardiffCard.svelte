<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { VardiffField, VardiffSettings } from '../../api';
  import Field from './Field.svelte';

  /**
   * One lane's vardiff. Drafts follow the document: every new document (after any save,
   * including another lane's on a shared section) replaces them.
   */
  let {
    title,
    lead,
    vardiff,
    disabled,
    busy,
    onsave,
    onrevert,
    oninvalid,
    note,
  }: {
    title: string;
    lead: string;
    vardiff: VardiffSettings;
    disabled: boolean;
    busy: boolean;
    onsave: (changes: Partial<Record<VardiffField, number>>) => void;
    onrevert: (field: VardiffField) => void;
    oninvalid: (message: string) => void;
    note: Snippet;
  } = $props();

  const fields: { key: VardiffField; label: string; hint: string }[] = [
    { key: 'initial_difficulty', label: 'Starting difficulty', hint: 'Share difficulty a new connection begins at.' },
    { key: 'min_difficulty', label: 'Minimum difficulty', hint: 'Vardiff never goes below this.' },
    { key: 'max_difficulty', label: 'Maximum difficulty', hint: 'Vardiff never goes above this.' },
    { key: 'target_share_seconds', label: 'Target seconds per share', hint: 'Difficulty is tuned so each worker submits about one share per this many seconds.' },
    { key: 'retarget_seconds', label: 'Retarget interval (s)', hint: 'Minimum time between difficulty changes; each change is based on the last four intervals.' },
    { key: 'variance_percent', label: 'Variance (%)', hint: 'Only retarget when the observed rate is off by more than this.' },
  ];

  // Number inputs hand back a number (or null when cleared); compare and send as text.
  let drafts = $state<Record<VardiffField, string | number | null>>({
    initial_difficulty: '',
    min_difficulty: '',
    max_difficulty: '',
    target_share_seconds: '',
    retarget_seconds: '',
    variance_percent: '',
  });
  $effect.pre(() => {
    for (const f of fields) drafts[f.key] = String(vardiff[f.key].value);
  });

  const text = (f: VardiffField) => String(drafts[f] ?? '').trim();
  const dirty = $derived(fields.some((f) => text(f.key) !== String(vardiff[f.key].value)));

  function save() {
    const changes: Partial<Record<VardiffField, number>> = {};
    for (const f of fields) {
      const t = text(f.key);
      if (t === String(vardiff[f.key].value)) continue;
      const n = Number(t);
      if (t === '' || !Number.isFinite(n)) {
        oninvalid(`${f.label} must be a number`);
        return;
      }
      changes[f.key] = n;
    }
    onsave(changes);
  }
</script>

<section class="panel">
  <h2>{title}</h2>
  <p class="lead muted">{lead}</p>
  <div class="two-col">
    {#each fields as f (f.key)}
      <Field label={f.label} hint={f.hint} setting={vardiff[f.key]} {disabled} onrevert={() => onrevert(f.key)}>
        <input class="field num" type="number" inputmode="decimal" step="any" bind:value={drafts[f.key]} {disabled} />
      </Field>
    {/each}
  </div>
  <div class="actions">
    <button class="primary" onclick={save} disabled={disabled || !dirty}>
      {busy ? 'Saving…' : 'Save'}
    </button>
    {@render note()}
  </div>
</section>

<style>
  .lead {
    margin: 0 0 0.9rem;
    font-size: 0.88rem;
  }
  .two-col {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
    column-gap: 1.25rem;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem;
  }
</style>
