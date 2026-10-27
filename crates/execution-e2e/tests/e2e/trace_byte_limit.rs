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

//! Launched-node coverage for `--arc.rpc.max-trace-bytes`: `debug_traceTransaction`
//! fails for a transaction whose calls pass the tracer more input than the cap, and
//! traces a transaction under it.

use std::num::NonZeroUsize;

use alloy_primitives::{address, Address, TxHash, U256};
use alloy_rpc_types_eth::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

use crate::helpers::contracts::repeated_call_contract_deploy_code;
use crate::helpers::eip7708::call_tracer_options;
use crate::helpers::utils::{deploy_and_mine, send_and_mine};

const CALL_TARGET: Address = address!("0x00000000000000000000000000000000000d1e00");
const ARGS_LEN: u16 = 1024;
/// Fits two calls of `ARGS_LEN` bytes, not three.
const MAX_TRACE_BYTES: usize = 3000;

/// Deploys a contract making `calls` calls of `ARGS_LEN` input bytes, mines a tx
/// calling it, and returns that tx's hash.
async fn mine_repeated_calls(
    node: &mut ArcTestNode,
    signer: PrivateKeySigner,
    calls: usize,
) -> Result<TxHash> {
    let (contract, _) = deploy_and_mine(
        node,
        signer.clone(),
        repeated_call_contract_deploy_code(CALL_TARGET, calls, ARGS_LEN),
        U256::ZERO,
        500_000,
    )
    .await?;
    let receipt = send_and_mine(
        node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(contract)),
            gas: Some(500_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status(), "call tx must succeed on chain");
    Ok(receipt.transaction_hash)
}

#[tokio::test]
async fn trace_byte_limit_fails_over_limit_trace() -> Result<()> {
    reth_tracing::init_test_tracing();

    let limit = NonZeroUsize::new(MAX_TRACE_BYTES).expect("limit is non-zero");
    let mut node = ArcTestNode::start(ArcSetup::new().with_max_trace_bytes(limit)).await?;
    let signer = node.wallet_signer(0)?;

    let under = mine_repeated_calls(&mut node, signer.clone(), 2).await?;
    node.trace_transaction(under, call_tracer_options())
        .await
        .expect("under-limit tx should trace");

    let over = mine_repeated_calls(&mut node, signer, 3).await?;
    let err = node
        .trace_transaction(over, call_tracer_options())
        .await
        .expect_err("over-limit tx should fail to trace");
    let expected = format!("trace byte limit of {MAX_TRACE_BYTES} exceeded");
    assert!(
        err.to_string().contains(&expected),
        "expected `{expected}`, got: {err}"
    );
    Ok(())
}
