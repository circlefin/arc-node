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

use eyre::{eyre, Context};
use reqwest::Url;
use std::{
    ops::Deref,
    path::Path,
    sync::{Arc, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use tracing::debug;

use alloy_rpc_types_engine::{
    ExecutionPayloadV3, ForkchoiceUpdated, PayloadAttributes, PayloadId as AlloyPayloadId,
    PayloadStatus, PayloadStatusEnum,
};
use alloy_rpc_types_txpool::{TxpoolInspect, TxpoolStatus};

use arc_consensus_types::{Address, BlockHash, ConsensusParams, ValidatorSet, B256};

use crate::capabilities::{check_capabilities, EngineCapabilities};
use crate::ipc::{engine_ipc::EngineIPC, ethereum_ipc::EthereumIPC};
use crate::json_structures::ExecutionBlock;
use crate::rpc::{engine_rpc::EngineRpc, ethereum_rpc::EthereumRPC};

#[cfg_attr(any(test, feature = "mocks"), mockall::automock)]
#[async_trait]
pub trait EngineAPI: Send + Sync {
    /// Exchange capabilities with the engine.
    async fn exchange_capabilities(&self) -> eyre::Result<EngineCapabilities>;
    /// Set the latest forkchoice state.
    async fn forkchoice_updated(
        &self,
        head_block_hash: BlockHash,
        maybe_payload_attributes: Option<PayloadAttributes>,
    ) -> eyre::Result<ForkchoiceUpdated>;
    /// Get a payload by its ID.
    /// When `use_v5` is true, uses `engine_getPayloadV5` (Osaka); otherwise uses V4.
    async fn get_payload(
        &self,
        payload_id: AlloyPayloadId,
        use_v5: bool,
    ) -> eyre::Result<ExecutionPayloadV3>;
    /// Notify that a new payload has been created.
    async fn new_payload(
        &self,
        execution_payload: &ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_block_hash: BlockHash,
    ) -> eyre::Result<PayloadStatus>;
}

#[cfg_attr(any(test, feature = "mocks"), mockall::automock)]
#[async_trait]
pub trait EthereumAPI: Send + Sync {
    /// Get the eth1 chain id of the given endpoint.
    async fn get_chain_id(&self) -> eyre::Result<String>;
    /// Get the genesis block.
    async fn get_genesis_block(&self) -> eyre::Result<ExecutionBlock>;
    /// Get the active validator set at a specific block height.
    async fn get_active_validator_set(&self, block_height: u64) -> eyre::Result<ValidatorSet>;
    /// Get the consensus parameters at a specific block height.
    async fn get_consensus_params(&self, block_height: u64) -> eyre::Result<ConsensusParams>;
    /// Get a block by its number.
    async fn get_block_by_number(&self, block_number: &str)
        -> eyre::Result<Option<ExecutionBlock>>;
    /// Get multiple full payloads.
    async fn get_execution_payloads(
        &self,
        block_numbers: &[String],
    ) -> eyre::Result<Vec<Option<ExecutionPayloadV3>>>;
    /// Get the status of the transaction pool.
    async fn txpool_status(&self) -> eyre::Result<TxpoolStatus>;
    /// Get the contents of the transaction pool.
    async fn txpool_inspect(&self) -> eyre::Result<TxpoolInspect>;
}

/// Function that checks whether Osaka is active at a given timestamp.
/// Used by the Engine to decide between `engine_getPayloadV4` and `engine_getPayloadV5`.
pub type IsOsakaActiveFn = Arc<dyn Fn(u64) -> bool + Send + Sync>;

/// Ethereum engine implementation.
/// Spec: https://github.com/ethereum/execution-apis/tree/main/src/engine
#[derive(Clone)]
pub struct Engine(Arc<Inner>);

impl Engine {
    /// Create a new engine using IPC.
    pub async fn new_ipc(execution_socket: &str, eth_socket: &str) -> eyre::Result<Self> {
        let api = Box::new(EngineIPC::new(execution_socket).await?);
        let eth = Box::new(EthereumIPC::new(eth_socket).await?);
        Ok(Self::new(api, eth))
    }

    /// Create a new engine using RPC.
    ///
    /// Probes the RPC server with `net_listening` to confirm it is
    /// reachable before returning.
    pub async fn new_rpc(
        execution_endpoint: Url,
        eth_endpoint: Url,
        execution_jwt: &str,
    ) -> eyre::Result<Self> {
        let api = Box::new(EngineRpc::new(
            execution_endpoint,
            Path::new(execution_jwt),
        )?);
        let eth = Box::new(EthereumRPC::new(eth_endpoint)?);

        // Probe the RPC server to confirm it is reachable.
        eth.check_connectivity().await?;

        Ok(Self::new(api, eth))
    }

    /// Create a new engine with custom API implementations.
    pub fn new(api: Box<dyn EngineAPI>, eth: Box<dyn EthereumAPI>) -> Self {
        Self(Arc::new(Inner::new(api, eth)))
    }

    /// Set the function that determines whether Osaka is active at a given timestamp.
    ///
    /// This should be called after construction once the chainspec is known.
    /// The provided function should use the same chainspec as the EL so that
    /// the V4/V5 decision always aligns.
    pub fn set_is_osaka_active(&self, f: IsOsakaActiveFn) {
        if self.0.is_osaka_active.set(f).is_err() {
            tracing::warn!("Osaka activation function already set; ignoring duplicate call");
        }
    }

    /// Configure the Osaka hardfork check by parsing the given genesis.json file.
    ///
    /// Builds an `ArcChainSpec` from the same file the EL uses, so the V4/V5
    /// decision always aligns — even when the file is patched at runtime
    /// (e.g. nightly-upgrade tests that set a future `osakaTime`).
    pub fn set_osaka_from_genesis_file(&self, genesis_path: &str) -> eyre::Result<()> {
        use arc_execution_config::chainspec::ArcChainSpec;
        use reth_chainspec::EthereumHardforks;

        let raw = std::fs::read_to_string(genesis_path)
            .wrap_err_with(|| format!("Failed to read genesis file: {genesis_path}"))?;
        let genesis: alloy_genesis::Genesis = serde_json::from_str(&raw)
            .wrap_err_with(|| format!("Failed to parse genesis file: {genesis_path}"))?;
        let chainspec = Arc::new(ArcChainSpec::from(genesis));
        let osaka_active_at_zero = chainspec.is_osaka_active_at_timestamp(0);
        tracing::info!(
            genesis_path,
            osaka_active_at_zero,
            "Osaka activation configured from genesis file"
        );
        self.set_is_osaka_active(Arc::new(move |timestamp| {
            chainspec.is_osaka_active_at_timestamp(timestamp)
        }));
        Ok(())
    }

    /// Configure the Osaka hardfork check from the static chainspec matching
    /// the given chain ID.
    ///
    /// This is the fallback when `--genesis` is not provided.
    pub fn set_osaka_from_chain_id(&self, chain_id: u64) {
        use arc_execution_config::chain_ids::*;
        use arc_execution_config::chainspec::{DEVNET, LOCAL_DEV, TESTNET};
        use reth_chainspec::EthereumHardforks;

        let chainspec = match chain_id {
            LOCALDEV_CHAIN_ID => LOCAL_DEV.clone(),
            DEVNET_CHAIN_ID => DEVNET.clone(),
            TESTNET_CHAIN_ID => TESTNET.clone(),
            _ => {
                tracing::warn!(
                    chain_id,
                    "Unknown chain ID for Osaka activation; defaulting to V4 (Osaka disabled)"
                );
                return;
            }
        };

        tracing::info!(
            chain_id,
            "Osaka activation configured from static chainspec"
        );
        self.set_is_osaka_active(Arc::new(move |timestamp| {
            chainspec.is_osaka_active_at_timestamp(timestamp)
        }));
    }

    /// Returns the duration since the unix epoch.
    pub fn timestamp_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Clock is before UNIX epoch!")
            .as_secs()
    }
}

impl Deref for Engine {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct Inner {
    /// Client for Engine API.
    pub api: Box<dyn EngineAPI>,
    /// Client for Ethereum API.
    pub eth: Box<dyn EthereumAPI>,
    /// Optional function to check if Osaka is active at a given timestamp.
    /// Set after construction via [`Engine::set_is_osaka_active`].
    /// When `None`, defaults to `false` (use V4).
    is_osaka_active: OnceLock<IsOsakaActiveFn>,
}

impl Inner {
    /// Create a new engine with custom API implementations.
    fn new(api: Box<dyn EngineAPI>, eth: Box<dyn EthereumAPI>) -> Self {
        Self {
            api,
            eth,
            is_osaka_active: OnceLock::new(),
        }
    }

    /// Returns whether Osaka is active at the given timestamp.
    fn use_v5(&self, timestamp: u64) -> bool {
        self.is_osaka_active
            .get()
            .map(|f| f(timestamp))
            .unwrap_or(false)
    }

    /// Check if the execution client supports the required methods.
    pub async fn check_capabilities(&self) -> eyre::Result<()> {
        check_capabilities(&self.api).await
    }

    /// Set the latest forkchoice state.
    pub async fn set_latest_forkchoice_state(
        &self,
        head_block_hash: BlockHash,
    ) -> eyre::Result<BlockHash> {
        debug!("🟠 set_latest_forkchoice_state: {:?}", head_block_hash);
        // Make this specific block the canonical chain head.
        //
        // EL Process:
        // - Retrieves pre-validated block from "validated payloads pool"
        // - Promotes the stored state to canonical state
        // - Updates canonical chain head pointer
        // - All other validated blocks for this height remain non-canonical
        let ForkchoiceUpdated {
            payload_status,
            payload_id,
        } = self.api.forkchoice_updated(head_block_hash, None).await?;
        match (payload_status.status, payload_id) {
            (PayloadStatusEnum::Valid, Some(payload_id)) => Err(eyre!(
                "When setting latest forkchoice, payload ID should be None, got: {:?}",
                payload_id
            )),
            (PayloadStatusEnum::Valid, None) => payload_status.latest_valid_hash.ok_or_else(|| {
                eyre!("set_latest_forkchoice_state: Valid status must have latest_valid_hash")
            }),
            (PayloadStatusEnum::Syncing, _) if payload_status.latest_valid_hash.is_none() => {
                // Engine API spec 8: SYNCING with null latestValidHash means the head
                // references an unknown payload. Arc never hits this path because we
                // always call new_payload before forkchoice_updated, so treat it as error.
                Err(eyre!(
                    "set_latest_forkchoice_state: headBlockHash={:?} references an unknown payload or a payload that can't be validated",
                    head_block_hash
                ))
            }
            (status, _) => Err(eyre!(
                "set_latest_forkchoice_state: Invalid payload status: {}",
                status
            )),
        }
    }

    /// Build a new block.
    /// - latest_block: The latest block to generate a new block on top of.
    /// - timestamp: Unix timestamp for when the payload is expected to be executed.
    ///   It should be greater than or equal to that of forkchoiceState.headBlockHash.
    pub async fn generate_block(
        &self,
        latest_block: &ExecutionBlock,
        timestamp: u64,
        suggested_fee_recipient: &Address,
    ) -> eyre::Result<ExecutionPayloadV3> {
        debug!("🟠 Generating block on top of {}", latest_block.block_hash);

        let block_hash = latest_block.block_hash;

        let payload_attributes = PayloadAttributes {
            timestamp,

            // Usually derived from the RANDAO mix (randomness accumulator) of the
            // parent beacon block. The beacon chain generates this value using
            // aggregated validator signatures over time.
            // Its purpose is to expose the consensus layer’s randomness to the EVM.
            // Arc, however, has neither a beacon chain, nor beacon blocks, so we
            // set it to zero to indicate that Arc doesn't use it.
            // NOTE: Smart contracts should therefore not rely on this value for
            // randomness.
            prev_randao: B256::ZERO,

            suggested_fee_recipient: suggested_fee_recipient.to_alloy_address(),

            // Cannot be None in V3.
            withdrawals: Some(vec![]),

            // Cannot be None in V3. Arc has no beacon chain, so we use the
            // execution block hash as parent_beacon_block_root.
            parent_beacon_block_root: Some(block_hash),
        };
        // Build a new block on top of parent_hash
        //
        // EL Process:
        // - Sets working state to parent_hash
        // - Collects transactions from mempool
        // - Executes transactions → computes new state
        // - Stores complete block in "built payloads pool" indexed by payload_id
        //
        // Returns payload_id (ticket to retrieve the built block)
        let ForkchoiceUpdated {
            payload_status,
            payload_id,
        } = self
            .api
            .forkchoice_updated(block_hash, Some(payload_attributes))
            .await
            .wrap_err_with(|| {
                format!("generate_block: forkchoice_updated failed; last_block {block_hash}")
            })?;

        match payload_status.status {
            PayloadStatusEnum::Valid => {
                let Some(payload_id) = payload_id else {
                    return Err(eyre!(
                        "When generating new payload, payload ID must be Some, got: {:?}",
                        payload_id
                    ));
                };
                if payload_status.latest_valid_hash != Some(block_hash) {
                    return Err(eyre!(
                        "When generating new payload, latest_valid_hash must be the parent block hash: {:?}, got: {:?}",
                        block_hash,
                        payload_status.latest_valid_hash
                    ));
                }

                // Complete ExecutionPayloadV3 with all transactions and computed state
                // See https://github.com/ethereum/consensus-specs/blob/v1.1.5/specs/merge/validator.md#block-proposal
                let use_v5 = self.use_v5(timestamp);
                match self.api.get_payload(payload_id, use_v5).await {
                    Ok(payload) => Ok(payload),
                    Err(e) => Err(e).wrap_err_with(|| {
                        format!("generate_block: get_payload failed; payload_id {payload_id}")
                    }),
                }
            }
            PayloadStatusEnum::Syncing if payload_status.latest_valid_hash.is_none() => {
                // Engine API spec 8: SYNCING with null latestValidHash means the head
                // references an unknown payload. Arc never hits this path because we
                // always call new_payload before forkchoice_updated, so treat it as error.
                Err(eyre!(
                    "generate_block: headBlockHash={:?} references an unknown payload or a payload that can't be validated",
                    block_hash
                ))
            }
            status => Err(eyre!("generate_block: Invalid payload status: {}", status)),
        }
    }

    /// Notify that a new block has been created.
    /// - execution_payload: The execution payload
    /// - versioned_hashes: The hashes of the blobs in the execution payload.
    pub async fn notify_new_block(
        &self,
        execution_payload: &ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
    ) -> eyre::Result<PayloadStatus> {
        let parent_block_hash = execution_payload.payload_inner.payload_inner.parent_hash;
        // Validate the block I just received.
        //
        // EL Process:
        // - Sets working state to parent_hash
        // - Executes transactions from execution_payload
        // - Computes state → verifies against execution_payload's state_root
        // - Stores validated block in "validated payloads pool" indexed by block_hash
        // - Returns VALID/INVALID
        //
        // Hopefully skips re-execution and just moves the pre-computed state to "validated payloads pool" on a proposer.
        self.api
            .new_payload(execution_payload, versioned_hashes, parent_block_hash)
            .await
    }
}

#[async_trait]
impl<T> EngineAPI for &T
where
    T: EngineAPI + ?Sized,
{
    async fn exchange_capabilities(&self) -> eyre::Result<EngineCapabilities> {
        (**self).exchange_capabilities().await
    }

    async fn forkchoice_updated(
        &self,
        head_block_hash: BlockHash,
        maybe_payload_attributes: Option<PayloadAttributes>,
    ) -> eyre::Result<ForkchoiceUpdated> {
        (**self)
            .forkchoice_updated(head_block_hash, maybe_payload_attributes)
            .await
    }

    async fn get_payload(
        &self,
        payload_id: AlloyPayloadId,
        use_v5: bool,
    ) -> eyre::Result<ExecutionPayloadV3> {
        (**self).get_payload(payload_id, use_v5).await
    }

    async fn new_payload(
        &self,
        execution_payload: &ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_block_hash: BlockHash,
    ) -> eyre::Result<PayloadStatus> {
        (**self)
            .new_payload(execution_payload, versioned_hashes, parent_block_hash)
            .await
    }
}

#[async_trait]
impl EngineAPI for Box<dyn EngineAPI> {
    async fn exchange_capabilities(&self) -> eyre::Result<EngineCapabilities> {
        (**self).exchange_capabilities().await
    }

    async fn forkchoice_updated(
        &self,
        head_block_hash: BlockHash,
        maybe_payload_attributes: Option<PayloadAttributes>,
    ) -> eyre::Result<ForkchoiceUpdated> {
        (**self)
            .forkchoice_updated(head_block_hash, maybe_payload_attributes)
            .await
    }

    async fn get_payload(
        &self,
        payload_id: AlloyPayloadId,
        use_v5: bool,
    ) -> eyre::Result<ExecutionPayloadV3> {
        (**self).get_payload(payload_id, use_v5).await
    }

    async fn new_payload(
        &self,
        execution_payload: &ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_block_hash: BlockHash,
    ) -> eyre::Result<PayloadStatus> {
        (**self)
            .new_payload(execution_payload, versioned_hashes, parent_block_hash)
            .await
    }
}

#[async_trait]
impl<T> EthereumAPI for &T
where
    T: EthereumAPI + ?Sized,
{
    async fn get_chain_id(&self) -> eyre::Result<String> {
        (**self).get_chain_id().await
    }

    async fn get_genesis_block(&self) -> eyre::Result<ExecutionBlock> {
        (**self).get_genesis_block().await
    }

    async fn get_active_validator_set(&self, block_height: u64) -> eyre::Result<ValidatorSet> {
        (**self).get_active_validator_set(block_height).await
    }

    async fn get_consensus_params(&self, block_height: u64) -> eyre::Result<ConsensusParams> {
        (**self).get_consensus_params(block_height).await
    }

    async fn get_block_by_number(
        &self,
        block_number: &str,
    ) -> eyre::Result<Option<ExecutionBlock>> {
        (**self).get_block_by_number(block_number).await
    }

    async fn get_execution_payloads(
        &self,
        block_numbers: &[String],
    ) -> eyre::Result<Vec<Option<ExecutionPayloadV3>>> {
        (**self).get_execution_payloads(block_numbers).await
    }

    async fn txpool_status(&self) -> eyre::Result<TxpoolStatus> {
        (**self).txpool_status().await
    }

    async fn txpool_inspect(&self) -> eyre::Result<TxpoolInspect> {
        (**self).txpool_inspect().await
    }
}

#[async_trait]
impl EthereumAPI for Box<dyn EthereumAPI> {
    async fn get_chain_id(&self) -> eyre::Result<String> {
        (**self).get_chain_id().await
    }

    async fn get_genesis_block(&self) -> eyre::Result<ExecutionBlock> {
        (**self).get_genesis_block().await
    }

    async fn get_active_validator_set(&self, block_height: u64) -> eyre::Result<ValidatorSet> {
        (**self).get_active_validator_set(block_height).await
    }

    async fn get_consensus_params(&self, block_height: u64) -> eyre::Result<ConsensusParams> {
        (**self).get_consensus_params(block_height).await
    }

    async fn get_block_by_number(
        &self,
        block_number: &str,
    ) -> eyre::Result<Option<ExecutionBlock>> {
        (**self).get_block_by_number(block_number).await
    }

    async fn get_execution_payloads(
        &self,
        block_numbers: &[String],
    ) -> eyre::Result<Vec<Option<ExecutionPayloadV3>>> {
        (**self).get_execution_payloads(block_numbers).await
    }

    async fn txpool_status(&self) -> eyre::Result<TxpoolStatus> {
        (**self).txpool_status().await
    }

    async fn txpool_inspect(&self) -> eyre::Result<TxpoolInspect> {
        (**self).txpool_inspect().await
    }
}

#[cfg(test)]
mod tests {
    use arc_execution_config::chain_ids::*;
    use rstest::rstest;

    use super::*;

    fn mock_engine() -> Engine {
        Engine::new(
            Box::new(MockEngineAPI::new()),
            Box::new(MockEthereumAPI::new()),
        )
    }

    #[test]
    fn test_timestamp_now() {
        let now = Engine::timestamp_now();
        assert!(now > 0);
    }

    #[rstest]
    #[case::osaka_at_zero(
        r#"{"config":{"chainId":1337,"osakaTime":0},"alloc":{}}"#,
        &[(0, true), (9999, true)],
    )]
    #[case::future_osaka(
        r#"{"config":{"chainId":1337,"osakaTime":5000},"alloc":{}}"#,
        &[(0, false), (4999, false), (5000, true), (10000, true)],
    )]
    #[case::no_osaka(
        r#"{"config":{"chainId":1337},"alloc":{}}"#,
        &[(0, false), (u64::MAX, false)],
    )]
    fn test_set_osaka_from_genesis_file(
        #[case] genesis_json: &str,
        #[case] expectations: &[(u64, bool)],
    ) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), genesis_json).unwrap();

        let engine = mock_engine();
        engine
            .set_osaka_from_genesis_file(tmp.path().to_str().unwrap())
            .unwrap();

        for &(timestamp, expected) in expectations {
            assert_eq!(
                engine.use_v5(timestamp),
                expected,
                "use_v5({timestamp}) should be {expected}"
            );
        }
    }

    #[rstest]
    #[case::missing_file(None, "/nonexistent/genesis.json")]
    #[case::invalid_json(Some("not json"), "")]
    fn test_set_osaka_from_genesis_file_errors(
        #[case] file_content: Option<&str>,
        #[case] hardcoded_path: &str,
    ) {
        let tmp;
        let path = match file_content {
            Some(content) => {
                tmp = tempfile::NamedTempFile::new().unwrap();
                std::fs::write(tmp.path(), content).unwrap();
                tmp.path().to_str().unwrap()
            }
            None => hardcoded_path,
        };

        let engine = mock_engine();
        assert!(engine.set_osaka_from_genesis_file(path).is_err());
        assert!(!engine.use_v5(0), "osaka should remain unset after error");
    }

    #[rstest]
    #[case::localdev(LOCALDEV_CHAIN_ID, true)]
    #[case::devnet(DEVNET_CHAIN_ID, false)]
    #[case::testnet(TESTNET_CHAIN_ID, false)]
    #[case::unknown(999999, false)]
    fn test_set_osaka_from_chain_id(#[case] chain_id: u64, #[case] osaka_at_zero: bool) {
        let engine = mock_engine();
        engine.set_osaka_from_chain_id(chain_id);

        assert_eq!(
            engine.use_v5(0),
            osaka_at_zero,
            "use_v5(0) for chain_id {chain_id}"
        );
    }
}
