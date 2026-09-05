<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type HashrateSample } from '../api';
  import { formatClock, formatHashrate } from '../format';

  let { live, workers }: { live: number; workers: string[] } = $props();

  const spans = [
    { label: '1h', secs: 3600 },
    { label: '24h', secs: 86_400 },
    { label: '7d', secs: 7 * 86_400 },
    { label: '30d', secs: 30 * 86_400 },
  ];
  let span = $state(86_400);
  let worker = $state('');
  let samples = $state<HashrateSample[]>([]);
  let width = $state(600);
  let hover = $state<number | null>(null);
  const height = 190;
  const pad = { top: 12, right: 12, bottom: 24, left: 56 };

  async function load() {
    try {
      samples = await api.hashrate(span, worker);
    } catch {
      // Keep the last good series; the header shows the connection state.
    }
  }

  onMount(() => {
    load();
    const timer = setInterval(load, 60_000);
    return () => clearInterval(timer);
  });
  $effect(() => {
    // Re-fetch when the selection changes.
    void span;
    void worker;
    load();
  });

  const points = $derived.by(() => {
    if (samples.length === 0) return [];
    const now = Math.floor(Date.now() / 1000);
    const start = now - span;
    return samples.filter((s) => s.ts >= start);
  });
  const maxY = $derived(Math.max(live, ...points.map((p) => p.hashrate)) * 1.15 || 1);
  const x = $derived((ts: number) => {
    const now = Math.floor(Date.now() / 1000);
    const start = now - span;
    return pad.left + ((ts - start) / span) * (width - pad.left - pad.right);
  });
  const y = $derived((h: number) => pad.top + (1 - h / maxY) * (height - pad.top - pad.bottom));
  const line = $derived(points.map((p, i) => `${i ? 'L' : 'M'}${x(p.ts).toFixed(1)},${y(p.hashrate).toFixed(1)}`).join(' '));
  const area = $derived(
    points.length
      ? `${line} L${x(points[points.length - 1].ts).toFixed(1)},${y(0)} L${x(points[0].ts).toFixed(1)},${y(0)} Z`
      : '',
  );
  const ticks = $derived([0.25, 0.5, 0.75, 1].map((f) => ({ v: maxY * f, y: y(maxY * f) })));
  const hovered = $derived(hover == null ? null : points[hover]);

  function onMove(e: MouseEvent) {
    if (points.length === 0) return;
    const rect = (e.currentTarget as SVGElement).getBoundingClientRect();
    const px = e.clientX - rect.left;
    let best = 0;
    let dist = Infinity;
    points.forEach((p, i) => {
      const d = Math.abs(x(p.ts) - px);
      if (d < dist) {
        dist = d;
        best = i;
      }
    });
    hover = best;
  }
</script>

<section class="panel">
  <div class="head">
    <h2>Hashrate</h2>
    <div class="controls">
      {#if workers.length}
        <select bind:value={worker} class="ghost">
          <option value="">Pool</option>
          {#each workers as w (w)}
            <option value={w}>{w}</option>
          {/each}
        </select>
      {/if}
      {#each spans as s (s.secs)}
        <button class="ghost" aria-pressed={span === s.secs} onclick={() => (span = s.secs)}>{s.label}</button>
      {/each}
    </div>
  </div>
  <div class="chart" bind:clientWidth={width}>
    <svg viewBox="0 0 {width} {height}" role="img" aria-label="Hashrate over time" onmousemove={onMove} onmouseleave={() => (hover = null)}>
      {#each ticks as t (t.v)}
        <line x1={pad.left} x2={width - pad.right} y1={t.y} y2={t.y} class="grid" />
        <text x={pad.left - 8} y={t.y + 4} class="tick">{formatHashrate(t.v)}</text>
      {/each}
      {#if points.length > 1}
        <path d={area} class="area" />
        <path d={line} class="line" />
      {:else if points.length === 1}
        <circle cx={x(points[0].ts)} cy={y(points[0].hashrate)} r="3" class="dot" />
      {/if}
      {#if hovered}
        <line x1={x(hovered.ts)} x2={x(hovered.ts)} y1={pad.top} y2={height - pad.bottom} class="cursor" />
        <circle cx={x(hovered.ts)} cy={y(hovered.hashrate)} r="4" class="dot" />
      {/if}
      <text x={pad.left} y={height - 6} class="tick left">{formatClock(Math.floor(Date.now() / 1000) - span)}</text>
      <text x={width - pad.right} y={height - 6} class="tick">now</text>
    </svg>
    <div class="legend muted">
      {#if hovered}
        <span class="num">{formatClock(hovered.ts)} · {formatHashrate(hovered.hashrate)}</span>
      {:else if points.length === 0}
        <span>No samples yet. The pool records one per minute while workers submit shares.</span>
      {:else}
        <span class="num">{points.length} samples · live {formatHashrate(live)}</span>
      {/if}
    </div>
  </div>
</section>

<style>
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1rem;
    flex-wrap: wrap;
    margin-bottom: 0.5rem;
  }
  .head h2 {
    margin: 0;
  }
  .controls {
    display: flex;
    gap: 0.35rem;
    flex-wrap: wrap;
  }
  select.ghost {
    background: var(--panel-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.3rem 0.5rem;
    font: inherit;
    font-size: 0.85rem;
    max-width: 14rem;
  }
  .chart {
    width: 100%;
  }
  svg {
    display: block;
    width: 100%;
    height: auto;
    overflow: visible;
  }
  .grid {
    stroke: var(--grid);
    stroke-width: 1;
  }
  .tick {
    fill: var(--muted);
    font-size: 11px;
    text-anchor: end;
  }
  .tick.left {
    text-anchor: start;
  }
  .line {
    fill: none;
    stroke: var(--accent);
    stroke-width: 2;
    stroke-linejoin: round;
  }
  .area {
    fill: var(--accent);
    opacity: 0.12;
  }
  .dot {
    fill: var(--accent);
  }
  .cursor {
    stroke: var(--muted);
    stroke-dasharray: 3 3;
  }
  .legend {
    font-size: 0.8rem;
    margin-top: 0.25rem;
    min-height: 1.2rem;
  }
</style>
