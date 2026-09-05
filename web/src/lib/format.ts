/** Human-friendly formatting helpers shared across the dashboard. */

const HASHRATE_UNITS = ['H/s', 'kH/s', 'MH/s', 'GH/s', 'TH/s', 'PH/s', 'EH/s'];
const SI_SUFFIXES = ['', 'k', 'M', 'G', 'T', 'P', 'E'];

function scaled(value: number, suffixes: string[]): [number, string] {
  let v = value;
  let unit = 0;
  while (v >= 1000 && unit < suffixes.length - 1) {
    v /= 1000;
    unit += 1;
  }
  return [v, suffixes[unit]];
}

function digits(v: number): string {
  return v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2);
}

export function formatHashrate(hashesPerSecond: number): string {
  if (!Number.isFinite(hashesPerSecond) || hashesPerSecond <= 0) return '0 H/s';
  const [v, unit] = scaled(hashesPerSecond, HASHRATE_UNITS);
  return `${digits(v)} ${unit}`;
}

/**
 * Difficulty and work, in pool difficulty-1 units, with an SI suffix. Regtest difficulties
 * are far below one, so small values keep three significant digits instead of collapsing
 * to "0.00".
 */
export function formatDifficulty(difficulty: number): string {
  if (!Number.isFinite(difficulty) || difficulty <= 0) return '0';
  if (difficulty < 0.001) return difficulty.toExponential(2);
  if (difficulty < 1) return difficulty.toPrecision(3);
  if (difficulty < 1000) return difficulty < 10 ? difficulty.toFixed(2) : difficulty.toFixed(0);
  const [v, suffix] = scaled(difficulty, SI_SUFFIXES);
  return `${digits(v)}${suffix}`;
}

/** Round progress: a percentage below 2x, a multiplier above it. */
export function formatProgress(progress: number): string {
  if (!Number.isFinite(progress) || progress <= 0) return '0%';
  if (progress >= 2) return `${progress >= 100 ? progress.toFixed(0) : progress.toFixed(1)}× expected`;
  const pct = progress * 100;
  return `${pct.toFixed(pct < 10 ? 1 : 0)}%`;
}

export function formatDuration(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds)) return '∞';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = seconds / 60;
  if (minutes < 60) return `${Math.round(minutes)}m`;
  const hours = minutes / 60;
  if (hours < 48) return `${hours.toFixed(1)}h`;
  const days = hours / 24;
  if (days < 365) return `${days.toFixed(1)}d`;
  return `${(days / 365.25).toFixed(2)}y`;
}

/** "3m ago", "2.1h ago", given unix seconds and the current unix time. */
export function formatAgo(unix: number, now: number): string {
  return `${formatDuration(Math.max(0, now - unix))} ago`;
}

export function formatPercent(probability: number): string {
  const pct = probability * 100;
  if (pct >= 99.95) return '>99.9%';
  if (pct >= 10) return `${pct.toFixed(1)}%`;
  if (pct >= 0.1) return `${pct.toFixed(2)}%`;
  if (pct === 0) return '0%';
  return `${pct.toPrecision(2)}%`;
}

/** A percentage that is already in percent, like luck. */
export function formatPercentValue(pct: number | null | undefined): string {
  if (pct == null || !Number.isFinite(pct)) return '—';
  return `${pct >= 100 ? pct.toFixed(0) : pct.toFixed(1)}%`;
}

/** Base units (litoshi, koinu) to whole coins. Both chains use 8 decimals. */
export function formatCoins(baseUnits: number | null | undefined, symbol: string): string {
  if (baseUnits == null) return '—';
  const coins = baseUnits / 1e8;
  const text = coins >= 1000 ? coins.toFixed(2) : coins >= 1 ? coins.toFixed(4) : coins.toFixed(8);
  return `${text.replace(/\.?0+$/, '')} ${symbol}`;
}

export function formatInteger(n: number): string {
  return n.toLocaleString('en-US');
}

export function shortHash(hash: string, keep = 8): string {
  return hash.length <= keep * 2 ? hash : `${hash.slice(0, keep)}…${hash.slice(-keep)}`;
}

export function shortAddress(address: string): string {
  if (!address) return '—';
  return address.length <= 18 ? address : `${address.slice(0, 9)}…${address.slice(-6)}`;
}

export function formatClock(unix: number): string {
  return new Date(unix * 1000).toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}
