#!/usr/bin/env python3
"""
Seed the wishlist-pulse SQLite database with fake snapshot data.

Usage:
    python scripts/seed_fake_data.py --app-id 12345 --days 300 --per-day 24
    python scripts/seed_fake_data.py --app-id 12345 --days 300 --per-day 24 --db ~/custom/path/data.db
    python scripts/seed_fake_data.py --app-id 12345 --days 300 --per-day 24 --app-name "My Cool Game"

Options:
    --app-id    Steam app ID (required)
    --days      Number of days of history to generate (required)
    --per-day   Number of snapshots per day (required)
    --db        Path to SQLite database (default: ~/.local/share/wishlist-pulse/data.db)
    --app-name  Game name for app_info table (default: "Fake Game <app_id>")
    --base-adds Starting daily add rate (default: 50)
    --trend     Daily trend multiplier, e.g. 1.01 = 1% growth/day (default: 1.002)
    --clear     Remove existing snapshots for this app before seeding
"""

import argparse
import math
import os
import random
import sqlite3
import sys
from datetime import datetime, timedelta

# Top countries by Steam usage with approximate relative weights
COUNTRIES = [
    ("US", 20), ("CN", 15), ("RU", 10), ("DE", 7), ("BR", 6),
    ("GB", 5), ("FR", 5), ("CA", 4), ("AU", 3), ("JP", 3),
    ("KR", 3), ("PL", 3), ("TR", 2), ("IT", 2), ("ES", 2),
    ("SE", 1), ("NL", 1), ("AR", 1), ("MX", 1), ("IN", 1),
]


def default_db_path():
    if sys.platform == "darwin":
        base = os.path.expanduser("~/Library/Application Support")
    elif sys.platform == "win32":
        base = os.environ.get("APPDATA", os.path.expanduser("~"))
    else:
        base = os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share"))
    return os.path.join(base, "wishlist-pulse", "data.db")


def generate_snapshots(app_id, days, per_day, base_adds, trend):
    """Generate cumulative snapshot data with realistic patterns."""
    now = datetime.utcnow()
    start = now - timedelta(days=days)
    interval = timedelta(hours=24 / per_day)

    # Cumulative counters
    total_adds = 0
    total_deletes = 0
    total_purchases = 0
    total_gifts = 0

    snapshots = []
    total_weight = sum(w for _, w in COUNTRIES)
    ts = start

    for day in range(days):
        # Daily rate with trend, seasonality, and noise
        day_factor = trend ** day
        # Weekly seasonality: weekends get ~30% more adds
        weekday = (start + timedelta(days=day)).weekday()
        weekend_boost = 1.3 if weekday >= 5 else 1.0
        # Slight monthly wave
        monthly_wave = 1.0 + 0.1 * math.sin(2 * math.pi * day / 30)

        daily_adds = base_adds * day_factor * weekend_boost * monthly_wave
        daily_deletes = daily_adds * random.uniform(0.05, 0.15)
        daily_purchases = daily_adds * random.uniform(0.02, 0.08)
        daily_gifts = daily_adds * random.uniform(0.005, 0.02)

        for snap_i in range(per_day):
            # Distribute daily totals across snapshots with some noise
            frac = (1.0 / per_day) * random.uniform(0.7, 1.3)

            snap_adds = max(0, int(daily_adds * frac + random.gauss(0, 2)))
            snap_deletes = max(0, int(daily_deletes * frac + random.gauss(0, 1)))
            snap_purchases = max(0, int(daily_purchases * frac + random.gauss(0, 0.5)))
            snap_gifts = max(0, int(daily_gifts * frac + random.gauss(0, 0.3)))

            total_adds += snap_adds
            total_deletes += snap_deletes
            total_purchases += snap_purchases
            total_gifts += snap_gifts

            # Platform split (roughly: 85% Windows, 10% Mac, 5% Linux)
            adds_win = int(snap_adds * random.uniform(0.80, 0.90))
            adds_mac = int(snap_adds * random.uniform(0.06, 0.12))
            adds_linux = max(0, snap_adds - adds_win - adds_mac)

            date_str = ts.strftime("%Y-%m-%d")
            fetched_at = ts.strftime("%Y-%m-%dT%H:%M:%SZ")

            # Country breakdown for this snapshot's adds/deletes
            countries = []
            remaining_adds = snap_adds
            remaining_deletes = snap_deletes
            remaining_purchases = snap_purchases
            remaining_gifts = snap_gifts

            for i, (code, weight) in enumerate(COUNTRIES):
                is_last = (i == len(COUNTRIES) - 1)
                if is_last:
                    c_adds = remaining_adds
                    c_deletes = remaining_deletes
                    c_purchases = remaining_purchases
                    c_gifts = remaining_gifts
                else:
                    share = (weight / total_weight) * random.uniform(0.7, 1.3)
                    c_adds = min(remaining_adds, max(0, int(snap_adds * share)))
                    c_deletes = min(remaining_deletes, max(0, int(snap_deletes * share)))
                    c_purchases = min(remaining_purchases, max(0, int(snap_purchases * share)))
                    c_gifts = min(remaining_gifts, max(0, int(snap_gifts * share)))
                    remaining_adds -= c_adds
                    remaining_deletes -= c_deletes
                    remaining_purchases -= c_purchases
                    remaining_gifts -= c_gifts

                if c_adds > 0 or c_deletes > 0 or c_purchases > 0 or c_gifts > 0:
                    countries.append((code, c_adds, c_deletes, c_purchases, c_gifts))

            snapshots.append({
                "app_id": app_id,
                "date": date_str,
                "adds": total_adds,
                "deletes": total_deletes,
                "purchases": total_purchases,
                "gifts": total_gifts,
                "adds_windows": adds_win,
                "adds_mac": adds_mac,
                "adds_linux": adds_linux,
                "fetched_at": fetched_at,
                "countries": countries,
            })

            ts += interval

    return snapshots


def main():
    parser = argparse.ArgumentParser(description="Seed wishlist-pulse DB with fake data")
    parser.add_argument("--app-id", type=int, required=True, help="Steam app ID")
    parser.add_argument("--days", type=int, required=True, help="Days of history")
    parser.add_argument("--per-day", type=int, required=True, help="Snapshots per day")
    parser.add_argument("--db", type=str, default=None, help="Database path")
    parser.add_argument("--app-name", type=str, default=None, help="Game name")
    parser.add_argument("--base-adds", type=float, default=50, help="Base daily adds rate")
    parser.add_argument("--trend", type=float, default=1.002, help="Daily trend multiplier")
    parser.add_argument("--clear", action="store_true", help="Clear existing data for this app first")
    args = parser.parse_args()

    db_path = args.db or os.environ.get("DATABASE_PATH") or default_db_path()
    app_name = args.app_name or f"Fake Game {args.app_id}"

    if not os.path.exists(db_path):
        print(f"Error: Database not found at {db_path}")
        print("Make sure the app has been run at least once, or specify --db path")
        sys.exit(1)

    total_snapshots = args.days * args.per_day
    print(f"Generating {total_snapshots} snapshots for app {args.app_id} ({app_name})")
    print(f"  Period: {args.days} days, {args.per_day} per day")
    print(f"  Base adds/day: {args.base_adds}, trend: {args.trend}")
    print(f"  Database: {db_path}")

    snapshots = generate_snapshots(
        args.app_id, args.days, args.per_day, args.base_adds, args.trend
    )

    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA foreign_keys=ON")

    if args.clear:
        print(f"  Clearing existing data for app {args.app_id}...")
        conn.execute("DELETE FROM snapshot_countries WHERE snapshot_id IN "
                      "(SELECT id FROM wishlist_snapshots WHERE app_id = ?)", (args.app_id,))
        conn.execute("DELETE FROM wishlist_snapshots WHERE app_id = ?", (args.app_id,))
        conn.execute("DELETE FROM crawled_dates WHERE app_id = ?", (args.app_id,))

    # Ensure app is tracked
    conn.execute("INSERT OR IGNORE INTO tracked_games (app_id) VALUES (?)", (args.app_id,))
    conn.execute(
        "INSERT INTO app_info (app_id, name, image_url) VALUES (?, ?, '') "
        "ON CONFLICT(app_id) DO UPDATE SET name = excluded.name",
        (args.app_id, app_name),
    )

    print("  Inserting snapshots...")
    for i, snap in enumerate(snapshots):
        cursor = conn.execute(
            "INSERT INTO wishlist_snapshots "
            "(app_id, date, adds, deletes, purchases, gifts, adds_windows, adds_mac, adds_linux, fetched_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (snap["app_id"], snap["date"], snap["adds"], snap["deletes"],
             snap["purchases"], snap["gifts"], snap["adds_windows"],
             snap["adds_mac"], snap["adds_linux"], snap["fetched_at"]),
        )
        snapshot_id = cursor.lastrowid
        for code, c_adds, c_del, c_pur, c_gifts in snap["countries"]:
            conn.execute(
                "INSERT INTO snapshot_countries "
                "(snapshot_id, country_code, adds, deletes, purchases, gifts) "
                "VALUES (?, ?, ?, ?, ?, ?)",
                (snapshot_id, code, c_adds, c_del, c_pur, c_gifts),
            )

        # Also mark dates as crawled
        conn.execute(
            "INSERT OR IGNORE INTO crawled_dates (app_id, date) VALUES (?, ?)",
            (snap["app_id"], snap["date"]),
        )

        if (i + 1) % 1000 == 0:
            conn.commit()
            print(f"    {i + 1}/{total_snapshots} inserted...")

    conn.commit()
    conn.close()

    final = snapshots[-1]
    print(f"\nDone! Inserted {total_snapshots} snapshots.")
    print(f"  Final cumulative totals:")
    print(f"    Adds: {final['adds']:,}")
    print(f"    Deletes: {final['deletes']:,}")
    print(f"    Purchases: {final['purchases']:,}")
    print(f"    Gifts: {final['gifts']:,}")


if __name__ == "__main__":
    main()
