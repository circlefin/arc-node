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

const ARC_HALT_AT_BLOCK_HEIGHT: &str = "ARC_HALT_AT_BLOCK_HEIGHT";
const ARC_CONSENSUS_DB_CACHE_SIZE_BYTES: &str = "ARC_CONSENSUS_DB_CACHE_SIZE_BYTES";
const ARC_SYNC_STATUS_UPDATE_INTERVAL: &str = "ARC_SYNC_STATUS_UPDATE_INTERVAL";
const ARC_SYNC_CATCH_UP_THRESHOLD: &str = "ARC_SYNC_CATCH_UP_THRESHOLD";
const ARC_GENESIS_FILE_PATH: &str = "ARC_GENESIS_FILE_PATH";

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
}

impl EnvConfig {
    /// Read configuration from environment variables.
    ///
    /// - `ARC_HALT_AT_BLOCK_HEIGHT`: parsed as `u64`; 0 and missing both mean *no halt*.
    /// - `ARC_CONSENSUS_DB_CACHE_SIZE_BYTES`: parsed as `usize`; missing means 1 GiB.
    /// - `ARC_SYNC_STATUS_UPDATE_INTERVAL`: parsed via `humantime` (e.g. `"5s"`, `"500ms"`, `"0s"`);
    ///   missing or unparseable means use the hardcoded default.
    pub fn from_env() -> Self {
        let halt_height = std::env::var(ARC_HALT_AT_BLOCK_HEIGHT)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&n| n != 0)
            .map(Height::new);

        let db_cache_size = std::env::var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_DB_CACHE_SIZE);

        let status_update_interval = std::env::var(ARC_SYNC_STATUS_UPDATE_INTERVAL)
            .ok()
            .and_then(|s| humantime::parse_duration(&s).ok());
        let sync_catch_up_threshold = std::env::var(ARC_SYNC_CATCH_UP_THRESHOLD)
            .ok()
            .and_then(|s| humantime::parse_duration(&s).ok())
            .unwrap_or(DEFAULT_SYNC_CATCH_UP_THRESHOLD);

        let genesis_file_path = std::env::var(ARC_GENESIS_FILE_PATH)
            .ok()
            .filter(|s| !s.is_empty());

        Self {
            halt_height,
            db_cache_size,
            status_update_interval,
            sync_catch_up_threshold,
            genesis_file_path,
        }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    struct EnvGuard {
        old_halt: Option<String>,
        old_cache: Option<String>,
        old_status_interval: Option<String>,
        old_genesis: Option<String>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let old_halt = std::env::var(ARC_HALT_AT_BLOCK_HEIGHT).ok();
            let old_cache = std::env::var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES).ok();
            let old_status_interval = std::env::var(ARC_SYNC_STATUS_UPDATE_INTERVAL).ok();
            let old_genesis = std::env::var(ARC_GENESIS_FILE_PATH).ok();
            Self {
                old_halt,
                old_cache,
                old_status_interval,
                old_genesis,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(ref val) = self.old_halt {
                unsafe { std::env::set_var(ARC_HALT_AT_BLOCK_HEIGHT, val) };
            } else {
                unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
            }
            if let Some(ref val) = self.old_cache {
                unsafe { std::env::set_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES, val) };
            } else {
                unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
            }
            if let Some(ref val) = self.old_status_interval {
                unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, val) };
            } else {
                unsafe { std::env::remove_var(ARC_SYNC_STATUS_UPDATE_INTERVAL) };
            }
            if let Some(ref val) = self.old_genesis {
                unsafe { std::env::set_var(ARC_GENESIS_FILE_PATH, val) };
            } else {
                unsafe { std::env::remove_var(ARC_GENESIS_FILE_PATH) };
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
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.halt_height, None);
    }

    #[test]
    #[serial]
    fn test_env_halt_height_invalid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_HALT_AT_BLOCK_HEIGHT, "not_a_number") };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.halt_height, None);
    }

    #[test]
    #[serial]
    fn test_env_halt_height_zero() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_HALT_AT_BLOCK_HEIGHT, "0") };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.halt_height, None);
    }

    #[test]
    #[serial]
    fn test_env_halt_height_valid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_HALT_AT_BLOCK_HEIGHT, "12345") };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.halt_height, Some(Height::new(12345)));
    }

    // db_cache_size tests

    #[test]
    #[serial]
    fn test_env_db_cache_size_not_set() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.db_cache_size, DEFAULT_DB_CACHE_SIZE);
    }

    #[test]
    #[serial]
    fn test_env_db_cache_size_valid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::set_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES, "2048") };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.db_cache_size, ByteSize::b(2048));
    }

    #[test]
    #[serial]
    fn test_env_db_cache_size_invalid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::set_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES, "not_a_number") };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.db_cache_size, DEFAULT_DB_CACHE_SIZE);
    }

    // status_update_interval tests

    #[test]
    #[serial]
    fn test_env_status_update_interval_not_set() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::remove_var(ARC_SYNC_STATUS_UPDATE_INTERVAL) };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.status_update_interval, None);
    }

    #[test]
    #[serial]
    fn test_env_status_update_interval_seconds() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "5s") };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.status_update_interval, Some(Duration::from_secs(5)));
    }

    #[test]
    #[serial]
    fn test_env_status_update_interval_millis() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "500ms") };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.status_update_interval, Some(Duration::from_millis(500)));
    }

    #[test]
    #[serial]
    fn test_env_status_update_interval_zero() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "0s") };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.status_update_interval, Some(Duration::ZERO));
    }

    #[test]
    #[serial]
    fn test_env_status_update_interval_invalid() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_HALT_AT_BLOCK_HEIGHT) };
        unsafe { std::env::remove_var(ARC_CONSENSUS_DB_CACHE_SIZE_BYTES) };
        unsafe { std::env::set_var(ARC_SYNC_STATUS_UPDATE_INTERVAL, "not_a_duration") };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.status_update_interval, None);
    }

    // genesis_file_path tests

    #[test]
    #[serial]
    fn test_env_genesis_file_path_not_set() {
        let _guard = EnvGuard::new();
        unsafe { std::env::remove_var(ARC_GENESIS_FILE_PATH) };
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.genesis_file_path, None);
    }

    #[test]
    #[serial]
    fn test_env_genesis_file_path_valid_value() {
        let _guard = EnvGuard::new();
        unsafe { std::env::set_var(ARC_GENESIS_FILE_PATH, "/app/assets/genesis.json") };
        let cfg = EnvConfig::from_env();
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
        let cfg = EnvConfig::from_env();
        assert_eq!(cfg.genesis_file_path, None);
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
    }
}
