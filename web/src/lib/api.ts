/** Typed client for the daemon's JSON API. */

export interface Health {
  status: string;
  version: string;
  uptime_seconds: number;
}

export interface Status {
  pool_name: string;
  version: string;
  uptime_seconds: number;
}

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
};
