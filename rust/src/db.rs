use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::error::{AppError, AppResult};
use crate::steam::{CountryReport, WishlistReport};

/// Result of attempting to insert a snapshot.
pub enum SnapshotChange {
    /// Data unchanged from the latest snapshot.
    NoChange,
    /// First ever snapshot for this app (no previous data to compare against).
    FirstSnapshot,
    /// Data changed — contains the previous snapshot for delta computation.
    Changed { previous: WishlistReport },
}

/// Row from the `game_sync_status` table.
pub struct GameSyncRow {
    pub app_id: u32,
    pub sync_type: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub total_dates: u64,
    pub requested_by: Option<String>,
}

/// A lightweight SQLite connection pool backed by a Tokio MPSC channel.
///
/// Instead of a single `Arc<Mutex<Connection>>`, this maintains multiple
/// connections in WAL mode (which supports concurrent readers). Connections
/// are checked out via an async channel and returned automatically via `Drop`.
struct Pool {
    sender: tokio::sync::mpsc::UnboundedSender<Connection>,
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Connection>>,
}

impl Pool {
    fn new(path: &Path, size: usize) -> AppResult<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for _ in 0..size {
            let conn = Connection::open(path)?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
            tx.send(conn)
                .map_err(|_| AppError::other("Failed to initialize connection pool"))?;
        }
        Ok(Self {
            sender: tx,
            receiver: tokio::sync::Mutex::new(rx),
        })
    }

    async fn get(&self) -> PooledConnection<'_> {
        let conn = self
            .receiver
            .lock()
            .await
            .recv()
            .await
            .expect("Connection pool channel closed unexpectedly");
        PooledConnection {
            conn: Some(conn),
            pool: self,
        }
    }
}

/// RAII guard that returns a connection to the pool on drop.
struct PooledConnection<'a> {
    conn: Option<Connection>,
    pool: &'a Pool,
}

impl<'a> std::ops::Deref for PooledConnection<'a> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().unwrap()
    }
}

impl<'a> Drop for PooledConnection<'a> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = self.pool.sender.send(conn);
        }
    }
}

const POOL_SIZE: usize = 4;

#[derive(Clone)]
pub struct Database {
    pool: Arc<Pool>,
}

impl Database {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::other(format!("Failed to create database directory: {e}"))
            })?;
        }

        // Run migrations on a temporary connection
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Self::migrate(&conn)?;
        drop(conn);

        let pool = Pool::new(path, POOL_SIZE)?;

        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    fn migrate(conn: &Connection) -> AppResult<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tracked_games (
                app_id       INTEGER PRIMARY KEY,
                tracked_since TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE TABLE IF NOT EXISTS wishlist_snapshots (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                app_id     INTEGER NOT NULL,
                date       TEXT NOT NULL,
                adds       INTEGER NOT NULL DEFAULT 0,
                deletes    INTEGER NOT NULL DEFAULT 0,
                purchases  INTEGER NOT NULL DEFAULT 0,
                gifts      INTEGER NOT NULL DEFAULT 0,
                fetched_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE INDEX IF NOT EXISTS idx_snapshots_app_date
                ON wishlist_snapshots(app_id, date);
            CREATE INDEX IF NOT EXISTS idx_snapshots_fetched
                ON wishlist_snapshots(fetched_at);
            CREATE INDEX IF NOT EXISTS idx_snapshots_app_fetched
                ON wishlist_snapshots(app_id, fetched_at);

            CREATE TABLE IF NOT EXISTS app_info (
                app_id    INTEGER PRIMARY KEY,
                name      TEXT NOT NULL,
                image_url TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS app_config (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS channel_subscriptions (
                provider      TEXT NOT NULL,
                channel_id    TEXT NOT NULL,
                app_id        INTEGER NOT NULL,
                subscribed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                PRIMARY KEY (provider, channel_id, app_id)
            );

            CREATE TABLE IF NOT EXISTS snapshot_countries (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                snapshot_id  INTEGER NOT NULL REFERENCES wishlist_snapshots(id) ON DELETE CASCADE,
                country_code TEXT NOT NULL,
                adds         INTEGER NOT NULL DEFAULT 0,
                deletes      INTEGER NOT NULL DEFAULT 0,
                purchases    INTEGER NOT NULL DEFAULT 0,
                gifts        INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_snapshot_countries_snapshot
                ON snapshot_countries(snapshot_id);

            CREATE TABLE IF NOT EXISTS crawled_dates (
                app_id  INTEGER NOT NULL,
                date    TEXT NOT NULL,
                PRIMARY KEY (app_id, date)
            );

            CREATE TABLE IF NOT EXISTS backfill_failed_dates (
                app_id         INTEGER NOT NULL,
                date           TEXT NOT NULL,
                fail_count     INTEGER NOT NULL DEFAULT 1,
                last_failed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                PRIMARY KEY (app_id, date)
            );",
        )?;

        // Add platform columns to existing snapshots table (safe to run repeatedly)
        for col in &["adds_windows", "adds_mac", "adds_linux"] {
            let _ = conn.execute_batch(&format!(
                "ALTER TABLE wishlist_snapshots ADD COLUMN {col} INTEGER NOT NULL DEFAULT 0"
            ));
        }

        // Add min_date column to app_info (safe to run repeatedly)
        let _ = conn.execute_batch("ALTER TABLE app_info ADD COLUMN min_date TEXT");

        // Sync status tracking table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS game_sync_status (
                app_id        INTEGER PRIMARY KEY,
                sync_type     TEXT NOT NULL DEFAULT 'initial',
                status        TEXT NOT NULL DEFAULT 'in_progress',
                started_at    TEXT NOT NULL,
                completed_at  TEXT,
                total_dates   INTEGER NOT NULL DEFAULT 0,
                requested_by  TEXT
            );",
        )?;

        Ok(())
    }

    // ── Config key-value store ──────────────────────────────────────

    pub async fn get_config(&self, key: &str) -> AppResult<Option<String>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare("SELECT value FROM app_config WHERE key = ?1")?;
        let result = stmt.query_row([key], |row| row.get(0)).ok();
        Ok(result)
    }

    pub async fn set_config(&self, key: &str, value: &str) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "INSERT INTO app_config (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub async fn delete_config(&self, key: &str) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute("DELETE FROM app_config WHERE key = ?1", [key])?;
        Ok(())
    }

    pub async fn get_all_config(&self) -> AppResult<HashMap<String, String>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare("SELECT key, value FROM app_config")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<String, String>, _>>()?;
        Ok(rows)
    }

    // ── Tracked games ───────────────────────────────────────────────

    pub async fn add_tracked_game(&self, app_id: u32) -> AppResult<bool> {
        let conn = self.pool.get().await;
        let changed = conn.execute(
            "INSERT OR IGNORE INTO tracked_games (app_id) VALUES (?1)",
            [app_id],
        )?;
        Ok(changed > 0)
    }

    pub async fn remove_tracked_game(&self, app_id: u32) -> AppResult<bool> {
        let conn = self.pool.get().await;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> AppResult<bool> {
            let changed = conn.execute("DELETE FROM tracked_games WHERE app_id = ?1", [app_id])?;
            if changed > 0 {
                conn.execute("DELETE FROM wishlist_snapshots WHERE app_id = ?1", [app_id])?;
                conn.execute("DELETE FROM app_info WHERE app_id = ?1", [app_id])?;
                conn.execute("DELETE FROM crawled_dates WHERE app_id = ?1", [app_id])?;
                conn.execute(
                    "DELETE FROM backfill_failed_dates WHERE app_id = ?1",
                    [app_id],
                )?;
                conn.execute("DELETE FROM game_sync_status WHERE app_id = ?1", [app_id])?;
            }
            Ok(changed > 0)
        })();
        match &result {
            Ok(_) => conn.execute_batch("COMMIT")?,
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
        result
    }

    pub async fn get_tracked_game_ids(&self) -> AppResult<Vec<u32>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare("SELECT app_id FROM tracked_games ORDER BY tracked_since")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<u32>, _>>()?;
        Ok(ids)
    }

    pub async fn get_tracked_games_with_dates(&self) -> AppResult<HashMap<u32, String>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare("SELECT app_id, tracked_since FROM tracked_games")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<u32, String>, _>>()?;
        Ok(rows)
    }

    pub async fn is_tracked(&self, app_id: u32) -> AppResult<bool> {
        let conn = self.pool.get().await;
        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM tracked_games WHERE app_id = ?1",
            [app_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Returns the most recent snapshot for an app, if any.
    pub async fn get_latest_snapshot(&self, app_id: u32) -> AppResult<Option<WishlistReport>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT id, app_id, date, adds, deletes, purchases, gifts,
                    adds_windows, adds_mac, adds_linux, fetched_at
             FROM wishlist_snapshots
             WHERE app_id = ?1
             ORDER BY fetched_at DESC
             LIMIT 1",
        )?;
        let result = stmt
            .query_row([app_id], |row| {
                let snapshot_id: i64 = row.get(0)?;
                Ok((
                    snapshot_id,
                    WishlistReport {
                        app_id: row.get(1)?,
                        date: row.get(2)?,
                        adds: row.get(3)?,
                        deletes: row.get(4)?,
                        purchases: row.get(5)?,
                        gifts: row.get(6)?,
                        adds_windows: row.get(7)?,
                        adds_mac: row.get(8)?,
                        adds_linux: row.get(9)?,
                        countries: Vec::new(),
                        fetched_at: row.get(10)?,
                        app_min_date: None,
                    },
                ))
            })
            .ok();

        match result {
            Some((snapshot_id, mut report)) => {
                report.countries = Self::load_countries(&conn, snapshot_id)?;
                Ok(Some(report))
            }
            None => Ok(None),
        }
    }

    /// Load country breakdown rows for a given snapshot.
    fn load_countries(conn: &Connection, snapshot_id: i64) -> AppResult<Vec<CountryReport>> {
        let mut stmt = conn.prepare(
            "SELECT country_code, adds, deletes, purchases, gifts
             FROM snapshot_countries
             WHERE snapshot_id = ?1",
        )?;
        let rows = stmt
            .query_map([snapshot_id], |row| {
                Ok(CountryReport {
                    country_code: row.get(0)?,
                    adds: row.get(1)?,
                    deletes: row.get(2)?,
                    purchases: row.get(3)?,
                    gifts: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Save country breakdown rows for a snapshot.
    fn save_countries(
        conn: &Connection,
        snapshot_id: i64,
        countries: &[CountryReport],
    ) -> AppResult<()> {
        let mut stmt = conn.prepare(
            "INSERT INTO snapshot_countries (snapshot_id, country_code, adds, deletes, purchases, gifts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for c in countries {
            stmt.execute(rusqlite::params![
                snapshot_id,
                c.country_code,
                c.adds,
                c.deletes,
                c.purchases,
                c.gifts,
            ])?;
        }
        Ok(())
    }

    /// Insert a snapshot only if it differs from the latest one (different date
    /// or different numbers). Returns the kind of change that occurred.
    pub async fn insert_snapshot_if_changed(
        &self,
        report: &WishlistReport,
    ) -> AppResult<SnapshotChange> {
        let conn = self.pool.get().await;

        // Wrap the read-compare-insert in a transaction so no concurrent write
        // can slip in between the comparison and the insert.
        conn.execute_batch("BEGIN IMMEDIATE")?;

        let result = (|| -> AppResult<SnapshotChange> {
            // Fetch latest snapshot inline (using the same connection/transaction)
            let prev = {
                let mut stmt = conn.prepare(
                    "SELECT id, app_id, date, adds, deletes, purchases, gifts,
                            adds_windows, adds_mac, adds_linux, fetched_at
                     FROM wishlist_snapshots
                     WHERE app_id = ?1
                     ORDER BY fetched_at DESC
                     LIMIT 1",
                )?;
                let result = stmt
                    .query_row([report.app_id], |row| {
                        let snapshot_id: i64 = row.get(0)?;
                        Ok((
                            snapshot_id,
                            WishlistReport {
                                app_id: row.get(1)?,
                                date: row.get(2)?,
                                adds: row.get(3)?,
                                deletes: row.get(4)?,
                                purchases: row.get(5)?,
                                gifts: row.get(6)?,
                                adds_windows: row.get(7)?,
                                adds_mac: row.get(8)?,
                                adds_linux: row.get(9)?,
                                countries: Vec::new(),
                                fetched_at: row.get(10)?,
                                app_min_date: None,
                            },
                        ))
                    })
                    .ok();

                match result {
                    Some((snapshot_id, mut r)) => {
                        r.countries = Self::load_countries(&conn, snapshot_id)?;
                        Some(r)
                    }
                    None => None,
                }
            };

            let is_first = prev.is_none();

            if let Some(ref prev) = prev
                && prev.date == report.date
                && prev.adds == report.adds
                && prev.deletes == report.deletes
                && prev.purchases == report.purchases
                && prev.gifts == report.gifts
            {
                return Ok(SnapshotChange::NoChange);
            }

            conn.execute(
                "INSERT INTO wishlist_snapshots (app_id, date, adds, deletes, purchases, gifts, adds_windows, adds_mac, adds_linux)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    report.app_id,
                    report.date,
                    report.adds,
                    report.deletes,
                    report.purchases,
                    report.gifts,
                    report.adds_windows,
                    report.adds_mac,
                    report.adds_linux,
                ],
            )?;

            let snapshot_id = conn.last_insert_rowid();
            Self::save_countries(&conn, snapshot_id, &report.countries)?;

            if is_first {
                Ok(SnapshotChange::FirstSnapshot)
            } else {
                Ok(SnapshotChange::Changed {
                    previous: prev.unwrap(),
                })
            }
        })();

        match &result {
            Ok(SnapshotChange::NoChange) => {
                conn.execute_batch("ROLLBACK")?;
            }
            Ok(_) => {
                conn.execute_batch("COMMIT")?;
            }
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }

        result
    }

    /// Returns the set of dates (YYYY-MM-DD) that already have snapshot data OR have been
    /// crawled (even if no data was available) for a game.
    pub async fn get_crawled_dates_for_game(
        &self,
        app_id: u32,
        include_snapshots: bool,
    ) -> AppResult<std::collections::HashSet<String>> {
        let conn = self.pool.get().await;
        let query = if include_snapshots {
            "SELECT DISTINCT date FROM wishlist_snapshots WHERE app_id = ?1
             UNION
             SELECT date FROM crawled_dates WHERE app_id = ?1"
        } else {
            "SELECT date FROM crawled_dates WHERE app_id = ?1"
        };
        let mut stmt = conn.prepare(query)?;
        let dates = stmt
            .query_map([app_id], |row| row.get::<_, String>(0))?
            .collect::<Result<std::collections::HashSet<String>, _>>()?;
        Ok(dates)
    }

    /// Mark a date as crawled for a game (even if no data was returned by Steam).
    pub async fn mark_date_crawled(&self, app_id: u32, date: &str) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "INSERT OR IGNORE INTO crawled_dates (app_id, date) VALUES (?1, ?2)",
            rusqlite::params![app_id, date],
        )?;
        Ok(())
    }

    /// Insert a snapshot with a specific fetched_at timestamp (for backfilling historical data).
    /// Skips the insert if a snapshot already exists for this (app_id, date).
    pub async fn insert_backfill_snapshot(
        &self,
        report: &WishlistReport,
        fetched_at: &str,
    ) -> AppResult<()> {
        let conn = self.pool.get().await;

        // Wrap exists-check + insert in a transaction to prevent duplicates
        // from concurrent backfill or polling operations.
        conn.execute_batch("BEGIN IMMEDIATE")?;

        let result = (|| -> AppResult<()> {
            // Check if a snapshot already exists for this date.
            // If a backfill snapshot exists (fetched_at ends with T23:59:59Z),
            // update it in-place so data stays visible during re-syncs.
            // If a real-time snapshot exists, skip — don't overwrite live data.
            let existing: Option<(i64, String)> = conn
                .prepare(
                    "SELECT id, fetched_at FROM wishlist_snapshots WHERE app_id = ?1 AND date = ?2",
                )?
                .query_row(rusqlite::params![report.app_id, report.date], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?;

            match existing {
                Some((snapshot_id, existing_fetched_at))
                    if existing_fetched_at.ends_with("T23:59:59Z") =>
                {
                    // Update the existing backfill snapshot in-place
                    conn.execute(
                        "UPDATE wishlist_snapshots
                            SET adds = ?1, deletes = ?2, purchases = ?3, gifts = ?4,
                                adds_windows = ?5, adds_mac = ?6, adds_linux = ?7,
                                fetched_at = ?8
                          WHERE id = ?9",
                        rusqlite::params![
                            report.adds,
                            report.deletes,
                            report.purchases,
                            report.gifts,
                            report.adds_windows,
                            report.adds_mac,
                            report.adds_linux,
                            fetched_at,
                            snapshot_id,
                        ],
                    )?;
                    // Replace country data for this snapshot
                    conn.execute(
                        "DELETE FROM snapshot_countries WHERE snapshot_id = ?1",
                        [snapshot_id],
                    )?;
                    Self::save_countries(&conn, snapshot_id, &report.countries)?;
                }
                Some(_) => {
                    // Real-time snapshot exists — don't overwrite
                    return Ok(());
                }
                None => {
                    // No snapshot for this date — insert new
                    conn.execute(
                        "INSERT INTO wishlist_snapshots (app_id, date, adds, deletes, purchases, gifts, adds_windows, adds_mac, adds_linux, fetched_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        rusqlite::params![
                            report.app_id,
                            report.date,
                            report.adds,
                            report.deletes,
                            report.purchases,
                            report.gifts,
                            report.adds_windows,
                            report.adds_mac,
                            report.adds_linux,
                            fetched_at,
                        ],
                    )?;
                    let snapshot_id = conn.last_insert_rowid();
                    Self::save_countries(&conn, snapshot_id, &report.countries)?;
                }
            }
            Ok(())
        })();

        match &result {
            Ok(_) => conn.execute_batch("COMMIT")?,
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }

        result
    }

    // ── Backfill failure tracking ──────────────────────────────────

    pub async fn mark_date_failed(&self, app_id: u32, date: &str) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "INSERT INTO backfill_failed_dates (app_id, date, fail_count, last_failed_at)
             VALUES (?1, ?2, 1, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             ON CONFLICT(app_id, date) DO UPDATE SET
                fail_count = fail_count + 1,
                last_failed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
            rusqlite::params![app_id, date],
        )?;
        Ok(())
    }

    pub async fn clear_failed_date(&self, app_id: u32, date: &str) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "DELETE FROM backfill_failed_dates WHERE app_id = ?1 AND date = ?2",
            rusqlite::params![app_id, date],
        )?;
        Ok(())
    }

    pub async fn get_failed_dates(
        &self,
        app_id: u32,
    ) -> AppResult<std::collections::HashSet<String>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare("SELECT date FROM backfill_failed_dates WHERE app_id = ?1")?;
        let dates = stmt
            .query_map([app_id], |row| row.get::<_, String>(0))?
            .collect::<Result<std::collections::HashSet<String>, _>>()?;
        Ok(dates)
    }

    // ── Sync status tracking ────────────────────────────────────────

    /// Record the start of a sync for a game. Replaces any previous sync row.
    pub async fn start_sync(
        &self,
        app_id: u32,
        sync_type: &str,
        requested_by: &str,
        total_dates: u64,
    ) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "INSERT OR REPLACE INTO game_sync_status (app_id, sync_type, status, started_at, completed_at, total_dates, requested_by)
             VALUES (?1, ?2, 'in_progress', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), NULL, ?3, ?4)",
            rusqlite::params![app_id, sync_type, total_dates as i64, requested_by],
        )?;
        Ok(())
    }

    /// Mark a sync as completed.
    pub async fn complete_sync(&self, app_id: u32) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "UPDATE game_sync_status SET status = 'completed', completed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE app_id = ?1",
            [app_id],
        )?;
        Ok(())
    }

    /// Mark a sync as failed.
    pub async fn fail_sync(&self, app_id: u32) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "UPDATE game_sync_status SET status = 'failed', completed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE app_id = ?1",
            [app_id],
        )?;
        Ok(())
    }

    /// Get sync status for a specific game.
    pub async fn get_sync_status(&self, app_id: u32) -> AppResult<Option<GameSyncRow>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT app_id, sync_type, status, started_at, completed_at, total_dates, requested_by
             FROM game_sync_status WHERE app_id = ?1",
        )?;
        match stmt.query_row([app_id], |row| {
            Ok(GameSyncRow {
                app_id: row.get(0)?,
                sync_type: row.get(1)?,
                status: row.get(2)?,
                started_at: row.get(3)?,
                completed_at: row.get(4)?,
                total_dates: row.get::<_, i64>(5)? as u64,
                requested_by: row.get(6)?,
            })
        }) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get sync statuses for all games.
    pub async fn get_all_sync_statuses(&self) -> AppResult<Vec<GameSyncRow>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT app_id, sync_type, status, started_at, completed_at, total_dates, requested_by
             FROM game_sync_status",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(GameSyncRow {
                    app_id: row.get(0)?,
                    sync_type: row.get(1)?,
                    status: row.get(2)?,
                    started_at: row.get(3)?,
                    completed_at: row.get(4)?,
                    total_dates: row.get::<_, i64>(5)? as u64,
                    requested_by: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count how many dates have been crawled for a game (efficient count query).
    pub async fn get_crawled_dates_count(&self, app_id: u32) -> AppResult<u64> {
        let conn = self.pool.get().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM (
                SELECT date FROM wishlist_snapshots WHERE app_id = ?1
                UNION
                SELECT date FROM crawled_dates WHERE app_id = ?1
            )",
            [app_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Clear crawled_dates and backfill_failed_dates for a game (used before a
    /// full re-sync).  Backfill snapshots are **kept** so the data remains
    /// visible while the re-sync is in progress; they will be upserted
    /// in-place by `insert_backfill_snapshot`.
    pub async fn clear_sync_progress(&self, app_id: u32) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> AppResult<()> {
            conn.execute("DELETE FROM crawled_dates WHERE app_id = ?1", [app_id])?;
            conn.execute(
                "DELETE FROM backfill_failed_dates WHERE app_id = ?1",
                [app_id],
            )?;
            Ok(())
        })();
        match &result {
            Ok(_) => conn.execute_batch("COMMIT")?,
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
        result
    }

    // ── App min date (cached from Steam API) ────────────────────────

    pub async fn store_app_min_date(&self, app_id: u32, min_date: &str) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "UPDATE app_info SET min_date = ?2 WHERE app_id = ?1",
            rusqlite::params![app_id, min_date],
        )?;
        Ok(())
    }

    pub async fn get_app_min_date(&self, app_id: u32) -> AppResult<Option<String>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare("SELECT min_date FROM app_info WHERE app_id = ?1")?;
        match stmt.query_row([app_id], |row| row.get::<_, Option<String>>(0)) {
            Ok(val) => Ok(val),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => {
                tracing::error!("Failed to get app_min_date for app {app_id}: {e}");
                Err(e.into())
            }
        }
    }

    // ── App info (name & image cache) ────────────────────────────────

    pub async fn upsert_app_info(&self, app_id: u32, name: &str, image_url: &str) -> AppResult<()> {
        let conn = self.pool.get().await;
        conn.execute(
            "INSERT INTO app_info (app_id, name, image_url) VALUES (?1, ?2, ?3)
             ON CONFLICT(app_id) DO UPDATE SET name = excluded.name, image_url = excluded.image_url",
            rusqlite::params![app_id, name, image_url],
        )?;
        Ok(())
    }

    pub async fn get_all_app_info(&self) -> AppResult<HashMap<u32, (String, String)>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare("SELECT app_id, name, image_url FROM app_info")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    (row.get::<_, String>(1)?, row.get::<_, String>(2)?),
                ))
            })?
            .collect::<Result<HashMap<u32, (String, String)>, _>>()?;
        Ok(rows)
    }

    // ── Channel subscriptions ────────────────────────────────────────

    pub async fn subscribe_channel(
        &self,
        provider: &str,
        channel_id: &str,
        app_id: u32,
    ) -> AppResult<bool> {
        let conn = self.pool.get().await;
        let changed = conn.execute(
            "INSERT OR IGNORE INTO channel_subscriptions (provider, channel_id, app_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![provider, channel_id, app_id],
        )?;
        Ok(changed > 0)
    }

    pub async fn unsubscribe_channel(
        &self,
        provider: &str,
        channel_id: &str,
        app_id: u32,
    ) -> AppResult<bool> {
        let conn = self.pool.get().await;
        let changed = conn.execute(
            "DELETE FROM channel_subscriptions WHERE provider = ?1 AND channel_id = ?2 AND app_id = ?3",
            rusqlite::params![provider, channel_id, app_id],
        )?;
        Ok(changed > 0)
    }

    pub async fn get_subscriptions_for_channel(
        &self,
        provider: &str,
        channel_id: &str,
    ) -> AppResult<Vec<u32>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT app_id FROM channel_subscriptions \
             WHERE provider = ?1 AND channel_id = ?2 \
             ORDER BY subscribed_at",
        )?;
        let ids = stmt
            .query_map(rusqlite::params![provider, channel_id], |row| row.get(0))?
            .collect::<Result<Vec<u32>, _>>()?;
        Ok(ids)
    }

    pub async fn get_subscribed_channels(&self, app_id: u32) -> AppResult<Vec<(String, String)>> {
        let conn = self.pool.get().await;
        let mut stmt = conn
            .prepare("SELECT provider, channel_id FROM channel_subscriptions WHERE app_id = ?1")?;
        let rows = stmt
            .query_map([app_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<(String, String)>, _>>()?;
        Ok(rows)
    }

    /// Get the latest snapshot for each tracked game (most recent per app_id).
    /// Games without any snapshot yet are included with zeroed stats.
    pub async fn get_latest_snapshots(&self) -> AppResult<Vec<WishlistReport>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT s.id,
                    t.app_id,
                    COALESCE(s.date, ''),
                    COALESCE(s.adds, 0),
                    COALESCE(s.deletes, 0),
                    COALESCE(s.purchases, 0),
                    COALESCE(s.gifts, 0),
                    COALESCE(s.adds_windows, 0),
                    COALESCE(s.adds_mac, 0),
                    COALESCE(s.adds_linux, 0),
                    s.fetched_at
             FROM tracked_games t
             LEFT JOIN wishlist_snapshots s ON s.app_id = t.app_id
                AND s.id = (
                    SELECT s2.id FROM wishlist_snapshots s2
                    WHERE s2.app_id = t.app_id
                    ORDER BY s2.fetched_at DESC
                    LIMIT 1
                )
             ORDER BY t.tracked_since",
        )?;
        let rows: Vec<(Option<i64>, WishlistReport)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    WishlistReport {
                        app_id: row.get(1)?,
                        date: row.get(2)?,
                        adds: row.get(3)?,
                        deletes: row.get(4)?,
                        purchases: row.get(5)?,
                        gifts: row.get(6)?,
                        adds_windows: row.get(7)?,
                        adds_mac: row.get(8)?,
                        adds_linux: row.get(9)?,
                        countries: Vec::new(),
                        fetched_at: row.get(10)?,
                        app_min_date: None,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut reports = Vec::with_capacity(rows.len());
        for (snapshot_id, mut report) in rows {
            if let Some(sid) = snapshot_id {
                report.countries = Self::load_countries(&conn, sid)?;
            }
            reports.push(report);
        }
        Ok(reports)
    }

    /// Get the total number of snapshots for a game.
    pub async fn get_snapshot_count(&self, app_id: u32) -> AppResult<usize> {
        let conn = self.pool.get().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM wishlist_snapshots WHERE app_id = ?1",
            [app_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Get all-time totals (sum of daily maximums) for a single game.
    /// Uses MAX() per date to avoid double-counting when multiple intra-day
    /// snapshots exist for the same date.
    pub async fn get_game_totals(&self, app_id: u32) -> AppResult<Option<GameTotals>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(SUM(daily_adds), 0),
                    COALESCE(SUM(daily_deletes), 0),
                    COALESCE(SUM(daily_purchases), 0),
                    COALESCE(SUM(daily_gifts), 0)
             FROM (
                SELECT MAX(adds) as daily_adds,
                       MAX(deletes) as daily_deletes,
                       MAX(purchases) as daily_purchases,
                       MAX(gifts) as daily_gifts
                FROM wishlist_snapshots
                WHERE app_id = ?1
                GROUP BY date
             )",
        )?;
        let result = stmt.query_row([app_id], |row| {
            Ok(GameTotals {
                adds: row.get(0)?,
                deletes: row.get(1)?,
                purchases: row.get(2)?,
                gifts: row.get(3)?,
            })
        });
        match result {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get all-time totals (sum of daily maximums) for every tracked game.
    /// Uses MAX() per (app_id, date) to avoid double-counting when multiple
    /// intra-day snapshots exist for the same date.
    pub async fn get_all_game_totals(&self) -> AppResult<HashMap<u32, GameTotals>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT app_id,
                    COALESCE(SUM(daily_adds), 0),
                    COALESCE(SUM(daily_deletes), 0),
                    COALESCE(SUM(daily_purchases), 0),
                    COALESCE(SUM(daily_gifts), 0)
             FROM (
                SELECT app_id,
                       MAX(adds) as daily_adds,
                       MAX(deletes) as daily_deletes,
                       MAX(purchases) as daily_purchases,
                       MAX(gifts) as daily_gifts
                FROM wishlist_snapshots
                GROUP BY app_id, date
             )
             GROUP BY app_id",
        )?;
        let map = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    GameTotals {
                        adds: row.get(1)?,
                        deletes: row.get(2)?,
                        purchases: row.get(3)?,
                        gifts: row.get(4)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect();
        Ok(map)
    }

    // ── Anomaly detection queries ──────────────────────────────────────

    /// Fetch pairwise deltas between consecutive snapshots within the lookback window.
    /// Fetch time-normalized pairwise deltas (rates per day) within the lookback window.
    ///
    /// Each delta is normalized by the actual time elapsed between consecutive snapshots,
    /// so the baseline is comparable regardless of polling frequency.
    pub async fn get_recent_deltas(
        &self,
        app_id: u32,
        lookback_days: u32,
        exclude_after: Option<&str>,
    ) -> AppResult<Vec<SnapshotDelta>> {
        let conn = self.pool.get().await;
        let offset = format!("-{lookback_days} days");
        let rows: Vec<(i64, i64, i64, i64, String)> = if let Some(cutoff) = exclude_after {
            let mut stmt = conn.prepare(
                "SELECT adds, deletes, purchases, gifts, fetched_at
                 FROM wishlist_snapshots
                 WHERE app_id = ?1 AND fetched_at >= datetime('now', ?2) AND fetched_at <= ?3
                 ORDER BY fetched_at ASC",
            )?;
            stmt.query_map(rusqlite::params![app_id, offset, cutoff], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = conn.prepare(
                "SELECT adds, deletes, purchases, gifts, fetched_at
                 FROM wishlist_snapshots
                 WHERE app_id = ?1 AND fetched_at >= datetime('now', ?2)
                 ORDER BY fetched_at ASC",
            )?;
            stmt.query_map(rusqlite::params![app_id, offset], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };

        let mut deltas = Vec::with_capacity(rows.len().saturating_sub(1));
        for pair in rows.windows(2) {
            let (pa, pd, pp, pg, ref prev_ts) = pair[0];
            let (ca, cd, cp, cg, ref curr_ts) = pair[1];
            let days = elapsed_days(prev_ts, curr_ts);
            if days <= 0.0 {
                continue; // skip zero/negative time gaps (e.g. duplicate timestamps)
            }
            let raw_adds = ca - pa;
            let raw_deletes = cd - pd;
            let raw_purchases = cp - pp;
            let raw_gifts = cg - pg;
            deltas.push(SnapshotDelta {
                adds_rate: raw_adds as f64 / days,
                deletes_rate: raw_deletes as f64 / days,
                purchases_rate: raw_purchases as f64 / days,
                gifts_rate: raw_gifts as f64 / days,
            });
        }
        Ok(deltas)
    }

    /// Fetch time-normalized pairwise country-level deltas (rates per day) within the lookback window.
    pub async fn get_recent_country_deltas(
        &self,
        app_id: u32,
        lookback_days: u32,
        exclude_after: Option<&str>,
    ) -> AppResult<HashMap<String, Vec<CountryDelta>>> {
        let conn = self.pool.get().await;
        let offset = format!("-{lookback_days} days");

        // Single query: fetch all country data with timestamps for snapshots in the window.
        let rows: Vec<(i64, String, String, i64, i64)> = if let Some(cutoff) = exclude_after {
            let mut stmt = conn.prepare(
                "SELECT ws.id, ws.fetched_at, sc.country_code, sc.adds, sc.deletes
                 FROM snapshot_countries sc
                 JOIN wishlist_snapshots ws ON ws.id = sc.snapshot_id
                 WHERE ws.app_id = ?1 AND ws.fetched_at >= datetime('now', ?2) AND ws.fetched_at <= ?3
                 ORDER BY ws.fetched_at ASC, sc.country_code ASC",
            )?;
            stmt.query_map(rusqlite::params![app_id, offset, cutoff], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = conn.prepare(
                "SELECT ws.id, ws.fetched_at, sc.country_code, sc.adds, sc.deletes
                 FROM snapshot_countries sc
                 JOIN wishlist_snapshots ws ON ws.id = sc.snapshot_id
                 WHERE ws.app_id = ?1 AND ws.fetched_at >= datetime('now', ?2)
                 ORDER BY ws.fetched_at ASC, sc.country_code ASC",
            )?;
            stmt.query_map(rusqlite::params![app_id, offset], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };

        // Group by snapshot ID, preserving order
        let mut snapshot_ids_ordered: Vec<i64> = Vec::new();
        let mut snapshot_timestamps: HashMap<i64, String> = HashMap::new();
        let mut snapshots_countries: HashMap<i64, HashMap<String, (i64, i64)>> = HashMap::new();
        for (sid, fetched_at, country_code, adds, deletes) in rows {
            if !snapshots_countries.contains_key(&sid) {
                snapshot_ids_ordered.push(sid);
                snapshot_timestamps.insert(sid, fetched_at);
            }
            snapshots_countries
                .entry(sid)
                .or_default()
                .insert(country_code, (adds, deletes));
        }

        if snapshot_ids_ordered.len() < 2 {
            return Ok(HashMap::new());
        }

        // Compute pairwise deltas per country, normalized by time elapsed
        let mut result: HashMap<String, Vec<CountryDelta>> = HashMap::new();
        for pair in snapshot_ids_ordered.windows(2) {
            let prev_ts = &snapshot_timestamps[&pair[0]];
            let curr_ts = &snapshot_timestamps[&pair[1]];
            let days = elapsed_days(prev_ts, curr_ts);
            if days <= 0.0 {
                continue;
            }

            let empty = HashMap::new();
            let prev = snapshots_countries.get(&pair[0]).unwrap_or(&empty);
            let curr = snapshots_countries.get(&pair[1]).unwrap_or(&empty);

            let all_countries: std::collections::HashSet<&String> =
                prev.keys().chain(curr.keys()).collect();

            for country in all_countries {
                let (pa, pd) = prev.get(country.as_str()).copied().unwrap_or((0, 0));
                let (ca, cd) = curr.get(country.as_str()).copied().unwrap_or((0, 0));
                let raw_adds = ca - pa;
                let raw_deletes = cd - pd;
                result
                    .entry(country.to_string())
                    .or_default()
                    .push(CountryDelta {
                        adds_rate: raw_adds as f64 / days,
                        deletes_rate: raw_deletes as f64 / days,
                    });
            }
        }

        Ok(result)
    }

    /// Get aggregated chart data for a game within a time range.
    /// Resolution determines the GROUP BY: "raw" returns individual snapshots,
    /// "daily" groups by date, "weekly" by ISO week, "monthly" by month.
    pub async fn get_chart_data(
        &self,
        app_id: u32,
        since: &str,
        resolution: &str,
    ) -> AppResult<Vec<ChartPoint>> {
        let conn = self.pool.get().await;
        let since = since.to_string();

        match resolution {
            "raw" => {
                let mut stmt = conn.prepare(
                    "SELECT COALESCE(fetched_at, date) as label,
                            adds, deletes, purchases, gifts,
                            adds_windows, adds_mac, adds_linux
                     FROM wishlist_snapshots
                     WHERE app_id = ?1 AND fetched_at >= ?2
                     ORDER BY fetched_at ASC",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![app_id, since], |row| {
                        Ok(ChartPoint {
                            label: row.get(0)?,
                            adds: row.get(1)?,
                            deletes: row.get(2)?,
                            purchases: row.get(3)?,
                            gifts: row.get(4)?,
                            adds_windows: row.get(5)?,
                            adds_mac: row.get(6)?,
                            adds_linux: row.get(7)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            }
            "daily" => {
                let mut stmt = conn.prepare(
                    "SELECT date as label,
                            MAX(adds), MAX(deletes), MAX(purchases), MAX(gifts),
                            MAX(adds_windows), MAX(adds_mac), MAX(adds_linux)
                     FROM wishlist_snapshots
                     WHERE app_id = ?1 AND fetched_at >= ?2
                     GROUP BY date
                     ORDER BY date ASC",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![app_id, since], |row| {
                        Ok(ChartPoint {
                            label: row.get(0)?,
                            adds: row.get(1)?,
                            deletes: row.get(2)?,
                            purchases: row.get(3)?,
                            gifts: row.get(4)?,
                            adds_windows: row.get(5)?,
                            adds_mac: row.get(6)?,
                            adds_linux: row.get(7)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            }
            "weekly" => {
                let mut stmt = conn.prepare(
                    "SELECT strftime('%Y-W%W', fetched_at) as label,
                            MAX(adds), MAX(deletes), MAX(purchases), MAX(gifts),
                            MAX(adds_windows), MAX(adds_mac), MAX(adds_linux)
                     FROM wishlist_snapshots
                     WHERE app_id = ?1 AND fetched_at >= ?2
                     GROUP BY strftime('%Y-W%W', fetched_at)
                     ORDER BY label ASC",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![app_id, since], |row| {
                        Ok(ChartPoint {
                            label: row.get(0)?,
                            adds: row.get(1)?,
                            deletes: row.get(2)?,
                            purchases: row.get(3)?,
                            gifts: row.get(4)?,
                            adds_windows: row.get(5)?,
                            adds_mac: row.get(6)?,
                            adds_linux: row.get(7)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            }
            "monthly" => {
                let mut stmt = conn.prepare(
                    "SELECT strftime('%Y-%m', fetched_at) as label,
                            MAX(adds), MAX(deletes), MAX(purchases), MAX(gifts),
                            MAX(adds_windows), MAX(adds_mac), MAX(adds_linux)
                     FROM wishlist_snapshots
                     WHERE app_id = ?1 AND fetched_at >= ?2
                     GROUP BY strftime('%Y-%m', fetched_at)
                     ORDER BY label ASC",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![app_id, since], |row| {
                        Ok(ChartPoint {
                            label: row.get(0)?,
                            adds: row.get(1)?,
                            deletes: row.get(2)?,
                            purchases: row.get(3)?,
                            gifts: row.get(4)?,
                            adds_windows: row.get(5)?,
                            adds_mac: row.get(6)?,
                            adds_linux: row.get(7)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Get aggregated country data for a game within a time range.
    /// Sums adds/deletes/purchases/gifts across all snapshots in the window,
    /// grouped by country_code, ordered by total adds descending.
    pub async fn get_aggregated_countries(
        &self,
        app_id: u32,
        since: &str,
    ) -> AppResult<Vec<crate::steam::CountryReport>> {
        let conn = self.pool.get().await;
        let since = since.to_string();

        let mut stmt = conn.prepare(
            "SELECT sc.country_code,
                    SUM(sc.adds), SUM(sc.deletes), SUM(sc.purchases), SUM(sc.gifts)
             FROM snapshot_countries sc
             INNER JOIN wishlist_snapshots ws ON ws.id = sc.snapshot_id
             WHERE ws.app_id = ?1 AND ws.fetched_at >= ?2
             GROUP BY sc.country_code
             ORDER BY SUM(sc.adds) DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![app_id, since], |row| {
                Ok(crate::steam::CountryReport {
                    country_code: row.get(0)?,
                    adds: row.get(1)?,
                    deletes: row.get(2)?,
                    purchases: row.get(3)?,
                    gifts: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get paginated snapshots for a game (newest first), without country data.
    /// Returns (snapshot_id, report) pairs and the total count.
    pub async fn get_snapshots_paginated(
        &self,
        app_id: u32,
        page: usize,
        per_page: usize,
    ) -> AppResult<PaginatedSnapshots> {
        let conn = self.pool.get().await;

        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM wishlist_snapshots WHERE app_id = ?1",
            [app_id],
            |row| row.get(0),
        )?;
        let total = total as usize;

        let offset = (page.saturating_sub(1) * per_page) as i64;
        let per_page_i64 = per_page as i64;
        let mut stmt = conn.prepare(
            "SELECT id, app_id, date, adds, deletes, purchases, gifts,
                    adds_windows, adds_mac, adds_linux, fetched_at
             FROM wishlist_snapshots
             WHERE app_id = ?1
             ORDER BY fetched_at DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![app_id, per_page_i64, offset], |row| {
                Ok((
                    row.get(0)?,
                    WishlistReport {
                        app_id: row.get(1)?,
                        date: row.get(2)?,
                        adds: row.get(3)?,
                        deletes: row.get(4)?,
                        purchases: row.get(5)?,
                        gifts: row.get(6)?,
                        adds_windows: row.get(7)?,
                        adds_mac: row.get(8)?,
                        adds_linux: row.get(9)?,
                        countries: Vec::new(),
                        fetched_at: row.get(10)?,
                        app_min_date: None,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(PaginatedSnapshots {
            snapshots: rows,
            total,
        })
    }

    /// Get raw snapshots within a bounded time range (for anomaly lookback context).
    pub async fn get_raw_snapshots_between(
        &self,
        app_id: u32,
        since: &str,
        until: &str,
    ) -> AppResult<Vec<ChartPoint>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(fetched_at, date) as label,
                    adds, deletes, purchases, gifts,
                    adds_windows, adds_mac, adds_linux
             FROM wishlist_snapshots
             WHERE app_id = ?1 AND fetched_at >= ?2 AND fetched_at <= ?3
             ORDER BY fetched_at ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![app_id, since, until], |row| {
                Ok(ChartPoint {
                    label: row.get(0)?,
                    adds: row.get(1)?,
                    deletes: row.get(2)?,
                    purchases: row.get(3)?,
                    gifts: row.get(4)?,
                    adds_windows: row.get(5)?,
                    adds_mac: row.get(6)?,
                    adds_linux: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get country data for a specific snapshot, verifying it belongs to the given app.
    /// Returns `None` if the snapshot doesn't exist or doesn't belong to `app_id`.
    pub async fn get_snapshot_countries(
        &self,
        app_id: u32,
        snapshot_id: i64,
    ) -> AppResult<Option<Vec<CountryReport>>> {
        let conn = self.pool.get().await;
        let owns: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM wishlist_snapshots WHERE id = ?1 AND app_id = ?2",
                rusqlite::params![snapshot_id, app_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if !owns {
            return Ok(None);
        }
        Ok(Some(Self::load_countries(&conn, snapshot_id)?))
    }
}

/// Parse a flexible timestamp/label string into a NaiveDateTime.
/// Supports: full ISO 8601, date-only, weekly ("2025-W03"), and monthly ("2025-01").
fn parse_flexible_timestamp(s: &str) -> Option<chrono::NaiveDateTime> {
    use chrono::{NaiveDate, NaiveDateTime};
    // Full datetime: "2025-01-15T12:30:00Z" or "2025-01-15 12:30:00"
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .ok()
        // Date only: "2025-01-15"
        .or_else(|| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })
        // Weekly: "2025-W03" (SQLite %W = Monday-start week 00-53)
        .or_else(|| {
            if s.len() >= 7 && s.contains("-W") {
                let parts: Vec<&str> = s.split("-W").collect();
                if parts.len() == 2 {
                    let year: i32 = parts[0].parse().ok()?;
                    let week: u32 = parts[1].parse().ok()?;
                    // Approximate: Jan 1 + week * 7 days
                    let jan1 = NaiveDate::from_ymd_opt(year, 1, 1)?;
                    let day = jan1 + chrono::TimeDelta::days(week as i64 * 7);
                    day.and_hms_opt(0, 0, 0)
                } else {
                    None
                }
            } else {
                None
            }
        })
        // Monthly: "2025-01"
        .or_else(|| {
            if s.len() == 7 && s.as_bytes()[4] == b'-' {
                let year: i32 = s[..4].parse().ok()?;
                let month: u32 = s[5..7].parse().ok()?;
                NaiveDate::from_ymd_opt(year, month, 1).and_then(|d| d.and_hms_opt(0, 0, 0))
            } else {
                None
            }
        })
}

/// Compute elapsed days between two ISO 8601 timestamps.
pub fn elapsed_days(from: &str, to: &str) -> f64 {
    match (parse_flexible_timestamp(from), parse_flexible_timestamp(to)) {
        (Some(a), Some(b)) => (b - a).num_seconds() as f64 / 86400.0,
        _ => 0.0,
    }
}

/// Convert a flexible timestamp/label string to epoch seconds.
/// Returns 0.0 if the string cannot be parsed.
pub fn label_to_epoch_secs(s: &str) -> f64 {
    parse_flexible_timestamp(s)
        .map(|dt| dt.and_utc().timestamp() as f64)
        .unwrap_or(0.0)
}

/// Snapshot-to-snapshot delta rate (per day) for anomaly detection.
pub struct SnapshotDelta {
    pub adds_rate: f64,
    pub deletes_rate: f64,
    pub purchases_rate: f64,
    pub gifts_rate: f64,
}

/// Country-level delta rate (per day) for anomaly detection.
pub struct CountryDelta {
    pub adds_rate: f64,
    pub deletes_rate: f64,
}

/// All-time totals for a single game (summed across all snapshots).
pub struct GameTotals {
    pub adds: i64,
    pub deletes: i64,
    pub purchases: i64,
    pub gifts: i64,
}

/// A single aggregated chart data point.
pub struct ChartPoint {
    pub label: String,
    pub adds: i64,
    pub deletes: i64,
    pub purchases: i64,
    pub gifts: i64,
    pub adds_windows: i64,
    pub adds_mac: i64,
    pub adds_linux: i64,
}

/// Result for a paginated history query.
pub struct PaginatedSnapshots {
    pub snapshots: Vec<(i64, WishlistReport)>,
    pub total: usize,
}

/// Determine the default database path for the current platform.
pub fn default_db_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("wishlist-pulse")
        .join("data.db")
}
