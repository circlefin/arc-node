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

mod helpers;

use alloy_network::eip2718::{Decodable2718, Encodable2718};
use alloy_primitives::{address, Address, TxKind, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_config::addresses_denylist::{
    AddressesDenylistConfig, DEFAULT_DENYLIST_ADDRESS, DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
};
use arc_execution_config::chainspec::ArcChainSpec;
use arc_execution_e2e::{
    actions::{
        AssertTxIncluded, AssertTxLogs, AssertTxTrace, ProduceBlocks, SendTransaction, TxStatus,
    },
    chainspec::localdev_with_denylisted_addresses,
    ArcEnvironment, ArcSetup, ArcTestBuilder,
};
use arc_execution_txpool::ArcTransactionValidatorError;
use helpers::constants::{SYSTEM_ADDRESS, WALLET_FIRST_ADDRESS};
use reth_chainspec::EthChainSpec;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_ethereum_primitives::TransactionSigned;
use reth_primitives_traits::SignerRecoverable;
use reth_transaction_pool::error::{PoolError, PoolErrorKind};
use reth_transaction_pool::{TransactionOrigin, TransactionPool};
use std::sync::Arc;

fn denylist_config_enabled(exclusions: Vec<Address>) -> eyre::Result<AddressesDenylistConfig> {
    Ok(AddressesDenylistConfig::try_new(
        true,
        Some(DEFAULT_DENYLIST_ADDRESS),
        Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
        exclusions,
    )?)
}

/// Builds a signed tx from a wallet to `to` with a given value.
///
/// Uses `wallet.inner` directly instead of regenerating signers via `wallet_gen()`.
async fn build_signed_tx_raw(
    wallet: &reth_e2e_test_utils::wallet::Wallet,
    to: Address,
    value: U256,
) -> alloy_primitives::Bytes {
    let tx = TransactionRequest {
        nonce: Some(0),
        value: Some(value),
        to: Some(TxKind::Call(to)),
        gas: Some(26000),
        max_fee_per_gas: Some(1_000_000_000_000),
        max_priority_fee_per_gas: Some(1_000_000_000),
        chain_id: Some(wallet.chain_id),
        input: TransactionInput::default(),
        ..Default::default()
    };
    let signed_tx = TransactionTestContext::sign_tx(wallet.inner.clone(), tx).await;
    signed_tx.encoded_2718().into()
}

/// Launches a node with denylist config, signs and submits a value transfer tx.
async fn sign_and_submit_value_tx(
    chain_spec: Arc<ArcChainSpec>,
    addresses_denylist_config: AddressesDenylistConfig,
    to: Address,
    value: U256,
) -> Result<(), eyre::Report> {
    let mut env = ArcEnvironment::new();
    ArcSetup::new()
        .with_chain_spec(chain_spec.clone())
        .with_addresses_denylist_config(addresses_denylist_config)
        .apply(&mut env)
        .await?;

    let wallet =
        reth_e2e_test_utils::wallet::Wallet::default().with_chain_id(chain_spec.chain().id());

    let raw_tx = build_signed_tx_raw(&wallet, to, value).await;
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

/// Test #27: Value transfer to a denylisted address is rejected by the txpool.
#[tokio::test]
async fn test_value_transfer_to_denylisted_rejected() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let denylisted_to = address!("0xdead000000000000000000000000000000000001");
    let chain_spec = localdev_with_denylisted_addresses(vec![denylisted_to]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new())?;

    let err = sign_and_submit_value_tx(
        chain_spec,
        addresses_denylist_config,
        denylisted_to,
        U256::from(1_000_000),
    )
    .await
    .expect_err("Expected pool to reject tx to denylisted address");

    let pool_err = err.downcast_ref::<PoolError>().expect("Expected PoolError");
    let invalid = match &pool_err.kind {
        PoolErrorKind::InvalidTransaction(e) => e,
        other => panic!("Expected InvalidTransaction (denylist), got: {:?}", other),
    };
    let arc_err = invalid
        .downcast_other_ref::<ArcTransactionValidatorError>()
        .expect("Expected ArcTransactionValidatorError");
    assert!(
        matches!(
            arc_err,
            ArcTransactionValidatorError::DenylistedAddressError(_)
        ),
        "Expected DenylistedAddressError, got: {:?}",
        arc_err
    );

    Ok(())
}

/// Test #28: Value transfer from a denylisted sender is rejected by the txpool.
#[tokio::test]
async fn test_value_transfer_from_denylisted_rejected() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_denylisted_addresses(vec![WALLET_FIRST_ADDRESS]);
    let addresses_denylist_config = denylist_config_enabled(Vec::new())?;

    let err = sign_and_submit_value_tx(
        chain_spec,
        addresses_denylist_config,
        Address::random(),
        U256::from(1_000_000),
    )
    .await
    .expect_err("Expected pool to reject tx from denylisted address");

    let pool_err = err.downcast_ref::<PoolError>().expect("Expected PoolError");
    let invalid = match &pool_err.kind {
        PoolErrorKind::InvalidTransaction(e) => e,
        other => panic!("Expected InvalidTransaction (denylist), got: {:?}", other),
    };
    let arc_err = invalid
        .downcast_other_ref::<ArcTransactionValidatorError>()
        .expect("Expected ArcTransactionValidatorError");
    assert!(
        matches!(
            arc_err,
            ArcTransactionValidatorError::DenylistedAddressError(_)
        ),
        "Expected DenylistedAddressError, got: {:?}",
        arc_err
    );

    Ok(())
}

/// Test #29: Excluded address can send value transfer and EIP-7708 log is emitted.
///
/// When denylist is enabled but the sender is in the exclusion list,
/// the transfer proceeds and the standard EIP-7708 Transfer log is emitted.
#[tokio::test]
async fn test_denylist_exclusion_allows_transfer_with_log() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000CAFE");
    let value = U256::from(1_000_000);

    // The sender (WALLET_FIRST_ADDRESS) is denylisted but also in the exclusion list
    let chain_spec = localdev_with_denylisted_addresses(vec![WALLET_FIRST_ADDRESS]);
    let addresses_denylist_config =
        denylist_config_enabled(vec![WALLET_FIRST_ADDRESS]).expect("denylist config");

    ArcTestBuilder::new()
        .with_setup(
            ArcSetup::new()
                .with_chain_spec(chain_spec)
                .with_addresses_denylist_config(addresses_denylist_config),
        )
        .with_action(
            SendTransaction::new("transfer")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("transfer")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_denylist_exclusion_allows_transfer_with_log failed");
}
