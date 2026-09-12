/**
 * Live connection to the daemon: the status document over `/api/ws`, with a bounded
 * ring of recent shares, and a polling fallback when the socket is down.
 */
import { api, wsUrl, type Push, type Share, type Status } from './api';

export const SHARE_LOG_SIZE = 100;
const POLL_MS = 5000;
const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 15000;

export type ConnectionState = 'connecting' | 'live' | 'polling' | 'offline';

class Live {
  status = $state<Status | null>(null);
  shares = $state<Share[]>([]);
  connection = $state<ConnectionState>('connecting');
  error = $state<string | null>(null);
  /** Bumps on every accepted share so cheap UI effects can react. */
  tick = $state(0);

  private socket: WebSocket | null = null;
  private poll: ReturnType<typeof setInterval> | null = null;
  private reconnectMs = RECONNECT_MIN_MS;
  private stopped = false;

  start() {
    this.stopped = false;
    void this.seedShares();
    this.connect();
  }

  stop() {
    this.stopped = true;
    this.socket?.close();
    this.socket = null;
    this.stopPolling();
  }

  private async seedShares() {
    try {
      const rows = await api.shares(SHARE_LOG_SIZE);
      // The API is newest first; keep it that way and let pushes prepend.
      if (this.shares.length === 0) this.shares = rows;
    } catch {
      // History is optional; the live log fills in as shares arrive.
    }
  }

  private connect() {
    if (this.stopped) return;
    this.connection = this.status ? this.connection : 'connecting';
    let ws: WebSocket;
    try {
      ws = new WebSocket(wsUrl());
    } catch (e) {
      this.onDown(e);
      return;
    }
    this.socket = ws;
    ws.onopen = () => {
      this.reconnectMs = RECONNECT_MIN_MS;
      this.connection = 'live';
      this.error = null;
      this.stopPolling();
    };
    ws.onmessage = (ev) => {
      let push: Push;
      try {
        push = JSON.parse(ev.data as string) as Push;
      } catch {
        return;
      }
      if (push.type === 'status') {
        this.status = push.data;
      } else if (push.type === 'share') {
        this.pushShare(push.data);
      }
    };
    ws.onerror = () => {
      /* onclose follows and handles it */
    };
    ws.onclose = () => {
      if (this.socket === ws) this.socket = null;
      this.onDown(null);
    };
  }

  private onDown(e: unknown) {
    if (this.stopped) return;
    if (e instanceof Error) this.error = e.message;
    this.startPolling();
    const delay = this.reconnectMs;
    this.reconnectMs = Math.min(this.reconnectMs * 2, RECONNECT_MAX_MS);
    setTimeout(() => this.connect(), delay);
  }

  private startPolling() {
    if (this.poll) return;
    const refresh = async () => {
      try {
        this.status = await api.status();
        this.connection = 'polling';
        this.error = null;
      } catch (e) {
        this.connection = 'offline';
        this.error = e instanceof Error ? e.message : String(e);
      }
    };
    void refresh();
    this.poll = setInterval(refresh, POLL_MS);
  }

  private stopPolling() {
    if (this.poll) {
      clearInterval(this.poll);
      this.poll = null;
    }
  }

  private pushShare(share: Share) {
    const next = [share, ...this.shares];
    if (next.length > SHARE_LOG_SIZE) next.length = SHARE_LOG_SIZE;
    this.shares = next;
    this.tick += 1;
  }
}

export const live = new Live();
