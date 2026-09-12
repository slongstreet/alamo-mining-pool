/** Human-friendly formatting helpers shared across the dashboard. */

const HASHRATE_UNITS = ['H/s', 'kH/s', 'MH/s', 'GH/s', 'TH/s', 'PH/s', 'EH/s'];
const SI = ['', 'k', 'M', 'G', 'T', 'P', 'E'];

function scaled(value: number, units: string[]): [number, string] {
  let v = value;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i += 1;
  }
  return [v, units[i]];
}

function digits(v: number): string {
  return v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2);
}

export function formatHashrate(hashesPerSecond: number): string {
  if (!Number.isFinite(hashesPerSecond) || hashesPerSecond <= 0) return '0 H/s';
  const [v, unit] = scaled(hashesPerSecond, HASHRATE_UNITS);
  return `${digits(v)} ${unit}`;
}

/** Difficulty and share counts: 1.2k, 34.5M, 1.00G. */
export function formatCompact(n: number): string {
  if (!Number.isFinite(n)) return '∞';
  if (n < 1000) return n < 10 && n !== Math.floor(n) ? n.toFixed(2) : Math.round(n).toLocaleString();
  const [v, unit] = scaled(n, SI);
  return `${digits(v)}${unit}`;
}

export function formatInt(n: number): string {
  return Math.round(n).toLocaleString();
}

export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds)) return '∞';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = seconds / 60;
  if (minutes < 60) return `${Math.round(minutes)}m`;
  const hours = minutes / 60;
  if (hours < 48) return `${hours.toFixed(1)}h`;
  const days = hours / 24;
  if (days < 365) return `${days.toFixed(1)}d`;
  return `${(days / 365.25).toFixed(2)}y`;
}

/** Longer form for the expected-time hero: "3 days 4 hours", "2.4 years". */
export function formatDurationLong(seconds: number): string {
  if (!Number.isFinite(seconds)) return 'never';
  if (seconds < 90) return `${Math.round(seconds)} seconds`;
  const minutes = seconds / 60;
  if (minutes < 90) return `${Math.round(minutes)} minutes`;
  const hours = minutes / 60;
  if (hours < 48) {
    const h = Math.floor(hours);
    const m = Math.round((hours - h) * 60);
    return m > 0 ? `${h} h ${m} min` : `${h} hours`;
  }
  const days = hours / 24;
  if (days < 60) {
    const d = Math.floor(days);
    const h = Math.round((days - d) * 24);
    return h > 0 ? `${d} days ${h} h` : `${d} days`;
  }
  if (days < 730) return `${(days / 30.44).toFixed(1)} months`;
  return `${(days / 365.25).toFixed(1)} years`;
}

export function formatPercent(probability: number): string {
  const pct = probability * 100;
  if (pct >= 99.95) return '>99.9%';
  if (pct >= 10) return `${pct.toFixed(1)}%`;
  if (pct >= 0.1) return `${pct.toFixed(2)}%`;
  if (pct === 0) return '0%';
  return `${pct.toPrecision(2)}%`;
}

/** Seconds ago, coarse: "just now", "12s ago", "3m ago", "2.5h ago". */
export function formatAgo(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds)) return 'never';
  if (seconds < 5) return 'just now';
  return `${formatDuration(seconds)} ago`;
}

export function formatTime(unix: number): string {
  return new Date(unix * 1000).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

export function formatDateTime(unix: number): string {
  return new Date(unix * 1000).toLocaleString([], {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/** Base units (1e-8) to a coin amount. */
export function formatCoin(baseUnits: number | null | undefined, symbol: string): string {
  if (baseUnits == null) return '—';
  const v = baseUnits / 1e8;
  const s = v >= 1000 ? v.toFixed(2) : v >= 1 ? v.toFixed(4) : v.toFixed(8);
  return `${s} ${symbol}`;
}

export function shortHash(hash: string, head = 10, tail = 6): string {
  if (hash.length <= head + tail + 1) return hash;
  return `${hash.slice(0, head)}…${hash.slice(-tail)}`;
}

/** Clamp a ratio into [0, 1]. */
export function clamp01(x: number): number {
  return x < 0 ? 0 : x > 1 ? 1 : Number.isFinite(x) ? x : 0;
}
