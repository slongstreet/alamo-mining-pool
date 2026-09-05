/**
 * Live pool status. Streams snapshots over the WebSocket, falls back to polling
 * `/api/status` while the socket is down, and keeps reconnecting with backoff.
 */
import { api, wsUrl, type Status } from './api';

export type Connection = 'connecting' | 'live' | 'polling' | 'offline';

const POLL_MS = 5_000;
const BACKOFF_MS = [1_000, 2_000, 5_000, 10_000, 30_000];

export class LiveStatus {
  status = $state<Status | null>(null);
  connection = $state<Connection>('connecting');
  error = $state<string | null>(null);

  #socket: WebSocket | null = null;
  #attempt = 0;
  #reconnect: ReturnType<typeof setTimeout> | null = null;
  #poll: ReturnType<typeof setInterval> | null = null;
  #stopped = false;

  start() {
    this.#stopped = false;
    this.#connect();
    this.#poll = setInterval(() => void this.#pollOnce(), POLL_MS);
    void this.#pollOnce();
  }

  stop() {
    this.#stopped = true;
    if (this.#reconnect) clearTimeout(this.#reconnect);
    if (this.#poll) clearInterval(this.#poll);
    this.#socket?.close();
    this.#socket = null;
  }

  #connect() {
    if (this.#stopped) return;
    let socket: WebSocket;
    try {
      socket = new WebSocket(wsUrl());
    } catch {
      this.#scheduleReconnect();
      return;
    }
    this.#socket = socket;
    socket.onopen = () => {
      this.#attempt = 0;
      this.connection = 'live';
      this.error = null;
    };
    socket.onmessage = (event) => {
      try {
        this.status = JSON.parse(event.data as string) as Status;
      } catch (e) {
        this.error = e instanceof Error ? e.message : String(e);
      }
    };
    socket.onclose = () => {
      if (this.#socket === socket) this.#socket = null;
      if (this.connection === 'live') this.connection = 'polling';
      this.#scheduleReconnect();
    };
    socket.onerror = () => socket.close();
  }

  #scheduleReconnect() {
    if (this.#stopped || this.#reconnect) return;
    const delay = BACKOFF_MS[Math.min(this.#attempt, BACKOFF_MS.length - 1)];
    this.#attempt += 1;
    this.#reconnect = setTimeout(() => {
      this.#reconnect = null;
      this.#connect();
    }, delay);
  }

  /** Polling covers the gap while the socket is down and doubles as a liveness check. */
  async #pollOnce() {
    if (this.#isLive()) return;
    try {
      const status = await api.status();
      // The socket may have opened while the request was in flight; its snapshots win.
      if (this.#isLive()) return;
      this.status = status;
      this.error = null;
      this.connection = 'polling';
    } catch (e) {
      if (this.#isLive()) return;
      this.error = e instanceof Error ? e.message : String(e);
      this.connection = 'offline';
    }
  }

  #isLive(): boolean {
    return this.#socket?.readyState === WebSocket.OPEN;
  }
}
