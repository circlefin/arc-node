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

//! Launched-node coverage for `debug_traceTransaction` on CallFrom subcalls whose
//! completion fails after the child succeeded.
//!
//! `Multicall3From.aggregate3` (with `allowFailure`) calls a helper through CallFrom. The
//! helper sends value, burns gas, and returns `size` bytes. CallFrom must ABI-encode that
//! returndata from the gas it retained; when it cannot, the subcall fails with OOG and the
//! helper's transfer is rolled back. The call trace must then report the transparent
//! helper node as failed.

use alloy_primitives::{address, Address, Bytes, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_rpc_types_trace::geth::GethTrace;
use alloy_sol_types::{sol, SolCall};
use arc_execution_config::call_from::MULTICALL3_FROM_ADDRESS;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;
use rstest::rstest;

use crate::helpers::contracts::transfer_burn_gas_return_contract_deploy_code;
use crate::helpers::eip7708::{assert_transfer_log, call_tracer_options};
use crate::helpers::utils::{deploy_and_mine, send_and_mine};

sol! {
    interface IMulticall3From {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }

        struct Result {
            bool success;
            bytes returnData;
        }

        function aggregate3(Call3[] calldata calls) external returns (Result[] memory returnData);
    }
}

const RECIPIENT: Address = address!("0x00000000000000000000000000000000c0ffee01");
/// 0.01 USDC in native 18-decimal units.
const AMOUNT: u64 = 10_000_000_000_000_000;
const TX_GAS_LIMIT: u64 = 400_000;

/// The helper transfers and returns `size` bytes. 32 bytes are cheap to encode; 128,000
/// bytes cost ~12k encode gas, more than CallFrom retains at `TX_GAS_LIMIT`.
#[rstest]
#[case::completion_fits(32, None)]
#[case::completion_oog(128_000, Some("out of gas"))]
#[tokio::test]
async fn call_from_trace_reports_final_subcall_result(
    #[case] size: u64,
    #[case] expected_error: Option<&str>,
) -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();

    let (helper, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        transfer_burn_gas_return_contract_deploy_code(RECIPIENT, AMOUNT),
        U256::from(AMOUNT),
        500_000,
    )
    .await?;

    let calldata = IMulticall3From::aggregate3Call {
        calls: vec![IMulticall3From::Call3 {
            target: helper,
            allowFailure: true,
            callData: Bytes::from(U256::from(size).to_be_bytes::<32>().to_vec()),
        }],
    }
    .abi_encode();
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(MULTICALL3_FROM_ADDRESS)),
            gas: Some(TX_GAS_LIMIT),
            input: TransactionInput::new(calldata.into()),
            ..Default::default()
        },
    )
    .await?;

    // Ground truth: the batch succeeds either way, the transfer only when completion fits.
    let completion_oog = expected_error.is_some();
    assert!(
        receipt.status(),
        "aggregate3 with allowFailure must succeed"
    );
    let expected_balance = if completion_oog {
        U256::ZERO
    } else {
        U256::from(AMOUNT)
    };
    assert_eq!(node.balance(RECIPIENT, None).await?, expected_balance);
    if completion_oog {
        assert!(receipt.logs().is_empty(), "reverted transfer must not log");
    } else {
        assert_eq!(receipt.logs().len(), 1);
        assert_transfer_log(&receipt, 0, helper, RECIPIENT, U256::from(AMOUNT));
    }

    let trace = node
        .trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    let GethTrace::CallTracer(top) = trace else {
        panic!("expected call tracer output, got {trace:?}");
    };
    assert_eq!(top.to, Some(MULTICALL3_FROM_ADDRESS));
    assert_eq!(top.error, None);
    let output = top.output.as_ref().expect("aggregate3 returns its results");
    let results = IMulticall3From::aggregate3Call::abi_decode_returns(output)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].success, !completion_oog);

    // CallFrom is transparent: the batch's only call is sender → helper.
    assert_eq!(top.calls.len(), 1, "expected only the transparent subcall");
    let subcall = &top.calls[0];
    assert_eq!(subcall.from, sender, "trace should keep the spoofed sender");
    assert_eq!(subcall.to, Some(helper));
    assert_eq!(subcall.error.as_deref(), expected_error);

    assert_eq!(subcall.calls.len(), 1, "helper makes one value transfer");
    let transfer = &subcall.calls[0];
    assert_eq!(transfer.from, helper);
    assert_eq!(transfer.to, Some(RECIPIENT));
    assert_eq!(transfer.value, Some(U256::from(AMOUNT)));
    Ok(())
}
