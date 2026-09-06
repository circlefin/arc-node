#!/usr/bin/env python3
"""Resumable, bounded-range Ethereum log scanner."""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import socket
import sqlite3
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any, Callable, Iterable


SCHEMA_VERSION = "1"
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
HEX_RE = re.compile(r"^0x[0-9a-fA-F]*$", re.IGNORECASE)


class ScanError(Exception):
    """An expected scanner or input failure."""


class RetryableError(ScanError):
    """A transport failure which can be retried."""


class QueryTimeoutError(ScanError):
    """The node timed out while evaluating a log query."""


class RangeLimitError(ScanError):
    """The node rejected a log range because it was too large."""


class ConcurrentScanError(ScanError):
    """Another scanner advanced the persisted cursor."""


class RpcError(ScanError):
    def __init__(self, code: int, message: str):
        self.code = code
        self.message = message
        hint = " (pruned historical data unavailable)" if code == 4444 else ""
        super().__init__(f"RPC error {code}{hint}")


def _quantity(value: Any, field: str) -> int:
    if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]+", value, re.IGNORECASE):
        raise ScanError(f"invalid {field}")
    try:
        result = int(value[2:], 16)
    except ValueError as exc:
        raise ScanError(f"invalid {field}") from exc
    if result > 9223372036854775807:
        raise ScanError(f"{field} is too large")
    return result


def _canonical_hex(value: Any, length: int, field: str) -> str:
    if not isinstance(value, str) or len(value) != 2 + length * 2 or not HEX_RE.fullmatch(value):
        raise ScanError(f"invalid {field}")
    return value.lower()


def _data(value: Any) -> str:
    if not isinstance(value, str) or not HEX_RE.fullmatch(value) or len(value[2:]) % 2:
        raise ScanError("invalid log data")
    return value.lower()


def normalize_address(value: str) -> str:
    return _canonical_hex(value, 20, "address")


def normalize_topics(value: Any) -> list[Any]:
    if not isinstance(value, list):
        raise ScanError("topics must be a JSON array")
    if len(value) > 4:
        raise ScanError("topics must contain at most four positions")
    result: list[Any] = []
    for item in value:
        if item is None:
            result.append(None)
        elif isinstance(item, str):
            result.append(_canonical_hex(item, 32, "topic"))
        elif isinstance(item, list):
            alternatives = [_canonical_hex(topic, 32, "topic") for topic in item]
            result.append(None if not alternatives else alternatives)
        else:
            raise ScanError("each topic filter must be null, a 32-byte hex string, or an array")
    return result


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def _retryable_transport(exc: BaseException) -> bool:
    if isinstance(exc, (TimeoutError, socket.timeout, ConnectionError)):
        return True
    if isinstance(exc, urllib.error.URLError):
        return isinstance(exc.reason, (TimeoutError, socket.timeout, ConnectionError, OSError))
    return False


class JsonRpcProvider:
    """Small JSON-RPC client which deliberately hides endpoint response details."""

    def __init__(self, url: str, timeout: float = 15.0):
        try:
            parsed = urllib.parse.urlsplit(url) if isinstance(url, str) else None
        except ValueError as exc:
            raise ScanError("RPC URL must be an HTTP or HTTPS URL") from exc
        if parsed is None or parsed.scheme not in ("http", "https") or not parsed.netloc:
            raise ScanError("RPC URL must be an HTTP or HTTPS URL")
        self.url = url
        self.timeout = timeout
        self._request_id = 0

    def call(self, method: str, params: list[Any]) -> Any:
        self._request_id += 1
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": self._request_id,
                "method": method,
                "params": params,
            }
        ).encode()
        try:
            request = urllib.request.Request(
                self.url,
                data=body,
                headers={"Content-Type": "application/json", "User-Agent": "arc-log-scanner/1.0"},
                method="POST",
            )
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read(MAX_RESPONSE_BYTES + 1)
                if len(raw) > MAX_RESPONSE_BYTES:
                    raise RangeLimitError("RPC response is too large")
        except urllib.error.HTTPError as exc:
            code = exc.code
            exc.close()
            if code == 504:
                raise QueryTimeoutError("RPC query timed out") from exc
            if code == 429 or 500 <= code <= 599:
                raise RetryableError(f"RPC HTTP {code}") from exc
            raise ScanError(f"RPC HTTP {code}") from exc
        except (RangeLimitError, QueryTimeoutError, RetryableError):
            raise
        except Exception as exc:
            if _retryable_transport(exc):
                raise RetryableError("RPC transport failed") from exc
            raise ScanError("RPC transport failed") from exc
        try:
            message = json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ScanError("malformed JSON-RPC response") from exc
        if (
            not isinstance(message, dict)
            or message.get("jsonrpc") != "2.0"
            or type(message.get("id")) is not int
            or message.get("id") != self._request_id
        ):
            raise ScanError("malformed JSON-RPC response")
        has_result = "result" in message
        has_error = "error" in message
        if has_result == has_error:
            raise ScanError("malformed JSON-RPC response")
        if has_error:
            error = message["error"]
            if (
                not isinstance(error, dict)
                or type(error.get("code")) is not int
                or not isinstance(error.get("message"), str)
            ):
                raise ScanError("malformed JSON-RPC response")
            raise RpcError(error["code"], error["message"])
        return message["result"]


def _rpc_call(
    provider: Any,
    method: str,
    params: list[Any],
    retries: int,
    sleep: Callable[[float], None],
) -> Any:
    attempt = 0
    while True:
        try:
            return provider.call(method, params)
        except RetryableError:
            if attempt >= retries:
                raise
            sleep(min(2.0, 0.25 * (2**attempt)))
            attempt += 1


def _is_range_error(exc: RpcError) -> bool:
    message = exc.message.lower()
    range_phrases = (
        "block range",
        "range too",
        "too many results",
        "result limit",
        "response too large",
        "maximum blocks",
        "max block range",
    )
    return exc.code in (-32005, -32602) and any(
        phrase in message for phrase in range_phrases
    )


def _is_query_timeout(exc: RpcError) -> bool:
    message = exc.message.lower()
    return exc.code in (-32000, -32005, -32603) and (
        "timeout" in message or "timed out" in message
    )


def _validate_log(
    raw: Any,
    start: int,
    end: int,
    address: str,
    topics: list[Any],
) -> dict[str, Any]:
    if not isinstance(raw, dict):
        raise ScanError("invalid log result")
    required = (
        "address",
        "topics",
        "data",
        "blockNumber",
        "transactionIndex",
        "logIndex",
        "blockHash",
        "transactionHash",
    )
    if any(key not in raw for key in required):
        raise ScanError("invalid log result")
    if "removed" in raw and raw["removed"] is not False:
        raise ScanError("removed logs are not supported")
    block_number = _quantity(raw["blockNumber"], "blockNumber")
    transaction_index = _quantity(raw["transactionIndex"], "transactionIndex")
    log_index = _quantity(raw["logIndex"], "logIndex")
    block_hash = _canonical_hex(raw["blockHash"], 32, "blockHash")
    transaction_hash = _canonical_hex(raw["transactionHash"], 32, "transactionHash")
    log_address = normalize_address(raw["address"])
    if block_number < start or block_number > end or log_address != address:
        raise ScanError("log is outside requested range or address filter")
    log_topics = raw["topics"]
    if not isinstance(log_topics, list):
        raise ScanError("invalid log topics")
    normalized_topics = [_canonical_hex(topic, 32, "topic") for topic in log_topics]
    for index, wanted in enumerate(topics):
        if index >= len(normalized_topics):
            raise ScanError("log does not match topic filter")
        if wanted is None:
            continue
        if (
            isinstance(wanted, list) and normalized_topics[index] not in wanted
        ) or (
            isinstance(wanted, str) and normalized_topics[index] != wanted
        ):
            raise ScanError("log does not match topic filter")
    return {
        "block_number": block_number,
        "transaction_index": transaction_index,
        "log_index": log_index,
        "block_hash": block_hash,
        "transaction_hash": transaction_hash,
        "address": log_address,
        "topics": canonical_json(normalized_topics),
        "data": canonical_json({
            "address": log_address,
            "topics": normalized_topics,
            "data": _data(raw["data"]),
            "blockNumber": hex(block_number),
            "transactionIndex": hex(transaction_index),
            "logIndex": hex(log_index),
            "blockHash": block_hash,
            "transactionHash": transaction_hash,
        }),
    }


def _deduplicate_logs(
    raw_logs: Iterable[Any],
    start: int,
    end: int,
    address: str,
    topics: list[Any],
) -> list[dict[str, Any]]:
    by_identity: dict[tuple[str, str, int], dict[str, Any]] = {}
    for raw in raw_logs:
        log = _validate_log(raw, start, end, address, topics)
        key = (log["block_hash"], log["transaction_hash"], log["log_index"])
        previous = by_identity.get(key)
        if previous is not None and previous != log:
            raise ScanError("conflicting logs with the same identity")
        by_identity[key] = log
    return sorted(
        by_identity.values(),
        key=lambda item: (
            item["block_number"],
            item["transaction_index"],
            item["log_index"],
            item["block_hash"],
            item["transaction_hash"],
        ),
    )


@dataclass(frozen=True)
class ScanIdentity:
    chain_id: int
    address: str
    topics: list[Any]
    from_block: int
    to_block: int
    to_block_hash: str


def connect_db(path: str) -> sqlite3.Connection:
    conn = sqlite3.connect(path, timeout=30.0)
    conn.execute("PRAGMA busy_timeout=30000")
    conn.execute("PRAGMA foreign_keys=ON")
    return conn


def _create_schema(conn: sqlite3.Connection) -> None:
    conn.execute("CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
    conn.execute(
        "CREATE TABLE IF NOT EXISTS scan_state ("
        "id INTEGER PRIMARY KEY CHECK (id = 1), next_block INTEGER NOT NULL)"
    )
    conn.execute("""CREATE TABLE IF NOT EXISTS logs (
        block_number INTEGER NOT NULL, transaction_index INTEGER NOT NULL,
        log_index INTEGER NOT NULL, block_hash TEXT NOT NULL,
        transaction_hash TEXT NOT NULL, address TEXT NOT NULL,
        topics TEXT NOT NULL, data TEXT NOT NULL,
        PRIMARY KEY (block_hash, transaction_hash, log_index)
    )""")


def _metadata(conn: sqlite3.Connection) -> dict[str, str]:
    try:
        return dict(conn.execute("SELECT key, value FROM metadata"))
    except sqlite3.OperationalError:
        return {}


def initialize(conn: sqlite3.Connection, identity: ScanIdentity, requested_to: str) -> int:
    """Create a scan or verify its immutable identity, returning its cursor."""
    expected = {
        "schema_version": SCHEMA_VERSION,
        "chain_id": str(identity.chain_id),
        "address": identity.address,
        "topics": canonical_json(identity.topics),
        "from_block": str(identity.from_block),
        "to_block": str(identity.to_block),
        "to_block_hash": identity.to_block_hash,
    }
    conn.execute("BEGIN IMMEDIATE")
    try:
        _create_schema(conn)
        found = _metadata(conn)
        if found:
            for key, value in expected.items():
                if found.get(key) != value:
                    raise ScanError(f"database identity mismatch: {key}")
            row = conn.execute("SELECT next_block FROM scan_state WHERE id = 1").fetchone()
            if row is None:
                raise ScanError("database cursor is missing")
            cursor = int(row[0])
            if cursor < identity.from_block or cursor > identity.to_block + 1:
                raise ScanError("database cursor is invalid")
            if requested_to != "latest" and int(requested_to) != identity.to_block:
                raise ScanError("database end block mismatch")
        else:
            conn.executemany("INSERT INTO metadata(key,value) VALUES (?,?)", expected.items())
            conn.execute(
                "INSERT INTO scan_state(id,next_block) VALUES (1,?)",
                (identity.from_block,),
            )
            cursor = identity.from_block
        conn.commit()
        return cursor
    except Exception:
        conn.rollback()
        raise


class Scanner:
    def __init__(
        self,
        conn: sqlite3.Connection,
        provider: Any,
        identity: ScanIdentity,
        chunk_size: int = 10000,
        retries: int = 3,
        sleep: Callable[[float], None] = time.sleep,
        progress: Callable[[str], None] | None = None,
    ):
        self.conn, self.provider, self.identity = conn, provider, identity
        self.chunk_size, self.retries, self.sleep = chunk_size, retries, sleep
        self.progress = progress or (lambda _message: None)

    def _query_once(self, start: int, end: int) -> Any:
        return self.provider.call(
            "eth_getLogs",
            [
                {
                    "fromBlock": hex(start),
                    "toBlock": hex(end),
                    "address": self.identity.address,
                    "topics": self.identity.topics,
                }
            ],
        )

    def _fetch_range(self, start: int, end: int) -> tuple[list[dict[str, Any]], int]:
        """Fetch one successful prefix, reducing its end without accumulating halves."""
        successful_end = end
        attempt = 0
        while True:
            try:
                result = self._query_once(start, successful_end)
                if not isinstance(result, list):
                    raise ScanError("invalid eth_getLogs result")
                return (
                    _deduplicate_logs(
                        result,
                        start,
                        successful_end,
                        self.identity.address,
                        self.identity.topics,
                    ),
                    successful_end,
                )
            except (RangeLimitError, QueryTimeoutError) as exc:
                if isinstance(exc, QueryTimeoutError) and attempt < self.retries:
                    self.sleep(min(2.0, 0.25 * (2**attempt)))
                    attempt += 1
                    continue
                if start == successful_end:
                    raise ScanError("single-block log query failed") from exc
                successful_end = start + (successful_end - start + 2) // 2 - 1
                attempt = 0
            except RpcError as exc:
                if exc.code == -32011:
                    if attempt >= self.retries:
                        raise
                    self.sleep(min(2.0, 0.25 * (2**attempt)))
                    attempt += 1
                    continue
                if _is_range_error(exc):
                    if start == successful_end:
                        raise ScanError("single-block log query failed") from exc
                    successful_end = start + (successful_end - start + 2) // 2 - 1
                    attempt = 0
                    continue
                if _is_query_timeout(exc):
                    if attempt < self.retries:
                        self.sleep(min(2.0, 0.25 * (2**attempt)))
                        attempt += 1
                        continue
                    if start == successful_end:
                        raise ScanError("single-block log query failed") from exc
                    successful_end = start + (successful_end - start + 2) // 2 - 1
                    attempt = 0
                    continue
                raise
            except RetryableError:
                if attempt >= self.retries:
                    raise
                self.sleep(min(2.0, 0.25 * (2**attempt)))
                attempt += 1

    def _store(self, expected_cursor: int, logs: list[dict[str, Any]], successful_end: int) -> None:
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            row = self.conn.execute("SELECT next_block FROM scan_state WHERE id = 1").fetchone()
            if row is None or int(row[0]) != expected_cursor:
                raise ConcurrentScanError("scan cursor changed while request was in flight")
            for log in logs:
                key = (log["block_hash"], log["transaction_hash"], log["log_index"])
                existing = self.conn.execute(
                    "SELECT block_number, transaction_index, log_index, "
                    "block_hash, transaction_hash, address, topics, data "
                    "FROM logs WHERE block_hash=? AND transaction_hash=? "
                    "AND log_index=?",
                    key,
                ).fetchone()
                fields = (
                    "block_number",
                    "transaction_index",
                    "log_index",
                    "block_hash",
                    "transaction_hash",
                    "address",
                    "topics",
                    "data",
                )
                values = tuple(log[field] for field in fields)
                if existing is not None:
                    if tuple(existing) != values:
                        raise ScanError("database contains a conflicting log")
                    continue
                self.conn.execute(
                    "INSERT INTO logs(block_number,transaction_index,log_index,"
                    "block_hash,transaction_hash,address,topics,data) "
                    "VALUES (?,?,?,?,?,?,?,?)",
                    values,
                )
            updated = self.conn.execute(
                "UPDATE scan_state SET next_block=? "
                "WHERE id=1 AND next_block=?",
                (successful_end + 1, expected_cursor),
            )
            if updated.rowcount != 1:
                raise ConcurrentScanError("scan cursor changed while writing")
            self.conn.commit()
        except Exception:
            self.conn.rollback()
            raise

    def run(self) -> dict[str, Any]:
        cursor = int(
            self.conn.execute(
                "SELECT next_block FROM scan_state WHERE id=1"
            ).fetchone()[0]
        )
        while cursor <= self.identity.to_block:
            requested_end = min(self.identity.to_block, cursor + self.chunk_size - 1)
            logs, successful_end = self._fetch_range(cursor, requested_end)
            self._store(cursor, logs, successful_end)
            self.chunk_size = min(self.chunk_size, successful_end - cursor + 1)
            cursor = successful_end + 1
            self.progress(f"scanned {cursor - 1}/{self.identity.to_block} ({len(logs)} logs)")
        count = int(self.conn.execute("SELECT COUNT(*) FROM logs").fetchone()[0])
        return {
            "fromBlock": self.identity.from_block,
            "toBlock": self.identity.to_block,
            "nextBlock": cursor,
            "logs": count,
            "complete": cursor > self.identity.to_block,
        }


def _parse_block(value: str, name: str) -> int | str:
    if value == "latest" and name == "to-block":
        return value
    if not re.fullmatch(r"[0-9]+", value):
        raise argparse.ArgumentTypeError(f"{name} must be a nonnegative decimal integer")
    return int(value)


def _parse_positive(value: str) -> int:
    try:
        result = int(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be a positive integer") from exc
    if result <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return result


def _parse_timeout(value: str) -> float:
    try:
        result = float(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be a positive finite number") from exc
    if not math.isfinite(result) or result <= 0:
        raise argparse.ArgumentTypeError("must be a positive finite number")
    return result


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpc-url", default=os.environ.get("ARC_RPC_URL"))
    parser.add_argument("--address", required=True)
    parser.add_argument("--topics", default="[]", help="JSON array of topic position filters")
    parser.add_argument(
        "--from-block",
        required=True,
        type=lambda value: _parse_block(value, "from-block"),
    )
    parser.add_argument(
        "--to-block",
        default="latest",
        type=lambda value: _parse_block(value, "to-block"),
    )
    parser.add_argument("--db", required=True)
    parser.add_argument("--chunk-size", default=10000, type=_parse_positive)
    parser.add_argument("--timeout", default=15.0, type=_parse_timeout)
    parser.add_argument("--retries", default=3, type=lambda value: _parse_nonnegative(value))
    return parser


def _parse_nonnegative(value: str) -> int:
    try:
        result = int(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be a nonnegative integer") from exc
    if result < 0:
        raise argparse.ArgumentTypeError("must be a nonnegative integer")
    return result


def _identity_from_rpc(
    provider: Any,
    address: str,
    topics: list[Any],
    from_block: int,
    to_block: int | str,
    retries: int,
    sleep: Callable[[float], None],
) -> ScanIdentity:
    chain_id = _quantity(_rpc_call(provider, "eth_chainId", [], retries, sleep), "chainId")
    if to_block == "latest":
        block_tag: str | int = "latest"
    else:
        block_tag = to_block
    block = _rpc_call(
        provider,
        "eth_getBlockByNumber",
        [block_tag if block_tag == "latest" else hex(block_tag), False],
        retries,
        sleep,
    )
    if not isinstance(block, dict) or block.get("number") is None or block.get("hash") is None:
        raise ScanError("endpoint block is unavailable")
    number = _quantity(block["number"], "block number")
    block_hash = _canonical_hex(block["hash"], 32, "block hash")
    if isinstance(to_block, int) and number != to_block:
        raise ScanError("requested end block is unavailable")
    if from_block > number:
        raise ScanError("from-block is after endpoint block")
    return ScanIdentity(chain_id, address, topics, from_block, number, block_hash)


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    try:
        args = parser.parse_args(argv)
        if not args.rpc_url:
            parser.error("--rpc-url or ARC_RPC_URL is required")
        address = normalize_address(args.address)
        topics = normalize_topics(json.loads(args.topics))
        if not isinstance(args.from_block, int):
            raise ScanError("from-block must be a nonnegative decimal integer")
        provider = JsonRpcProvider(args.rpc_url, args.timeout)
        conn = connect_db(args.db)
        try:
            existing = _metadata(conn)
            if existing and args.to_block == "latest":
                fixed_end: int | str = int(existing.get("to_block", "-1"))
            else:
                fixed_end = args.to_block
            identity = _identity_from_rpc(
                provider,
                address,
                topics,
                args.from_block,
                fixed_end,
                args.retries,
                time.sleep,
            )
            initialize(conn, identity, args.to_block)
            summary = Scanner(
                conn,
                provider,
                identity,
                args.chunk_size,
                args.retries,
                time.sleep,
                lambda msg: print(msg, file=sys.stderr),
            ).run()
            print(json.dumps(summary, sort_keys=True, separators=(",", ":")))
            return 0
        finally:
            conn.close()
    except KeyboardInterrupt:
        print("scan interrupted; progress is resumable", file=sys.stderr)
        return 130
    except (ScanError, ValueError, json.JSONDecodeError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except (sqlite3.Error, OSError):
        print("error: local database or file operation failed", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
