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

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

//! E2E tests for the addresses denylist.
//!
//! Covers:
//! - to: denylisted → rejected
//! - from: denylisted → rejected
//! - addresses-exclusions: from denylisted but excluded → accepted

use alloy_primitives::{address, Address, TxKind, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_config::addresses_denylist::{
    AddressesDenylistConfig, DEFAULT_DENYLIST_ERC7201_BASE_SLOT, DENYLIST_ADDRESS_LOCALDEV,
};
use arc_execution_config::chainspec::ArcChainSpec;
use arc_execution_e2e::{chainspec::localdev_with_denylisted_addresses, ArcSetup, ArcTestNode};
use eyre::Result;
use jsonrpsee::core::client::Error as RpcClientError;
use reth_e2e_test_utils::wallet::Wallet;
use std::sync::Arc;

fn assert_denylisted_address_error(err: &eyre::Report, expected_addr: Address) {
    let rpc_err = err
        .downcast_ref::<RpcClientError>()
        .expect("Expected JSON-RPC client error");
    let RpcClientError::Call(call_err) = rpc_err else {
        panic!("Expected JSON-RPC call error, got: {rpc_err:?}");
    };
    let message = call_err.message().to_lowercase();
    assert!(
        message.contains("is denylisted"),
        "Expected denylist rejection in RPC error, got: {call_err:?}"
    );
    assert!(
        message.contains(&expected_addr.to_string().to_lowercase()),
        "Expected denylisted address {expected_addr} in RPC error, got: {call_err:?}"
    );
}

fn denylist_config_enabled(exclusions: Vec<Address>) -> AddressesDenylistConfig {
    AddressesDenylistConfig::new(
        DENYLIST_ADDRESS_LOCALDEV,
        DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
        exclusions,
    )
}

/// Launches a node with the given chain spec and denylist config, then signs and submits
/// a tx from the first wallet to `to`. Returns pool result (Err is PoolError when pool rejects).
async fn sign_and_submit_tx(
    chain_spec: Arc<ArcChainSpec>,
    addresses_denylist_config: AddressesDenylistConfig,
    to: Address,
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
            value: Some(U256::from(1)),
            to: Some(TxKind::Call(to)),
            gas: Some(26_000),
            input: TransactionInput::default(),
            ..Default::default()
        },
    )
    .await
    .map(|_| ())
}

/// Transaction to a denylisted address is rejected.
#[tokio::test]
async fn test_denylisted_to_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let denylisted_to = address!("0xdead000000000000000000000000000000000001");
    let chain_spec = localdev_with_denylisted_addresses(vec![denylisted_to]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new());
    let err = sign_and_submit_tx(chain_spec, addresses_denylist_config, denylisted_to)
        .await
        .expect_err("Expected RPC submission to reject tx to denylisted address");
    assert_denylisted_address_error(&err, denylisted_to);
    Ok(())
}

/// Transaction from a denylisted address is rejected.
#[tokio::test]
async fn test_denylisted_from_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let sender = Wallet::new(1).wallet_gen()[0].address();
    let chain_spec = localdev_with_denylisted_addresses(vec![sender]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new());
    let err = sign_and_submit_tx(chain_spec, addresses_denylist_config, Address::random())
        .await
        .expect_err("Expected RPC submission to reject tx from denylisted address");
    assert_denylisted_address_error(&err, sender);
    Ok(())
}

/// An address in `--arc.denylist.addresses-exclusions` is accepted despite being denylisted.
#[tokio::test]
async fn test_denylist_exclusion_accepts_from_denylisted() -> Result<()> {
    reth_tracing::init_test_tracing();
    let sender = Wallet::new(1).wallet_gen()[0].address();
    let chain_spec = localdev_with_denylisted_addresses(vec![sender]);
    let addresses_denylist_config = denylist_config_enabled(vec![sender]);
    sign_and_submit_tx(chain_spec, addresses_denylist_config, Address::random())
        .await
        .expect("Expected RPC submission to accept tx when sender in addresses-exclusions");
    Ok(())
}
