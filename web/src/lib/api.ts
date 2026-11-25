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
  /** Rejected shares, lifetime (survives restarts). */
  shares_rejected: number;
  /**
   * Rejected shares by reason slug (`stale_job`, `low_difficulty`, ...) since the daemon
   * last started or the lane's counters were reset; zero counts are left out. Can sum to
   * less than `shares_rejected`, which is lifetime.
   */
  rejects: Record<string, number>;
  best_difficulty: number;
  /** Unix seconds the best share arrived; null when unknown (older rows) or no share yet. */
  best_at: number | null;
  work_accepted: number;
  last_share_seconds: number | null;
  /** Display name the operator gave the worker; null when none. */
  alias: string | null;
}

export type BlockStatus = 'accepted' | 'rejected' | 'confirmed' | 'orphaned';

export interface BlockRow {
  id: number;
  /** Lane that found it: the parent coin's config key (a merge-mined aux block carries its parent's). */
  lane: string;
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

export type LaneState = 'connecting' | 'running' | 'failed';

/** A coin configured in a lane, known from startup whether or not its node has answered. */
export interface LaneCoin {
  /** Config key (`ltc`). */
  key: string;
  /** Ticker (`LTC`), as `CoinStatus.symbol` spells it. */
  symbol: string;
  /** Human-readable name (`Litecoin`). */
  name: string;
}

/** One lane: a parent coin, its aux coins, its stratum listener, and its own statistics. */
export interface LaneStatus {
  /** The parent coin's config key, which is the lane id. */
  key: string;
  /** Proof-of-work algorithm. */
  algorithm: 'scrypt' | 'sha256d';
  /** TCP port this lane's miners connect to. */
  stratum_port: number;
  state: LaneState;
  /** Why the lane failed to start; null unless `state` is 'failed'. */
  error: string | null;
  /** 'main', 'test', or 'regtest'; null until the parent's node has answered. */
  chain: string | null;
  /**
   * Stratum share units per unit of network difficulty on this lane's parent chain
   * (65536 for scrypt, 1 for sha256d). Job difficulties are in stratum units, as the
   * miner sees them; every other difficulty the API reports, including `share_diff`, is
   * in network units.
   */
  share_multiplier: number;
  hashrate: number;
  shares_accepted: number;
  shares_rejected: number;
  total_work: number;
  best_share_difficulty: number;
  best_share_worker: string | null;
  best_share_at: number | null;
  /** Unix time the lane first saw a worker: when it started keeping score. */
  scoring_since: number | null;
  /** Seconds between shares the lane's vardiff aims for, as currently in force. */
  target_share_seconds: number;
  /** Every coin configured in this lane, parent first, whatever the lane's state. */
  configured_coins: LaneCoin[];
  /** Chains mined in this lane, parent first, once they have had a template. */
  coins: CoinStatus[];
  workers: WorkerStatus[];
}

export interface Status {
  pool_name: string;
  version: string;
  uptime_seconds: number;
  /** TCP port the primary lane's miners connect to, on the same host as this dashboard. */
  stratum_port: number;
  now: number;
  /** Every configured lane, primary first. */
  lanes: LaneStatus[];
  /** Every lane's recent blocks; each `share_diff` is in its own lane's network units. */
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
  /** The lane the coin is mined in: its parent's key, or its own if it is a parent. */
  lane: string;
  /** Whether the coin's lane has connected; until then a fallback-address change is refused. */
  connected: boolean;
  /** null until the coin's node has answered. */
  chain: string | null;
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

export interface LaneSettings {
  key: string;
  /** Address the lane's stratum listener binds. */
  stratum_listen: string;
  /** Whether the lane has its own [coins.<key>.stratum]; lanes without one share [stratum]. */
  own_stratum_section: boolean;
  vardiff: VardiffSettings;
}

export interface SettingsDoc {
  read_only: boolean;
  pool_name: Setting<string>;
  coinbase_tag: Setting<string>;
  coinbase_tag_max_bytes: number;
  /** Every configured coin, primary lane first, connected or not. */
  coins: CoinSettings[];
  /** Every configured lane, primary first. */
  lanes: LaneSettings[];
  log_level: Setting<string>;
  config_toml: string;
}

/** A change: a value stores an override, `null` reverts to the config file, absent leaves it. */
export interface SettingsPatch {
  pool_name?: string | null;
  coinbase_tag?: string | null;
  fallback_addresses?: Record<string, string | null>;
  /**
   * Per-lane changes. A lane on the default [stratum] section shares its vardiff
   * overrides with every other lane on that section.
   */
  lanes?: Record<string, { vardiff?: Partial<Record<VardiffField, number | null>> }>;
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

/** `&lane=<key>` when a lane is named; the daemon defaults to the primary lane. */
function laneParam(lane: string | undefined, first = false): string {
  return lane ? `${first ? '?' : '&'}lane=${encodeURIComponent(lane)}` : '';
}

export const api = {
  health: () => getJson<Health>('/api/health'),
  /** Zero accepted/rejected share counts and best share for every worker of a lane. */
  resetStats: (lane?: string) =>
    sendJson<{ status: string }>('POST', `/api/stats/reset${laneParam(lane, true)}`),
  /** Forget an offline worker of a lane: its row, share log, and hashrate history. */
  removeWorker: (name: string, lane?: string) =>
    sendJson<{ status: string }>(
      'DELETE',
      `/api/workers/${encodeURIComponent(name)}${laneParam(lane, true)}`,
    ),
  /** Give a worker of a lane a display alias (1–32 characters), or clear it with null or ''. */
  setWorkerAlias: (name: string, alias: string | null, lane?: string) =>
    sendJson<{ status: string }>(
      'PUT',
      `/api/workers/${encodeURIComponent(name)}/alias${laneParam(lane, true)}`,
      { alias },
    ),
  status: () => getJson<Status>('/api/status'),
  /** Samples for a lane (empty worker) or one worker over the last `span` seconds. */
  hashrate: (span: number, worker = '', lane?: string) =>
    getJson<HashrateSample[]>(
      `/api/hashrate?span=${span}${worker ? `&worker=${encodeURIComponent(worker)}` : ''}${laneParam(lane)}`,
    ),
  shares: (limit: number, lane?: string) =>
    getJson<ShareRow[]>(`/api/shares?limit=${limit}${laneParam(lane)}`),
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
