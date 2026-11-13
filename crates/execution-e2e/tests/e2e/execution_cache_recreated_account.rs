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

//! The execution cache must hold post-block storage for an account that a block
//! deletes under EIP-161 and then recreates through an EIP-7702 delegation.

use crate::eip161_empty_account_clearing::signed_drain_calldata;
use alloy_eips::eip7702::Authorization;
use alloy_network::ReceiptResponse;
use alloy_primitives::{address, hex, Address, Bytes, B256, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use arc_execution_config::chain_ids::LOCALDEV_CHAIN_ID;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use arc_precompiles::helpers::NATIVE_FIAT_TOKEN_ADDRESS;
use eyre::Result;
use reth_execution_cache::{CachedStatus, ExecutionCache};
use reth_provider::{BlockHashReader, StateProvider, StateProviderFactory};
use std::convert::Infallible;

/// Slot the delegate code writes.
const SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Value the delegate code writes to [`SLOT`].
const WRITTEN: U256 = U256::from_limbs([7, 0, 0, 0]);

/// Receives the drained balance.
const DRAIN_DESTINATION: Address = address!("0x00000000000000000000000000000000ca5e0001");

/// Init code that deploys `PUSH1 7 PUSH1 1 SSTORE STOP`.
const DELEGATE_INIT_CODE: [u8; 15] = hex!("656007600155006000526006601af3");

/// Block N drains an EOA to empty (EIP-161 deletes it), then recreates it with a
/// delegation whose code writes [`SLOT`]. A prewarm read of the slot against the
/// parent state caches zero; applying block N's state must replace it.
#[tokio::test]
async fn recreated_account_storage_replaces_prewarmed_cache_entry() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let funder = node.wallet_signer(0)?;
    let eoa = PrivateKeySigner::random();

    let delegate = funder
        .address()
        .create(node.nonce(funder.address(), None).await?);
    let deploy_tx = node
        .send_tx(
            funder.clone(),
            TransactionRequest {
                from: Some(funder.address()),
                to: Some(TxKind::Create),
                input: TransactionInput::new(Bytes::from_static(&DELEGATE_INIT_CODE)),
                gas: Some(200_000),
                ..Default::default()
            },
        )
        .await?;
    let fund_tx = node
        .send_tx(
            funder.clone(),
            TransactionRequest {
                from: Some(funder.address()),
                to: Some(TxKind::Call(eoa.address())),
                value: Some(U256::from(1_000_000_000_000_000_000u64)),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    assert!(node.get_receipt(deploy_tx).await?.status());
    assert!(node.get_receipt(fund_tx).await?.status());
    assert_eq!(
        node.get_receipt(deploy_tx).await?.contract_address(),
        Some(delegate),
        "delegate was not deployed at the expected address"
    );

    let drain_calldata =
        signed_drain_calldata(&node, &eoa, DRAIN_DESTINATION, U256::from(1_000_000u64)).await?;
    let drain_tx = node
        .send_tx(
            funder.clone(),
            TransactionRequest {
                from: Some(funder.address()),
                to: Some(TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS)),
                input: TransactionInput::new(drain_calldata),
                gas: Some(500_000),
                ..Default::default()
            },
        )
        .await?;
    let authorization = Authorization {
        chain_id: U256::from(LOCALDEV_CHAIN_ID),
        address: delegate,
        nonce: 0,
    };
    let signature = eoa.sign_hash_sync(&authorization.signature_hash())?;
    let recreate_tx = node
        .send_tx(
            funder.clone(),
            TransactionRequest {
                from: Some(funder.address()),
                to: Some(TxKind::Call(eoa.address())),
                authorization_list: Some(vec![authorization.into_signed(signature)]),
                gas: Some(200_000),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let drain_receipt = node.get_receipt(drain_tx).await?;
    let recreate_receipt = node.get_receipt(recreate_tx).await?;
    assert!(drain_receipt.status(), "drain reverted");
    assert!(recreate_receipt.status(), "recreate reverted");
    let block_n = recreate_receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("recreate receipt is missing its block number"))?;
    assert_eq!(
        drain_receipt.block_number,
        Some(block_n),
        "drain and recreate must share a block"
    );
    assert_eq!(
        node.storage_at(eoa.address(), SLOT, None).await?,
        B256::from(WRITTEN)
    );

    let provider = node.node.inner.provider();
    let block_state = provider
        .canonical_in_memory_state()
        .state_by_number(block_n)
        .ok_or_else(|| eyre::eyre!("block {block_n} is not in memory"))?;
    let bundle = &block_state.block_ref().execution_output.state;
    let account = bundle
        .account(&eoa.address())
        .ok_or_else(|| eyre::eyre!("block {block_n} did not change the EOA"))?;
    assert!(account.was_destroyed(), "EIP-161 did not delete the EOA");
    assert!(
        account.info.is_some(),
        "the delegation did not recreate the EOA"
    );
    assert!(
        account
            .original_info
            .as_ref()
            .is_none_or(|info| info.is_empty_code_hash()),
        "the EOA had code before block {block_n}"
    );

    let cache = ExecutionCache::new(1_000_000);
    let parent_hash = provider
        .block_hash(block_n - 1)?
        .ok_or_else(|| eyre::eyre!("block {} is not known", block_n - 1))?;
    let parent = provider.state_by_block_hash(parent_hash)?;
    // Prewarm runs the delegation against the parent state and fills the cache on miss.
    let prewarmed =
        cache.get_or_try_insert_storage_with(eoa.address(), B256::from(SLOT), || {
            parent
                .storage(eoa.address(), B256::from(SLOT))
                .map(Option::unwrap_or_default)
        })?;
    assert!(matches!(prewarmed, CachedStatus::NotCached(v) if v.is_zero()));

    cache
        .insert_state(bundle)
        .map_err(|()| eyre::eyre!("insert_state rejected block {block_n}"))?;

    let cached = cache.get_or_try_insert_storage_with(eoa.address(), B256::from(SLOT), || {
        Ok::<_, Infallible>(U256::MAX)
    })?;
    assert!(
        matches!(cached, CachedStatus::Cached(v) if v == WRITTEN),
        "execution cache holds {cached:?} for the recreated EOA's slot, database holds {WRITTEN}"
    );
    Ok(())
}
