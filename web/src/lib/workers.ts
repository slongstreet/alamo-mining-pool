/**
 * Worker display helpers shared by the dashboard panels: aliases, reject reasons, the
 * quiet-worker rule, and the Workers table's sort.
 */
import type { WorkerStatus } from './api';
import { formatDuration, shortWorker } from './format';

/** A lane's worker aliases: full worker name → alias. Workers without one are absent. */
export type WorkerAliases = ReadonlyMap<string, string>;

/** The aliases of a lane's workers, from the snapshot's `workers`. */
export function aliasMap(workers: readonly WorkerStatus[]): WorkerAliases {
  const map = new Map<string, string>();
  for (const w of workers) {
    if (w.alias) map.set(w.name, w.alias);
  }
  return map;
}

/** What to show for a worker: its alias, else its shortened name. */
export function workerLabel(name: string, aliases: WorkerAliases): string {
  return aliases.get(name) ?? shortWorker(name);
}

/** Tooltip for a worker label: the full name, after the alias when there is one. */
export function workerTitle(name: string, aliases: WorkerAliases): string {
  const alias = aliases.get(name);
  return alias ? `${alias}: ${name}` : name;
}

/** Short labels for reject reason slugs (`RejectReason::as_str` on the daemon side). */
export const REJECT_LABELS: Readonly<Record<string, string>> = {
  stale_job: 'stale',
  unknown_job: 'unknown job',
  low_difficulty: 'low diff',
  duplicate: 'duplicate',
  invalid_ntime: 'bad ntime',
  invalid_extranonce2: 'bad extranonce',
  unauthorized: 'unauthorized',
  invalid_version_bits: 'bad version bits',
};

/** The label for a reject slug; unknown slugs are shown as sent. */
export function rejectLabel(slug: string | null | undefined): string {
  if (!slug) return 'rejected';
  return REJECT_LABELS[slug] ?? slug;
}

/** Shortest silence that marks a connected worker quiet, whatever the vardiff target. */
export const QUIET_FLOOR_SECONDS = 120;
/** Silence, in target share intervals, that marks a connected worker quiet. */
export const QUIET_INTERVALS = 12;

/** Seconds without an accepted share after which a connected worker is flagged quiet. */
export function quietThreshold(targetShareSeconds: number): number {
  const target = Number.isFinite(targetShareSeconds) && targetShareSeconds > 0 ? targetShareSeconds : 0;
  return Math.max(QUIET_FLOOR_SECONDS, QUIET_INTERVALS * target);
}

/**
 * Whether a worker is connected but has stopped submitting: its last accepted share is
 * older than `threshold` seconds. A worker that has never had a share is not flagged,
 * since how long it has been connected is unknown.
 */
export function isQuiet(w: WorkerStatus, threshold: number): boolean {
  return w.connections > 0 && w.last_share_seconds != null && w.last_share_seconds > threshold;
}

/** Tooltip for the quiet badge. */
export function quietHelp(w: WorkerStatus, targetShareSeconds: number): string {
  const threshold = quietThreshold(targetShareSeconds);
  const silent = w.last_share_seconds == null ? '' : ` for ${formatDuration(w.last_share_seconds)}`;
  return (
    `Connected, but no accepted share${silent}. A connected worker is flagged once it has gone longer than ` +
    `${formatDuration(threshold)} without one: ${QUIET_INTERVALS} × this lane's vardiff target of ` +
    `${formatDuration(targetShareSeconds)} between shares, and never less than ${formatDuration(QUIET_FLOOR_SECONDS)}.`
  );
}

/** Lifetime reject rate in percent, or null with no shares at all. */
export function rejectRate(w: WorkerStatus): number | null {
  const total = w.shares_accepted + w.shares_rejected;
  return total > 0 ? (w.shares_rejected / total) * 100 : null;
}

/** Reject rate above which it is shown as a warning, in percent. */
export const REJECT_RATE_WARN_PERCENT = 5;

/** Tooltip for a worker's rejected count: the per-reason breakdown. */
export function rejectsHelp(w: WorkerStatus): string {
  const lines = Object.entries(w.rejects)
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([slug, n]) => `${rejectLabel(slug)}: ${n.toLocaleString()}`);
  const head = 'Rejected since the pool last restarted (or its counters were reset)';
  return lines.length ? `${head}:\n${lines.join('\n')}` : `${head}: none. The rejected total is older.`;
}

export type WorkerSortKey = 'name' | 'hashrate' | 'difficulty' | 'shares' | 'best' | 'last_share';
export type SortDirection = 'asc' | 'desc';
export interface WorkerSort {
  key: WorkerSortKey;
  dir: SortDirection;
}

const SORT_KEYS: readonly WorkerSortKey[] = ['name', 'hashrate', 'difficulty', 'shares', 'best', 'last_share'];

/** The direction a column sorts in on its first click: names A→Z, newest share first, numbers high→low. */
export function firstDirection(key: WorkerSortKey): SortDirection {
  return key === 'name' || key === 'last_share' ? 'asc' : 'desc';
}

/** Next sort after clicking `key`: its first direction, then reversed, then the daemon's order (null). */
export function nextSort(current: WorkerSort | null, key: WorkerSortKey): WorkerSort | null {
  const first = firstDirection(key);
  if (!current || current.key !== key) return { key, dir: first };
  if (current.dir === first) return { key, dir: first === 'asc' ? 'desc' : 'asc' };
  return null;
}

/** A stored sort, or null when absent or not one this version knows. */
export function parseSort(raw: string | null): WorkerSort | null {
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as Partial<WorkerSort>;
    if (v && SORT_KEYS.includes(v.key as WorkerSortKey) && (v.dir === 'asc' || v.dir === 'desc')) {
      return { key: v.key as WorkerSortKey, dir: v.dir };
    }
  } catch {
    // Fall through.
  }
  return null;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

/** The sorted value, or null for "unknown", which sorts last in either direction. */
function sortValue(w: WorkerStatus, key: WorkerSortKey): number | null {
  switch (key) {
    case 'hashrate':
      return w.hashrate;
    case 'difficulty':
      return w.difficulty;
    case 'shares':
      return w.shares_accepted;
    case 'best':
      return w.best_difficulty;
    case 'last_share':
      return w.last_share_seconds;
    case 'name':
      return null;
  }
}

/**
 * `workers` (in the daemon's order: connected first, then natural name order) sorted by
 * `sort`. Ties keep the daemon's order, so equal rows do not swap between snapshots.
 * Names sort by alias where there is one, digits numerically (`rig2` before `rig10`).
 */
export function sortWorkers(
  workers: readonly WorkerStatus[],
  sort: WorkerSort,
  aliases: WorkerAliases,
): WorkerStatus[] {
  const sign = sort.dir === 'asc' ? 1 : -1;
  const indexed = workers.map((w, i) => ({ w, i }));
  indexed.sort((a, b) => {
    let c: number;
    if (sort.key === 'name') {
      c = sign * collator.compare(aliases.get(a.w.name) ?? a.w.name, aliases.get(b.w.name) ?? b.w.name);
    } else {
      const x = sortValue(a.w, sort.key);
      const y = sortValue(b.w, sort.key);
      if (x == null || y == null) c = x == null ? (y == null ? 0 : 1) : -1;
      else c = sign * (x - y);
    }
    return c || a.i - b.i;
  });
  return indexed.map((e) => e.w);
}
