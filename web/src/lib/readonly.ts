/**
 * Whether the dashboard is read-only (`[web] read_only = true`), from the settings
 * document. Asked once per page load; a failed answer (the pool still connecting) is
 * treated as writable and asked again next time, since the daemon refuses writes anyway.
 */
import { api } from './api';

let answer: Promise<boolean> | null = null;

export function dashboardReadOnly(): Promise<boolean> {
  answer ??= api.settings().then(
    (doc) => doc.read_only,
    () => {
      answer = null;
      return false;
    },
  );
  return answer;
}
