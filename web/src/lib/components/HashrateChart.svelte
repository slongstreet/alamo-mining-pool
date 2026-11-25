<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { api, type HashrateSample, type WorkerStatus } from '../api';
  import { formatClock, formatHashrate } from '../format';
  import { axisTicks, chartTop } from '../scale';
  import { aliasMap, workerLabel } from '../workers';

  /**
   * One lane's hashrate history: scrypt and SHA-256 rates do not add up. `workers` are the
   * lane's, from the snapshot, for the worker selector (shown by alias where there is one).
   */
  let {
    live,
    workers,
    lane,
    allLabel = 'Pool',
  }: { live: number; workers: WorkerStatus[]; lane: string; allLabel?: string } = $props();

  const aliases = $derived(aliasMap(workers));

  const spans = [
    { label: '1h', secs: 3600 },
    { label: '24h', secs: 86_400 },
    { label: '7d', secs: 7 * 86_400 },
    { label: '30d', secs: 30 * 86_400 },
  ];
  // The parent rebuilds its props from every snapshot, so effects read the lane through a
  // derived primitive, which only notifies when the key itself changes.
  const laneKey = $derived(lane);
  let span = $state(86_400);
  let worker = $state('');
  let samples = $state<HashrateSample[]>([]);
  let width = $state(600);
  let hover = $state<number | null>(null);
  const height = 190;
  const pad = { top: 12, right: 12, bottom: 24, left: 56 };

  // The time axis follows one clock, ticking at a pace that suits the span (a pixel or
  // so of scroll per tick), not the snapshots: the plot scrolls steadily and the start
  // label changes only when the clock does.
  const nowSecs = () => Math.floor(Date.now() / 1000);
  let clock = $state(nowSecs());
  const tickMs = $derived(span <= 3600 ? 10_000 : 60_000);
  $effect(() => {
    const ms = tickMs;
    clock = nowSecs();
    const timer = setInterval(() => (clock = nowSecs()), ms);
    return () => clearInterval(timer);
  });

  // Answers can arrive out of order after a lane or span change; only the latest counts.
  let request = 0;
  async function load() {
    const mine = ++request;
    try {
      const next = await api.hashrate(span, worker, laneKey);
      if (mine === request) {
        samples = next;
        // Fresh samples run up to now; catch the axis up so the newest is not past it.
        clock = nowSecs();
      }
    } catch {
      // Keep the last good series; the header shows the connection state.
    }
  }

  // Another lane has other workers and another series: start from its whole-lane view.
  let shownLane: string | undefined;
  $effect.pre(() => {
    const key = laneKey;
    untrack(() => {
      if (shownLane !== undefined && key !== shownLane) {
        worker = '';
        samples = [];
        hover = null;
        shownTop = null;
      }
      shownLane = key;
    });
  });

  onMount(() => {
    const timer = setInterval(load, 60_000);
    return () => clearInterval(timer);
  });
  // First fetch, and a re-fetch when the selection changes.
  $effect(() => {
    void span;
    void worker;
    void laneKey;
    untrack(load);
  });

  const start = $derived(clock - span);
  const points = $derived(samples.length === 0 ? [] : samples.filter((s) => s.ts >= start));
  const dataMax = $derived(points.reduce((m, p) => Math.max(m, p.hashrate), live));
  // The top steps along a ladder of round values, with hysteresis (see `chartTop`), so
  // the live estimate's wobble does not rescale the chart. `shownTop` is the memory.
  let shownTop: number | null = null;
  const maxY = $derived.by(() => {
    shownTop = chartTop(dataMax, shownTop);
    return shownTop;
  });
  const x = $derived((ts: number) => pad.left + ((ts - start) / span) * (width - pad.left - pad.right));
  const y = $derived((h: number) => pad.top + (1 - h / maxY) * (height - pad.top - pad.bottom));
  const line = $derived(points.map((p, i) => `${i ? 'L' : 'M'}${x(p.ts).toFixed(1)},${y(p.hashrate).toFixed(1)}`).join(' '));
  const area = $derived(
    points.length
      ? `${line} L${x(points[points.length - 1].ts).toFixed(1)},${y(0)} L${x(points[0].ts).toFixed(1)},${y(0)} Z`
      : '',
  );
  // Keyed by position, not value: when the scale steps, the tick elements move and
  // relabel rather than being rebuilt.
  const ticks = $derived(axisTicks(maxY).map((v, i) => ({ i, v, y: y(v) })));
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
          <option value="">{allLabel}</option>
          {#each workers as w (w.name)}
            <option value={w.name} title={w.name}>{workerLabel(w.name, aliases)}</option>
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
      {#each ticks as t (t.i)}
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
      <text x={pad.left} y={height - 6} class="tick left">{formatClock(start)}</text>
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
