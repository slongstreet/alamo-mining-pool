/**
 * Lanes: one parent coin plus its merge-mined aux coins, with their own stratum port,
 * algorithm, and statistics. Scrypt and SHA-256 hashrates cannot be added together, so
 * the dashboard shows one lane at a time; these helpers name, describe, and pick lanes.
 */
import type { LaneStatus } from './api';
import { formatInteger } from './format';

export const ALGORITHM_LABELS: Record<string, string> = {
  scrypt: 'Scrypt',
  sha256d: 'SHA-256',
};

export function algorithmLabel(algorithm: string): string {
  return ALGORITHM_LABELS[algorithm] ?? algorithm;
}

/** "Litecoin + Dogecoin", "Bitcoin". */
export function laneName(lane: LaneStatus): string {
  return lane.configured_coins.map((c) => c.name).join(' + ') || lane.key.toUpperCase();
}

/** Tickers of the lane's configured coins, parent first. */
export function laneSymbols(lane: LaneStatus): string[] {
  return lane.configured_coins.length ? lane.configured_coins.map((c) => c.symbol) : [lane.key.toUpperCase()];
}

/** "Scrypt · port 3333". */
export function laneTagline(lane: LaneStatus): string {
  return `${algorithmLabel(lane.algorithm)} · port ${lane.stratum_port}`;
}

export interface LaneHealth {
  /** Badge text; never colour alone. */
  label: string;
  /** Badge class: ok, warn, bad, or '' for neutral. */
  kind: 'ok' | 'warn' | 'bad' | '';
  /** The longer explanation, for a title and for screen readers. */
  detail: string;
}

/** One badge's worth of lane health: its state, else the worst of its nodes. */
export function laneHealth(lane: LaneStatus): LaneHealth {
  if (lane.state === 'failed') {
    return { label: 'failed', kind: 'bad', detail: lane.error ?? 'The lane could not start.' };
  }
  if (lane.state === 'connecting') {
    return { label: 'connecting', kind: 'warn', detail: 'Waiting for the node to answer.' };
  }
  const down = lane.coins.find((c) => !c.node.connected);
  if (down) {
    return {
      label: `${down.symbol} node ${down.node.stale ? 'stale' : 'unreachable'}`,
      kind: 'bad',
      detail: down.node.last_error ?? `${down.node.failures} failed polls`,
    };
  }
  const missing = lane.configured_coins.find((c) => !lane.coins.some((s) => s.symbol === c.symbol));
  if (missing) {
    return {
      label: `${missing.symbol} waiting`,
      kind: 'warn',
      detail: `No ${missing.symbol} block template yet; the lane mines without it until its node answers.`,
    };
  }
  const zmqDown = lane.coins.find((c) => c.node.zmq === false);
  if (zmqDown) {
    return {
      label: 'zmq down',
      kind: 'warn',
      detail: `${zmqDown.symbol} ZMQ subscription down; polling for new blocks instead.`,
    };
  }
  return { label: 'nodes ok', kind: 'ok', detail: 'Every node of this lane is answering.' };
}

/**
 * How the lane's stratum (job) difficulties relate to network units, in one sentence:
 * the share multiplier is 65,536 for scrypt and 1 for SHA-256.
 */
export function unitsHelp(lane: LaneStatus): string {
  const alg = algorithmLabel(lane.algorithm);
  const m = lane.share_multiplier;
  if (m === 1) return `On this ${alg} lane the two units are the same (share multiplier 1).`;
  if (!(m > 0)) return '';
  return `On this ${alg} lane a network unit is ${formatInteger(m)} stratum units (the share multiplier).`;
}

/** Workers with at least one live connection. */
export function onlineWorkers(lane: LaneStatus): number {
  return lane.workers.filter((w) => w.connections > 0).length;
}

/** Expected seconds to a block on the lane's parent chain; null while it has no template. */
export function parentExpectedSeconds(lane: LaneStatus): number | null | undefined {
  return lane.coins[0]?.odds.expected_seconds;
}

/** Example addresses for the miner-setup hints, by ticker and network. */
const ADDRESS_EXAMPLES: Record<string, Record<string, string>> = {
  LTC: { main: 'ltc1q…', test: 'tltc1q…', regtest: 'rltc1q…' },
  BTC: { main: 'bc1q…', test: 'tb1q…', regtest: 'bcrt1q…' },
  DOGE: { main: 'D…', test: 'n…', regtest: 'm…' },
};

export function addressExample(symbol: string, chain: string | null): string {
  const byChain = ADDRESS_EXAMPLES[symbol];
  return byChain?.[chain ?? 'main'] ?? byChain?.main ?? '…';
}

// --- Selection: `#/<lane key>` in the URL, remembered across visits. ---

const KEY = 'alamo.lane';

export function rememberedLane(): string | null {
  try {
    return localStorage.getItem(KEY);
  } catch {
    return null;
  }
}

export function rememberLane(key: string) {
  try {
    localStorage.setItem(KEY, key);
  } catch {
    // Storage may be unavailable; the hash still carries the choice.
  }
}

export type Route = { page: 'settings' } | { page: 'dashboard'; lane: string | null };

/** `#/settings`, `#/<lane>`, or the dashboard's default (`#/`, empty). */
export function parseHash(hash: string): Route {
  if (hash.startsWith('#/settings')) return { page: 'settings' };
  const lane = hash.startsWith('#/') ? decodeURIComponent(hash.slice(2)).trim() : '';
  return { page: 'dashboard', lane: lane || null };
}

/**
 * The lane to show: the one the hash names, the primary if it names one that does not
 * exist, and with no lane in the hash the remembered one (or the primary).
 */
export function pickLane(lanes: LaneStatus[], fromHash: string | null, remembered: string | null): LaneStatus | null {
  if (lanes.length === 0) return null;
  const find = (key: string | null) => (key ? lanes.find((l) => l.key === key) : undefined);
  if (fromHash) return find(fromHash) ?? lanes[0];
  return find(remembered) ?? lanes[0];
}
