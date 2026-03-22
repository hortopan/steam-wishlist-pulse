export interface ServiceHealthInfo {
  status: "ok" | "disabled" | "not_configured" | "error";
  message?: string;
}

export interface HealthData {
  steam: ServiceHealthInfo;
  telegram: ServiceHealthInfo;
  discord: ServiceHealthInfo;
}

type Listener = (data: HealthData | null) => void;

let _health: HealthData | null = null;
let _listeners: Listener[] = [];
let _timer: ReturnType<typeof setInterval> | null = null;
let _active = false;

const POLL_INTERVAL = 30_000;

function notify() {
  for (const fn of _listeners) fn(_health);
}

export async function fetchHealth() {
  try {
    const res = await fetch("/api/admin/health");
    if (res.ok) {
      _health = await res.json();
      notify();
    }
  } catch {
    // ignore network errors
  }
}

export function getHealth(): HealthData | null {
  return _health;
}

export function hasAnyIssue(): boolean {
  if (!_health) return false;
  return [_health.steam, _health.telegram, _health.discord].some(
    (s) => s.status === "error" || s.status === "not_configured"
  );
}

export function subscribe(fn: Listener): () => void {
  _listeners.push(fn);
  // Immediately call with current value
  fn(_health);
  return () => {
    _listeners = _listeners.filter((l) => l !== fn);
  };
}

export function startPolling() {
  if (_active) return;
  _active = true;
  fetchHealth();
  _timer = setInterval(fetchHealth, POLL_INTERVAL);
}

export function stopPolling() {
  _active = false;
  _health = null;
  if (_timer) {
    clearInterval(_timer);
    _timer = null;
  }
  notify();
}
