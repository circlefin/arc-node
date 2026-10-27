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

//! Launched-node coverage pinning the raw-tx submission param guard to the
//! shapes the RPC handler itself binds.
//!
//! The guard rejecting pre-EIP-155 transactions is the only enforcement point
//! for that policy, and it reads the payload out of the request itself. Its
//! extraction has to accept every shape the handler accepts: a shape the guard
//! misses is forwarded, and if the handler then binds it, the transaction is
//! admitted unchecked. Unit tests can only assert the guard against its own
//! literals, so they cannot see the two drifting apart.
//!
//! Differential test — both halves of the pair run for real here:
//!
//! - Protected transaction in each shape must be accepted. Failure means the
//!   handler no longer binds a shape the guard reads (a renamed named-param
//!   field, for instance), so the guard's view is stale.
//! - Pre-EIP-155 transaction in each shape must be rejected by the guard.
//!   Failure means the handler binds a shape the guard misses.

use alloy_consensus::{SignableTransaction, TxEnvelope, TxLegacy};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{hex, Address, TxKind, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use arc_execution_e2e::{ArcSetup, ArcTestNode};
use eyre::Result;
use jsonrpsee::core::ClientError;

/// Mirrors the guard's rejection, duplicated to keep this test independent of
/// the middleware's private constants.
const UNPROTECTED_TX_ERROR_CODE: i32 = -32000;
const UNPROTECTED_TX_ERROR_MSG: &str =
    "only replay-protected (EIP-155) transactions allowed over RPC";

const SEND_RAW_TX: &str = "eth_sendRawTransaction";
const LOCALDEV_CHAIN_ID: u64 = 1337;

/// Builds a params frame around a raw transaction hex string.
type ShapeBuilder = fn(&str) -> String;

/// Every params shape the handler binds for its single `bytes` argument.
const SHAPES: [(&str, ShapeBuilder); 3] = [
    ("positional", |raw| format!("[\"{raw}\"]")),
    ("padded positional", |raw| format!("[\"{raw}\",0]")),
    ("named", |raw| format!("{{\"bytes\":\"{raw}\"}}")),
];

/// Signs a legacy transaction, replay-protected when `chain_id` is set.
fn signed_legacy_raw(
    signer: &PrivateKeySigner,
    chain_id: Option<u64>,
    nonce: u64,
) -> Result<String> {
    let tx = TxLegacy {
        chain_id,
        nonce,
        gas_price: 1_000_000_000_000,
        gas_limit: 21_000,
        to: TxKind::Call(Address::ZERO),
        value: U256::ZERO,
        input: Default::default(),
    };
    let signature = signer.sign_hash_sync(&tx.signature_hash())?;
    let envelope: TxEnvelope = tx.into_signed(signature).into();
    Ok(format!("0x{}", hex::encode(envelope.encoded_2718())))
}

#[tokio::test]
async fn protected_tx_accepted_in_every_handler_bound_shape() -> Result<()> {
    let node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;

    // A distinct nonce per shape so each submission stands on its own in the pool.
    for (index, (label, build_params)) in SHAPES.into_iter().enumerate() {
        let nonce = u64::try_from(index)?;
        let raw = signed_legacy_raw(&signer, Some(LOCALDEV_CHAIN_ID), nonce)?;
        let response = node
            .raw_rpc_request(SEND_RAW_TX, &build_params(&raw))
            .await?;

        assert!(
            response.is_ok(),
            "{label}: handler must accept a shape the guard reads, got {:?}",
            response.err()
        );
    }

    Ok(())
}

#[tokio::test]
async fn unprotected_tx_rejected_in_every_handler_bound_shape() -> Result<()> {
    let node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let raw = signed_legacy_raw(&signer, None, 0)?;

    for (label, build_params) in SHAPES {
        let response = node
            .raw_rpc_request(SEND_RAW_TX, &build_params(&raw))
            .await?;

        let Err(ClientError::Call(err)) = response else {
            panic!("{label}: pre-EIP-155 tx must be rejected, got {response:?}");
        };
        assert_eq!(
            err.code(),
            UNPROTECTED_TX_ERROR_CODE,
            "{label}: must be the guard's rejection, not the handler's own error"
        );
        assert_eq!(err.message(), UNPROTECTED_TX_ERROR_MSG, "{label}");
    }

    Ok(())
}
