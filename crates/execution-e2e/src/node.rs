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

//! Thin execution e2e node API.

use crate::ArcSetup;
use alloy_eips::eip7685::RequestsOrHash;
use alloy_network::eip2718::Encodable2718;
use alloy_primitives::{address, Address, Bytes, TxHash, B256, U256};
use alloy_rpc_types_engine::{
    CancunPayloadFields, ExecutionData, ExecutionPayload, ExecutionPayloadEnvelopeV4,
    ExecutionPayloadEnvelopeV5, ExecutionPayloadSidecar, ForkchoiceState, ForkchoiceUpdated,
    PayloadAttributes, PayloadId, PayloadStatusEnum, PraguePayloadFields,
};
use alloy_rpc_types_eth::{
    Block as RpcBlock, BlockId, BlockNumberOrTag, Header, Transaction, TransactionReceipt,
    TransactionRequest,
};
use alloy_rpc_types_trace::geth::{GethDebugTracingOptions, GethTrace};
use alloy_serde::JsonStorageKey;
use alloy_signer_local::PrivateKeySigner;
use arc_evm_node::node::ArcNode;
use eyre::WrapErr;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_e2e_test_utils::wallet::Wallet;
use reth_e2e_test_utils::NodeHelperType;
use reth_ethereum::node::EthEngineTypes;
use reth_ethereum_primitives::TransactionSigned;
use reth_node_builder::NodeTypesWithDBAdapter;
use reth_provider::providers::BlockchainProvider;
use reth_rpc_api::{clients::EngineApiClient, DebugApiClient, EthApiClient};

/// JSON-RPC error code for "Unsupported Fork" per the Engine API spec.
const UNSUPPORTED_FORK_CODE: i32 = -38005;

/// Default non-zero fee recipient used by e2e block production.
pub const DEFAULT_FEE_RECIPIENT: Address = address!("0x65E0a200006D4FF91bD59F9694220dafc49dbBC1");

type ArcNodeTestContext = NodeHelperType<
    ArcNode,
    BlockchainProvider<NodeTypesWithDBAdapter<ArcNode, reth_e2e_test_utils::TmpDB>>,
>;

/// Live Arc execution test node.
pub struct ArcTestNode {
    /// In-process Reth node test context.
    pub node: ArcNodeTestContext,
    /// Deterministic localdev wallet set.
    wallet: Wallet,
}

/// `EthApiClient`/`DebugApiClient` with Arc's concrete RPC types fixed, so read helpers
/// can call the client directly without repeating the turbofish generics at every site.
trait ArcEthRpc:
    EthApiClient<TransactionRequest, Transaction, RpcBlock, TransactionReceipt, Header, Bytes>
    + DebugApiClient<TransactionRequest>
{
}

impl<T> ArcEthRpc for T where
    T: EthApiClient<TransactionRequest, Transaction, RpcBlock, TransactionReceipt, Header, Bytes>
        + DebugApiClient<TransactionRequest>
{
}

impl ArcTestNode {
    /// Starts a new single-node test environment from setup.
    pub async fn start(setup: ArcSetup) -> eyre::Result<Self> {
        let (node, wallet) = setup.launch().await?;
        Ok(Self { node, wallet })
    }

    /// Returns a generated localdev signer.
    pub fn wallet_signer(&self, wallet_index: usize) -> eyre::Result<PrivateKeySigner> {
        let wallets = self.wallet.wallet_gen();
        let signer = wallets.get(wallet_index).ok_or_else(|| {
            eyre::eyre!(
                "wallet index {} not available (only {} wallets)",
                wallet_index,
                wallets.len()
            )
        })?;

        Ok(signer.clone())
    }

    /// Prepares a transaction request with Arc e2e defaults.
    pub async fn prepare_tx(
        &self,
        signer: &PrivateKeySigner,
        mut request: TransactionRequest,
    ) -> eyre::Result<TransactionRequest> {
        if request.nonce.is_none() {
            request.nonce = Some(
                self.nonce(
                    signer.address(),
                    Some(BlockId::Number(BlockNumberOrTag::Pending)),
                )
                .await?,
            );
        }
        request.chain_id.get_or_insert(self.wallet.chain_id);
        request.gas.get_or_insert(21_000);

        if request.gas_price.is_none() {
            request.max_fee_per_gas.get_or_insert(1_000_000_000_000u128);
            request
                .max_priority_fee_per_gas
                .get_or_insert(1_000_000_000u128);
        }

        Ok(request)
    }

    /// Prepares and signs a transaction request with the provided signer.
    pub async fn sign_tx(
        &self,
        signer: PrivateKeySigner,
        request: TransactionRequest,
    ) -> eyre::Result<TransactionSigned> {
        let request = self.prepare_tx(&signer, request).await?;
        Ok(TransactionTestContext::sign_tx(signer, request)
            .await
            .into())
    }

    /// Signs a transaction request with the provided signer and submits it through JSON-RPC.
    pub async fn send_tx(
        &self,
        signer: PrivateKeySigner,
        request: TransactionRequest,
    ) -> eyre::Result<TxHash> {
        let tx_signed = self.sign_tx(signer, request).await?;
        self.send_signed_tx(tx_signed).await
    }

    /// Sends an already signed transaction through `eth_sendRawTransaction`.
    pub async fn send_signed_tx(&self, tx_signed: TransactionSigned) -> eyre::Result<TxHash> {
        Ok(self
            .rpc_client()?
            .send_raw_transaction(tx_signed.encoded_2718().into())
            .await?)
    }

    /// Produces one block from the current canonical head and finalizes it.
    /// Mutable access serializes block production because each call advances the canonical head.
    pub async fn produce_block(&mut self) -> eyre::Result<()> {
        let parent = self.get_block(BlockNumberOrTag::Latest).await?;
        let fork_choice_state = forkchoice_state(parent.header.hash);
        let payload_attributes = default_payload_attributes(parent.header.timestamp);
        let fcu_result = self
            .fork_choice_updated(fork_choice_state, Some(payload_attributes))
            .await?;
        assert_valid(
            &fcu_result.payload_status.status,
            "forkChoiceUpdated while building payload",
        )?;
        let payload_id = fcu_result
            .payload_id
            .ok_or_else(|| eyre::eyre!("forkChoiceUpdated did not return a payload ID"))?;
        let payload = self.get_payload(payload_id).await?;
        let block_hash = payload.block_hash();
        let status = self.new_payload(payload).await?;
        assert_valid(&status, "newPayload while producing block")?;

        let fcu_result = self
            .fork_choice_updated(forkchoice_state(block_hash), None)
            .await?;
        assert_valid(
            &fcu_result.payload_status.status,
            "forkChoiceUpdated while finalizing block",
        )?;

        Ok(())
    }

    /// Produces `count` blocks.
    pub async fn produce_blocks(&mut self, count: u64) -> eyre::Result<()> {
        for _ in 0..count {
            self.produce_block().await?;
        }
        Ok(())
    }

    pub async fn fork_choice_updated(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<PayloadAttributes>,
    ) -> eyre::Result<ForkchoiceUpdated> {
        let engine_client = self.node.inner.auth_server_handle().http_client();
        Ok(EngineApiClient::<EthEngineTypes>::fork_choice_updated_v3(
            &engine_client,
            fork_choice_state,
            payload_attributes,
        )
        .await?)
    }

    /// Calls `engine_getPayload` and returns the raw payload plus request sidecar.
    pub async fn get_payload(&self, payload_id: PayloadId) -> eyre::Result<ExecutionData> {
        let engine_client = self.node.inner.auth_server_handle().http_client();
        match EngineApiClient::<EthEngineTypes>::get_payload_v5(&engine_client, payload_id).await {
            Ok(envelope) => Ok(execution_data_from_v5(envelope)),
            Err(err) => {
                if !matches!(
                    &err,
                    jsonrpsee::core::client::Error::Call(obj)
                        if obj.code() == UNSUPPORTED_FORK_CODE
                ) {
                    return Err(eyre::eyre!("getPayloadV5 failed: {err}"));
                }
                let envelope =
                    EngineApiClient::<EthEngineTypes>::get_payload_v4(&engine_client, payload_id)
                        .await?;
                Ok(execution_data_from_v4(envelope))
            }
        }
    }

    /// Calls `engine_newPayloadV4` and returns the raw Engine API status.
    pub async fn new_payload(&self, payload: ExecutionData) -> eyre::Result<PayloadStatusEnum> {
        let engine_client = self.node.inner.auth_server_handle().http_client();
        let ExecutionData { payload, sidecar } = payload;
        let ExecutionPayload::V3(payload) = payload else {
            return Err(eyre::eyre!("newPayloadV4 requires ExecutionPayloadV3"));
        };

        let versioned_hashes = sidecar.versioned_hashes().cloned().unwrap_or_default();
        let parent_beacon_block_root = sidecar.parent_beacon_block_root().unwrap_or(B256::ZERO);
        let execution_requests = sidecar
            .requests()
            .cloned()
            .ok_or_else(|| eyre::eyre!("payload sidecar is missing execution requests"))?;
        let result = EngineApiClient::<EthEngineTypes>::new_payload_v4(
            &engine_client,
            payload,
            versioned_hashes,
            parent_beacon_block_root,
            RequestsOrHash::Requests(execution_requests),
        )
        .await?;

        Ok(result.status)
    }

    /// Returns a block through `eth_getBlockByNumber`.
    pub async fn get_block(&self, block: BlockNumberOrTag) -> eyre::Result<RpcBlock> {
        self.rpc_client()?
            .block_by_number(block, false)
            .await?
            .ok_or_else(|| eyre::eyre!("block {block:?} not found"))
    }

    /// Returns account nonce through `eth_getTransactionCount`.
    pub async fn nonce(&self, address: Address, block_id: Option<BlockId>) -> eyre::Result<u64> {
        let nonce = self
            .rpc_client()?
            .transaction_count(address, block_id)
            .await?;
        u64::try_from(nonce).wrap_err("nonce does not fit in u64")
    }

    /// Returns the raw JSON-RPC transaction receipt.
    pub async fn get_receipt(&self, hash: TxHash) -> eyre::Result<TransactionReceipt> {
        self.rpc_client()?
            .transaction_receipt(hash)
            .await?
            .ok_or_else(|| eyre::eyre!("receipt for transaction {hash} not found"))
    }

    /// Executes `eth_call` and returns raw output bytes.
    pub async fn call(&self, request: TransactionRequest) -> eyre::Result<Bytes> {
        Ok(self.rpc_client()?.call(request, None, None, None).await?)
    }

    /// Returns account balance through `eth_getBalance`.
    pub async fn balance(&self, address: Address, block_id: Option<BlockId>) -> eyre::Result<U256> {
        Ok(self.rpc_client()?.balance(address, block_id).await?)
    }

    /// Returns account storage through `eth_getStorageAt`.
    pub async fn storage_at(
        &self,
        address: Address,
        index: U256,
        block_id: Option<BlockId>,
    ) -> eyre::Result<B256> {
        Ok(self
            .rpc_client()?
            .storage_at(address, JsonStorageKey::from(index), block_id)
            .await?)
    }

    /// Calls `debug_traceTransaction` with caller-provided tracing options.
    pub async fn trace_transaction(
        &self,
        hash: TxHash,
        options: GethDebugTracingOptions,
    ) -> eyre::Result<GethTrace> {
        Ok(self
            .rpc_client()?
            .debug_trace_transaction(hash, Some(options))
            .await?)
    }

    fn rpc_client(&self) -> eyre::Result<impl ArcEthRpc> {
        self.node
            .rpc_client()
            .ok_or_else(|| eyre::eyre!("RPC client not available"))
    }
}

fn execution_data_from_v5(envelope: ExecutionPayloadEnvelopeV5) -> ExecutionData {
    let sidecar = ExecutionPayloadSidecar::v4(
        CancunPayloadFields::new(B256::ZERO, envelope.blobs_bundle.versioned_hashes()),
        PraguePayloadFields::new(envelope.execution_requests),
    );
    ExecutionData::new(ExecutionPayload::V3(envelope.execution_payload), sidecar)
}

fn execution_data_from_v4(envelope: ExecutionPayloadEnvelopeV4) -> ExecutionData {
    let sidecar = ExecutionPayloadSidecar::v4(
        CancunPayloadFields::new(B256::ZERO, envelope.blobs_bundle.versioned_hashes()),
        PraguePayloadFields::new(envelope.execution_requests),
    );
    ExecutionData::new(
        ExecutionPayload::V3(envelope.envelope_inner.execution_payload),
        sidecar,
    )
}

fn forkchoice_state(block_hash: B256) -> ForkchoiceState {
    ForkchoiceState {
        head_block_hash: block_hash,
        safe_block_hash: block_hash,
        finalized_block_hash: block_hash,
    }
}

fn default_payload_attributes(parent_timestamp: u64) -> PayloadAttributes {
    PayloadAttributes {
        timestamp: parent_timestamp + 1,
        prev_randao: B256::random(),
        suggested_fee_recipient: DEFAULT_FEE_RECIPIENT,
        withdrawals: Some(vec![]),
        parent_beacon_block_root: Some(B256::ZERO),
        slot_number: None,
    }
}

fn assert_valid(status: &PayloadStatusEnum, context: &str) -> eyre::Result<()> {
    match status {
        PayloadStatusEnum::Valid => Ok(()),
        other => Err(eyre::eyre!(
            "{context} returned unexpected status: {other:?}"
        )),
    }
}
