/** Human-friendly formatting helpers shared across the dashboard. */

const HASHRATE_UNITS = ['H/s', 'kH/s', 'MH/s', 'GH/s', 'TH/s', 'PH/s', 'EH/s'];

export function formatHashrate(hashesPerSecond: number): string {
  let value = hashesPerSecond;
  let unit = 0;
  while (value >= 1000 && unit < HASHRATE_UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${HASHRATE_UNITS[unit]}`;
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

export function formatPercent(probability: number): string {
  const pct = probability * 100;
  if (pct >= 10) return `${pct.toFixed(1)}%`;
  if (pct >= 0.1) return `${pct.toFixed(2)}%`;
  return `${pct.toPrecision(2)}%`;
}
