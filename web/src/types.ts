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
}

export interface AnomalyMetrics {
  adds: boolean;
  deletes: boolean;
  purchases: boolean;
  gifts: boolean;
}

export interface SnapshotEntry {
  date: string;
  adds: number;
  deletes: number;
  purchases: number;
  gifts: number;
  adds_windows: number;
  adds_mac: number;
  adds_linux: number;
  countries: CountryEntry[];
  fetched_at: string;
  is_anomaly: boolean;
  anomaly_metrics: AnomalyMetrics;
}

export interface GameDetailResponse {
  app_id: number;
  name: string;
  image_url: string;
  latest: GameReport | null;
  history: SnapshotEntry[];
}

export interface TrackedGame {
  app_id: number;
  name: string;
  image_url: string;
  tracked_since: string;
}
