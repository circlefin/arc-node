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

//! Contiguous CL history window used by sync-facing handlers.

use arc_consensus_types::Height;

use crate::store::{Store, StoreError};

/// Read the contiguous history window this node can serve to sync peers.
///
/// A store error must not take down the consensus loop: callers log, reply with
/// a safe empty/default value, and return `Ok`.
pub(crate) fn history_window_from_store_reads(
    latest: Result<Option<Height>, StoreError>,
    earliest: Result<Option<Height>, StoreError>,
) -> Result<(Height, Height), String> {
    let latest_height = latest
        .map_err(|e| format!("max_height: {e}"))?
        .unwrap_or_default();
    let earliest_height = earliest
        .map_err(|e| format!("min_height: {e}"))?
        .unwrap_or_default();
    Ok((latest_height, earliest_height))
}

/// Parallel store reads for the history window. Both queries always run so a
/// failure still reports which bound failed.
pub(crate) async fn read_history_window(store: &Store) -> Result<(Height, Height), String> {
    let (latest, earliest) = tokio::join!(store.max_height(), store.min_height());
    history_window_from_store_reads(latest, earliest)
}

#[cfg(test)]
mod tests {
    use bytesize::ByteSize;
    use tempfile::tempdir;

    use arc_consensus_db::{DbMetrics, DbUpgrade};

    use super::*;

    async fn test_store() -> (Store, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let store = Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            ByteSize::mib(64),
        )
        .await
        .unwrap();
        (store, dir)
    }

    #[test]
    fn history_window_from_store_reads_ok() {
        let window =
            history_window_from_store_reads(Ok(Some(Height::new(10))), Ok(Some(Height::new(3))))
                .expect("store reads succeeded");
        assert_eq!(window, (Height::new(10), Height::new(3)));
    }

    #[test]
    fn history_window_from_store_reads_empty_store() {
        let window = history_window_from_store_reads(Ok(None), Ok(None)).expect("empty is ok");
        assert_eq!(window, (Height::new(0), Height::new(0)));
    }

    #[test]
    fn history_window_from_store_reads_tags_max_height_error() {
        let err = StoreError::other("disk I/O failed");
        let message = history_window_from_store_reads(Err(err), Ok(None)).unwrap_err();
        assert!(
            message.starts_with("max_height:"),
            "expected max_height tag, got {message}"
        );
    }

    #[test]
    fn history_window_from_store_reads_tags_min_height_error() {
        let err = StoreError::other("disk I/O failed");
        let message = history_window_from_store_reads(Ok(None), Err(err)).unwrap_err();
        assert!(
            message.starts_with("min_height:"),
            "expected min_height tag, got {message}"
        );
    }

    #[test]
    fn history_window_from_store_reads_prefers_max_height_when_both_fail() {
        let message = history_window_from_store_reads(
            Err(StoreError::other("max failed")),
            Err(StoreError::other("min failed")),
        )
        .unwrap_err();
        assert!(
            message.starts_with("max_height:"),
            "latest is checked first, got {message}"
        );
    }

    #[tokio::test]
    async fn read_history_window_empty_store() {
        let (store, _dir) = test_store().await;
        let window = read_history_window(&store)
            .await
            .expect("empty store is a zero window");
        assert_eq!(window, (Height::new(0), Height::new(0)));
    }
}
