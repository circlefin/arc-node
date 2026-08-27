// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::time::Duration;

use arc_consensus_types::Height;
use bytesize::ByteSize;

// Environment variables read at startup; leaving one unset keeps its hardcoded default.
const ARC_HALT_AT_BLOCK_HEIGHT: &str = "ARC_HALT_AT_BLOCK_HEIGHT";
const ARC_CONSENSUS_DB_CACHE_SIZE_BYTES: &str = "ARC_CONSENSUS_DB_CACHE_SIZE_BYTES";
const ARC_SYNC_STATUS_UPDATE_INTERVAL: &str = "ARC_SYNC_STATUS_UPDATE_INTERVAL";
const ARC_SYNC_CATCH_UP_THRESHOLD: &str = "ARC_SYNC_CATCH_UP_THRESHOLD";
const ARC_GENESIS_FILE_PATH: &str = "ARC_GENESIS_FILE_PATH";
const ARC_SYNC_REQUEST_TIMEOUT: &str = "ARC_SYNC_REQUEST_TIMEOUT";
const ARC_SYNC_MAX_REQUEST_SIZE: &str = "ARC_SYNC_MAX_REQUEST_SIZE";
const ARC_SYNC_MAX_RESPONSE_SIZE: &str = "ARC_SYNC_MAX_RESPONSE_SIZE";
const ARC_SYNC_PARALLEL_REQUESTS: &str = "ARC_SYNC_PARALLEL_REQUESTS";
const ARC_SYNC_INACTIVE_THRESHOLD: &str = "ARC_SYNC_INACTIVE_THRESHOLD";
const ARC_SYNC_BATCH_SIZE: &str = "ARC_SYNC_BATCH_SIZE";
const ARC_CONSENSUS_WAL_REPLAY_DELAY: &str = "ARC_CONSENSUS_WAL_REPLAY_DELAY";
const ARC_CONSENSUS_QUEUE_CAPACITY: &str = "ARC_CONSENSUS_QUEUE_CAPACITY";
const ARC_CONSENSUS_QUEUE_PER_HEIGHT_CAPACITY: &str = "ARC_CONSENSUS_QUEUE_PER_HEIGHT_CAPACITY";
const ARC_DISCOVERY_EPHEMERAL_CONNECTION_TIMEOUT: &str =
    "ARC_DISCOVERY_EPHEMERAL_CONNECTION_TIMEOUT";
const ARC_REMOTE_SIGNING_TIMEOUT: &str = "ARC_REMOTE_SIGNING_TIMEOUT";

/// Default cache size for the database (1 GiB).
const DEFAULT_DB_CACHE_SIZE: ByteSize = ByteSize::gib(1);

/// Default sync catch up threshold (1.5 seconds).
///
/// Block timestamps are truncated to seconds, adding up to 1s variance to elapsed time.
/// Combined with ~500ms consensus+wait, elapsed ranges are in [500ms ..1.5s) even when perfectly in sync.
const DEFAULT_SYNC_CATCH_UP_THRESHOLD: Duration = Duration::from_millis(1500);

/// Environment-based configuration read once at startup.
pub struct EnvConfig {
    /// If set, the node will halt when reaching this block height.
    pub halt_height: Option<Height>,
    /// Cache size in bytes for the consensus database.
    pub db_cache_size: ByteSize,
    /// If set, overrides the hardcoded sync status update interval.
    /// A value of `0s` means "update on every block".
    pub status_update_interval: Option<Duration>,
    /// Catch up threshold for determining whether the node is syncing or not
    pub sync_catch_up_threshold: Duration,
    /// Path to the EL genesis.json file (for reading hardfork activation conditions).
    pub genesis_file_path: Option<String>,
    /// Overrides `value_sync::REQUEST_TIMEOUT` if set.
    pub value_sync_request_timeout: Option<Duration>,
    /// Overrides `value_sync::MAX_REQUEST_SIZE` if set.
    pub value_sync_max_request_size: Option<ByteSize>,
    /// Overrides `value_sync::MAX_RESPONSE_SIZE` if set.
    pub value_sync_max_response_size: Option<ByteSize>,
    /// Overrides `value_sync::PARALLEL_REQUESTS` if set.
    pub value_sync_parallel_requests: Option<usize>,
    /// Overrides `value_sync::INACTIVE_THRESHOLD` if set.
    pub value_sync_inactive_threshold: Option<Duration>,
    /// Overrides `value_sync::BATCH_SIZE` if set.
    pub value_sync_batch_size: Option<usize>,
    /// Overrides `consensus::WAL_REPLAY_DELAY` if set.
    pub wal_replay_delay: Option<Duration>,
    /// Overrides `consensus::QUEUE_CAPACITY` if set.
    pub queue_capacity: Option<usize>,
    /// Overrides `consensus::QUEUE_PER_HEIGHT_CAPACITY` if set.
    pub queue_per_height_capacity: Option<usize>,
    /// Overrides `discovery::EPHEMERAL_CONNECTION_TIMEOUT` if set.
    pub ephemeral_connection_timeout: Option<Duration>,
    /// Overrides `remote_signing::TIMEOUT` if set (remote signing only).
    pub remote_signing_timeout: Option<Duration>,
}

/// Read an environment variable, returning `None` when it is unset or empty.
/// An empty value is treated as "unset" rather than as a parse error.
fn env_var_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// Parse an environment variable as a `humantime` duration (e.g. `"5s"`, `"500ms"`).
///
/// Returns `Ok(None)` if unset, `Ok(Some(_))` if it parses, and `Err` if it is
/// set to a value that cannot be parsed.
fn env_duration(key: &str) -> eyre::Result<Option<Duration>> {
    match env_var_opt(key) {
        None => Ok(None),
        Some(s) => humantime::parse_duration(&s)
            .map(Some)
            .map_err(|e| eyre::eyre!("{key}: invalid duration {s:?}: {e}")),
    }
}

/// Parse an environment variable via its `FromStr` impl.
///
/// Returns `Ok(None)` if unset, `Ok(Some(_))` if it parses, and `Err` if it is
/// set to a value that cannot be parsed.
fn env_parse<T: std::str::FromStr>(key: &str) -> eyre::Result<Option<T>> {
    match env_var_opt(key) {
        None => Ok(None),
        Some(s) => s
            .parse::<T>()
            .map(Some)
            .map_err(|_| eyre::eyre!("{key}: invalid value {s:?}")),
    }
}

/// Reject an explicit zero for a value that must be strictly positive.
///
/// Parsing already rejects unparseable input; this additionally fails startup on
/// a parseable-but-nonsensical zero rather than silently substituting the
/// hardcoded default, so a misconfigured variable is surfaced instead of leaving
/// the operator under the impression their setting took effect.
fn reject_zero<T>(
    key: &str,
    value: Option<T>,
    is_zero: impl Fn(&T) -> bool,
) -> eyre::Result<Option<T>> {
    if let Some(v) = value.as_ref() {
        if is_zero(v) {
            eyre::bail!("{key}: must be greater than 0");
        }
    }
    Ok(value)
}

/// Parse an environment variable as a `usize` that must be strictly positive.
fn env_nonzero_usize(key: &str) -> eyre::Result<Option<usize>> {
    reject_zero(key, env_parse::<usize>(key)?, |&n| n == 0)
}

/// Parse an environment variable as a `ByteSize` that must be strictly positive.
fn env_nonzero_bytesize(key: &str) -> eyre::Result<Option<ByteSize>> {
    reject_zero(key, env_parse::<ByteSize>(key)?, |b| b.as_u64() == 0)
}

/// Parse an environment variable as a `humantime` duration that must be non-zero.
///
/// For timeouts/thresholds a zero duration is nonsensical, so it is rejected.
/// Durations where zero is a valid setting use `env_duration` instead.
fn env_nonzero_duration(key: &str) -> eyre::Result<Option<Duration>> {
    reject_zero(key, env_duration(key)?, Duration::is_zero)
}

impl EnvConfig {
    /// Read configuration from environment variables.
    ///
    /// A variable that is unset (or empty) falls back to its hardcoded default.
    /// A variable that is *set to a value that cannot be parsed* is a hard error
    /// — startup fails rather than silently using the default.
    ///
    /// Counts, byte sizes, and timeouts/thresholds must be strictly positive: an
    /// explicit `0` is a hard error rather than a silent fallback to the default,
    /// since a zero would break the buffer/sizing/timeout it controls. The few
    /// durations where `0` is a valid setting are exempt (noted below).
    ///
    /// - `ARC_HALT_AT_BLOCK_HEIGHT`: parsed as `u64`; 0 and unset both mean *no halt*.
    /// - `ARC_CONSENSUS_DB_CACHE_SIZE_BYTES`: parsed via `bytesize`; unset means 1 GiB; 0 rejected.
    /// - `ARC_SYNC_STATUS_UPDATE_INTERVAL`: parsed via `humantime`; `0s` means *update every block*.
    /// - `ARC_CONSENSUS_WAL_REPLAY_DELAY`: parsed via `humantime`; `0s` means *no replay delay*.
    pub fn from_env() -> eyre::Result<Self> {
        Ok(Self {
            halt_height: env_parse::<u64>(ARC_HALT_AT_BLOCK_HEIGHT)?
                .filter(|&n| n != 0)
                .map(Height::new),
            db_cache_size: env_nonzero_bytesize(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES)?
                .unwrap_or(DEFAULT_DB_CACHE_SIZE),
            status_update_interval: env_duration(ARC_SYNC_STATUS_UPDATE_INTERVAL)?,
            sync_catch_up_threshold: env_nonzero_duration(ARC_SYNC_CATCH_UP_THRESHOLD)?
                .unwrap_or(DEFAULT_SYNC_CATCH_UP_THRESHOLD),
            genesis_file_path: env_var_opt(ARC_GENESIS_FILE_PATH),
            value_sync_request_timeout: env_nonzero_duration(ARC_SYNC_REQUEST_TIMEOUT)?,
            value_sync_max_request_size: env_nonzero_bytesize(ARC_SYNC_MAX_REQUEST_SIZE)?,
            value_sync_max_response_size: env_nonzero_bytesize(ARC_SYNC_MAX_RESPONSE_SIZE)?,
            value_sync_parallel_requests: env_nonzero_usize(ARC_SYNC_PARALLEL_REQUESTS)?,
            value_sync_inactive_threshold: env_nonzero_duration(ARC_SYNC_INACTIVE_THRESHOLD)?,
            value_sync_batch_size: env_nonzero_usize(ARC_SYNC_BATCH_SIZE)?,
            wal_replay_delay: env_duration(ARC_CONSENSUS_WAL_REPLAY_DELAY)?,
            queue_capacity: env_nonzero_usize(ARC_CONSENSUS_QUEUE_CAPACITY)?,
            queue_per_height_capacity: env_nonzero_usize(ARC_CONSENSUS_QUEUE_PER_HEIGHT_CAPACITY)?,
            ephemeral_connection_timeout: env_nonzero_duration(
                ARC_DISCOVERY_EPHEMERAL_CONNECTION_TIMEOUT,
            )?,
            remote_signing_timeout: env_nonzero_duration(ARC_REMOTE_SIGNING_TIMEOUT)?,
        })
    }
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            halt_height: None,
            db_cache_size: DEFAULT_DB_CACHE_SIZE,
            status_update_interval: None,
            sync_catch_up_threshold: DEFAULT_SYNC_CATCH_UP_THRESHOLD,
            genesis_file_path: None,
            value_sync_request_timeout: None,
            value_sync_max_request_size: None,
            value_sync_max_response_size: None,
            value_sync_parallel_requests: None,
            value_sync_inactive_threshold: None,
            value_sync_batch_size: None,
            wal_replay_delay: None,
            queue_capacity: None,
            queue_per_height_capacity: None,
            ephemeral_connection_timeout: None,
            remote_signing_timeout: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    /// Every `ARC_*` variable this module reads, so the guard can snapshot,
    /// clear, and restore them around each `#[serial]` test.
    const MANAGED_VARS: &[&str] = &[
        ARC_HALT_AT_BLOCK_HEIGHT,
        ARC_CONSENSUS_DB_CACHE_SIZE_BYTES,
        ARC_SYNC_STATUS_UPDATE_INTERVAL,
        ARC_SYNC_CATCH_UP_THRESHOLD,
        ARC_GENESIS_FILE_PATH,
        ARC_SYNC_REQUEST_TIMEOUT,
        ARC_SYNC_MAX_REQUEST_SIZE,
        ARC_SYNC_MAX_RESPONSE_SIZE,
        ARC_SYNC_PARALLEL_REQUESTS,
        ARC_SYNC_INACTIVE_THRESHOLD,
        ARC_SYNC_BATCH_SIZE,
        ARC_CONSENSUS_WAL_REPLAY_DELAY,
        ARC_CONSENSUS_QUEUE_CAPACITY,
        ARC_CONSENSUS_QUEUE_PER_HEIGHT_CAPACITY,
        ARC_DISCOVERY_EPHEMERAL_CONNECTION_TIMEOUT,
        ARC_REMOTE_SIGNING_TIMEOUT,
    ];

    /// Snapshots and clears all managed env vars on construction, restoring them on drop.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let saved = MANAGED_VARS
                .iter()
                .map(|&key| (key, std::env::var(key).ok()))
                .collect();
            for &key in MANAGED_VARS {
                unsafe { std::env::remove_var(key) };
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in &self.saved {
                match val {
                    Some(v) => unsafe { std::env::set_var(key, v) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        }
    }

    // halt_height tests

    #[test]
    #[serial]
    fn test_env_halt_height_not_set() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.halt_height, None);
    }

    #[test]
    #[serial]
    fn test_env_halt_height_invalid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_HALT_AT_BLOCK_HEIGHT, "not_a_number") };
        assert!(EnvConfig::from_env().is_err());
    }

    #[test]
    #[serial]
    fn test_env_halt_height_zero() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_HALT_AT_BLOCK_HEIGHT, "0") };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.halt_height, None);
    }

    #[test]
    #[serial]
    fn test_env_halt_height_valid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_HALT_AT_BLOCK_HEIGHT, "12345") };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.halt_height, Some(Height::new(12345)));
    }

    // db_cache_size tests

    #[test]
    #[serial]
    fn test_env_db_cache_size_not_set() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.db_cache_size, DEFAULT_DB_CACHE_SIZE);
    }

    #[test]
    #[serial]
    fn test_env_db_cache_size_valid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::set_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES, "2048") };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.db_cache_size, ByteSize::b(2048));
    }

    #[test]
    #[serial]
    fn test_env_db_cache_size_invalid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES, "not_a_number") };
        assert!(EnvConfig::from_env().is_err());
    }

    // status_update_interval tests

    #[test]
    #[serial]
    fn test_env_status_update_interval_not_set() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::remove_var(ARC_SYNC_STATUS_UPDATE_INTERVAL) };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.status_update_interval, None);
    }

    #[test]
    #[serial]
    fn test_env_status_update_interval_seconds() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "5s") };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.status_update_interval, Some(Duration::from_secs(5)));
    }

    #[test]
    #[serial]
    fn test_env_status_update_interval_millis() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "500ms") };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.status_update_interval, Some(Duration::from_millis(500)));
    }

    #[test]
    #[serial]
    fn test_env_status_update_interval_zero() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "0s") };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.status_update_interval, Some(Duration::ZERO));
    }

    #[test]
    #[serial]
    fn test_env_status_update_interval_invalid() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "not_a_duration") };
        assert!(EnvConfig::from_env().is_err());
    }

    // genesis_file_path tests

    #[test]
    #[serial]
    fn test_env_genesis_file_path_not_set() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_GENESIS_FILE_PATH) };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.genesis_file_path, None);
    }

    #[test]
    #[serial]
    fn test_env_genesis_file_path_valid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_GENESIS_FILE_PATH, "/app/assets/genesis.json") };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(
            cfg.genesis_file_path,
            Some("/app/assets/genesis.json".to_string())
        );
    }

    #[test]
    #[serial]
    fn test_env_genesis_file_path_empty_string() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_GENESIS_FILE_PATH, "") };
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.genesis_file_path, None);
    }

    // operational tunable tests

    #[test]
    #[serial]
    fn test_env_operational_tunables_unset() {
        let _guard = EnvGuard::new();
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.value_sync_request_timeout, None);
        assert_eq!(cfg.value_sync_max_request_size, None);
        assert_eq!(cfg.value_sync_max_response_size, None);
        assert_eq!(cfg.value_sync_parallel_requests, None);
        assert_eq!(cfg.value_sync_inactive_threshold, None);
        assert_eq!(cfg.value_sync_batch_size, None);
        assert_eq!(cfg.wal_replay_delay, None);
        assert_eq!(cfg.queue_capacity, None);
        assert_eq!(cfg.queue_per_height_capacity, None);
        assert_eq!(cfg.ephemeral_connection_timeout, None);
        assert_eq!(cfg.remote_signing_timeout, None);
    }

    #[test]
    #[serial]
    fn test_env_operational_tunables_all_set() {
        let _guard = EnvGuard::new();
        unsafe {
            std::env::set_var(ARC_SYNC_REQUEST_TIMEOUT, "2s");
            std::env::set_var(ARC_SYNC_MAX_REQUEST_SIZE, "2 MiB");
            std::env::set_var(ARC_SYNC_MAX_RESPONSE_SIZE, "20 MiB");
            std::env::set_var(ARC_SYNC_PARALLEL_REQUESTS, "7");
            std::env::set_var(ARC_SYNC_INACTIVE_THRESHOLD, "90s");
            std::env::set_var(ARC_SYNC_BATCH_SIZE, "25");
            std::env::set_var(ARC_CONSENSUS_WAL_REPLAY_DELAY, "3s");
            std::env::set_var(ARC_CONSENSUS_QUEUE_CAPACITY, "32");
            std::env::set_var(ARC_CONSENSUS_QUEUE_PER_HEIGHT_CAPACITY, "750");
            std::env::set_var(ARC_DISCOVERY_EPHEMERAL_CONNECTION_TIMEOUT, "8s");
            std::env::set_var(ARC_REMOTE_SIGNING_TIMEOUT, "45s");
        }
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.value_sync_request_timeout, Some(Duration::from_secs(2)));
        assert_eq!(cfg.value_sync_max_request_size, Some(ByteSize::mib(2)));
        assert_eq!(cfg.value_sync_max_response_size, Some(ByteSize::mib(20)));
        assert_eq!(cfg.value_sync_parallel_requests, Some(7));
        assert_eq!(
            cfg.value_sync_inactive_threshold,
            Some(Duration::from_secs(90))
        );
        assert_eq!(cfg.value_sync_batch_size, Some(25));
        assert_eq!(cfg.wal_replay_delay, Some(Duration::from_secs(3)));
        assert_eq!(cfg.queue_capacity, Some(32));
        assert_eq!(cfg.queue_per_height_capacity, Some(750));
        assert_eq!(
            cfg.ephemeral_connection_timeout,
            Some(Duration::from_secs(8))
        );
        assert_eq!(cfg.remote_signing_timeout, Some(Duration::from_secs(45)));
    }

    #[test]
    #[serial]
    fn test_env_tunable_invalid_values_error() {
        let _guard = EnvGuard::new();

        // An unparseable value for each parse kind (duration / byte size / count)
        // fails startup rather than silently falling back to the default.
        unsafe { std::env::set_var(ARC_SYNC_REQUEST_TIMEOUT, "not_a_duration") };
        assert!(EnvConfig::from_env().is_err());
        unsafe { std::env::remove_var(ARC_SYNC_REQUEST_TIMEOUT) };

        unsafe { std::env::set_var(ARC_SYNC_MAX_REQUEST_SIZE, "not_a_size") };
        assert!(EnvConfig::from_env().is_err());
        unsafe { std::env::remove_var(ARC_SYNC_MAX_REQUEST_SIZE) };

        unsafe { std::env::set_var(ARC_CONSENSUS_QUEUE_CAPACITY, "not_a_number") };
        assert!(EnvConfig::from_env().is_err());
    }

    #[test]
    #[serial]
    fn test_env_zero_counts_and_byte_sizes_error() {
        let _guard = EnvGuard::new();

        // A zero count or byte size is nonsensical for the buffer/sizing it
        // controls, so it fails startup rather than silently using the default.
        for key in [
            ARC_SYNC_PARALLEL_REQUESTS,
            ARC_SYNC_BATCH_SIZE,
            ARC_CONSENSUS_QUEUE_CAPACITY,
            ARC_CONSENSUS_QUEUE_PER_HEIGHT_CAPACITY,
            ARC_SYNC_MAX_REQUEST_SIZE,
            ARC_SYNC_MAX_RESPONSE_SIZE,
            ARC_CONSENSUS_DB_CACHE_SIZE_BYTES,
        ] {
            unsafe { std::env::set_var(key, "0") };
            assert!(
                EnvConfig::from_env().is_err(),
                "{key}=0 should fail startup"
            );
            unsafe { std::env::remove_var(key) };
        }
    }

    #[test]
    #[serial]
    fn test_env_zero_timeouts_error() {
        let _guard = EnvGuard::new();

        // A zero timeout/threshold is nonsensical, so it fails startup rather
        // than silently using the default.
        for key in [
            ARC_SYNC_CATCH_UP_THRESHOLD,
            ARC_SYNC_REQUEST_TIMEOUT,
            ARC_SYNC_INACTIVE_THRESHOLD,
            ARC_DISCOVERY_EPHEMERAL_CONNECTION_TIMEOUT,
            ARC_REMOTE_SIGNING_TIMEOUT,
        ] {
            unsafe { std::env::set_var(key, "0s") };
            assert!(
                EnvConfig::from_env().is_err(),
                "{key}=0s should fail startup"
            );
            unsafe { std::env::remove_var(key) };
        }
    }

    #[test]
    #[serial]
    fn test_env_zero_durations_allowed_where_meaningful() {
        let _guard = EnvGuard::new();

        // `0` is a valid setting for these durations and must be preserved.
        unsafe {
            std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "0s");
            std::env::set_var(ARC_CONSENSUS_WAL_REPLAY_DELAY, "0s");
        }
        let cfg = EnvConfig::from_env().unwrap();
        assert_eq!(cfg.status_update_interval, Some(Duration::ZERO));
        assert_eq!(cfg.wal_replay_delay, Some(Duration::ZERO));
    }

    #[test]
    #[serial]
    fn test_env_default() {
        let _guard = EnvGuard::new();
        let cfg = EnvConfig::default();
        assert_eq!(cfg.halt_height, None);
        assert_eq!(cfg.db_cache_size, DEFAULT_DB_CACHE_SIZE);
        assert_eq!(cfg.status_update_interval, None);
        assert_eq!(cfg.genesis_file_path, None);
        assert_eq!(cfg.value_sync_request_timeout, None);
        assert_eq!(cfg.value_sync_max_request_size, None);
        assert_eq!(cfg.value_sync_max_response_size, None);
        assert_eq!(cfg.value_sync_parallel_requests, None);
        assert_eq!(cfg.value_sync_inactive_threshold, None);
        assert_eq!(cfg.value_sync_batch_size, None);
        assert_eq!(cfg.wal_replay_delay, None);
        assert_eq!(cfg.queue_capacity, None);
        assert_eq!(cfg.queue_per_height_capacity, None);
        assert_eq!(cfg.ephemeral_connection_timeout, None);
        assert_eq!(cfg.remote_signing_timeout, None);
    }
}
