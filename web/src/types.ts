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

export interface HistoryEntry {
  snapshot_id: number;
  date: string;
  adds: number;
  deletes: number;
  purchases: number;
  gifts: number;
  adds_windows: number;
  adds_mac: number;
  adds_linux: number;
  fetched_at: string;
  is_anomaly: boolean;
  anomaly_metrics: AnomalyMetrics;
}

export interface PaginatedHistoryResponse {
  entries: HistoryEntry[];
  total: number;
  page: number;
  per_page: number;
}

export interface SnapshotCountriesResponse {
  snapshot_id: number;
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
