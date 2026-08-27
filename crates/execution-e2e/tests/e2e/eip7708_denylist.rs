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

//! EIP-7708 denylist interaction e2e tests.
//!
//! Verifies that the addresses denylist correctly blocks transactions with value
//! transfers to/from denylisted addresses, and that exclusion lists allow
//! transfers with proper EIP-7708 log emission.

use super::helpers::{
    eip7708::{assert_transfer_log, call_tracer_options},
    utils::send_and_mine,
};
use alloy_primitives::{address, Address, TxKind, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_config::addresses_denylist::{
    AddressesDenylistConfig, DEFAULT_DENYLIST_ERC7201_BASE_SLOT, DENYLIST_ADDRESS_LOCALDEV,
};
use arc_execution_config::chainspec::ArcChainSpec;
use arc_execution_e2e::{chainspec::localdev_with_denylisted_addresses, ArcSetup, ArcTestNode};
use jsonrpsee::core::client::Error as RpcClientError;
use reth_e2e_test_utils::wallet::Wallet;
use std::sync::Arc;

fn denylist_config_enabled(exclusions: Vec<Address>) -> AddressesDenylistConfig {
    AddressesDenylistConfig::new(
        DENYLIST_ADDRESS_LOCALDEV,
        DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
        exclusions,
    )
}

/// Launches a node with denylist config, signs and submits a value transfer tx.
async fn sign_and_submit_value_tx(
    chain_spec: Arc<ArcChainSpec>,
    addresses_denylist_config: AddressesDenylistConfig,
    to: Address,
    value: U256,
) -> Result<(), eyre::Report> {
    let node = ArcTestNode::start(
        ArcSetup::new()
            .with_chain_spec(chain_spec.clone())
            .with_addresses_denylist_config(addresses_denylist_config),
    )
    .await?;
    let signer = node.wallet_signer(0)?;

    node.send_tx(
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            value: Some(value),
            to: Some(TxKind::Call(to)),
            gas: Some(26_000),
            input: TransactionInput::default(),
            ..Default::default()
        },
    )
    .await
    .map(|_| ())
}

fn assert_denylisted_rpc_error(err: &eyre::Report) {
    let rpc_err = err
        .downcast_ref::<RpcClientError>()
        .expect("Expected JSON-RPC client error");
    let RpcClientError::Call(call_err) = rpc_err else {
        panic!("Expected JSON-RPC call error, got: {rpc_err:?}");
    };
    assert!(
        call_err.message().to_lowercase().contains("is denylisted"),
        "Expected denylist rejection in RPC error, got: {call_err:?}"
    );
}

/// Test #27: Value transfer to a denylisted address is rejected through RPC submission.
#[tokio::test]
async fn test_value_transfer_to_denylisted_rejected() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let denylisted_to = address!("0xdead000000000000000000000000000000000001");
    let chain_spec = localdev_with_denylisted_addresses(vec![denylisted_to]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new());

    let err = sign_and_submit_value_tx(
        chain_spec,
        addresses_denylist_config,
        denylisted_to,
        U256::from(1_000_000),
    )
    .await
    .expect_err("Expected RPC submission to reject tx to denylisted address");

    assert_denylisted_rpc_error(&err);

    Ok(())
}

/// Test #28: Value transfer from a denylisted sender is rejected through RPC submission.
#[tokio::test]
async fn test_value_transfer_from_denylisted_rejected() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let sender = Wallet::new(1).wallet_gen()[0].address();
    let chain_spec = localdev_with_denylisted_addresses(vec![sender]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new());

    let err = sign_and_submit_value_tx(
        chain_spec,
        addresses_denylist_config,
        Address::random(),
        U256::from(1_000_000),
    )
    .await
    .expect_err("Expected RPC submission to reject tx from denylisted address");

    assert_denylisted_rpc_error(&err);

    Ok(())
}

/// Test #29: Excluded address can send value transfer and EIP-7708 log is emitted.
///
/// When denylist is enabled but the sender is in the exclusion list,
/// the transfer proceeds and the standard EIP-7708 Transfer log is emitted.
#[tokio::test]
async fn test_denylist_exclusion_allows_transfer_with_log() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000CAFE");
    let value = U256::from(1_000_000);

    let sender = Wallet::new(1).wallet_gen()[0].address();
    let chain_spec = localdev_with_denylisted_addresses(vec![sender]);
    let addresses_denylist_config = denylist_config_enabled(vec![sender]);

    let mut node = ArcTestNode::start(
        ArcSetup::new()
            .with_chain_spec(chain_spec)
            .with_addresses_denylist_config(addresses_denylist_config),
    )
    .await?;
    let signer = node.wallet_signer(0)?;
    assert_eq!(signer.address(), sender);
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(recipient)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, recipient, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}
