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

//! Shared EIP-7708 test constants and helpers.

use alloy_primitives::{address, Address, B256, U256};
use alloy_rpc_types_eth::TransactionReceipt;
use alloy_rpc_types_trace::geth::{
    GethDebugBuiltInTracerType, GethDebugTracerConfig, GethDebugTracerType, GethDebugTracingOptions,
};
use alloy_sol_types::{sol, SolEvent};

sol! {
    event Transfer(address indexed from, address indexed to, uint256 value);
    event NativeCoinTransferred(address indexed from, address indexed to, uint256 amount);
}

/// ERC-20 Transfer event signature.
pub const TRANSFER_EVENT_SIGNATURE: B256 = Transfer::SIGNATURE_HASH;

/// EIP-7708 system address — emitter of Transfer logs under Zero5.
pub const SYSTEM_ADDRESS: Address = address!("0xfffffffffffffffffffffffffffffffffffffffe");

/// NativeCoinAuthority precompile — emitter of NativeCoinTransferred logs before Zero5.
pub const NATIVE_COIN_AUTHORITY_ADDRESS: Address =
    address!("0x1800000000000000000000000000000000000000");

pub fn call_tracer_options() -> GethDebugTracingOptions {
    GethDebugTracingOptions {
        tracer: Some(GethDebugTracerType::BuiltInTracer(
            GethDebugBuiltInTracerType::CallTracer,
        )),
        tracer_config: GethDebugTracerConfig(
            serde_json::json!({ "withLog": true, "onlyTopCall": false }),
        ),
        ..Default::default()
    }
}

/// Asserts `receipt` carries an EIP-7708 Transfer log at `index` from `from` to `to` for `value`.
pub fn assert_transfer_log(
    receipt: &TransactionReceipt,
    index: usize,
    from: Address,
    to: Address,
    value: U256,
) {
    let log = &receipt.logs()[index];
    assert_eq!(log.address(), SYSTEM_ADDRESS);
    let topics = log.topics();
    assert_eq!(topics.len(), 3);
    assert_eq!(topics[0], TRANSFER_EVENT_SIGNATURE);
    assert_eq!(topics[1], from.into_word());
    assert_eq!(topics[2], to.into_word());
    assert_eq!(
        log.data().data.as_ref(),
        value.to_be_bytes::<32>().as_slice()
    );
}
