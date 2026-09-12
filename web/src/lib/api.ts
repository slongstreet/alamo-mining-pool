/** Typed client for the daemon's JSON API. Shapes mirror `alamo-web/src/snapshot.rs`. */

export interface Health {
  status: string;
  version: string;
  uptime_seconds: number;
}

export interface Odds {
  hashrate: number;
  difficulty: number;
  p_hour: number;
  p_day: number;
  p_week: number;
  p_month: number;
  p_year: number;
  expected_seconds: number | null;
}

export interface Round {
  started_at: number;
  work: number;
  shares: number;
  best_share: number;
  expected_work: number;
  luck_percent: number;
}

export interface CoinStatus {
  symbol: string;
  name: string;
  chain: string;
  height: number;
  network_difficulty: number;
  template_age_seconds: number;
  coinbase_value: number;
  coinbase_maturity: number;
  odds: Odds | null;
  round: Round | null;
}

export interface AuxPayout {
  coin: string;
  address: string;
  fallback: boolean;
}

export interface Worker {
  name: string;
  address: string;
  fallback: boolean;
  aux_payouts: AuxPayout[];
  connections: number;
  difficulty: number;
  hashrate: number;
  shares_accepted: number;
  shares_rejected: number;
  best_difficulty: number;
  last_share_seconds: number | null;
}

export type BlockStatus = 'accepted' | 'rejected' | 'confirmed' | 'orphaned';

export interface Block {
  id: number;
  coin: string;
  height: number;
  hash: string;
  worker: string;
  difficulty: number;
  share_diff: number;
  reward_sats: number | null;
  found_at: number;
  status: BlockStatus;
  confirmations: number;
}

export interface Status {
  pool_name: string;
  version: string;
  uptime_seconds: number;
  now: number;
  coins: CoinStatus[];
  hashrate: number;
  shares_accepted: number;
  shares_rejected: number;
  best_share_difficulty: number;
  workers: Worker[];
  blocks: Block[];
  odds: Odds | null;
}

/** A share, as stored (`/api/shares`) or as pushed live (`accepted` is a boolean there). */
export interface Share {
  ts: number;
  worker: string;
  coin?: string;
  difficulty: number;
  share_diff: number;
  accepted: number | boolean;
  reject_reason: string | null;
}

export type HashrateRange = '1h' | '6h' | '24h' | '7d' | '30d';
export const HASHRATE_RANGES: HashrateRange[] = ['1h', '6h', '24h', '7d', '30d'];

export interface HashratePoint {
  ts: number;
  hashrate: number;
}

export interface HashrateHistory {
  range: HashrateRange;
  since: number;
  now: number;
  worker: string;
  points: HashratePoint[];
}

export type Push = { type: 'status'; data: Status } | { type: 'share'; data: Share };

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(path, { headers: { accept: 'application/json' } });
  if (!res.ok) {
    throw new Error(`${path}: HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}

export const api = {
  health: () => getJson<Health>('/api/health'),
  status: () => getJson<Status>('/api/status'),
  hashrate: (range: HashrateRange, worker = '') =>
    getJson<HashrateHistory>(
      `/api/hashrate?range=${range}&worker=${encodeURIComponent(worker)}`,
    ),
  shares: (limit = 100) => getJson<Share[]>(`/api/shares?limit=${limit}`),
  blocks: (limit = 100) => getJson<Block[]>(`/api/blocks?limit=${limit}`),
};

export function wsUrl(): string {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${location.host}/api/ws`;
}
