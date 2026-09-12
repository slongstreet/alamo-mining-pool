<script lang="ts">
  import { onMount } from 'svelte';
  import { api, HASHRATE_RANGES, type HashratePoint, type HashrateRange } from '../lib/api';
  import { formatHashrate, formatTime, formatDateTime } from '../lib/format';

  interface Props {
    /** Current live hashrate for the selected series, appended as the last point. */
    live: number;
    /** Worker names to offer besides the pool total. */
    workers: string[];
  }
  let { live, workers }: Props = $props();

  let range = $state<HashrateRange>('24h');
  let worker = $state('');
  let history = $state<HashratePoint[]>([]);
  let since = $state(0);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let width = $state(640);
  let hover = $state<number | null>(null);
  let tableOpen = $state(false);

  const HEIGHT = 220;
  const PAD = { top: 14, right: 16, bottom: 26, left: 56 };
  const REFRESH_MS = 60_000;

  async function load() {
    loading = true;
    try {
      const h = await api.hashrate(range, worker);
      history = h.points;
      since = h.since;
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    const timer = setInterval(load, REFRESH_MS);
    return () => clearInterval(timer);
  });

  $effect(() => {
    // Re-fetch whenever the range or worker changes.
    void range;
    void worker;
    void load();
  });

  const now = $derived(Math.floor(Date.now() / 1000));
  const points = $derived.by(() => {
    const cutoff = since;
    const pts = history.filter((p) => p.ts >= cutoff);
    return [...pts, { ts: now, hashrate: live }];
  });

  const xMin = $derived(since || (points[0]?.ts ?? now));
  const xMax = $derived(now);
  const yMax = $derived.by(() => {
    const m = Math.max(...points.map((p) => p.hashrate), 0);
    return niceCeil(m > 0 ? m * 1.15 : 1);
  });

  const plotW = $derived(Math.max(width - PAD.left - PAD.right, 10));
  const plotH = HEIGHT - PAD.top - PAD.bottom;

  function x(ts: number): number {
    const span = Math.max(xMax - xMin, 1);
    return PAD.left + ((ts - xMin) / span) * plotW;
  }
  function y(v: number): number {
    return PAD.top + plotH - (v / yMax) * plotH;
  }

  function niceCeil(v: number): number {
    const exp = Math.pow(10, Math.floor(Math.log10(v)));
    const m = v / exp;
    const nice = m <= 1 ? 1 : m <= 2 ? 2 : m <= 2.5 ? 2.5 : m <= 5 ? 5 : 10;
    return nice * exp;
  }

  const yTicks = $derived([0, 0.25, 0.5, 0.75, 1].map((f) => f * yMax));
  const xTicks = $derived.by(() => {
    const n = width < 480 ? 3 : 5;
    return Array.from({ length: n + 1 }, (_, i) => xMin + ((xMax - xMin) * i) / n);
  });

  const linePath = $derived.by(() => {
    if (points.length === 0) return '';
    return points.map((p, i) => `${i === 0 ? 'M' : 'L'}${x(p.ts).toFixed(1)},${y(p.hashrate).toFixed(1)}`).join(' ');
  });
  const areaPath = $derived.by(() => {
    if (points.length === 0) return '';
    const base = (PAD.top + plotH).toFixed(1);
    return `${linePath} L${x(points[points.length - 1].ts).toFixed(1)},${base} L${x(points[0].ts).toFixed(1)},${base} Z`;
  });

  const hovered = $derived(hover == null ? null : points[hover]);

  function onMove(ev: PointerEvent) {
    const svg = ev.currentTarget as SVGSVGElement;
    const rect = svg.getBoundingClientRect();
    const px = ((ev.clientX - rect.left) / rect.width) * width;
    let best = 0;
    let bestD = Infinity;
    points.forEach((p, i) => {
      const d = Math.abs(x(p.ts) - px);
      if (d < bestD) {
        bestD = d;
        best = i;
      }
    });
    hover = best;
  }

  function fmtTick(ts: number): string {
    return range === '1h' || range === '6h' || range === '24h' ? formatTime(ts).slice(0, 5) : formatDateTime(ts).replace(/,.*$/, '');
  }
</script>

<section class="panel">
  <h2>
    <span>Hashrate</span>
    <span class="controls">
      {#if workers.length > 0}
        <select bind:value={worker} aria-label="Series">
          <option value="">Pool total</option>
          {#each workers as w (w)}
            <option value={w}>{w}</option>
          {/each}
        </select>
      {/if}
      <span class="seg" role="group" aria-label="Range">
        {#each HASHRATE_RANGES as r (r)}
          <button type="button" aria-pressed={range === r} onclick={() => (range = r)}>{r}</button>
        {/each}
      </span>
    </span>
  </h2>

  <div class="chart" bind:clientWidth={width}>
    <svg
      viewBox="0 0 {width} {HEIGHT}"
      width={width}
      height={HEIGHT}
      role="img"
      aria-label="Hashrate over the last {range}"
      onpointermove={onMove}
      onpointerleave={() => (hover = null)}
    >
      {#each yTicks as t, i (i)}
        <line class="grid" x1={PAD.left} x2={width - PAD.right} y1={y(t)} y2={y(t)} />
        <text class="tick" x={PAD.left - 8} y={y(t) + 4} text-anchor="end">{formatHashrate(t)}</text>
      {/each}
      {#each xTicks as t, i (i)}
        <text class="tick" x={x(t)} y={HEIGHT - 8} text-anchor="middle">{fmtTick(t)}</text>
      {/each}
      <path class="area" d={areaPath} />
      <path class="line" d={linePath} />
      {#if points.length > 0}
        {@const last = points[points.length - 1]}
        <circle class="end" cx={x(last.ts)} cy={y(last.hashrate)} r="4" />
      {/if}
      {#if hovered}
        <line class="cross" x1={x(hovered.ts)} x2={x(hovered.ts)} y1={PAD.top} y2={PAD.top + plotH} />
        <circle class="end" cx={x(hovered.ts)} cy={y(hovered.hashrate)} r="4" />
      {/if}
    </svg>
    {#if hovered}
      <div class="tip" style:left="{Math.min(x(hovered.ts) + 12, width - 150)}px">
        <b class="num">{formatHashrate(hovered.hashrate)}</b>
        <span class="muted">{formatDateTime(hovered.ts)}</span>
      </div>
    {/if}
  </div>
  <div class="foot muted">
    {#if error}
      history unavailable: {error}
    {:else if loading && history.length === 0}
      loading…
    {:else if history.length === 0}
      no samples yet; the daemon records one per minute
    {:else}
      {history.length} samples
    {/if}
    <button class="plain" type="button" onclick={() => (tableOpen = !tableOpen)}>{tableOpen ? 'hide table' : 'show table'}</button>
  </div>
  {#if tableOpen}
    <div class="scroll-x table">
      <table>
        <thead><tr><th>Time</th><th class="r">Hashrate</th></tr></thead>
        <tbody>
          {#each [...points].reverse().slice(0, 200) as p (p.ts)}
            <tr><td>{formatDateTime(p.ts)}</td><td class="r">{formatHashrate(p.hashrate)}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

<style>
  .controls {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    flex-wrap: wrap;
    text-transform: none;
    letter-spacing: 0;
  }
  .chart {
    position: relative;
    width: 100%;
  }
  svg {
    display: block;
    width: 100%;
    touch-action: none;
  }
  .grid {
    stroke: var(--line);
    stroke-width: 1;
  }
  .tick {
    fill: var(--muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .area {
    fill: var(--series);
    fill-opacity: 0.1;
  }
  .line {
    fill: none;
    stroke: var(--series);
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .end {
    fill: var(--series);
    stroke: var(--panel);
    stroke-width: 2;
  }
  .cross {
    stroke: var(--muted);
    stroke-width: 1;
  }
  .tip {
    position: absolute;
    top: 8px;
    background: var(--panel-2);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 0.3rem 0.6rem;
    font-size: 0.82rem;
    display: flex;
    flex-direction: column;
    pointer-events: none;
    white-space: nowrap;
  }
  .foot {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 0.8rem;
    margin-top: 0.4rem;
  }
  .table {
    max-height: 260px;
    overflow-y: auto;
    margin-top: 0.5rem;
  }
</style>
