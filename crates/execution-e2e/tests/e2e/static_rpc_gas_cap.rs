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

//! Launched-node coverage for the static `--rpc.gascap=30_000_000` default
//! installed by `crates/node/src/args.rs`.
//!
//! Differential test:
//!
//! - Chainspec block gas limit = 50M (above the static cap, so the cap is the
//!   binding constraint).
//! - Node configured with `rpc_gas_cap = 30M` (the production Arc default).
//! - Deploys a counted-loop gas burner sized to consume ~35M gas.
//! - Issues `eth_call(gas = 40M)` against the burner.
//!
//! Direction-of-effect:
//! - With the cap at 30M, Reth clamps the request to 30M; the burner needs
//!   ~35M and the call returns out-of-gas. Test passes.
//! - Without the cap (e.g. Reth's stock 50M default), the 40M request is not
//!   clamped; the burner runs to completion and the call returns Ok. Test
//!   fails — that is the registration-direction signal.

use alloy_primitives::{Bytes, TxKind};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_config::chainspec::localdev_with_block_gas_limit;
use arc_execution_e2e::{ArcSetup, ArcTestNode};
use eyre::Result;

/// Block gas limit for the test. Set above the static cap so the cap is the
/// binding constraint on `eth_call`.
const TEST_BLOCK_GAS_LIMIT: u64 = 50_000_000;
/// The production Arc default for `--rpc.gascap`.
const STATIC_GAS_CAP: u64 = 30_000_000;
/// Gas the differential check passes to `eth_call`. Above `STATIC_GAS_CAP`
/// (so the cap must clamp) and below `TEST_BLOCK_GAS_LIMIT` (so without the
/// cap the call would run to completion).
const REQUESTED_CALL_GAS: u64 = 40_000_000;

/// Hand-crafted runtime that loops `KECCAK256(empty)` 448_000 times then
/// `STOP`s, consuming ~35M gas. Sized to OOG when clamped to 30M and to
/// complete when run with 40M.
///
/// Init code (12 bytes):
/// ```text
/// 60 19  PUSH1 25            ; runtime size
/// 60 0c  PUSH1 12            ; runtime offset within init code
/// 60 00  PUSH1 0             ; mem dest
/// 39     CODECOPY
/// 60 19  PUSH1 25
/// 60 00  PUSH1 0
/// f3     RETURN
/// ```
///
/// Runtime (25 bytes):
/// ```text
/// 62 06 d4 00  PUSH3 0x06d400   ; counter = 448_000 (pc=0)
/// 5b           JUMPDEST          ; loop start (pc=4)
/// 80           DUP1              ; copy counter
/// 15           ISZERO
/// 60 17        PUSH1 23          ; end pc
/// 57           JUMPI             ; if counter == 0, jump to STOP
/// 60 00        PUSH1 0           ; size
/// 60 00        PUSH1 0           ; offset
/// 20           KECCAK256         ; hash empty mem (~30 gas)
/// 50           POP
/// 60 01        PUSH1 1
/// 90           SWAP1
/// 03           SUB               ; counter -= 1
/// 60 04        PUSH1 4
/// 56           JUMP              ; back to loop start
/// 5b           JUMPDEST          ; end (pc=23)
/// 00           STOP
/// ```
const GAS_BURNER_BYTECODE_HEX: &str =
    "6019600c60003960196000f36206d4005b8015601757600060002050600190036004565b00";

fn gas_burner_bytecode() -> Bytes {
    alloy_primitives::hex::decode(GAS_BURNER_BYTECODE_HEX)
        .expect("GAS_BURNER_BYTECODE_HEX is valid hex")
        .into()
}

/// One launched-node test: with `--rpc.gascap=30M` and `block.gas_limit=50M`,
/// `eth_call(gas=40M)` against a ~35M gas burner returns out-of-gas. The
/// cap is the binding constraint; removing it (Reth's stock 50M default)
/// would let the call complete and fail this test.
#[tokio::test]
async fn static_rpc_gas_cap_clamps_eth_call() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(
        ArcSetup::new()
            .with_chain_spec(localdev_with_block_gas_limit(TEST_BLOCK_GAS_LIMIT))
            .with_rpc_gas_cap(STATIC_GAS_CAP),
    )
    .await?;
    let signer = node.wallet_signer(0)?;
    let deploy_tx = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Create),
                gas: Some(200_000),
                input: TransactionInput::new(gas_burner_bytecode()),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let deploy_receipt = node.get_receipt(deploy_tx).await?;
    assert!(deploy_receipt.status());
    let burner = deploy_receipt
        .contract_address
        .expect("gas burner deploy receipt must include contract address");

    let request = TransactionRequest {
        to: Some(TxKind::Call(burner)),
        gas: Some(REQUESTED_CALL_GAS),
        input: TransactionInput::default(),
        ..Default::default()
    };

    let result = node.call(request).await;

    match result {
        Ok(output) => Err(eyre::eyre!(
            "eth_call against gas burner with gas={REQUESTED_CALL_GAS} succeeded \
             (output: {output}); Reth did not clamp to --rpc.gascap. \
             Is the Arc default in place?"
        )),
        Err(err) => {
            // Match revm's `OutOfGas(...)` family. Other gas-related errors
            // should surface rather than silently passing this differential test.
            let msg = err.to_string().to_lowercase();
            if msg.contains("out of gas") || msg.contains("outofgas") {
                Ok(())
            } else {
                Err(eyre::eyre!(
                    "eth_call failed with non-OOG error (expected out-of-gas): {err}"
                ))
            }
        }
    }
}
