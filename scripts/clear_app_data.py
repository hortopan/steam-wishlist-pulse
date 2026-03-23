#!/usr/bin/env python3
"""
Remove all data for a given Steam app ID from the wishlist-pulse database.

Usage:
    python scripts/clear_app_data.py --app-id 12345
    python scripts/clear_app_data.py --app-id 12345 --db ~/custom/path/data.db
    python scripts/clear_app_data.py --app-id 12345 --keep-tracked

Options:
    --app-id        Steam app ID (required)
    --db            Path to SQLite database (default: platform-specific data dir)
    --keep-tracked  Keep the app in tracked_games (only remove snapshots/history)
    --yes           Skip confirmation prompt
"""

import argparse
import os
import sqlite3
import sys


def default_db_path():
    if sys.platform == "darwin":
        base = os.path.expanduser("~/Library/Application Support")
    elif sys.platform == "win32":
        base = os.environ.get("APPDATA", os.path.expanduser("~"))
    else:
        base = os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share"))
    return os.path.join(base, "wishlist-pulse", "data.db")


def main():
    parser = argparse.ArgumentParser(description="Clear all data for a Steam app ID")
    parser.add_argument("--app-id", type=int, required=True, help="Steam app ID")
    parser.add_argument("--db", type=str, default=None, help="Database path")
    parser.add_argument("--keep-tracked", action="store_true",
                        help="Keep the app in tracked_games table")
    parser.add_argument("--yes", "-y", action="store_true",
                        help="Skip confirmation prompt")
    args = parser.parse_args()

    db_path = args.db or os.environ.get("DATABASE_PATH") or default_db_path()

    if not os.path.exists(db_path):
        print(f"Error: Database not found at {db_path}")
        print("Make sure the app has been run at least once, or specify --db path")
        sys.exit(1)

    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA foreign_keys=ON")

    # Show current data summary
    row = conn.execute(
        "SELECT COUNT(*) FROM wishlist_snapshots WHERE app_id = ?", (args.app_id,)
    ).fetchone()
    snapshot_count = row[0]

    row = conn.execute(
        "SELECT COUNT(*) FROM crawled_dates WHERE app_id = ?", (args.app_id,)
    ).fetchone()
    date_count = row[0]

    name_row = conn.execute(
        "SELECT name FROM app_info WHERE app_id = ?", (args.app_id,)
    ).fetchone()
    app_name = name_row[0] if name_row else "Unknown"

    print(f"App {args.app_id} ({app_name}):")
    print(f"  Snapshots: {snapshot_count:,}")
    print(f"  Crawled dates: {date_count:,}")
    print(f"  Database: {db_path}")

    if snapshot_count == 0 and date_count == 0:
        print("\nNo data found for this app ID.")
        conn.close()
        return

    if not args.yes:
        answer = input("\nAre you sure you want to delete all data? [y/N] ").strip().lower()
        if answer not in ("y", "yes"):
            print("Aborted.")
            conn.close()
            return

    print("\nDeleting...")

    deleted_countries = conn.execute(
        "DELETE FROM snapshot_countries WHERE snapshot_id IN "
        "(SELECT id FROM wishlist_snapshots WHERE app_id = ?)", (args.app_id,)
    ).rowcount

    deleted_snapshots = conn.execute(
        "DELETE FROM wishlist_snapshots WHERE app_id = ?", (args.app_id,)
    ).rowcount

    deleted_dates = conn.execute(
        "DELETE FROM crawled_dates WHERE app_id = ?", (args.app_id,)
    ).rowcount

    if not args.keep_tracked:
        conn.execute("DELETE FROM app_info WHERE app_id = ?", (args.app_id,))
        conn.execute("DELETE FROM tracked_games WHERE app_id = ?", (args.app_id,))
        print("  Removed from tracked_games and app_info")

    conn.commit()
    conn.close()

    print(f"  Deleted {deleted_countries:,} country rows")
    print(f"  Deleted {deleted_snapshots:,} snapshots")
    print(f"  Deleted {deleted_dates:,} crawled dates")
    print("\nDone!")


if __name__ == "__main__":
    main()
