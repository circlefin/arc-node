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

//! E2E tests for the addresses denylist.
//!
//! Covers:
//! - to: denylisted → rejected
//! - from: denylisted → rejected
//! - denylist disabled: from denylisted → accepted
//! - addresses-exclusions: from denylisted but excluded → accepted

use alloy_network::eip2718::{Decodable2718, Encodable2718};
use alloy_primitives::{address, Address, TxKind, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_config::addresses_denylist::{
    AddressesDenylistConfig, DEFAULT_DENYLIST_ADDRESS, DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
};
use arc_execution_config::chainspec::ArcChainSpec;
use arc_execution_e2e::{chainspec::localdev_with_denylisted_addresses, ArcEnvironment, ArcSetup};
use arc_execution_txpool::ArcTransactionValidatorError;
use eyre::Result;
use reth_chainspec::EthChainSpec;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_ethereum_primitives::TransactionSigned;
use reth_primitives_traits::SignerRecoverable;
use reth_transaction_pool::error::{PoolError, PoolErrorKind};
use reth_transaction_pool::{TransactionOrigin, TransactionPool};
use std::sync::Arc;

/// First account from test mnemonic (0xf39Fd...), funded in localdev genesis.
const WALLET_FIRST_ADDRESS: Address = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");

fn assert_denylisted_address_error(err: &PoolError, expected_addr: Address) {
    let invalid = match &err.kind {
        PoolErrorKind::InvalidTransaction(e) => e,
        other => panic!("Expected InvalidTransaction (denylist), got: {:?}", other),
    };
    let arc_err = invalid
        .downcast_other_ref::<ArcTransactionValidatorError>()
        .expect("Expected ArcTransactionValidatorError");
    match arc_err {
        ArcTransactionValidatorError::DenylistedAddressError(addr) => {
            assert_eq!(addr, &expected_addr);
        }
        other => panic!("Expected DenylistedAddressError, got: {:?}", other),
    }
}

fn denylist_config_enabled(exclusions: Vec<Address>) -> Result<AddressesDenylistConfig> {
    Ok(AddressesDenylistConfig::try_new(
        true,
        Some(DEFAULT_DENYLIST_ADDRESS),
        Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
        exclusions,
    )?)
}

fn denylist_config_disabled() -> Result<AddressesDenylistConfig> {
    Ok(AddressesDenylistConfig::try_new(
        false,
        None,
        None,
        Vec::new(),
    )?)
}

/// Builds a signed tx from the first test wallet (WALLET_FIRST_ADDRESS) to `to`, returns raw encoded bytes.
async fn build_signed_tx_raw(chain_spec: &ArcChainSpec, to: Address) -> alloy_primitives::Bytes {
    let wallet =
        reth_e2e_test_utils::wallet::Wallet::default().with_chain_id(chain_spec.chain().id());
    let signer = wallet
        .wallet_gen()
        .first()
        .cloned()
        .expect("First wallet from test mnemonic");
    let tx = TransactionRequest {
        nonce: Some(0),
        value: Some(U256::from(1)),
        to: Some(TxKind::Call(to)),
        gas: Some(26000),
        max_fee_per_gas: Some(1000e9 as u128),
        max_priority_fee_per_gas: Some(1e9 as u128),
        chain_id: Some(wallet.chain_id),
        input: TransactionInput::default(),
        ..Default::default()
    };
    let signed_tx = TransactionTestContext::sign_tx(signer, tx).await;
    signed_tx.encoded_2718().into()
}

/// Launches a node with the given chain spec and denylist config, then signs and submits
/// a tx from the first wallet to `to`. Returns pool result (Err is PoolError when pool rejects).
async fn sign_and_submit_tx(
    chain_spec: Arc<ArcChainSpec>,
    addresses_denylist_config: AddressesDenylistConfig,
    to: Address,
) -> Result<(), eyre::Report> {
    let mut env = ArcEnvironment::new();
    ArcSetup::new()
        .with_chain_spec(chain_spec.clone())
        .with_addresses_denylist_config(addresses_denylist_config)
        .apply(&mut env)
        .await?;

    let raw_tx = build_signed_tx_raw(&chain_spec, to).await;
    let tx_signed = TransactionSigned::decode_2718(&mut raw_tx.as_ref()).expect("Decode tx");
    let recovered_tx = tx_signed.try_into_recovered().expect("Recover signer");
    env.node()
        .inner
        .pool
        .add_consensus_transaction(recovered_tx, TransactionOrigin::Local)
        .await
        .map_err(Into::into)
        .map(|_| ())
}

/// Transaction to a denylisted address is rejected.
#[tokio::test]
async fn test_denylisted_to_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let denylisted_to = address!("0xdead000000000000000000000000000000000001");
    let chain_spec = localdev_with_denylisted_addresses(vec![denylisted_to]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new())?;
    let err = sign_and_submit_tx(chain_spec, addresses_denylist_config, denylisted_to)
        .await
        .expect_err("Expected pool to reject tx to denylisted address");
    let pool_err = err.downcast_ref::<PoolError>().expect("Expected PoolError");
    assert_denylisted_address_error(pool_err, denylisted_to);
    Ok(())
}

/// Transaction from a denylisted address is rejected.
#[tokio::test]
async fn test_denylisted_from_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let chain_spec = localdev_with_denylisted_addresses(vec![WALLET_FIRST_ADDRESS]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new())?;
    let err = sign_and_submit_tx(chain_spec, addresses_denylist_config, Address::random())
        .await
        .expect_err("Expected pool to reject tx from denylisted address");
    let pool_err = err.downcast_ref::<PoolError>().expect("Expected PoolError");
    assert_denylisted_address_error(pool_err, WALLET_FIRST_ADDRESS);
    Ok(())
}

/// When denylist is disabled (--arc.denylist.enabled=false), tx from denylisted address is accepted.
#[tokio::test]
async fn test_denylist_disabled_accepts_from_denylisted() -> Result<()> {
    reth_tracing::init_test_tracing();
    let chain_spec = localdev_with_denylisted_addresses(vec![WALLET_FIRST_ADDRESS]);
    let addresses_denylist_config = denylist_config_disabled()?;
    sign_and_submit_tx(chain_spec, addresses_denylist_config, Address::random())
        .await
        .expect("Expected pool to accept tx when denylist disabled");
    Ok(())
}

/// When denylist is enabled --arc.denylist.enabled=true, but address is in --arc.denylist.addresses-exclusions, tx from that denylisted address is accepted.
#[tokio::test]
async fn test_denylist_exclusion_accepts_from_denylisted() -> Result<()> {
    reth_tracing::init_test_tracing();
    let chain_spec = localdev_with_denylisted_addresses(vec![WALLET_FIRST_ADDRESS]);
    let addresses_denylist_config = denylist_config_enabled(vec![WALLET_FIRST_ADDRESS])?;
    sign_and_submit_tx(chain_spec, addresses_denylist_config, Address::random())
        .await
        .expect("Expected pool to accept tx when sender in addresses-exclusions");
    Ok(())
}
