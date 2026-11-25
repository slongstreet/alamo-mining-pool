/**
 * Things worth a notice on the dashboard, found by comparing one status snapshot with the
 * previous: a block found, a worker that lost its connection a minute ago, a node that
 * went away. Pure functions over snapshots; the components decide how to show them.
 *
 * The first snapshot after page load, or after a gap in updates, is only a baseline:
 * what is already in it is not news. The caller passes `null` as the baseline then.
 */
import type { BlockRow, BlockStatus, Status } from './api';
import { formatDuration, formatHashrate } from './format';
import { laneName, onlineWorkers } from './lanes';
import { aliasMap, workerLabel } from './workers';

/** Seconds a worker that was connected must have had no connection before it is reported. */
export const WORKER_OFFLINE_SECONDS = 60;

/** What the previous snapshots established. Treat as immutable; `observe` returns a new one. */
export interface AlertBaseline {
  /** Newest block id seen so far; null when no snapshot has had a block yet. */
  newestBlock: number | null;
  /** Workers with a connection in the last snapshot, as `lane/name`. */
  connected: ReadonlySet<string>;
  /**
   * Workers that were connected and now have none, not yet reported: `lane/name` → the
   * snapshot time (`Status.now`) they were first seen without a connection.
   */
  dropped: ReadonlyMap<string, number>;
  /** Coin nodes connected in the last snapshot, as `lane/symbol`. */
  nodesUp: ReadonlySet<string>;
}

export type Alert =
  | {
      kind: 'block';
      key: string;
      block: BlockRow;
      /** The finder's alias, else its shortened name. */
      worker: string;
      /** The finding lane's name ("Litecoin + Dogecoin"), when known. */
      lane: string | null;
    }
  | {
      kind: 'worker-offline';
      key: string;
      /** Full worker name. */
      name: string;
      /** Alias, else shortened name. */
      worker: string;
      lane: string;
      /** Seconds without a connection so far. */
      seconds: number;
    }
  | { kind: 'node-down'; key: string; coin: string; lane: string; error: string | null };

export interface Observation {
  baseline: AlertBaseline;
  /** New alerts, oldest first. Empty when `previous` was null. */
  alerts: Alert[];
}

/**
 * Compares `next` with what `previous` established. With `previous` null (first snapshot,
 * or the first after a gap) nothing is reported and `next` becomes the baseline.
 */
export function observe(previous: AlertBaseline | null, next: Status): Observation {
  const alerts: Alert[] = [];
  const lanes = new Map(next.lanes.map((l) => [l.key, l] as const));

  // Blocks: anything newer than the newest id known. Comparing ids, not membership,
  // means a block sliding out of the recent list, or changing status, is not news.
  let newestBlock = previous?.newestBlock ?? null;
  const fresh: BlockRow[] = [];
  for (const b of next.blocks) {
    if (previous && (previous.newestBlock == null || b.id > previous.newestBlock)) fresh.push(b);
    if (newestBlock == null || b.id > newestBlock) newestBlock = b.id;
  }
  fresh.sort((a, b) => a.id - b.id);
  for (const block of fresh) {
    const lane = lanes.get(block.lane);
    alerts.push({
      kind: 'block',
      key: `block/${block.id}`,
      block,
      worker: workerLabel(block.worker, aliasMap(lane?.workers ?? [])),
      lane: lane ? laneName(lane) : null,
    });
  }

  // Workers: connected last time → no connection now starts the clock; still none after
  // WORKER_OFFLINE_SECONDS is one alert. Reconnecting, or leaving the table, forgets it.
  const connected = new Set<string>();
  const dropped = new Map<string, number>();
  for (const lane of next.lanes) {
    let aliases: ReturnType<typeof aliasMap> | null = null;
    for (const w of lane.workers) {
      const key = `${lane.key}/${w.name}`;
      if (w.connections > 0) {
        connected.add(key);
        continue;
      }
      if (!previous) continue;
      const since = previous.connected.has(key) ? next.now : previous.dropped.get(key);
      if (since == null) continue;
      const seconds = next.now - since;
      if (seconds >= WORKER_OFFLINE_SECONDS) {
        aliases ??= aliasMap(lane.workers);
        alerts.push({
          kind: 'worker-offline',
          key: `worker/${key}/${since}`,
          name: w.name,
          worker: workerLabel(w.name, aliases),
          lane: laneName(lane),
          seconds,
        });
      } else {
        dropped.set(key, since);
      }
    }
  }

  // Nodes: connected last time → disconnected now.
  const nodesUp = new Set<string>();
  for (const lane of next.lanes) {
    for (const c of lane.coins) {
      const key = `${lane.key}/${c.symbol}`;
      if (c.node.connected) {
        nodesUp.add(key);
      } else if (previous?.nodesUp.has(key)) {
        alerts.push({
          kind: 'node-down',
          key: `node/${key}/${next.now}`,
          coin: c.symbol,
          lane: laneName(lane),
          error: c.node.last_error,
        });
      }
    }
  }

  return { baseline: { newestBlock, connected, dropped, nodesUp }, alerts };
}

export type AlertTone = 'ok' | 'warn' | 'bad';

export interface AlertText {
  title: string;
  body: string;
  tone: AlertTone;
}

const BLOCK_STATUS_TEXT: Record<BlockStatus, string> = {
  accepted: 'accepted by the node',
  confirmed: 'confirmed',
  rejected: 'rejected by the node',
  orphaned: 'orphaned',
};

/** Words for an alert. `multiLane` adds the lane's name, which a one-lane pool does not need. */
export function describeAlert(alert: Alert, multiLane: boolean): AlertText {
  const where = (lane: string | null) => (multiLane && lane ? ` · ${lane}` : '');
  switch (alert.kind) {
    case 'block': {
      const b = alert.block;
      const bad = b.status === 'rejected' || b.status === 'orphaned';
      return {
        title: `${b.coin} block ${b.height.toLocaleString('en-US')} found`,
        body: `by ${alert.worker} · ${BLOCK_STATUS_TEXT[b.status] ?? b.status}${where(alert.lane)}`,
        tone: bad ? 'bad' : 'ok',
      };
    }
    case 'worker-offline':
      return {
        title: `${alert.worker} disconnected`,
        body: `No connection for ${formatDuration(alert.seconds)}${where(alert.lane)}`,
        tone: 'warn',
      };
    case 'node-down':
      return {
        title: `${alert.coin} node unreachable`,
        body: `${alert.error ?? 'The pool cannot reach it.'}${where(alert.lane)}`,
        tone: 'bad',
      };
  }
}

/** The tab-title prefix for a found block: "BTC block found". */
export function blockFlash(block: BlockRow): string {
  return `${block.coin} block found`;
}

/**
 * The document title: the pool at a glance, readable in a background tab.
 * "1.20 TH/s · 4 workers — Alamo Solo Pool", or each lane's hashrate with several lanes
 * ("720 MH/s + 42.0 TH/s — …"), prefixed "⚠ offline — " while stale and
 * "★ BTC block found — " while a block waits to be seen.
 */
export function tabTitle(status: Status | null, opts: { stale: boolean; flash: string | null }): string {
  const pool = status?.pool_name || 'Alamo';
  const lanes = status?.lanes ?? [];
  let title = pool;
  if (lanes.length === 1) {
    const n = onlineWorkers(lanes[0]);
    title = `${formatHashrate(lanes[0].hashrate)} · ${n} worker${n === 1 ? '' : 's'} — ${pool}`;
  } else if (lanes.length > 1) {
    title = `${lanes.map((l) => formatHashrate(l.hashrate)).join(' + ')} — ${pool}`;
  }
  if (opts.stale) title = `⚠ offline — ${title}`;
  if (opts.flash) title = `★ ${opts.flash} — ${title}`;
  return title;
}
