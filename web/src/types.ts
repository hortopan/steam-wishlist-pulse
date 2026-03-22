export interface GameReport {
  app_id: number;
  name: string;
  image_url: string;
  date: string;
  adds: number;
  deletes: number;
  purchases: number;
  gifts: number;
  changed_at: string | null;
}

export interface SnapshotEntry {
  date: string;
  adds: number;
  deletes: number;
  purchases: number;
  gifts: number;
  fetched_at: string;
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
