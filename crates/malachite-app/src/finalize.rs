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

use ssz::Encode;
use tracing::{debug, info};

use alloy_rpc_types_engine::ExecutionPayloadV3;
use arc_consensus_types::{BlockHash, Height};
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;

use crate::metrics::AppMetrics;
use crate::stats::Stats;

/// Abstracts the finalization of decided blocks by the execution engine.
///
/// This trait allows for testing block finalization logic without requiring
/// a full execution engine by enabling mock implementations.
#[cfg_attr(test, mockall::automock)]
pub trait BlockFinalizer {
    async fn finalize_decided_block(
        &self,
        height: Height,
        execution_payload: &ExecutionPayloadV3,
    ) -> Result<(ExecutionBlock, BlockHash), eyre::Report>;
}

impl<T> BlockFinalizer for &T
where
    T: BlockFinalizer + ?Sized,
{
    async fn finalize_decided_block(
        &self,
        height: Height,
        execution_payload: &ExecutionPayloadV3,
    ) -> Result<(ExecutionBlock, BlockHash), eyre::Report> {
        (**self)
            .finalize_decided_block(height, execution_payload)
            .await
    }
}

pub struct EngineBlockFinalizer<'a> {
    engine: &'a Engine,
    stats: &'a Stats,
    metrics: &'a AppMetrics,
}

impl<'a> EngineBlockFinalizer<'a> {
    pub fn new(engine: &'a Engine, stats: &'a Stats, metrics: &'a AppMetrics) -> Self {
        Self {
            engine,
            stats,
            metrics,
        }
    }
}

impl BlockFinalizer for EngineBlockFinalizer<'_> {
    async fn finalize_decided_block(
        &self,
        height: Height,
        execution_payload: &ExecutionPayloadV3,
    ) -> Result<(ExecutionBlock, BlockHash), eyre::Report> {
        finalize_decided_block(
            self.engine,
            self.stats,
            self.metrics,
            height,
            execution_payload,
        )
        .await
    }
}

/// Finalizes a decided block by decoding, logging stats, and updating forkchoice
async fn finalize_decided_block(
    engine: &Engine,
    stats: &Stats,
    metrics: &AppMetrics,
    height: Height,
    execution_payload: &ExecutionPayloadV3,
) -> Result<(ExecutionBlock, BlockHash), eyre::Report> {
    let payload_inner = &execution_payload.payload_inner.payload_inner;

    // Decode bytes into execution payload (a block)
    let new_block_hash = payload_inner.block_hash;
    let new_block_number = payload_inner.block_number;
    let new_block_timestamp = execution_payload.timestamp();

    // Log stats
    let tx_count = payload_inner.transactions.len() as u64;
    let block_size = execution_payload.ssz_bytes_len() as u64;
    let gas_used = payload_inner.gas_used;

    stats.add_txs_count(tx_count);
    stats.add_chain_bytes(block_size);

    info!("👉 Stats at height {height}: {stats}");

    // Make the decided block canonical using forkchoice_updated()
    // The block should already be in the validated payloads pool from proposer/receiver validation
    let latest_valid_hash = {
        let _guard = metrics.start_engine_api_timer("set_latest_forkchoice_state");

        engine.set_latest_forkchoice_state(new_block_hash).await?
    };

    debug!(
        "🚀 Forkchoice updated to height {} for block hash={} and latest_valid_hash={}",
        height, new_block_hash, latest_valid_hash
    );

    // Update Prometheus metrics after successful finalization
    metrics.observe_block_transactions_count(tx_count);
    metrics.observe_block_size_bytes(block_size);
    metrics.observe_block_gas_used(gas_used);
    metrics.inc_total_transactions_count(tx_count);
    metrics.inc_total_chain_bytes(block_size);

    let new_latest_block = ExecutionBlock {
        block_hash: new_block_hash,
        block_number: new_block_number,
        parent_hash: payload_inner.parent_hash,
        timestamp: new_block_timestamp,
    };

    Ok((new_latest_block, latest_valid_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::{Address as AlloyAddress, Bloom, Bytes as AlloyBytes, U256};
    use alloy_rpc_types_engine::{
        ExecutionPayloadV1, ExecutionPayloadV2, ForkchoiceUpdated, PayloadStatus, PayloadStatusEnum,
    };

    use arc_consensus_types::B256;
    use arc_eth_engine::engine::{MockEngineAPI, MockEthereumAPI};

    use crate::metrics::app::AppMetrics;
    use crate::stats::Stats;

    fn make_payload(parent_hash: B256, block_hash: B256) -> ExecutionPayloadV3 {
        ExecutionPayloadV3 {
            payload_inner: ExecutionPayloadV2 {
                payload_inner: ExecutionPayloadV1 {
                    parent_hash,
                    fee_recipient: AlloyAddress::ZERO,
                    state_root: B256::ZERO,
                    receipts_root: B256::ZERO,
                    logs_bloom: Bloom::default(),
                    prev_randao: B256::ZERO,
                    block_number: 1,
                    gas_limit: 0,
                    gas_used: 0,
                    timestamp: 1000,
                    extra_data: AlloyBytes::default(),
                    base_fee_per_gas: U256::from(1u64),
                    block_hash,
                    transactions: vec![],
                },
                withdrawals: vec![],
            },
            blob_gas_used: 0,
            excess_blob_gas: 0,
        }
    }

    /// finalize_decided_block must set parent_hash from the payload,
    /// not from latest_valid_hash (which equals the block's own hash per Engine API spec).
    #[tokio::test]
    async fn finalize_decided_block_sets_correct_parent_hash() {
        let parent_hash = B256::with_last_byte(0x11);
        let block_hash = B256::with_last_byte(0x22);

        let mut mock_engine = MockEngineAPI::new();
        // Engine API spec: forkchoice_updated returns latest_valid_hash == head block hash
        mock_engine
            .expect_forkchoice_updated()
            .returning(move |_, _| {
                Ok(ForkchoiceUpdated {
                    payload_status: PayloadStatus {
                        status: PayloadStatusEnum::Valid,
                        latest_valid_hash: Some(block_hash),
                    },
                    payload_id: None,
                })
            });

        let engine = Engine::new(Box::new(mock_engine), Box::new(MockEthereumAPI::new()));
        let stats = Stats::default();
        let metrics = AppMetrics::default();
        let payload = make_payload(parent_hash, block_hash);

        let (execution_block, _) =
            finalize_decided_block(&engine, &stats, &metrics, Height::new(1), &payload)
                .await
                .expect("finalization should succeed");

        assert_eq!(execution_block.block_hash, block_hash);
        assert_eq!(
            execution_block.parent_hash, parent_hash,
            "parent_hash should come from the payload, not from latest_valid_hash"
        );
        assert_ne!(
            execution_block.parent_hash, execution_block.block_hash,
            "parent_hash must differ from block_hash"
        );
    }
}
