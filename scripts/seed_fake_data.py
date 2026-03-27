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
    --base-adds Starting daily add rate (default: 15)
    --trend     Daily trend multiplier, e.g. 1.01 = 1% growth/day (default: 1.001)
    --clear     Remove existing snapshots for this app before seeding
"""

import argparse
import math
import os
import random
import sqlite3
import sys
from datetime import datetime, timedelta

# Western and European countries with approximate relative weights
COUNTRIES = [
    ("US", 20), ("DE", 10), ("GB", 8), ("FR", 7), ("CA", 6),
    ("AU", 5), ("PL", 5), ("IT", 4), ("ES", 4), ("NL", 3),
    ("SE", 3), ("RO", 3), ("BE", 2), ("AT", 2), ("CZ", 2),
    ("PT", 2), ("DK", 2), ("NO", 2), ("FI", 2), ("CH", 2),
]


def default_db_path():
    if sys.platform == "darwin":
        base = os.path.expanduser("~/Library/Application Support")
    elif sys.platform == "win32":
        base = os.environ.get("APPDATA", os.path.expanduser("~"))
    else:
        base = os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share"))
    return os.path.join(base, "wishlist-pulse", "data.db")


def _generate_events(days, start):
    """Pre-generate random events that affect wishlist activity.

    The key insight for anomaly detection: most days must be stable/predictable
    so that MAD stays low. Anomalous events are rare, sharp, and isolated —
    they need to clearly exceed the baseline variability.
    """
    events = {}

    # Steam seasonal sales (roughly accurate windows)
    sale_ranges = []
    for year_offset in range(-1, 3):
        base_year = start.year + year_offset
        sale_ranges.append((datetime(base_year, 6, 23), datetime(base_year, 7, 7)))
        sale_ranges.append((datetime(base_year, 11, 21), datetime(base_year, 11, 28)))
        sale_ranges.append((datetime(base_year, 12, 19), datetime(base_year + 1, 1, 2)))
        sale_ranges.append((datetime(base_year, 3, 14), datetime(base_year, 3, 21)))
        sale_ranges.append((datetime(base_year, 10, 28), datetime(base_year, 11, 1)))

    for day in range(days):
        dt = start + timedelta(days=day)
        for sale_start, sale_end in sale_ranges:
            if sale_start <= dt <= sale_end:
                days_into_sale = (dt - sale_start).days
                sale_intensity = max(0.3, 1.0 - days_into_sale * 0.08)
                events[day] = {"type": "sale", "intensity": sale_intensity}
                break

    # Guaranteed anomaly spikes — these are sharp 1-2 day events designed to
    # stand out against a stable baseline. Placed at least 20 days apart so
    # they don't contaminate each other's lookback windows.
    anomaly_candidates = [d for d in range(20, days) if d not in events]
    random.shuffle(anomaly_candidates)
    num_anomalies = max(3, days // 40)
    placed = []
    for d in anomaly_candidates:
        if len(placed) >= num_anomalies:
            break
        # Ensure at least 20 days gap from other anomalies
        if any(abs(d - p) < 20 for p in placed):
            continue
        placed.append(d)
        # Sharp spike: 3-8x the baseline rate (realistic for a small/medium game)
        events[d] = {"type": "anomaly_spike", "intensity": random.uniform(3.0, 8.0)}
        # Optional second day at reduced intensity
        if d + 1 < days and d + 1 not in events and random.random() < 0.4:
            events[d + 1] = {"type": "anomaly_spike", "intensity": random.uniform(1.5, 3.0)}

    # Guaranteed anomaly drops — sudden near-zero days
    num_drops = max(2, days // 60)
    drop_placed = []
    for d in anomaly_candidates:
        if len(drop_placed) >= num_drops:
            break
        if d in events or any(abs(d - p) < 20 for p in placed + drop_placed):
            continue
        drop_placed.append(d)
        events[d] = {"type": "anomaly_drop", "intensity": random.uniform(0.02, 0.1)}

    # Mild viral bumps (not necessarily anomalous, just variety)
    num_bumps = max(1, days // 60)
    for _ in range(num_bumps):
        bump_day = random.randint(0, days - 1)
        if bump_day not in events:
            events[bump_day] = {"type": "viral", "intensity": random.uniform(1.3, 2.0)}

    # Mild lulls
    num_lulls = max(1, days // 60)
    for _ in range(num_lulls):
        lull_day = random.randint(0, days - 1)
        duration = random.randint(2, 5)
        for d in range(duration):
            if lull_day + d < days and lull_day + d not in events:
                events[lull_day + d] = {"type": "lull", "intensity": random.uniform(0.5, 0.7)}

    return events


def generate_snapshots(app_id, days, per_day, base_adds, trend):
    """Generate cumulative snapshot data with realistic patterns."""
    now = datetime.utcnow()
    start = now - timedelta(days=days)
    interval = timedelta(hours=24 / per_day)

    snapshots = []
    total_weight = sum(w for _, w in COUNTRIES)
    ts = start

    # Pre-generate events
    events = _generate_events(days, start)

    # Running "momentum" — smooths day-to-day changes
    momentum = 1.0

    for day in range(days):
        # Per-date running totals (reset each day, as Steam reports them)
        day_adds = 0
        day_deletes = 0
        day_purchases = 0
        day_gifts = 0
        # Daily rate with trend
        day_factor = trend ** day

        # Initial launch hype decay (modest for a small/medium game)
        if day < 5:
            launch_factor = 2.0 - (day * 0.15)
        elif day < 30:
            launch_factor = 1.3 * (0.97 ** (day - 5))
        else:
            launch_factor = 1.0

        # Weekly seasonality — kept mild so it doesn't inflate MAD
        weekday = (start + timedelta(days=day)).weekday()
        if weekday == 5:    # Saturday
            weekday_factor = random.uniform(1.10, 1.20)
        elif weekday == 6:  # Sunday
            weekday_factor = random.uniform(1.05, 1.15)
        elif weekday == 4:  # Friday
            weekday_factor = random.uniform(1.02, 1.08)
        elif weekday in (1, 2):  # Tue, Wed
            weekday_factor = random.uniform(0.88, 0.95)
        else:
            weekday_factor = random.uniform(0.95, 1.05)

        # Monthly wave — subtle
        day_of_month = (start + timedelta(days=day)).day
        if day_of_month <= 3 or 14 <= day_of_month <= 17:
            monthly_wave = random.uniform(1.03, 1.08)
        else:
            monthly_wave = 1.0 + 0.03 * math.sin(2 * math.pi * day / 30)

        # Event effects
        event = events.get(day)
        if event:
            if event["type"] == "sale":
                event_mult = 1.0 + event["intensity"] * random.uniform(1.0, 1.8)
                sale_purchase_boost = 1.5 + event["intensity"] * 2.0
                sale_delete_boost = 1.2 + event["intensity"] * 0.6
            elif event["type"] == "anomaly_spike":
                # Sharp isolated spike — guaranteed to exceed MAD threshold
                event_mult = event["intensity"]
                sale_purchase_boost = random.uniform(1.5, 3.0)
                sale_delete_boost = random.uniform(1.2, 2.0)
            elif event["type"] == "anomaly_drop":
                # Sharp isolated drop — near zero activity
                event_mult = event["intensity"]
                sale_purchase_boost = event["intensity"]
                sale_delete_boost = event["intensity"]
            elif event["type"] == "viral":
                event_mult = event["intensity"]
                sale_purchase_boost = 1.0
                sale_delete_boost = 1.0
            elif event["type"] == "lull":
                event_mult = event["intensity"]
                sale_purchase_boost = 1.0
                sale_delete_boost = 1.0
            else:
                event_mult = 1.0
                sale_purchase_boost = 1.0
                sale_delete_boost = 1.0
        else:
            event_mult = 1.0
            sale_purchase_boost = 1.0
            sale_delete_boost = 1.0

        # Day-to-day random walk — small steps to keep baseline stable
        momentum += random.gauss(0, 0.03)
        momentum = max(0.8, min(1.2, momentum))  # tight clamp

        # Combine all factors
        combined = (base_adds * day_factor * launch_factor * weekday_factor
                     * monthly_wave * event_mult * momentum)

        # Tight per-day noise — keeps MAD low so anomalies stand out
        daily_adds = combined * random.uniform(0.92, 1.08)

        # Deletes/purchases/gifts — lower conversion for small/medium games
        delete_rate = random.uniform(0.05, 0.12) * sale_delete_boost
        purchase_rate = random.uniform(0.02, 0.05) * sale_purchase_boost
        gift_rate = random.uniform(0.003, 0.01)

        daily_deletes = daily_adds * delete_rate
        daily_purchases = daily_adds * purchase_rate
        daily_gifts = daily_adds * gift_rate

        # Time-of-day activity curve (peak at ~20:00 UTC, trough at ~06:00 UTC)
        hour_weights = []
        for snap_i in range(per_day):
            hour = (snap_i * 24.0 / per_day) % 24
            # Bell curve peaking around 19-20 UTC
            w = 0.4 + 0.6 * math.exp(-0.5 * ((hour - 19.5) / 5.0) ** 2)
            # Secondary smaller peak around 10 UTC (Europe morning)
            w += 0.2 * math.exp(-0.5 * ((hour - 10.0) / 3.0) ** 2)
            hour_weights.append(w)
        weight_sum = sum(hour_weights)
        hour_weights = [w / weight_sum for w in hour_weights]

        for snap_i in range(per_day):
            frac = hour_weights[snap_i] * random.uniform(0.85, 1.15)

            snap_adds = max(0, int(daily_adds * frac + random.gauss(0, max(1, daily_adds * frac * 0.1))))
            snap_deletes = max(0, int(daily_deletes * frac + random.gauss(0, max(0.5, daily_deletes * frac * 0.15))))
            snap_purchases = max(0, int(daily_purchases * frac + random.gauss(0, max(0.3, daily_purchases * frac * 0.2))))
            snap_gifts = max(0, int(daily_gifts * frac + random.gauss(0, 0.3)))

            day_adds += snap_adds
            day_deletes += snap_deletes
            day_purchases += snap_purchases
            day_gifts += snap_gifts

            # Platform split with realistic variance
            win_share = random.gauss(0.85, 0.03)
            mac_share = random.gauss(0.10, 0.02)
            linux_share = max(0.02, 1.0 - win_share - mac_share)
            total_share = win_share + mac_share + linux_share
            adds_win = int(snap_adds * win_share / total_share)
            adds_mac = int(snap_adds * mac_share / total_share)
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
                "adds": day_adds,
                "deletes": day_deletes,
                "purchases": day_purchases,
                "gifts": day_gifts,
                "adds_windows": adds_win,
                "adds_mac": adds_mac,
                "adds_linux": adds_linux,
                "fetched_at": fetched_at,
                "countries": countries,
            })

            ts += interval

    return snapshots, events


def main():
    parser = argparse.ArgumentParser(description="Seed wishlist-pulse DB with fake data")
    parser.add_argument("--app-id", type=int, required=True, help="Steam app ID")
    parser.add_argument("--days", type=int, required=True, help="Days of history")
    parser.add_argument("--per-day", type=int, required=True, help="Snapshots per day")
    parser.add_argument("--db", type=str, default=None, help="Database path")
    parser.add_argument("--app-name", type=str, default=None, help="Game name")
    parser.add_argument("--base-adds", type=float, default=15, help="Base daily adds rate")
    parser.add_argument("--trend", type=float, default=1.001, help="Daily trend multiplier")
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

    snapshots, injected_events = generate_snapshots(
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
    print(f"  Last day's running totals:")
    print(f"    Adds: {final['adds']:,}")
    print(f"    Deletes: {final['deletes']:,}")
    print(f"    Purchases: {final['purchases']:,}")
    print(f"    Gifts: {final['gifts']:,}")

    # Show injected anomaly events so user can verify detection
    anomaly_events = {d: e for d, e in injected_events.items()
                      if e["type"] in ("anomaly_spike", "anomaly_drop")}
    if anomaly_events:
        now = datetime.utcnow()
        event_start = now - timedelta(days=args.days)
        print(f"\n  Injected anomaly events ({len(anomaly_events)} days):")
        for d in sorted(anomaly_events):
            e = anomaly_events[d]
            dt = event_start + timedelta(days=d)
            label = "SPIKE" if e["type"] == "anomaly_spike" else "DROP"
            print(f"    {dt.strftime('%Y-%m-%d')} — {label} {e['intensity']:.1f}x")


if __name__ == "__main__":
    main()
