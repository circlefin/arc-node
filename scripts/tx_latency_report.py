#!/usr/bin/env python3
"""
Query and report transaction latency produced by `spammer --tx-latency`.

This script reads an append-only CSV and uses an auto-cached SQLite DB for
fast queries. The SQLite cache is created (or refreshed) only if missing or
stale compared to the CSV.
"""

from __future__ import annotations

import argparse
import csv
import os
import sqlite3
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Iterable, Optional


# Bump this when the SQLite schema changes to force cache rebuild.
SCHEMA_VERSION = "1"


@dataclass(frozen=True)
class CsvStat:
    """
    Immutable snapshot of CSV file metadata.

    Used to detect whether the SQLite cache is stale compared to the source
    CSV. If any of these values differ, the cache must be rebuilt.
    """

    path: str
    size_bytes: int
    mtime_ns: int


def parse_args(argv: list[str]) -> argparse.Namespace:
    """
    Parse command-line arguments.

    Returns a namespace with the CSV path, optional DB path, rebuild flag,
    and a subcommand. Subcommands:
    - report (default): compute latency statistics with optional filters
    - timestamps: print min/max submission timestamps
    - blocks: print min/max block numbers
    """
    p = argparse.ArgumentParser(
        description="Query tx latency CSV with SQLite auto-cache."
    )
    p.add_argument("--csv", required=True, help="Path to tx-latency CSV.")
    p.add_argument(
        "--db",
        required=False,
        help="SQLite database path (default: CSV + .sqlite).",
    )
    p.add_argument(
        "--rebuild-db",
        dest="rebuild_db",
        action="store_true",
        help="Force reload data from CSV into SQLite.",
    )

    # Defaults for the report filter flags so they exist on the
    # namespace even when no subcommand is given.
    p.set_defaults(
        from_time=None,
        from_block=None,
        between_time=None,
        between_block=None,
        block=None,
    )

    subparsers = p.add_subparsers(dest="command")

    # "report" subcommand (default behavior)
    report_p = subparsers.add_parser(
        "report",
        help="Report latency statistics (default if no subcommand given).",
    )
    q = report_p.add_mutually_exclusive_group(required=False)
    q.add_argument(
        "--from-time",
        dest="from_time",
        metavar="START",
        help=(
            "Filter by submitted_unix_ms >= START (inclusive). "
            "Accepts Unix epoch ms or ISO 8601. "
            "If no timezone is provided, UTC is assumed."
        ),
    )
    q.add_argument(
        "--from-block",
        dest="from_block",
        metavar="BLOCK",
        help="Filter by included_block_number >= BLOCK (inclusive).",
    )
    q.add_argument(
        "--between-time",
        nargs=2,
        metavar=("START_UNIX_MS", "END_UNIX_MS"),
        help=(
            "Filter by submitted_unix_ms range (inclusive). "
            "Accepts Unix epoch ms (e.g. 1700000000000) or ISO 8601 "
            "(e.g. 2026-02-03T10:15:00Z). If no timezone is provided, "
            "UTC is assumed."
        ),
    )
    q.add_argument(
        "--between-block",
        nargs=2,
        metavar=("START_BLOCK", "END_BLOCK"),
        help="Filter by included_block_number range (inclusive).",
    )
    q.add_argument(
        "--block",
        metavar="BLOCK",
        help="Filter by included_block_number == BLOCK.",
    )

    # "timestamps" subcommand
    subparsers.add_parser(
        "timestamps",
        help="Print min and max submission timestamps in the CSV.",
    )

    # "blocks" subcommand
    subparsers.add_parser(
        "blocks",
        help="Print min and max block numbers in the CSV.",
    )

    return p.parse_args(argv)


def csv_stat(csv_path: str) -> CsvStat:
    """
    Capture current metadata of the CSV file.

    Returns a CsvStat with absolute path, size, and modification time used
    for cache freshness checks.
    """
    st = os.stat(csv_path)
    return CsvStat(
        path=os.path.abspath(csv_path),
        size_bytes=int(st.st_size),
        mtime_ns=int(st.st_mtime_ns),
    )


def default_db_path(csv_path: str) -> str:
    """Return the default SQLite cache path by appending .sqlite to the CSV."""
    return csv_path + ".sqlite"


def connect(db_path: str) -> sqlite3.Connection:
    """
    Open a SQLite connection with performance-optimized pragmas.

    Uses WAL mode for better concurrency, relaxed synchronous mode for speed,
    and in-memory temp storage to reduce disk I/O.
    """
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")
    conn.execute("PRAGMA temp_store=MEMORY")
    return conn


def has_table(conn: sqlite3.Connection, name: str) -> bool:
    """Check whether a table with the given name exists in the database."""
    cur = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
        (name,),
    )
    return cur.fetchone() is not None


def get_meta(conn: sqlite3.Connection) -> dict[str, str]:
    """
    Retrieve all key-value pairs from the meta table.

    Returns an empty dict if the meta table does not exist, which indicates
    the database was not created by this script or is from an older version.
    """
    if not has_table(conn, "meta"):
        return {}
    out: dict[str, str] = {}
    for k, v in conn.execute("SELECT key, value FROM meta"):
        out[str(k)] = str(v)
    return out


def cache_is_fresh(
    conn: sqlite3.Connection,
    csv_info: CsvStat,
) -> bool:
    """
    Determine whether the SQLite cache is up-to-date with the source CSV.

    The cache is considered fresh only if the schema version matches and the
    CSV path, size, and mtime all match the values stored when the cache was
    built. Any mismatch triggers a rebuild.
    """
    meta = get_meta(conn)
    return (
        meta.get("schema_version") == SCHEMA_VERSION
        and meta.get("source_csv_path") == csv_info.path
        and meta.get("source_csv_size_bytes") == str(csv_info.size_bytes)
        and meta.get("source_csv_mtime_ns") == str(csv_info.mtime_ns)
    )


def rebuild_cache(csv_path: str, db_path: str, csv_info: CsvStat) -> None:
    """
    Delete any existing cache and create a fresh SQLite database from the CSV.

    Creates the tx_latency table with indexes for efficient time and block
    queries, inserts all rows from the CSV, and stores metadata for future
    freshness checks.
    """
    if os.path.exists(db_path):
        os.remove(db_path)

    conn = connect(db_path)
    try:
        conn.execute(
            """
            CREATE TABLE tx_latency (
              tx_hash TEXT PRIMARY KEY,
              submitted_at TEXT NOT NULL,
              finalized_observed_at TEXT NOT NULL,
              included_block_number INTEGER NOT NULL,
              included_block_hash TEXT NOT NULL,
              included_block_timestamp TEXT NOT NULL,
              submitted_unix_ms INTEGER NOT NULL,
              finalized_observed_unix_ms INTEGER NOT NULL,
              included_block_unix_s INTEGER NOT NULL
            )
            """
        )
        conn.execute(
            """
            CREATE TABLE meta (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            )
            """
        )
        conn.execute(
            "CREATE INDEX idx_submitted_ms ON tx_latency(submitted_unix_ms)"
        )
        conn.execute(
            "CREATE INDEX idx_included_block ON tx_latency(included_block_number)"
        )

        insert_rows(conn, csv_path)

        conn.executemany(
            "INSERT INTO meta(key, value) VALUES(?, ?)",
            [
                ("schema_version", SCHEMA_VERSION),
                ("source_csv_path", csv_info.path),
                ("source_csv_size_bytes", str(csv_info.size_bytes)),
                ("source_csv_mtime_ns", str(csv_info.mtime_ns)),
            ],
        )
        conn.commit()
    finally:
        conn.close()


def insert_rows(conn: sqlite3.Connection, csv_path: str) -> None:
    """
    Read the CSV and insert all rows into the tx_latency table.

    Parses ISO 8601 timestamps into Unix milliseconds for efficient range
    queries. Rows with fewer than 6 columns are skipped as malformed.
    """

    def iter_rows() -> Iterable[tuple]:
        with open(csv_path, newline="") as f:
            r = csv.reader(f)
            header = next(r, None)
            if header is None:
                return
            for row in r:
                if len(row) < 6:
                    continue
                tx_hash = row[0]
                submitted_at = row[1]
                finalized_at = row[2]
                block_no = int(row[3])
                block_hash = row[4]
                block_ts_at = row[5]

                sub_ms = parse_time_ms(submitted_at)
                fin_ms = parse_time_ms(finalized_at)
                block_unix_s = parse_time_ms(block_ts_at) // 1000
                yield (
                    tx_hash,
                    submitted_at,
                    finalized_at,
                    block_no,
                    block_hash,
                    block_ts_at,
                    sub_ms,
                    fin_ms,
                    block_unix_s,
                )

    conn.execute("BEGIN")
    conn.executemany(
        """
        INSERT OR REPLACE INTO tx_latency(
          tx_hash,
          submitted_at,
          finalized_observed_at,
          included_block_number,
          included_block_hash,
          included_block_timestamp,
          submitted_unix_ms,
          finalized_observed_unix_ms,
          included_block_unix_s
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        iter_rows(),
    )
    conn.execute("COMMIT")


def ensure_db(csv_path: str, db_path: str, rebuild: bool) -> None:
    """
    Ensure the SQLite database exists and is up-to-date.

    Rebuilds the database if it doesn't exist, if the rebuild flag is set, or
    if the stored CSV metadata differs from the current CSV file.
    """
    csv_info = csv_stat(csv_path)
    if rebuild or not os.path.exists(db_path):
        rebuild_cache(csv_path, db_path, csv_info)
        return

    conn = connect(db_path)
    try:
        if not cache_is_fresh(conn, csv_info):
            conn.close()
            rebuild_cache(csv_path, db_path, csv_info)
    finally:
        try:
            conn.close()
        except Exception:
            pass


def percentile_nearest_rank(sorted_vals: list[int], p: int) -> Optional[int]:
    """
    Compute the p-th percentile using the nearest-rank method.

    Expects a pre-sorted list. Returns None for empty lists or invalid
    percentiles (p <= 0). Percentiles above 100 are clamped to 100.
    """
    if not sorted_vals or p <= 0:
        return None
    p = min(p, 100)
    n = len(sorted_vals)
    rank = (p * n + 99) // 100
    idx = max(rank - 1, 0)
    return sorted_vals[idx]


def fmt_opt(v: Optional[int]) -> str:
    """Format an optional int as a string, returning '-' for None."""
    return "-" if v is None else str(v)


def parse_time_ms(s: str) -> int:
    """
    Parse a time argument into Unix epoch milliseconds.

    Accepted formats:
    - Unix epoch milliseconds (integer string)
    - ISO 8601 (e.g. 2026-02-03T10:15:00Z or with an offset)

    If the ISO string has no timezone info, UTC is assumed.
    """
    s = s.strip()
    try:
        return int(s)
    except ValueError:
        pass

    iso = s
    if iso.endswith("Z"):
        iso = iso[:-1] + "+00:00"

    try:
        dt = datetime.fromisoformat(iso)
    except ValueError:
        raise ValueError(
            f"Invalid time format: '{s}'. "
            f"Expected Unix epoch milliseconds (e.g. 1700000000000) "
            f"or ISO 8601 (e.g. 2026-02-03T10:15:00Z)."
        ) from None

    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    dt_utc = dt.astimezone(timezone.utc)
    return int(dt_utc.timestamp() * 1000)


def query_timestamp_range(
    conn: sqlite3.Connection,
) -> tuple[str, str] | None:
    """
    Query the min and max submission timestamps from the database.

    Returns a tuple of (min_submitted_at, max_submitted_at) as RFC 3339
    strings, or None if the table is empty.
    """
    cur = conn.execute(
        "SELECT MIN(submitted_at), MAX(submitted_at) FROM tx_latency"
    )
    row = cur.fetchone()
    if row and row[0] and row[1]:
        return (row[0], row[1])
    return None


def query_block_range(conn: sqlite3.Connection) -> tuple[int, int] | None:
    """
    Query the min and max block numbers from the database.

    Returns a tuple of (min_block, max_block) as integers, or None if the
    table is empty.
    """
    cur = conn.execute(
        "SELECT MIN(included_block_number), MAX(included_block_number) "
        "FROM tx_latency"
    )
    row = cur.fetchone()
    if row and row[0] is not None and row[1] is not None:
        return (int(row[0]), int(row[1]))
    return None


def query_latencies_ms(
    conn: sqlite3.Connection,
    args: argparse.Namespace,
) -> list[int]:
    """
    Query latency values from the database based on the provided filters.

    Latency is computed as finalized_observed_unix_ms - submitted_unix_ms.
    Returns a list of latency values in milliseconds for all matching rows.
    """
    where = ""
    params: tuple = ()

    if args.from_time is not None:
        start_ms = parse_time_ms(args.from_time)
        where = "WHERE submitted_unix_ms >= ?"
        params = (start_ms,)
    elif args.from_block is not None:
        start_b = int(args.from_block)
        where = "WHERE included_block_number >= ?"
        params = (start_b,)
    elif args.between_time is not None:
        start_ms = parse_time_ms(args.between_time[0])
        end_ms = parse_time_ms(args.between_time[1])
        where = "WHERE submitted_unix_ms >= ? AND submitted_unix_ms <= ?"
        params = (start_ms, end_ms)
    elif args.between_block is not None:
        start_b = int(args.between_block[0])
        end_b = int(args.between_block[1])
        where = (
            "WHERE included_block_number >= ? AND included_block_number <= ?"
        )
        params = (start_b, end_b)
    elif args.block is not None:
        b = int(args.block)
        where = "WHERE included_block_number = ?"
        params = (b,)
    # No filter provided: query all rows (where clause remains empty)

    sql = (
        "SELECT finalized_observed_unix_ms - submitted_unix_ms AS latency_ms "
        "FROM tx_latency "
        + where
    )
    cur = conn.execute(sql, params)
    return [int(row[0]) for row in cur.fetchall()]


def report(latencies_ms: list[int]) -> None:
    """
    Print a statistical summary of the latency values.

    Outputs count, min, max, average, and percentiles (p50, p90, p95, p99).
    Prints a message and returns early if no data is available.
    """
    if not latencies_ms:
        print("No matching rows.")
        return

    latencies_ms.sort()
    count = len(latencies_ms)
    min_ms = latencies_ms[0]
    max_ms = latencies_ms[-1]
    avg_ms = sum(latencies_ms) // count
    p50 = percentile_nearest_rank(latencies_ms, 50)
    p90 = percentile_nearest_rank(latencies_ms, 90)
    p95 = percentile_nearest_rank(latencies_ms, 95)
    p99 = percentile_nearest_rank(latencies_ms, 99)

    print(f"No. of txs={count}")
    print(f"Min latency={min_ms}ms")
    print(f"Max latency={max_ms}ms")
    print(f"Avg latency={avg_ms}ms")
    print(f"P50 latency={fmt_opt(p50)}ms")
    print(f"P90 latency={fmt_opt(p90)}ms")
    print(f"P95 latency={fmt_opt(p95)}ms")
    print(f"P99 latency={fmt_opt(p99)}ms")


def main(argv: list[str]) -> int:
    """
    Entry point for the tx_latency_report CLI.

    Parses arguments, ensures the SQLite cache is up-to-date, and dispatches
    to the appropriate subcommand handler.
    """
    args = parse_args(argv)
    csv_path = args.csv
    db_path = args.db or default_db_path(csv_path)

    ensure_db(csv_path, db_path, args.rebuild_db)

    conn = connect(db_path)
    try:
        if args.command == "timestamps":
            result = query_timestamp_range(conn)
            if result:
                print(f"min_timestamp={result[0]}")
                print(f"max_timestamp={result[1]}")
            else:
                print("No data.")
        elif args.command == "blocks":
            result = query_block_range(conn)
            if result:
                print(f"min_block={result[0]}")
                print(f"max_block={result[1]}")
            else:
                print("No data.")
        else:
            # Default: "report" subcommand or no subcommand
            latencies_ms = query_latencies_ms(conn, args)
            report(latencies_ms)
    finally:
        conn.close()

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
