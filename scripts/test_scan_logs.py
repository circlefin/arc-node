#!/usr/bin/env python3
"""Unit tests for scan_logs.py."""

from __future__ import annotations

import json
import http.server
import io
import os
import sqlite3
import tempfile
import threading
import unittest
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from contextlib import contextmanager
from unittest.mock import patch

from scan_logs import (
    ConcurrentScanError,
    JsonRpcProvider,
    MAX_RESPONSE_BYTES,
    QueryTimeoutError,
    RangeLimitError,
    RetryableError,
    RpcError,
    ScanError,
    ScanIdentity,
    Scanner,
    _deduplicate_logs,
    _identity_from_rpc,
    _validate_log,
    initialize,
    main,
    normalize_topics,
)


ADDRESS = "0x" + "aa" * 20
TOPIC = "0x" + "11" * 32
HASH = "0x" + "22" * 32
TX = "0x" + "33" * 32


def log(
    block: int,
    index: int = 0,
    tx: str = TX,
    block_hash: str | None = None,
    **extra,
):
    value = {
        "address": "0x" + ("aa" * 20).upper(),
        "topics": ["0x" + ("11" * 32).upper()],
        "data": "0xABCD",
        "blockNumber": hex(block),
        "transactionIndex": hex(index),
        "logIndex": hex(index),
        "blockHash": block_hash or ("0x" + f"{block:064x}"),
        "transactionHash": tx,
    }
    value.update(extra)
    return value


class Provider:
    def __init__(self, logs=None, limit=None, failures=None, end=5):
        self.logs = logs or []
        self.limit = limit
        self.failures = list(failures or [])
        self.calls = []
        self.end = end

    def call(self, method, params):
        self.calls.append((method, params))
        if self.failures:
            failure = self.failures.pop(0)
            if isinstance(failure, BaseException):
                raise failure
        if method == "eth_chainId":
            return "0x1234"
        if method == "eth_getBlockByNumber":
            tag = params[0]
            number = self.end if tag == "latest" else int(tag, 16)
            if number > self.end:
                return None
            return {"number": hex(number), "hash": HASH}
        start, end = int(params[0]["fromBlock"], 16), int(params[0]["toBlock"], 16)
        if self.limit is not None and end - start + 1 > self.limit:
            raise RpcError(-32005, "block range too large")
        return [item for item in self.logs if start <= int(item["blockNumber"], 16) <= end]


class RpcHandler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        size = int(self.headers.get("Content-Length", "0"))
        request = json.loads(self.rfile.read(size))
        request["_user_agent"] = self.headers.get("User-Agent")
        status, body = self.server.callback(request)
        encoded = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, _format, *_args):
        pass


@contextmanager
def rpc_server(callback):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), RpcHandler)
    server.callback = callback
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}"
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()


def identity(to=5, start=0):
    return ScanIdentity(0x1234, ADDRESS.lower(), [TOPIC.lower()], start, to, HASH.lower())


class ScannerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.db = sqlite3.connect(os.path.join(self.temp.name, "scan.sqlite"))

    def tearDown(self):
        self.db.close()
        self.temp.cleanup()

    def init(self, provider, end=5, start=0):
        initialize(
            self.db,
            identity(end, start),
            "latest" if end == 5 else str(end),
        )

    def test_empty_range_and_ordered_dedup(self):
        provider = Provider(
            [
                log(3),
                log(2, 1, tx="0x" + "44" * 32),
                log(2, 0, tx="0x" + "55" * 32),
            ]
        )
        self.init(provider)
        result = Scanner(self.db, provider, identity(), chunk_size=6).run()
        self.assertEqual(result["logs"], 3)
        rows = self.db.execute(
            "SELECT block_number, transaction_index FROM logs "
            "ORDER BY block_number, transaction_index"
        ).fetchall()
        self.assertEqual(rows, [(2, 0), (2, 1), (3, 0)])
        payload = json.loads(self.db.execute("SELECT data FROM logs").fetchone()[0])
        self.assertEqual(payload["blockNumber"], "0x2")

    def test_same_transaction_distinct_log_indices_and_exact_duplicate(self):
        tx = "0x" + "44" * 32
        first, second = log(1, 0, tx=tx), log(1, 1, tx=tx)
        self.assertEqual(
            len(
                _deduplicate_logs(
                    [first, first, second],
                    0,
                    2,
                    ADDRESS.lower(),
                    [TOPIC.lower()],
                )
            ),
            2,
        )

    def test_range_splitting_has_no_gap_and_reduces_future_chunks(self):
        provider = Provider([log(block) for block in range(6)], limit=2)
        self.init(provider)
        result = Scanner(self.db, provider, identity(), chunk_size=6).run()
        self.assertEqual(result["logs"], 6)
        calls = [
            params[0] for method, params in provider.calls if method == "eth_getLogs"
        ]
        self.assertEqual(
            [
                (int(x["fromBlock"], 16), int(x["toBlock"], 16))
                for x in calls[:3]
            ],
            [(0, 5), (0, 2), (0, 1)],
        )
        self.assertEqual(
            self.db.execute("SELECT next_block FROM scan_state").fetchone()[0],
            6,
        )

    def test_bad_filter_removed_and_out_of_range_rejected(self):
        for item in (
            log(0, removed=True),
            log(3),
            log(0, blockHash="0x99"),
            log(0, address="0x" + "bb" * 20),
            log(0, topics=["0x" + "cc" * 32]),
        ):
            with self.subTest(item=item), self.assertRaises(ScanError):
                _validate_log(item, 0, 2, ADDRESS.lower(), [TOPIC.lower()])

    def test_conflicting_identity_rejected(self):
        other = dict(log(0), data="0x00")
        with self.assertRaises(ScanError):
            _deduplicate_logs(
                [log(0), other], 0, 1, ADDRESS.lower(), [TOPIC.lower()]
            )

    def test_atomic_cursor_and_rows_rollback(self):
        provider = Provider([log(0), log(1, 1, tx="0x" + "44" * 32)])
        self.init(provider, end=1)
        self.db.execute(
            "CREATE TRIGGER fail_second BEFORE INSERT ON logs "
            "WHEN NEW.block_number=1 BEGIN SELECT RAISE(ABORT, 'test'); END"
        )
        with self.assertRaises(sqlite3.IntegrityError):
            Scanner(self.db, provider, identity(1), chunk_size=2).run()
        self.assertEqual(
            self.db.execute("SELECT COUNT(*) FROM logs").fetchone()[0], 0
        )
        self.assertEqual(
            self.db.execute("SELECT next_block FROM scan_state").fetchone()[0], 0
        )

    def test_cas_rejects_stale_writer(self):
        provider = Provider()
        self.init(provider)
        self.db.execute("UPDATE scan_state SET next_block=2")
        self.db.commit()
        with self.assertRaises(ConcurrentScanError):
            Scanner(self.db, provider, identity())._store(0, [], 0)

    def test_cas_uses_two_database_connections(self):
        provider = Provider()
        self.init(provider)
        other = sqlite3.connect(os.path.join(self.temp.name, "scan.sqlite"))
        try:
            other.execute("UPDATE scan_state SET next_block=1")
            other.commit()
            with self.assertRaises(ConcurrentScanError):
                Scanner(self.db, provider, identity())._store(0, [], 0)
        finally:
            other.close()

    def test_topics_normalize_and_conflicting_identity(self):
        upper_topic = "0x" + ("11" * 32).upper()
        self.assertEqual(
            normalize_topics([upper_topic, None, [upper_topic]]),
            [TOPIC.lower(), None, [TOPIC.lower()]],
        )
        provider = Provider()
        self.init(provider)
        with self.assertRaises(ScanError):
            initialize(
                self.db,
                ScanIdentity(0x1234, ADDRESS.lower(), [], 0, 5, HASH.lower()),
                "latest",
            )

    def test_failed_batch_keeps_cursor_and_can_resume(self):
        provider = Provider(failures=[ScanError("temporary fixture failure")])
        self.init(provider)
        with self.assertRaises(ScanError):
            Scanner(self.db, provider, identity(), chunk_size=1).run()
        self.assertEqual(
            self.db.execute("SELECT next_block FROM scan_state").fetchone()[0], 0
        )
        result = Scanner(
            self.db, Provider([log(0)]), identity(), chunk_size=1
        ).run()
        self.assertEqual(result["logs"], 1)

    def test_latest_resume_uses_frozen_numeric_end(self):
        first = Provider()
        self.init(first)

        class NoMovingLatest(Provider):
            def call(self, method, params):
                if method == "eth_getBlockByNumber":
                    self.assert_numeric(params[0])
                return super().call(method, params)

            def assert_numeric(self, tag):
                if tag == "latest":
                    raise AssertionError("latest endpoint was re-queried")

        resumed = NoMovingLatest()
        checked = _identity_from_rpc(
            resumed,
            ADDRESS.lower(),
            [TOPIC.lower()],
            0,
            5,
            0,
            lambda _delay: None,
        )
        self.assertEqual(checked.to_block, 5)

    def test_rpc_concurrency_limit_retries_same_range(self):
        provider = Provider(failures=[RpcError(-32011, "concurrency limit")])
        delays = []
        logs, successful_end = Scanner(
            self.db,
            provider,
            identity(),
            retries=1,
            sleep=delays.append,
        )._fetch_range(0, 0)
        self.assertEqual((len(logs), successful_end), (0, 0))
        self.assertEqual(len(delays), 1)

    def test_query_timeout_retries_then_shrinks(self):
        provider = Provider(
            [log(0)],
            failures=[QueryTimeoutError("RPC query timed out") for _ in range(3)],
        )
        delays = []
        logs, successful_end = Scanner(
            self.db,
            provider,
            identity(),
            retries=2,
            sleep=delays.append,
        )._fetch_range(0, 3)
        self.assertEqual((len(logs), successful_end), (1, 1))
        ranges = [
            (
                int(params[0]["fromBlock"], 16),
                int(params[0]["toBlock"], 16),
            )
            for method, params in provider.calls
            if method == "eth_getLogs"
        ]
        self.assertEqual(ranges, [(0, 3), (0, 3), (0, 3), (0, 1)])
        self.assertEqual(len(delays), 2)

    def test_split_then_later_failure_keeps_successful_prefix(self):
        class FailsAfterTwo(Provider):
            def call(self, method, params):
                if method == "eth_getLogs" and int(params[0]["fromBlock"], 16) >= 2:
                    raise RpcError(4444, "pruned history unavailable")
                return super().call(method, params)

        provider = FailsAfterTwo([log(0), log(1)], limit=2)
        self.init(provider, end=3)
        with self.assertRaises(RpcError):
            Scanner(self.db, provider, identity(3), chunk_size=4).run()
        self.assertEqual(
            self.db.execute("SELECT next_block FROM scan_state").fetchone()[0], 2
        )
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM logs").fetchone()[0], 2)


class ProviderAndRpcTests(unittest.TestCase):
    def setUp(self):
        self.connections = []

    def tearDown(self):
        for connection in self.connections:
            connection.close()

    def memory_connection(self):
        connection = sqlite3.connect(":memory:")
        self.connections.append(connection)
        return connection

    def test_main_freezes_latest_and_resumes_without_duplicates(self):
        state = {"head": 3, "latest": 0, "calls": []}

        def callback(request):
            state["calls"].append(request)
            method = request["method"]
            if method == "eth_chainId":
                result = "0x1234"
            elif method == "eth_getBlockByNumber":
                tag = request["params"][0]
                if tag == "latest":
                    state["latest"] += 1
                    number = state["head"]
                else:
                    number = int(tag, 16)
                result = None if number > state["head"] else {
                    "number": hex(number),
                    "hash": HASH,
                }
            else:
                spec = request["params"][0]
                start = int(spec["fromBlock"], 16)
                end = int(spec["toBlock"], 16)
                result = [log(block) for block in range(start, end + 1)]
            return 200, {
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": result,
            }

        with tempfile.TemporaryDirectory() as directory, rpc_server(callback) as url:
            db_path = os.path.join(directory, "scan.sqlite")
            args = [
                "--rpc-url", url,
                "--address", ADDRESS,
                "--from-block", "0",
                "--to-block", "latest",
                "--chunk-size", "2",
                "--db", db_path,
            ]
            first_out, first_err = io.StringIO(), io.StringIO()
            with redirect_stdout(first_out), redirect_stderr(first_err):
                self.assertEqual(main(args), 0)
            state["head"] = 5
            second_out, second_err = io.StringIO(), io.StringIO()
            with redirect_stdout(second_out), redirect_stderr(second_err):
                self.assertEqual(main(args), 0)

            self.assertEqual(state["latest"], 1)
            self.assertTrue(
                all(
                    request["_user_agent"] == "arc-log-scanner/1.0"
                    for request in state["calls"]
                )
            )
            self.assertEqual(json.loads(first_out.getvalue())["logs"], 4)
            self.assertEqual(json.loads(second_out.getvalue())["logs"], 4)
            with sqlite3.connect(db_path) as db:
                self.assertEqual(db.execute("SELECT COUNT(*) FROM logs").fetchone()[0], 4)
                self.assertEqual(db.execute("SELECT next_block FROM scan_state").fetchone()[0], 4)

    def test_main_rejects_identity_and_endpoint_mismatches(self):
        state = {"chain": "0x1234", "hash": HASH, "head": 2}

        def callback(request):
            method = request["method"]
            if method == "eth_chainId":
                result = state["chain"]
            elif method == "eth_getBlockByNumber":
                tag = request["params"][0]
                number = state["head"] if tag == "latest" else int(tag, 16)
                result = None if number > state["head"] else {
                    "number": hex(number), "hash": state["hash"]
                }
            else:
                result = []
            return 200, {"jsonrpc": "2.0", "id": request["id"], "result": result}

        with tempfile.TemporaryDirectory() as directory, rpc_server(callback) as url:
            db_path = os.path.join(directory, "scan.sqlite")
            args = [
                "--rpc-url", url, "--address", ADDRESS,
                "--topics", "[]", "--from-block", "0", "--to-block", "2",
                "--db", db_path,
            ]
            def invoke(argv):
                with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                    return main(argv)

            self.assertEqual(invoke(args), 0)
            with sqlite3.connect(db_path) as db:
                self.assertEqual(db.execute("SELECT next_block FROM scan_state").fetchone()[0], 3)

            state["chain"] = "0x1235"
            self.assertEqual(invoke(args), 2)
            state["chain"] = "0x1234"
            state["hash"] = "0x" + "99" * 32
            self.assertEqual(invoke(args), 2)
            state["hash"] = HASH
            filtered_args = list(args)
            filtered_args[filtered_args.index("[]")] = "[null]"
            self.assertEqual(invoke(filtered_args), 2)
            with sqlite3.connect(db_path) as db:
                self.assertEqual(db.execute("SELECT next_block FROM scan_state").fetchone()[0], 3)

    def test_main_rejects_unavailable_numeric_end_without_db_cursor(self):
        def callback(request):
            if request["method"] == "eth_chainId":
                result = "0x1234"
            elif request["method"] == "eth_getBlockByNumber":
                result = None
            else:
                result = []
            return 200, {"jsonrpc": "2.0", "id": request["id"], "result": result}

        with tempfile.TemporaryDirectory() as directory, rpc_server(callback) as url:
            db_path = os.path.join(directory, "scan.sqlite")
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertEqual(main([
                    "--rpc-url", url, "--address", ADDRESS,
                    "--from-block", "0", "--to-block", "99", "--db", db_path,
                ]), 2)
            with sqlite3.connect(db_path) as db:
                tables = db.execute(
                    "SELECT name FROM sqlite_master WHERE type='table'"
                ).fetchall()
            self.assertNotIn(("scan_state",), tables)

    def test_http_errors_are_bounded_and_sanitized(self):
        attempts = []

        def callback(request):
            attempts.append(request)
            return 429, {"secret": "https://user:password@example.invalid"}

        with rpc_server(callback) as url:
            provider = JsonRpcProvider(url)
            with self.assertRaises(RetryableError) as context:
                Scanner(
                    self.memory_connection(),
                    provider,
                    identity(),
                    retries=2,
                    sleep=lambda _delay: None,
                )._fetch_range(0, 0)
            self.assertEqual(len(attempts), 3)
            self.assertNotIn("password", str(context.exception))

    def test_http_403_is_fatal_without_retry(self):
        attempts = []

        def callback(request):
            attempts.append(request)
            return 403, {"error": "forbidden"}

        with rpc_server(callback) as url:
            with self.assertRaises(ScanError):
                JsonRpcProvider(url).call("eth_chainId", [])
        self.assertEqual(len(attempts), 1)

    def test_http_malformed_and_oversized_responses_fail_safely(self):
        def malformed(_request):
            return 200, b"not-json"

        with rpc_server(malformed) as url:
            with self.assertRaises(ScanError) as context:
                JsonRpcProvider(url).call("eth_chainId", [])
        self.assertNotIn("not-json", str(context.exception))

        def oversized(_request):
            return 200, b"x" * (MAX_RESPONSE_BYTES + 1)

        with rpc_server(oversized) as url:
            with self.assertRaises(RangeLimitError):
                JsonRpcProvider(url).call("eth_getLogs", [])

        def rpc_error(request):
            return 200, {
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": {
                    "code": 4444,
                    "message": "secret provider detail",
                },
            }

        with rpc_server(rpc_error) as url:
            with self.assertRaises(RpcError) as context:
                JsonRpcProvider(url).call("eth_getLogs", [])
        self.assertNotIn("secret provider detail", str(context.exception))

    def test_range_error_messages_split_but_generic_invalid_params_does_not(self):
        split_provider = Provider(
            [log(0)], failures=[RpcError(-32602, "block range exceeds maximum")]
        )
        fetched, end = Scanner(
            self.memory_connection(), split_provider, identity()
        )._fetch_range(0, 1)
        self.assertEqual((len(fetched), end), (1, 0))
        self.assertEqual(len(split_provider.calls), 2)

        fatal_provider = Provider(
            failures=[RpcError(-32602, "invalid params")]
        )
        with self.assertRaises(RpcError):
            Scanner(
                self.memory_connection(), fatal_provider, identity()
            )._fetch_range(0, 1)
        self.assertEqual(len(fatal_provider.calls), 1)

    def test_pruned_and_single_block_limit_fail_without_retry_or_skip(self):
        pruned = Provider(failures=[RpcError(4444, "pruned history unavailable")])
        with self.assertRaises(RpcError) as context:
            Scanner(
                self.memory_connection(), pruned, identity(), retries=3
            )._fetch_range(0, 1)
        self.assertEqual(len(pruned.calls), 1)
        self.assertNotIn("pruned history unavailable", str(context.exception))

        limited = Provider(limit=0)
        with self.assertRaises(ScanError):
            Scanner(
                self.memory_connection(), limited, identity(), retries=3
            )._fetch_range(0, 0)
        self.assertEqual(len(limited.calls), 1)

    def test_transient_retry_budget_is_bounded_on_same_range(self):
        provider = Provider(failures=[RetryableError("temporary")] * 3)
        with self.assertRaises(RetryableError):
            Scanner(
            self.memory_connection(),
                provider,
                identity(),
                retries=2,
                sleep=lambda _delay: None,
            )._fetch_range(0, 0)
        ranges = [
            (int(params[0]["fromBlock"], 16), int(params[0]["toBlock"], 16))
            for method, params in provider.calls
            if method == "eth_getLogs"
        ]
        self.assertEqual(ranges, [(0, 0), (0, 0), (0, 0)])

    def test_retry_fatal_rpc_and_timeout_split(self):
        provider = Provider([log(0)], failures=[RpcError(401, "unauthorized")])
        with self.assertRaises(RpcError):
            Scanner(
                self.memory_connection(), provider, identity(), retries=2
            )._fetch_range(0, 0)

    def test_json_rpc_rejects_malformed_envelope(self):
        class Response:
            def __enter__(self): return self
            def __exit__(self, *args): return None
            def read(self, _size): return b'{"jsonrpc":"2.0","id":1,"result":null,"error":{}}'
        with patch("scan_logs.urllib.request.urlopen", return_value=Response()):
            with self.assertRaises(ScanError):
                JsonRpcProvider("http://127.0.0.1:1").call("eth_chainId", [])


if __name__ == "__main__":
    unittest.main()
