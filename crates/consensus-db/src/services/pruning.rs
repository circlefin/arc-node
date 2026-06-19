// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

use arc_consensus_types::{Height, PruningConfig};
use tracing::{error, info};

use crate::store::{Store, StoreError};

#[cfg_attr(any(test, feature = "mock"), mockall::automock(type Error = std::io::Error;))]
pub trait PruningService {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Prune decided blocks.
    ///
    /// # Important
    /// Should be called regardless of whether pruning is enabled.
    ///
    /// As historical decided blocks are fetched from EL, we just keep a minimum number of blocks
    /// in the DB to help with EL's amnesia upon recovery.
    async fn prune_decided_blocks(&self) -> Result<Vec<Height>, Self::Error>;

    /// Prune historical data that shares the certificate retention window.
    ///
    /// Implementations are expected to prune the certificates table and any
    /// additional height-keyed tables that use the same retain window (e.g.
    /// diagnostic records). The returned heights reflect only what was pruned
    /// from the certificates table; additional tables pruned alongside are not
    /// surfaced in the return value.
    ///
    /// # Important
    /// Only runs when pruning is enabled.
    ///
    /// # Arguments
    /// - `latest_height`: The latest committed height. Used to determine the effective retain height.
    async fn prune_historical_certs(
        &self,
        latest_height: Height,
    ) -> Result<Vec<Height>, Self::Error>;

    /// Clean up stale consensus data (undecided blocks and pending proposals) for committed heights.
    ///
    /// # Important
    /// Should always be called when committing a block, regardless of pruning configuration.
    ///
    /// # Arguments
    /// - `current_height`: All undecided/pending data with `height <= current_height` will be removed
    async fn clean_stale_consensus_data(&self, current_height: Height) -> Result<(), Self::Error>;
}

impl<T> PruningService for &T
where
    T: PruningService + ?Sized,
{
    type Error = T::Error;

    async fn prune_decided_blocks(&self) -> Result<Vec<Height>, Self::Error> {
        T::prune_decided_blocks(*self).await
    }

    async fn prune_historical_certs(
        &self,
        latest_height: Height,
    ) -> Result<Vec<Height>, Self::Error> {
        T::prune_historical_certs(*self, latest_height).await
    }

    async fn clean_stale_consensus_data(&self, current_height: Height) -> Result<(), Self::Error> {
        T::clean_stale_consensus_data(*self, current_height).await
    }
}

pub struct ProdPruningService<'a> {
    store: &'a Store,
    config: &'a PruningConfig,
}

impl<'a> ProdPruningService<'a> {
    pub fn new(store: &'a Store, config: &'a PruningConfig) -> Self {
        Self { store, config }
    }
}

impl<'a> PruningService for ProdPruningService<'a> {
    type Error = StoreError;

    async fn prune_decided_blocks(&self) -> Result<Vec<Height>, StoreError> {
        self.store.prune_blocks().await
    }

    async fn prune_historical_certs(
        &self,
        latest_height: Height,
    ) -> Result<Vec<Height>, StoreError> {
        if !self.config.enabled() {
            return Ok(Vec::new());
        }

        let retain_height = self.config.effective_certificates_min_height(latest_height);

        info!(height = %latest_height, %retain_height, "Pruning historical data");

        // Each table is pruned independently so a transient failure on one (e.g. a
        // brief I/O hiccup) does not prevent the others from making progress. The
        // certificate-table result is surfaced to the caller; diagnostic-table
        // failures are logged here.
        let pruned_certs = self.store.prune_historical_certs(retain_height).await;

        if let Err(e) = self.store.prune_proposal_monitor_data(retain_height).await {
            error!(%retain_height, "Failed to prune proposal monitor data: {e}");
        }
        if let Err(e) = self.store.prune_misbehavior_evidence(retain_height).await {
            error!(%retain_height, "Failed to prune misbehavior evidence: {e}");
        }
        if let Err(e) = self.store.prune_invalid_payloads(retain_height).await {
            error!(%retain_height, "Failed to prune invalid payloads: {e}");
        }

        pruned_certs
    }

    async fn clean_stale_consensus_data(&self, current_height: Height) -> Result<(), StoreError> {
        self.store.clean_stale_consensus_data(current_height).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use alloy_rpc_types_engine::ExecutionPayloadV3;
    use arbitrary::Unstructured;
    use arc_consensus_types::block::ConsensusBlock;
    use arc_consensus_types::evidence::StoredMisbehaviorEvidence;
    use arc_consensus_types::proposal_monitor::ProposalMonitor;
    use arc_consensus_types::{Address, ArcContext, Round, ValueId};
    use bytesize::ByteSize;
    use malachitebft_app_channel::app::types::core::{CommitCertificate, Validity};
    use tempfile::tempdir;

    use super::*;
    use crate::invalid_payloads::InvalidPayload;
    use crate::metrics::DbMetrics;
    use crate::store::DbUpgrade;

    async fn create_store() -> Store {
        let dir = tempdir().unwrap();
        Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            ByteSize::mib(64),
        )
        .await
        .unwrap()
    }

    fn arbitrary_payload() -> ExecutionPayloadV3 {
        Unstructured::new(&[0xab; 1024])
            .arbitrary::<ExecutionPayloadV3>()
            .unwrap()
    }

    async fn seed_all_tables_at_height(store: &Store, height: Height) {
        let round = Round::new(0);
        let payload = arbitrary_payload();
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let value_id = ValueId::new(block_hash);
        let cert = CommitCertificate::<ArcContext>::new(height, round, value_id, vec![]);
        let block = ConsensusBlock {
            height,
            round,
            valid_round: round,
            proposer: Address::new([0u8; 20]),
            validity: Validity::Valid,
            execution_payload: payload,
            signature: None,
        };
        store
            .store_decided_block(cert, block.execution_payload, block.proposer)
            .await
            .unwrap();
        store
            .store_proposal_monitor_data(ProposalMonitor::new(
                height,
                Address::new([0u8; 20]),
                SystemTime::now(),
            ))
            .await
            .unwrap();
        store
            .store_misbehavior_evidence(StoredMisbehaviorEvidence::empty(height))
            .await
            .unwrap();
        store
            .append_invalid_payload(InvalidPayload::new_without_payload(
                height,
                round,
                Address::new([0u8; 20]),
                "test",
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn prune_historical_certs_also_prunes_diagnostic_tables_when_enabled() {
        let store = create_store().await;
        for h in 1u64..=5 {
            seed_all_tables_at_height(&store, Height::new(h)).await;
        }

        let config = PruningConfig {
            certificates_distance: 0,
            certificates_before: Height::new(4),
        };
        let service = ProdPruningService::new(&store, &config);

        let pruned = service
            .prune_historical_certs(Height::new(5))
            .await
            .unwrap();
        assert_eq!(pruned, vec![Height::new(1), Height::new(2), Height::new(3)]);

        // Re-running with the same retain_height must be a no-op across every table.
        assert!(store
            .prune_historical_certs(Height::new(4))
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .prune_proposal_monitor_data(Height::new(4))
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .prune_misbehavior_evidence(Height::new(4))
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .prune_invalid_payloads(Height::new(4))
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn prune_historical_certs_is_noop_when_pruning_disabled() {
        let store = create_store().await;
        for h in 1u64..=3 {
            seed_all_tables_at_height(&store, Height::new(h)).await;
        }

        let config = PruningConfig::default();
        let service = ProdPruningService::new(&store, &config);

        let pruned = service
            .prune_historical_certs(Height::new(3))
            .await
            .unwrap();
        assert!(pruned.is_empty());

        // All diagnostic tables must still have their rows — a follow-up prune at a
        // high retain_height would drain them.
        let drained_monitor = store
            .prune_proposal_monitor_data(Height::new(999))
            .await
            .unwrap();
        assert_eq!(drained_monitor.len(), 3);
        let drained_evidence = store
            .prune_misbehavior_evidence(Height::new(999))
            .await
            .unwrap();
        assert_eq!(drained_evidence.len(), 3);
        let drained_invalid = store
            .prune_invalid_payloads(Height::new(999))
            .await
            .unwrap();
        assert_eq!(drained_invalid.len(), 3);
    }
}
