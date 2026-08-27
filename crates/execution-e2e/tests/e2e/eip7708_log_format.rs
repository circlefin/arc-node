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

//! EIP-7708 log format compliance e2e tests.
//!
//! Verifies the byte-level ERC-20 Transfer log format: emitter address,
//! topic[0] (event signature), topic[1] (from), topic[2] (to), data (value).

use super::helpers::{
    eip7708::{call_tracer_options, SYSTEM_ADDRESS, TRANSFER_EVENT_SIGNATURE},
    utils::send_and_mine,
};
use alloy_primitives::{address, Bytes, U256};
use alloy_rpc_types_eth::TransactionRequest;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

/// Test #37: topic[0] matches ERC-20 Transfer(address,address,uint256) signature.
#[tokio::test]
async fn test_transfer_log_topic0_matches_erc20_signature() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000001111");
    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let expected_topics = vec![
        TRANSFER_EVENT_SIGNATURE,
        sender.into_word(),
        recipient.into_word(),
    ];
    let expected_data = Bytes::from(value.to_be_bytes::<32>().to_vec());
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
    let log = &receipt.logs()[0];
    assert_eq!(log.address(), SYSTEM_ADDRESS);
    assert_eq!(log.topics(), expected_topics.as_slice());
    assert_eq!(log.data().data, expected_data);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #38: topic[1] encodes sender address as left-padded bytes32.
#[tokio::test]
async fn test_transfer_log_topic1_encodes_sender() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000002222");
    let value = U256::from(42);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
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
    let log = &receipt.logs()[0];
    let topics = log.topics();
    assert_eq!(topics.len(), 3);
    assert_eq!(topics[0], TRANSFER_EVENT_SIGNATURE);
    assert_eq!(topics[1], sender.into_word());
    assert_eq!(topics[2], recipient.into_word());
    assert_eq!(
        log.data().data.as_ref(),
        value.to_be_bytes::<32>().as_slice()
    );
    Ok(())
}

/// Test #39: topic[2] encodes recipient address as left-padded bytes32.
#[tokio::test]
async fn test_transfer_log_topic2_encodes_recipient() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000003333");
    let value = U256::from(999);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
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
    let log = &receipt.logs()[0];
    let topics = log.topics();
    assert_eq!(topics.len(), 3);
    assert_eq!(topics[0], TRANSFER_EVENT_SIGNATURE);
    assert_eq!(topics[1], sender.into_word());
    assert_eq!(topics[2], recipient.into_word());
    assert_eq!(
        log.data().data.as_ref(),
        value.to_be_bytes::<32>().as_slice()
    );
    Ok(())
}

/// Test #40: data encodes value as big-endian uint256.
#[tokio::test]
async fn test_transfer_log_data_encodes_value() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000004444");
    // Use a distinctive value to verify encoding
    let value = U256::from(0xDEADBEEFu64);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
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
    let log = &receipt.logs()[0];
    let topics = log.topics();
    assert_eq!(topics.len(), 3);
    assert_eq!(topics[0], TRANSFER_EVENT_SIGNATURE);
    assert_eq!(topics[1], sender.into_word());
    assert_eq!(topics[2], recipient.into_word());
    assert_eq!(
        log.data().data.as_ref(),
        value.to_be_bytes::<32>().as_slice()
    );
    Ok(())
}

/// Test #41: emitter address is SYSTEM_ADDRESS, not the sender or NativeCoinAuthority.
#[tokio::test]
async fn test_transfer_log_emitter_is_system_address() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000005555");
    let value = U256::from(1);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
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
    let log = &receipt.logs()[0];
    assert_eq!(log.address(), SYSTEM_ADDRESS);
    Ok(())
}
