/** Typed client for the daemon's JSON API. Mirrors `alamo-web::snapshot` and the store rows. */

export interface Health {
  status: string;
  version: string;
  uptime_seconds: number;
}

export interface OddsSummary {
  hashrate: number;
  difficulty: number;
  p_hour: number;
  p_day: number;
  p_week: number;
  p_month: number;
  p_year: number;
  expected_seconds: number | null;
}

export interface RoundStatus {
  blocks_found: number;
  started_at: number | null;
  work: number;
  expected_work: number;
  progress: number;
  luck_percent: number | null;
  /** Blocks the pool's lifetime work would find on average at the current difficulty. */
  expected_blocks: number;
}

export interface NodeStatus {
  connected: boolean;
  stale: boolean;
  failures: number;
  last_error: string | null;
  last_ok_seconds: number | null;
  /** null when ZMQ is not configured, else whether the subscription is up. */
  zmq: boolean | null;
}

export interface CoinStatus {
  symbol: string;
  chain: string;
  height: number;
  network_difficulty: number;
  template_age_seconds: number;
  coinbase_value: number;
  odds: OddsSummary;
  round: RoundStatus;
  node: NodeStatus;
}

export interface AuxPayoutStatus {
  coin: string;
  address: string;
  fallback: boolean;
}

export interface WorkerStatus {
  name: string;
  address: string;
  fallback: boolean;
  aux_payouts: AuxPayoutStatus[];
  connections: number;
  difficulty: number;
  hashrate: number;
  shares_accepted: number;
  shares_rejected: number;
  best_difficulty: number;
  work_accepted: number;
  last_share_seconds: number | null;
}

export type BlockStatus = 'accepted' | 'rejected' | 'confirmed' | 'orphaned';

export interface BlockRow {
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
  work_at_found: number | null;
}

export interface ShareRow {
  id: number;
  ts: number;
  worker: string;
  difficulty: number;
  share_diff: number;
  accepted: boolean;
  reject_reason: string | null;
}

export interface HashrateSample {
  ts: number;
  worker: string;
  hashrate: number;
}

export interface Status {
  pool_name: string;
  version: string;
  uptime_seconds: number;
  /** TCP port miners connect to, on the same host as this dashboard. */
  stratum_port: number;
  now: number;
  coins: CoinStatus[];
  hashrate: number;
  shares_accepted: number;
  shares_rejected: number;
  total_work: number;
  best_share_difficulty: number;
  /** Unix time the pool first saw a worker: when it started keeping score. */
  scoring_since: number | null;
  workers: WorkerStatus[];
  blocks: BlockRow[];
}

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(path, { headers: { accept: 'application/json' } });
  if (!res.ok) {
    throw new Error(`${path}: HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}

async function sendJson<T>(method: 'POST' | 'DELETE', path: string): Promise<T> {
  const res = await fetch(path, { method, headers: { accept: 'application/json' } });
  if (!res.ok) {
    let detail = '';
    try {
      detail = ((await res.json()) as { error?: string }).error ?? '';
    } catch {
      // Not JSON; the status is enough.
    }
    throw new Error(detail || `${path}: HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}

export const api = {
  health: () => getJson<Health>('/api/health'),
  /** Zero accepted/rejected share counts and best share for every worker. */
  resetStats: () => sendJson<{ status: string }>('POST', '/api/stats/reset'),
  /** Forget an offline worker: its row, share log, and hashrate history. */
  removeWorker: (name: string) =>
    sendJson<{ status: string }>('DELETE', `/api/workers/${encodeURIComponent(name)}`),
  status: () => getJson<Status>('/api/status'),
  /** Samples for the pool (empty worker) or one worker over the last `span` seconds. */
  hashrate: (span: number, worker = '') =>
    getJson<HashrateSample[]>(
      `/api/hashrate?span=${span}${worker ? `&worker=${encodeURIComponent(worker)}` : ''}`,
    ),
  shares: (limit: number) => getJson<ShareRow[]>(`/api/shares?limit=${limit}`),
  blocks: (limit: number) => getJson<BlockRow[]>(`/api/blocks?limit=${limit}`),
};

/** WebSocket URL for the live snapshot stream, relative to where the page was served. */
export function wsUrl(): string {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${location.host}/api/ws`;
}
