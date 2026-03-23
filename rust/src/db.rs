use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

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
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::other(format!("Failed to create database directory: {e}")))?;
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
            );",
        )?;

        // Add platform columns to existing snapshots table (safe to run repeatedly)
        for col in &["adds_windows", "adds_mac", "adds_linux"] {
            let _ = conn.execute_batch(
                &format!("ALTER TABLE wishlist_snapshots ADD COLUMN {col} INTEGER NOT NULL DEFAULT 0"),
            );
        }

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
        let changed =
            conn.execute("DELETE FROM tracked_games WHERE app_id = ?1", [app_id])?;
        if changed > 0 {
            conn.execute("DELETE FROM wishlist_snapshots WHERE app_id = ?1", [app_id])?;
            conn.execute("DELETE FROM app_info WHERE app_id = ?1", [app_id])?;
            conn.execute("DELETE FROM crawled_dates WHERE app_id = ?1", [app_id])?;
        }
        Ok(changed > 0)
    }

    pub async fn get_tracked_game_ids(&self) -> AppResult<Vec<u32>> {
        let conn = self.pool.get().await;
        let mut stmt =
            conn.prepare("SELECT app_id FROM tracked_games ORDER BY tracked_since")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<u32>, _>>()?;
        Ok(ids)
    }

    pub async fn get_tracked_games_with_dates(&self) -> AppResult<HashMap<u32, String>> {
        let conn = self.pool.get().await;
        let mut stmt =
            conn.prepare("SELECT app_id, tracked_since FROM tracked_games")?;
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
                Ok((snapshot_id, WishlistReport {
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
                }))
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
    fn save_countries(conn: &Connection, snapshot_id: i64, countries: &[CountryReport]) -> AppResult<()> {
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
        let prev = self.get_latest_snapshot(report.app_id).await?;

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

        let conn = self.pool.get().await;
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
    }

    /// Returns the set of dates (YYYY-MM-DD) that already have snapshot data OR have been
    /// crawled (even if no data was available) for a game.
    pub async fn get_crawled_dates_for_game(&self, app_id: u32) -> AppResult<std::collections::HashSet<String>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT date FROM wishlist_snapshots WHERE app_id = ?1
             UNION
             SELECT date FROM crawled_dates WHERE app_id = ?1",
        )?;
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
    pub async fn insert_backfill_snapshot(&self, report: &WishlistReport, fetched_at: &str) -> AppResult<()> {
        let conn = self.pool.get().await;
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
        Ok(())
    }

    pub async fn purge_old_snapshots(&self, retention_days: u32) -> AppResult<u64> {
        let conn = self.pool.get().await;
        let deleted = conn.execute(
            "DELETE FROM wishlist_snapshots
             WHERE date < date('now', ?1)",
            [format!("-{retention_days} days")],
        )?;
        Ok(deleted as u64)
    }

    // ── App info (name & image cache) ────────────────────────────────

    pub async fn upsert_app_info(
        &self,
        app_id: u32,
        name: &str,
        image_url: &str,
    ) -> AppResult<()> {
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

    pub async fn get_subscribed_channels(
        &self,
        app_id: u32,
    ) -> AppResult<Vec<(String, String)>> {
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

    /// Get all snapshots for a specific game, ordered by date then fetched_at.
    pub async fn get_snapshots_for_game(&self, app_id: u32) -> AppResult<Vec<WishlistReport>> {
        let conn = self.pool.get().await;
        let mut stmt = conn.prepare(
            "SELECT id, app_id, date, adds, deletes, purchases, gifts,
                    adds_windows, adds_mac, adds_linux, fetched_at
             FROM wishlist_snapshots
             WHERE app_id = ?1
             ORDER BY fetched_at ASC",
        )?;
        let rows: Vec<(i64, WishlistReport)> = stmt
            .query_map([app_id], |row| {
                Ok((row.get(0)?, WishlistReport {
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
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut reports = Vec::with_capacity(rows.len());
        for (snapshot_id, mut report) in rows {
            report.countries = Self::load_countries(&conn, snapshot_id)?;
            reports.push(report);
        }
        Ok(reports)
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
                Ok((row.get(0)?, WishlistReport {
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
                }))
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
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
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
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
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
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
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
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
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
}

/// Compute elapsed days between two ISO 8601 timestamps.
pub fn elapsed_days(from: &str, to: &str) -> f64 {
    use chrono::NaiveDateTime;
    let parse = |s: &str| {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
            .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
            .ok()
    };
    match (parse(from), parse(to)) {
        (Some(a), Some(b)) => (b - a).num_seconds() as f64 / 86400.0,
        _ => 0.0,
    }
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

/// Determine the default database path for the current platform.
pub fn default_db_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("wishlist-pulse")
        .join("data.db")
}
