<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { Setting } from '../../api';

  /**
   * A labelled setting: the control, a hint, and, when an override is in force, a badge
   * with the file value and a way back to it.
   */
  let {
    label,
    hint = '',
    setting,
    fileLabel = (v: unknown) => String(v),
    disabled = false,
    onrevert,
    children,
  }: {
    label: string;
    hint?: string;
    setting: Setting<unknown>;
    fileLabel?: (value: unknown) => string;
    disabled?: boolean;
    onrevert: () => void;
    children: Snippet;
  } = $props();
</script>

<div class="field">
  <div class="label">
    <span>{label}</span>
    {#if setting.overridden}
      <span class="badge accent" title="Set from this page; the config file says {fileLabel(setting.file_value)}">override</span>
      <button type="button" class="link" onclick={onrevert} {disabled}>
        revert to file value ({fileLabel(setting.file_value)})
      </button>
    {/if}
  </div>
  {@render children()}
  {#if hint}<p class="hint">{hint}</p>{/if}
</div>

<style>
  .field {
    margin-bottom: 1rem;
  }
  .label {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem;
    margin-bottom: 0.3rem;
    font-size: 0.85rem;
    font-weight: 600;
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    font-size: 0.8rem;
    font-weight: 400;
    color: var(--accent-2);
    cursor: pointer;
    text-decoration: underline;
  }
  .link:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .hint {
    margin: 0.3rem 0 0;
    font-size: 0.8rem;
    color: var(--muted);
  }
</style>
