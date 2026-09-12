<script lang="ts">
  interface Props {
    label: string;
    value: string;
    sub?: string;
    tone?: 'default' | 'accent' | 'ok' | 'bad';
  }
  let { label, value, sub, tone = 'default' }: Props = $props();
</script>

<div class="tile panel">
  <div class="label">{label}</div>
  <div class="value num" data-tone={tone}>{value}</div>
  {#if sub}
    <div class="sub">{sub}</div>
  {/if}
</div>

<style>
  .tile {
    padding: 0.9rem 1.1rem;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  .label {
    font-size: 0.8rem;
    color: var(--muted);
  }
  .value {
    font-size: 1.55rem;
    font-weight: 600;
    letter-spacing: -0.02em;
    line-height: 1.2;
  }
  .value[data-tone='accent'] {
    color: var(--accent);
  }
  .value[data-tone='ok'] {
    color: var(--ok);
  }
  .value[data-tone='bad'] {
    color: var(--bad);
  }
  .sub {
    font-size: 0.8rem;
    color: var(--muted);
  }
</style>
