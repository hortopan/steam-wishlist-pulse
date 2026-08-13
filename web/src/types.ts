export interface CountryEntry {
  country_code: string;
  adds: number;
  deletes: number;
  purchases: number;
  gifts: number;
}

export interface GameReport {
  app_id: number;
  name: string;
  image_url: string;
  date: string;
  adds: number;
  deletes: number;
  purchases: number;
  gifts: number;
  adds_windows: number;
  adds_mac: number;
  adds_linux: number;
  countries: CountryEntry[];
  changed_at: string | null;
  total_adds: number;
  total_deletes: number;
  total_purchases: number;
  total_gifts: number;
  current_wishlists: number;
}

export interface AnomalyMetrics {
  adds: boolean;
  deletes: boolean;
  purchases: boolean;
  gifts: boolean;
  descriptions?: string[];
}

// ── Split API types ──────────────────────────────────────────────

export interface GameDetailResponse {
  app_id: number;
  name: string;
  image_url: string;
  latest: GameReport | null;
  total_snapshots: number;
}

export interface ChartPoint {
  label: string;
  /** Steam reporting date (YYYY-MM-DD, Pacific). */
  date: string;
  adds: number;
  deletes: number;
  purchases: number;
  gifts: number;
  adds_windows: number;
  adds_mac: number;
  adds_linux: number;
  is_anomaly: boolean;
  anomaly_metrics: AnomalyMetrics;
}

export interface ChartResponse {
  resolution: string;
  points: ChartPoint[];
}

export interface HistorySnapshotEntry {
  snapshot_id: number;
  fetched_at: string;
  delta_adds: number;
  delta_deletes: number;
  delta_purchases: number;
  delta_gifts: number;
  delta_adds_windows: number;
  delta_adds_mac: number;
  delta_adds_linux: number;
  /** True for backfill/verification rows whose fetched_at is a sentinel, not a real time. */
  is_eod_report: boolean;
}

export interface HistoryDayEntry {
  /** Steam reporting date (YYYY-MM-DD, Pacific). */
  date: string;
  adds: number;
  deletes: number;
  purchases: number;
  gifts: number;
  adds_windows: number;
  adds_mac: number;
  adds_linux: number;
  snapshot_count: number;
  last_snapshot_id: number;
  is_daily_report: boolean;
  is_anomaly: boolean;
  anomaly_metrics: AnomalyMetrics;
  snapshots: HistorySnapshotEntry[];
}

export interface DailyHistoryResponse {
  days: HistoryDayEntry[];
  total_days: number;
  page: number;
  per_page: number;
}

export interface DayCountriesResponse {
  date: string;
  countries: CountryEntry[];
}

export interface AggregatedCountriesResponse {
  countries: CountryEntry[];
}

export interface TrackedGame {
  app_id: number;
  name: string;
  image_url: string;
  tracked_since: string;
  is_syncing: boolean;
  sync_type: string | null;
  sync_progress_crawled: number;
  sync_progress_total: number;
  last_sync_completed_at: string | null;
  cooldown_active: boolean;
}

export interface SyncStatus {
  app_id: number;
  is_syncing: boolean;
  sync_type: string | null;
  started_at: string | null;
  completed_at: string | null;
  progress_crawled: number;
  progress_total: number;
  last_completed_at: string | null;
  cooldown_active: boolean;
  requested_by: string | null;
}
