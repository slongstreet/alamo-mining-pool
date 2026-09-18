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
  /**
   * Stratum share units per unit of network difficulty (65536 for scrypt). Job
   * difficulties are in stratum units, as the miner sees them; every other difficulty
   * the API reports, including `share_diff`, is in network units.
   */
  share_multiplier: number;
  best_share_difficulty: number;
  /** Worker that found the best share, once there is one. */
  best_share_worker: string | null;
  /** Unix time the best share arrived, when known. */
  best_share_at: number | null;
  /** Unix time the pool first saw a worker: when it started keeping score. */
  scoring_since: number | null;
  workers: WorkerStatus[];
  blocks: BlockRow[];
}

/** A non-2xx answer, with the daemon's reason when it gave one. */
export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function detail(res: Response): Promise<string> {
  try {
    return ((await res.json()) as { error?: string }).error ?? '';
  } catch {
    return '';
  }
}

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(path, { headers: { accept: 'application/json' } });
  if (!res.ok) {
    throw new ApiError(res.status, (await detail(res)) || `${path}: HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}

/** One operator setting: the value in force, the config file's value, and which applies. */
export interface Setting<T> {
  value: T;
  file_value: T;
  overridden: boolean;
}

export interface CoinSettings {
  key: string;
  symbol: string;
  chain: string;
  merge_mined_with: string | null;
  fallback_address: Setting<string>;
  rpc_url: string;
  rpc_user: string;
  zmq_hashblock: string | null;
  poll_interval_ms: number;
  template_refresh_secs: number;
  template_stale_secs: number;
}

export type VardiffField =
  | 'initial_difficulty'
  | 'min_difficulty'
  | 'max_difficulty'
  | 'target_share_seconds'
  | 'retarget_seconds'
  | 'variance_percent';

export type VardiffSettings = Record<VardiffField, Setting<number>>;

export interface SettingsDoc {
  read_only: boolean;
  pool_name: Setting<string>;
  coinbase_tag: Setting<string>;
  coinbase_tag_max_bytes: number;
  coins: CoinSettings[];
  vardiff: VardiffSettings;
  log_level: Setting<string>;
  config_toml: string;
}

/** A change: a value stores an override, `null` reverts to the config file, absent leaves it. */
export interface SettingsPatch {
  pool_name?: string | null;
  coinbase_tag?: string | null;
  fallback_addresses?: Record<string, string | null>;
  vardiff?: Partial<Record<VardiffField, number | null>>;
  log_level?: string | null;
}

export interface NodeProbe {
  key: string;
  symbol: string;
  chain: string;
  height: number;
  subversion: string;
  protocol_version: number;
  connections: number;
  initial_block_download: boolean;
  latency_ms: number;
}

async function sendJson<T>(method: 'POST' | 'PUT' | 'DELETE', path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { accept: 'application/json' };
  if (body !== undefined) {
    headers['content-type'] = 'application/json';
  }
  const res = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  if (!res.ok) {
    throw new ApiError(res.status, (await detail(res)) || `${path}: HTTP ${res.status}`);
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
  settings: () => getJson<SettingsDoc>('/api/settings'),
  /** Apply a change; the daemon validates the whole patch before storing any of it. */
  updateSettings: (patch: SettingsPatch) => sendJson<SettingsDoc>('PUT', '/api/settings', patch),
  /** Ask a coin's node who it is. */
  testNode: (key: string) => sendJson<NodeProbe>('POST', `/api/nodes/${encodeURIComponent(key)}/test`),
};

/** Where the log and database downloads live; plain links, so the browser saves them. */
export const downloads = {
  logs: '/api/logs',
  backup: '/api/backup',
};

/** WebSocket URL for the live snapshot stream, relative to where the page was served. */
export function wsUrl(): string {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${location.host}/api/ws`;
}
