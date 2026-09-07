#!/usr/bin/env python3
"""Unit tests for engine-bench-report.py."""

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("engine-bench-report.py")
SPEC = importlib.util.spec_from_file_location("engine_bench_report", SCRIPT_PATH)
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


class TestSummaryMarkerStatus(unittest.TestCase):
    def test_missing_summary_is_incomplete(self):
        with tempfile.TemporaryDirectory() as results_dir:
            self.assertEqual(
                REPORT.summary_marker_status(results_dir),
                REPORT.SUMMARY_MARKER_MISSING,
            )

    def test_empty_summary_is_incomplete(self):
        with tempfile.TemporaryDirectory() as results_dir:
            Path(results_dir, "summary.csv").touch()

            self.assertEqual(
                REPORT.summary_marker_status(results_dir),
                REPORT.SUMMARY_MARKER_EMPTY,
            )

    def test_non_empty_summary_is_complete(self):
        with tempfile.TemporaryDirectory() as results_dir:
            Path(results_dir, "summary.csv").write_text(
                "block_number,new_payload_ms\n",
                encoding="utf-8",
            )

            self.assertEqual(
                REPORT.summary_marker_status(results_dir),
                REPORT.SUMMARY_MARKER_PRESENT,
            )

    def test_empty_summary_makes_report_partial(self):
        status = REPORT.resolve_report_status(
            REPORT.SUMMARY_MARKER_EMPTY,
            {"samples": 1},
        )

        self.assertEqual(status, REPORT.REPORT_STATUS_PARTIAL)


if __name__ == "__main__":
    unittest.main()
