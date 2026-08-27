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

//! E2E regression for a fee paid to a beneficiary that is created and self-destructed in the
//! same transaction: it must not be silently burned.

use alloy_primitives::{address, Address, Bytes, TxHash, U256};
use alloy_rpc_types_engine::{ExecutionData, PayloadStatusEnum};
use alloy_rpc_types_eth::{BlockId, BlockNumberOrTag, TransactionInput, TransactionRequest};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

use super::helpers::payload::{forkchoice_state, mutate_payload, next_payload_attributes};

/// Initcode that self-destructs during construction: `PUSH1 0x00; SELFDESTRUCT`.
/// Under EIP-6780 the account is created and destroyed within the same tx, so revm leaves the
/// self-destruct marker in place through post-execution (when `reward_beneficiary` runs).
const SELFDESTRUCT_IN_CONSTRUCTOR: [u8; 3] = [0x60, 0x00, 0xff];
const VALID_RECIPIENT: Address = address!("0x1000000000000000000000000000000000000001");

async fn submit_selfdestructing_create(
    node: &ArcTestNode,
    signer_index: usize,
) -> Result<(TxHash, Address)> {
    let signer = node.wallet_signer(signer_index)?;
    let nonce = node
        .nonce(
            signer.address(),
            Some(BlockId::Number(BlockNumberOrTag::Pending)),
        )
        .await?;
    let tx_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Create),
                value: Some(U256::ZERO),
                nonce: Some(nonce),
                gas: Some(200_000),
                input: TransactionInput::new(Bytes::from(SELFDESTRUCT_IN_CONSTRUCTOR.to_vec())),
                ..Default::default()
            },
        )
        .await?;

    Ok((tx_hash, signer.address().create(nonce)))
}

async fn build_payload_with_fee_recipient(
    node: &ArcTestNode,
    fee_recipient: Address,
) -> Result<ExecutionData> {
    let parent = node.get_block(BlockNumberOrTag::Latest).await?;
    let fork_choice_state = forkchoice_state(parent.header.hash);
    let mut payload_attributes = next_payload_attributes(parent.header.timestamp);
    payload_attributes.suggested_fee_recipient = fee_recipient;
    let fcu_result = node
        .fork_choice_updated(fork_choice_state, Some(payload_attributes))
        .await?;
    let payload_id = fcu_result
        .payload_id
        .ok_or_else(|| eyre::eyre!("forkChoiceUpdated did not return a payload ID"))?;

    node.get_payload(payload_id).await
}

fn assert_valid(status: &PayloadStatusEnum, context: &str) -> Result<()> {
    match status {
        PayloadStatusEnum::Valid => Ok(()),
        other => Err(eyre::eyre!(
            "{context} returned unexpected status: {other:?}"
        )),
    }
}

/// Reproduces the deploy-and-self-destruct-as-fee-recipient scenario end to end.
///
/// - A CREATE tx self-destructs in its constructor; its deterministic address becomes the block
///   beneficiary (proposer-selected fee recipient).
/// - Under Zero8 (active at localdev genesis) crediting that self-destructed beneficiary is
///   rejected, so the payload must be INVALID rather than silently burning the fee.
#[tokio::test]
async fn test_proposer_selected_selfdestructed_beneficiary_is_invalid() -> Result<()> {
    reth_tracing::init_test_tracing();

    let node = ArcTestNode::start(ArcSetup::new()).await?;

    // Send a deploy-and-self-destruct tx and derive its deterministic CREATE address.
    let (_deploy_tx_hash, selfdestructed_beneficiary) =
        submit_selfdestructing_create(&node, 0).await?;

    // Build the block with the default (valid) beneficiary so the tx is included.
    let parent = node.get_block(BlockNumberOrTag::Latest).await?;
    let fork_choice_state = forkchoice_state(parent.header.hash);
    let payload_attributes = next_payload_attributes(parent.header.timestamp);
    let fcu_result = node
        .fork_choice_updated(fork_choice_state, Some(payload_attributes))
        .await?;
    let payload_id = fcu_result
        .payload_id
        .ok_or_else(|| eyre::eyre!("forkChoiceUpdated did not return a payload ID"))?;
    let mut payload = node.get_payload(payload_id).await?;
    assert_eq!(
        payload.transaction_count(),
        1,
        "deploy-and-self-destruct tx must be included in the built block"
    );

    // Re-point the proposer-selected fee recipient at the self-destructed contract.
    mutate_payload(&mut payload, |payload| {
        payload.as_v1_mut().fee_recipient = selfdestructed_beneficiary;
    })?;

    let status = node.new_payload(payload).await?;

    assert!(
        matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.to_ascii_lowercase().contains("selfdestructed")
        ),
        "Expected INVALID with self-destructed-balance validation error, got {:?}",
        status
    );

    Ok(())
}

/// The local proposer should treat the deploy-and-self-destruct tx as invalid for this payload
/// attempt, skip it, and keep building the block with other independent transactions.
#[tokio::test]
async fn test_payload_builder_skips_selfdestructed_beneficiary_tx_in_multi_tx_block() -> Result<()>
{
    reth_tracing::init_test_tracing();

    let node = ArcTestNode::start(ArcSetup::new()).await?;

    let (deploy_tx_hash, selfdestructed_beneficiary) =
        submit_selfdestructing_create(&node, 0).await?;
    let valid_a_signer = node.wallet_signer(1)?;
    let valid_a_from = valid_a_signer.address();
    let valid_a_hash = node
        .send_tx(
            valid_a_signer,
            TransactionRequest {
                from: Some(valid_a_from),
                to: Some(TxKind::Call(VALID_RECIPIENT)),
                ..Default::default()
            },
        )
        .await?;
    let valid_b_signer = node.wallet_signer(2)?;
    let valid_b_from = valid_b_signer.address();
    let valid_b_hash = node
        .send_tx(
            valid_b_signer,
            TransactionRequest {
                from: Some(valid_b_from),
                to: Some(TxKind::Call(VALID_RECIPIENT)),
                ..Default::default()
            },
        )
        .await?;

    let payload = build_payload_with_fee_recipient(&node, selfdestructed_beneficiary).await?;
    assert_eq!(
        payload.transaction_count(),
        2,
        "payload builder should skip the selfdestructing tx and keep both independent valid txs"
    );

    let status = node.new_payload(payload.clone()).await?;
    assert_valid(&status, "newPayload")?;

    let fcu_result = node
        .fork_choice_updated(forkchoice_state(payload.block_hash()), None)
        .await?;
    assert_valid(
        &fcu_result.payload_status.status,
        "forkChoiceUpdated while finalizing block",
    )?;

    let block = node.get_block(BlockNumberOrTag::Number(1)).await?;
    assert!(
        !block
            .transactions
            .hashes()
            .any(|tx_hash| tx_hash == deploy_tx_hash),
        "deploy-and-self-destruct tx should not be included"
    );
    assert!(node.get_receipt(deploy_tx_hash).await.is_err());
    assert!(node.get_receipt(valid_a_hash).await?.status());
    assert!(node.get_receipt(valid_b_hash).await?.status());

    Ok(())
}
