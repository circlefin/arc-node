#!/usr/bin/env python3
"""
Unit tests for tx_latency_report.py.

Run with:
    cd scripts && python3 -m unittest test_tx_latency_report -v
    # or directly:
    python3 scripts/test_tx_latency_report.py
"""

from __future__ import annotations

import csv
import os
import shutil
import sqlite3
import tempfile
import unittest
from io import StringIO
from unittest.mock import patch

from tx_latency_report import (
    CsvStat,
    cache_is_fresh,
    connect,
    default_db_path,
    ensure_db,
    fmt_opt,
    get_meta,
    has_table,
    parse_time_ms,
    percentile_nearest_rank,
    query_latencies_ms,
    rebuild_cache,
    report,
    SCHEMA_VERSION,
)


class TestParseTimeMs(unittest.TestCase):
    """Tests for parse_time_ms function."""

    def test_epoch_ms_integer(self):
        """Parse plain integer epoch milliseconds."""
        self.assertEqual(parse_time_ms("1700000000000"), 1700000000000)

    def test_epoch_ms_with_whitespace(self):
        """Parse epoch ms with leading/trailing whitespace."""
        self.assertEqual(parse_time_ms("  1700000000000  "), 1700000000000)

    def test_iso8601_with_z_suffix(self):
        """Parse ISO 8601 with Z (Zulu/UTC) suffix."""
        # 2024-01-01T00:00:00Z -> 1704067200000 ms
        result = parse_time_ms("2024-01-01T00:00:00Z")
        self.assertEqual(result, 1704067200000)

    def test_iso8601_with_offset(self):
        """Parse ISO 8601 with explicit timezone offset."""
        # 2024-01-01T01:00:00+01:00 is same as 2024-01-01T00:00:00Z
        result = parse_time_ms("2024-01-01T01:00:00+01:00")
        self.assertEqual(result, 1704067200000)

    def test_iso8601_no_timezone_assumes_utc(self):
        """Parse ISO 8601 without timezone assumes UTC."""
        result = parse_time_ms("2024-01-01T00:00:00")
        self.assertEqual(result, 1704067200000)

    def test_iso8601_with_milliseconds(self):
        """Parse ISO 8601 with fractional seconds."""
        result = parse_time_ms("2024-01-01T00:00:00.500Z")
        self.assertEqual(result, 1704067200500)

    def test_invalid_format_raises_value_error(self):
        """Invalid format raises ValueError with helpful message."""
        with self.assertRaises(ValueError) as ctx:
            parse_time_ms("garbage")
        self.assertIn("Invalid time format", str(ctx.exception))
        self.assertIn("garbage", str(ctx.exception))
        self.assertIn("Unix epoch milliseconds", str(ctx.exception))
        self.assertIn("ISO 8601", str(ctx.exception))

    def test_empty_string_raises_value_error(self):
        """Empty string raises ValueError."""
        with self.assertRaises(ValueError) as ctx:
            parse_time_ms("")
        self.assertIn("Invalid time format", str(ctx.exception))


class TestPercentileNearestRank(unittest.TestCase):
    """Tests for percentile_nearest_rank function."""

    def test_empty_list_returns_none(self):
        """Empty list returns None."""
        self.assertIsNone(percentile_nearest_rank([], 50))

    def test_zero_percentile_returns_none(self):
        """Zero percentile returns None."""
        self.assertIsNone(percentile_nearest_rank([1, 2, 3], 0))

    def test_negative_percentile_returns_none(self):
        """Negative percentile returns None."""
        self.assertIsNone(percentile_nearest_rank([1, 2, 3], -10))

    def test_single_element(self):
        """Single element list returns that element for any percentile."""
        self.assertEqual(percentile_nearest_rank([42], 50), 42)
        self.assertEqual(percentile_nearest_rank([42], 99), 42)

    def test_p50_odd_count(self):
        """P50 of odd-count sorted list."""
        # [1, 2, 3, 4, 5] -> p50 should be 3 (middle)
        self.assertEqual(percentile_nearest_rank([1, 2, 3, 4, 5], 50), 3)

    def test_p50_even_count(self):
        """P50 of even-count sorted list (nearest rank)."""
        # [1, 2, 3, 4] -> rank = (50*4+99)//100 = 2 -> index 1 -> value 2
        self.assertEqual(percentile_nearest_rank([1, 2, 3, 4], 50), 2)

    def test_p99_small_list(self):
        """P99 of small list returns max."""
        self.assertEqual(percentile_nearest_rank([1, 2, 3, 4, 5], 99), 5)

    def test_p100_clamped(self):
        """Percentile > 100 is clamped to 100."""
        self.assertEqual(percentile_nearest_rank([1, 2, 3], 150), 3)

    def test_larger_dataset(self):
        """Percentiles on larger dataset."""
        data = list(range(1, 101))  # 1 to 100
        self.assertEqual(percentile_nearest_rank(data, 50), 50)
        self.assertEqual(percentile_nearest_rank(data, 90), 90)
        self.assertEqual(percentile_nearest_rank(data, 99), 99)
        self.assertEqual(percentile_nearest_rank(data, 100), 100)


class TestFmtOpt(unittest.TestCase):
    """Tests for fmt_opt function."""

    def test_none_returns_dash(self):
        """None value returns dash."""
        self.assertEqual(fmt_opt(None), "-")

    def test_int_returns_string(self):
        """Integer returns string representation."""
        self.assertEqual(fmt_opt(42), "42")
        self.assertEqual(fmt_opt(0), "0")


class TestDefaultDbPath(unittest.TestCase):
    """Tests for default_db_path function."""

    def test_appends_sqlite_extension(self):
        """Appends .sqlite to CSV path."""
        self.assertEqual(default_db_path("data.csv"), "data.csv.sqlite")
        self.assertEqual(
            default_db_path("/path/to/file.csv"),
            "/path/to/file.csv.sqlite",
        )


class TestSqliteHelpers(unittest.TestCase):
    """Tests for SQLite helper functions."""

    def setUp(self):
        """Create in-memory database for testing."""
        self.conn = sqlite3.connect(":memory:")

    def tearDown(self):
        """Close database connection."""
        self.conn.close()

    def test_has_table_false_for_missing(self):
        """has_table returns False for non-existent table."""
        self.assertFalse(has_table(self.conn, "nonexistent"))

    def test_has_table_true_for_existing(self):
        """has_table returns True for existing table."""
        self.conn.execute("CREATE TABLE test_table (id INTEGER)")
        self.assertTrue(has_table(self.conn, "test_table"))

    def test_get_meta_empty_when_no_table(self):
        """get_meta returns empty dict when meta table doesn't exist."""
        self.assertEqual(get_meta(self.conn), {})

    def test_get_meta_returns_values(self):
        """get_meta returns stored key-value pairs."""
        self.conn.execute(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)"
        )
        self.conn.execute("INSERT INTO meta VALUES ('k1', 'v1')")
        self.conn.execute("INSERT INTO meta VALUES ('k2', 'v2')")
        self.assertEqual(get_meta(self.conn), {"k1": "v1", "k2": "v2"})


class TestCacheIsFresh(unittest.TestCase):
    """Tests for cache_is_fresh function."""

    def setUp(self):
        """Create in-memory database with meta table."""
        self.conn = sqlite3.connect(":memory:")
        self.conn.execute(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)"
        )

    def tearDown(self):
        """Close database connection."""
        self.conn.close()

    def test_fresh_when_all_match(self):
        """Cache is fresh when all metadata matches."""
        csv_info = CsvStat(
            path="/path/to/file.csv",
            size_bytes=1234,
            mtime_ns=9999999999,
        )
        self.conn.executemany(
            "INSERT INTO meta VALUES (?, ?)",
            [
                ("schema_version", SCHEMA_VERSION),
                ("source_csv_path", "/path/to/file.csv"),
                ("source_csv_size_bytes", "1234"),
                ("source_csv_mtime_ns", "9999999999"),
            ],
        )
        self.assertTrue(cache_is_fresh(self.conn, csv_info))

    def test_stale_when_size_differs(self):
        """Cache is stale when file size differs."""
        csv_info = CsvStat(
            path="/path/to/file.csv",
            size_bytes=5678,  # Different size
            mtime_ns=9999999999,
        )
        self.conn.executemany(
            "INSERT INTO meta VALUES (?, ?)",
            [
                ("schema_version", SCHEMA_VERSION),
                ("source_csv_path", "/path/to/file.csv"),
                ("source_csv_size_bytes", "1234"),
                ("source_csv_mtime_ns", "9999999999"),
            ],
        )
        self.assertFalse(cache_is_fresh(self.conn, csv_info))

    def test_stale_when_mtime_differs(self):
        """Cache is stale when mtime differs."""
        csv_info = CsvStat(
            path="/path/to/file.csv",
            size_bytes=1234,
            mtime_ns=1111111111,  # Different mtime
        )
        self.conn.executemany(
            "INSERT INTO meta VALUES (?, ?)",
            [
                ("schema_version", SCHEMA_VERSION),
                ("source_csv_path", "/path/to/file.csv"),
                ("source_csv_size_bytes", "1234"),
                ("source_csv_mtime_ns", "9999999999"),
            ],
        )
        self.assertFalse(cache_is_fresh(self.conn, csv_info))

    def test_stale_when_schema_version_differs(self):
        """Cache is stale when schema version differs."""
        csv_info = CsvStat(
            path="/path/to/file.csv",
            size_bytes=1234,
            mtime_ns=9999999999,
        )
        self.conn.executemany(
            "INSERT INTO meta VALUES (?, ?)",
            [
                ("schema_version", "OLD_VERSION"),
                ("source_csv_path", "/path/to/file.csv"),
                ("source_csv_size_bytes", "1234"),
                ("source_csv_mtime_ns", "9999999999"),
            ],
        )
        self.assertFalse(cache_is_fresh(self.conn, csv_info))


class TestIntegration(unittest.TestCase):
    """Integration tests for cache building and querying."""

    def setUp(self):
        """Create temporary directory for test files."""
        self.tmpdir = tempfile.mkdtemp()
        self.csv_path = os.path.join(self.tmpdir, "test.csv")
        self.db_path = os.path.join(self.tmpdir, "test.csv.sqlite")

    def tearDown(self):
        """Clean up temporary files."""
        import shutil
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def _write_csv(self, rows: list[list[str]]) -> None:
        """Write CSV file with header and rows."""
        header = [
            "tx_hash",
            "submitted_at",
            "finalized_observed_at",
            "included_block_number",
            "included_block_hash",
            "included_block_timestamp",
        ]
        with open(self.csv_path, "w", newline="") as f:
            f.write(",".join(header) + "\n")
            for row in rows:
                f.write(",".join(row) + "\n")

    def test_rebuild_cache_creates_tables(self):
        """rebuild_cache creates required tables and indexes."""
        self._write_csv([])
        from tx_latency_report import csv_stat
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        conn = connect(self.db_path)
        try:
            self.assertTrue(has_table(conn, "tx_latency"))
            self.assertTrue(has_table(conn, "meta"))
            # Verify indexes exist
            cur = conn.execute(
                "SELECT name FROM sqlite_master WHERE type='index'"
            )
            indexes = {row[0] for row in cur.fetchall()}
            self.assertIn("idx_submitted_ms", indexes)
            self.assertIn("idx_included_block", indexes)
        finally:
            conn.close()

    def test_rebuild_cache_inserts_rows(self):
        """rebuild_cache inserts CSV rows into database."""
        self._write_csv([
            [
                "0xabc123",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:00:01.000Z",
                "100",
                "0xblockhash",
                "2024-01-01T00:00:00Z",
            ],
            [
                "0xdef456",
                "2024-01-01T00:00:02.000Z",
                "2024-01-01T00:00:03.000Z",
                "101",
                "0xblockhash2",
                "2024-01-01T00:00:02Z",
            ],
        ])
        from tx_latency_report import csv_stat
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        conn = connect(self.db_path)
        try:
            cur = conn.execute("SELECT COUNT(*) FROM tx_latency")
            self.assertEqual(cur.fetchone()[0], 2)
        finally:
            conn.close()

    def test_ensure_db_rebuilds_when_missing(self):
        """ensure_db rebuilds when DB doesn't exist."""
        self._write_csv([])
        self.assertFalse(os.path.exists(self.db_path))
        ensure_db(self.csv_path, self.db_path, rebuild=False)
        self.assertTrue(os.path.exists(self.db_path))

    def test_ensure_db_skips_when_fresh(self):
        """ensure_db skips rebuild when DB is up-to-date."""
        self._write_csv([])
        from tx_latency_report import csv_stat
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        # Get mtime of DB after first build
        mtime1 = os.stat(self.db_path).st_mtime_ns

        # Small delay to ensure mtime would differ if rebuilt
        import time
        time.sleep(0.01)

        # ensure_db should not rebuild
        ensure_db(self.csv_path, self.db_path, rebuild=False)
        mtime2 = os.stat(self.db_path).st_mtime_ns

        self.assertEqual(mtime1, mtime2)

    def test_query_latencies_by_block(self):
        """Query latencies filtered by block number."""
        self._write_csv([
            [
                "0xabc",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:00:01.000Z",
                "100",
                "0xhash1",
                "2024-01-01T00:00:00Z",
            ],
            [
                "0xdef",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:00:02.000Z",
                "101",
                "0xhash2",
                "2024-01-01T00:00:01Z",
            ],
        ])
        from tx_latency_report import csv_stat
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        conn = connect(self.db_path)
        try:
            # Create mock args for --block query
            class Args:
                from_time = None
                from_block = None
                between_time = None
                between_block = None
                block = "100"

            latencies = query_latencies_ms(conn, Args())
            self.assertEqual(len(latencies), 1)
            self.assertEqual(latencies[0], 1000)  # 1 second = 1000ms
        finally:
            conn.close()

    def test_query_latencies_by_time_range(self):
        """Query latencies filtered by time range."""
        self._write_csv([
            [
                "0xabc",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:00:01.000Z",
                "100",
                "0xhash1",
                "2024-01-01T00:00:00Z",
            ],
            [
                "0xdef",
                "2024-01-01T00:01:00.000Z",
                "2024-01-01T00:01:02.000Z",
                "101",
                "0xhash2",
                "2024-01-01T00:01:00Z",
            ],
        ])
        from tx_latency_report import csv_stat
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        conn = connect(self.db_path)
        try:
            # Create mock args for --between-time query
            class Args:
                from_time = None
                from_block = None
                between_time = [
                    "2024-01-01T00:00:30.000Z",
                    "2024-01-01T00:01:30.000Z",
                ]
                between_block = None
                block = None

            latencies = query_latencies_ms(conn, Args())
            self.assertEqual(len(latencies), 1)
            self.assertEqual(latencies[0], 2000)  # 2 seconds = 2000ms
        finally:
            conn.close()


class TestTimestampRange(unittest.TestCase):
    """Tests for query_timestamp_range function."""

    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()
        self.csv_path = os.path.join(self.temp_dir, "test.csv")
        self.db_path = os.path.join(self.temp_dir, "test.sqlite")

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def _write_csv(self, rows: list[list[str]]):
        header = [
            "tx_hash",
            "submitted_at",
            "finalized_observed_at",
            "included_block_number",
            "included_block_hash",
            "included_block_timestamp",
        ]
        with open(self.csv_path, "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(header)
            for row in rows:
                w.writerow(row)

    def test_query_timestamp_range(self):
        """Query min/max timestamps from CSV."""
        self._write_csv([
            [
                "0xabc",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:00:01.000Z",
                "100",
                "0xhash1",
                "2024-01-01T00:00:00Z",
            ],
            [
                "0xdef",
                "2024-01-01T00:01:00.000Z",
                "2024-01-01T00:01:02.000Z",
                "101",
                "0xhash2",
                "2024-01-01T00:01:00Z",
            ],
        ])
        from tx_latency_report import (
            csv_stat,
            query_timestamp_range,
        )
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        conn = connect(self.db_path)
        try:
            result = query_timestamp_range(conn)
            self.assertIsNotNone(result)
            self.assertEqual(result[0], "2024-01-01T00:00:00.000Z")
            self.assertEqual(result[1], "2024-01-01T00:01:00.000Z")
        finally:
            conn.close()

    def test_query_timestamp_range_empty(self):
        """Query returns None for empty CSV."""
        self._write_csv([])
        from tx_latency_report import (
            csv_stat,
            query_timestamp_range,
        )
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        conn = connect(self.db_path)
        try:
            result = query_timestamp_range(conn)
            self.assertIsNone(result)
        finally:
            conn.close()


class TestBlockRange(unittest.TestCase):
    """Tests for query_block_range function."""

    def setUp(self):
        self.temp_dir = tempfile.mkdtemp()
        self.csv_path = os.path.join(self.temp_dir, "test.csv")
        self.db_path = os.path.join(self.temp_dir, "test.sqlite")

    def tearDown(self):
        shutil.rmtree(self.temp_dir)

    def _write_csv(self, rows: list[list[str]]):
        header = [
            "tx_hash",
            "submitted_at",
            "finalized_observed_at",
            "included_block_number",
            "included_block_hash",
            "included_block_timestamp",
        ]
        with open(self.csv_path, "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(header)
            for row in rows:
                w.writerow(row)

    def test_query_block_range(self):
        """Query min/max block numbers from CSV."""
        self._write_csv([
            [
                "0xabc",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:00:01.000Z",
                "100",
                "0xhash1",
                "2024-01-01T00:00:00Z",
            ],
            [
                "0xdef",
                "2024-01-01T00:01:00.000Z",
                "2024-01-01T00:01:02.000Z",
                "200",
                "0xhash2",
                "2024-01-01T00:01:00Z",
            ],
        ])
        from tx_latency_report import (
            csv_stat,
            query_block_range,
        )
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        conn = connect(self.db_path)
        try:
            result = query_block_range(conn)
            self.assertIsNotNone(result)
            self.assertEqual(result[0], 100)
            self.assertEqual(result[1], 200)
        finally:
            conn.close()

    def test_query_block_range_empty(self):
        """Query returns None for empty CSV."""
        self._write_csv([])
        from tx_latency_report import (
            csv_stat,
            query_block_range,
        )
        csv_info = csv_stat(self.csv_path)
        rebuild_cache(self.csv_path, self.db_path, csv_info)

        conn = connect(self.db_path)
        try:
            result = query_block_range(conn)
            self.assertIsNone(result)
        finally:
            conn.close()


class TestReport(unittest.TestCase):
    """Tests for report function."""

    def test_report_empty_list(self):
        """Report handles empty list."""
        with patch("sys.stdout", new_callable=StringIO) as mock_out:
            report([])
            self.assertIn("No matching rows", mock_out.getvalue())

    def test_report_single_value(self):
        """Report handles single value."""
        with patch("sys.stdout", new_callable=StringIO) as mock_out:
            report([1000])
            output = mock_out.getvalue()
            self.assertIn("No. of txs=1", output)
            self.assertIn("Min latency=1000ms", output)
            self.assertIn("Max latency=1000ms", output)
            self.assertIn("Avg latency=1000ms", output)

    def test_report_multiple_values(self):
        """Report computes correct statistics."""
        with patch("sys.stdout", new_callable=StringIO) as mock_out:
            report([100, 200, 300, 400, 500])
            output = mock_out.getvalue()
            self.assertIn("No. of txs=5", output)
            self.assertIn("Min latency=100ms", output)
            self.assertIn("Max latency=500ms", output)
            self.assertIn("Avg latency=300ms", output)
            self.assertIn("P50 latency=300ms", output)


class TestParseArgs(unittest.TestCase):
    """Tests for parse_args function."""

    def test_parse_args_timestamps_subcommand(self):
        """Parse timestamps subcommand."""
        from tx_latency_report import parse_args

        args = parse_args(["--csv", "test.csv", "timestamps"])
        self.assertEqual(args.csv, "test.csv")
        self.assertEqual(args.command, "timestamps")

    def test_parse_args_blocks_subcommand(self):
        """Parse blocks subcommand."""
        from tx_latency_report import parse_args

        args = parse_args(["--csv", "test.csv", "blocks"])
        self.assertEqual(args.csv, "test.csv")
        self.assertEqual(args.command, "blocks")

    def test_parse_args_report_subcommand(self):
        """Parse report subcommand with filters."""
        from tx_latency_report import parse_args

        args = parse_args(["--csv", "test.csv", "report", "--block", "100"])
        self.assertEqual(args.csv, "test.csv")
        self.assertEqual(args.command, "report")
        self.assertEqual(args.block, "100")

    def test_parse_args_no_subcommand_defaults_to_none(self):
        """Parse with no subcommand sets command to None."""
        from tx_latency_report import parse_args

        args = parse_args(["--csv", "test.csv"])
        self.assertEqual(args.csv, "test.csv")
        self.assertIsNone(args.command)

    def test_parse_args_from_time(self):
        """Parse report subcommand with --from-time."""
        from tx_latency_report import parse_args

        args = parse_args([
            "--csv", "test.csv", "report",
            "--from-time", "2024-01-01T00:00:00Z"
        ])
        self.assertEqual(args.command, "report")
        self.assertEqual(args.from_time, "2024-01-01T00:00:00Z")

    def test_parse_args_from_block(self):
        """Parse report subcommand with --from-block."""
        from tx_latency_report import parse_args

        args = parse_args([
            "--csv", "test.csv", "report",
            "--from-block", "100"
        ])
        self.assertEqual(args.command, "report")
        self.assertEqual(args.from_block, "100")


if __name__ == "__main__":
    unittest.main()
