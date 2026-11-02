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

//! Launched-node coverage for the guard that keeps simulation RPCs within
//! `--rpc.gascap` where Reth does not apply the cap itself.
//!
//! A call that omits `gas` and sets a non-zero fee gets the smaller of the
//! caller's allowance and the block gas limit. With a balance override, a
//! block gas-limit override is the only bound left, so the guard must read it
//! wherever a handler applies one. `eth_createAccessList` runs the request's
//! own `gas` uncapped, so the guard reads that too.
//!
//! - A gas limit above the cap, in each shape, must be rejected by the guard.
//!   Failure means a handler runs a gas limit the guard misses.
//! - A low gas limit must lower the gas the call runs with. Failure means the
//!   handler no longer binds the argument the guard reads.
//! - Without an override, the gas stays within the cap.

use alloy_primitives::U256;
use arc_execution_e2e::{ArcSetup, ArcTestNode};
use eyre::Result;
use jsonrpsee::core::ClientError;

/// Mirrors the guard's rejection code, duplicated to keep this test independent
/// of the middleware's private constants.
const RPC_GAS_CAP_REJECTED_ERROR_CODE: i32 = -32602;
/// The production Arc default for `--rpc.gascap`.
const RPC_GAS_CAP: u64 = 30_000_000;
const ABOVE_CAP_HEX: &str = "0x1c9c381";
const AT_CAP_HEX: &str = "0x1c9c380";
/// Block gas-limit override well below the cap, to see it take effect.
const LOW_GAS_LIMIT: u64 = 1_000_000;

const CALLER: &str = "0x00000000000000000000000000000000000ca11e";
const TARGET: &str = "0x0000000000000000000000000000000000000ab1";
/// `GAS PUSH0 MSTORE PUSH1 0x20 PUSH0 RETURN`: returns the gas left at entry.
const GAS_LEFT_RUNTIME: &str = "0x5a5f5260205ff3";

/// A priced call with no `gas`, which takes the allowance-based gas limit.
fn priced_call() -> String {
    format!(r#"{{"from":"{CALLER}","to":"{TARGET}","gasPrice":"0x1"}}"#)
}

/// Funds the caller far past any gas limit and installs the gas-left probe.
fn state_overrides() -> String {
    format!(
        r#"{{"{CALLER}":{{"balance":"0xffffffffffffffffffffffffffffffff"}},"{TARGET}":{{"code":"{GAS_LEFT_RUNTIME}"}}}}"#
    )
}

/// `JUMPDEST PUSH1 0 JUMP` as init code: loops until the gas runs out.
const LOOP_INIT_CODE: &str = "0x5b600056";

/// A call with an explicit `gas` of the given hex.
fn call_with_gas(gas: &str) -> String {
    format!(r#"{{"from":"{CALLER}","to":"{TARGET}","gas":"{gas}"}}"#)
}

/// Builds a params frame whose uncapped gas limit is the given hex.
type ShapeBuilder = fn(&str) -> String;

/// Every handler argument that runs a gas limit Reth does not cap.
const SHAPES: [(&str, &str, ShapeBuilder); 9] = [
    (
        "eth_createAccessList positional",
        "eth_createAccessList",
        |gas| format!(r#"[{},"latest",{}]"#, call_with_gas(gas), state_overrides()),
    ),
    (
        "eth_createAccessList named",
        "eth_createAccessList",
        |gas| {
            format!(
                r#"{{"request":{},"block_number":"latest","state_override":{}}}"#,
                call_with_gas(gas),
                state_overrides()
            )
        },
    ),
    ("eth_call positional", "eth_call", |gas| {
        format!(
            r#"[{},"latest",{},{{"gasLimit":"{gas}"}}]"#,
            priced_call(),
            state_overrides()
        )
    }),
    ("eth_call named", "eth_call", |gas| {
        format!(
            r#"{{"request":{},"block_number":"latest","state_overrides":{},"block_overrides":{{"gasLimit":"{gas}"}}}}"#,
            priced_call(),
            state_overrides()
        )
    }),
    ("eth_call named camelCase", "eth_call", |gas| {
        format!(
            r#"{{"request":{},"blockNumber":"latest","stateOverrides":{},"blockOverrides":{{"gasLimit":"{gas}"}}}}"#,
            priced_call(),
            state_overrides()
        )
    }),
    ("trace_call", "trace_call", |gas| {
        format!(
            r#"[{},["trace"],"latest",{},{{"gasLimit":"{gas}"}}]"#,
            priced_call(),
            state_overrides()
        )
    }),
    ("eth_callMany", "eth_callMany", |gas| {
        format!(
            r#"[[{{"transactions":[{}],"blockOverride":{{"gasLimit":"{gas}"}}}}],{{"blockNumber":"latest"}},{}]"#,
            priced_call(),
            state_overrides()
        )
    }),
    ("debug_traceCall", "debug_traceCall", |gas| {
        format!(
            r#"[{},"latest",{{"stateOverrides":{},"blockOverrides":{{"gasLimit":"{gas}"}}}}]"#,
            priced_call(),
            state_overrides()
        )
    }),
    ("debug_traceCallMany", "debug_traceCallMany", |gas| {
        format!(
            r#"[[{{"transactions":[{}],"blockOverride":{{"gasLimit":"{gas}"}}}}],{{"blockNumber":"latest"}},{{"stateOverrides":{}}}]"#,
            priced_call(),
            state_overrides()
        )
    }),
];

async fn start_node() -> Result<ArcTestNode> {
    ArcTestNode::start(ArcSetup::new().with_rpc_gas_cap(RPC_GAS_CAP)).await
}

/// Decodes the gas-left probe's 32-byte return value.
fn gas_left(result: &serde_json::Value) -> Result<U256> {
    let hex = result
        .as_str()
        .ok_or_else(|| eyre::eyre!("eth_call result is not a string: {result}"))?;
    Ok(U256::from_str_radix(hex.trim_start_matches("0x"), 16)?)
}

#[tokio::test]
async fn gas_limit_above_cap_rejected() -> Result<()> {
    let node = start_node().await?;

    for (label, method, build_params) in SHAPES {
        let response = node
            .raw_rpc_request(method, &build_params(ABOVE_CAP_HEX))
            .await?;
        let Err(ClientError::Call(err)) = response else {
            panic!("{label}: gas limit above the cap must be rejected, got {response:?}");
        };
        assert_eq!(
            err.code(),
            RPC_GAS_CAP_REJECTED_ERROR_CODE,
            "{label}: must be the guard's rejection, not the handler's own error: {err:?}"
        );
        assert!(
            err.message().contains("--rpc.gascap"),
            "{label}: {}",
            err.message()
        );
    }

    Ok(())
}

#[tokio::test]
async fn gas_limit_at_cap_accepted_in_every_shape() -> Result<()> {
    let node = start_node().await?;

    for (label, method, build_params) in SHAPES {
        let response = node
            .raw_rpc_request(method, &build_params(AT_CAP_HEX))
            .await?;
        assert!(
            response.is_ok(),
            "{label}: handler must accept a gas limit at the cap, got {:?}",
            response.err()
        );
    }

    Ok(())
}

#[tokio::test]
async fn eth_call_binds_the_block_gas_override_the_guard_reads() -> Result<()> {
    let node = start_node().await?;
    let low_hex = format!("{LOW_GAS_LIMIT:#x}");

    for (label, method, build_params) in SHAPES.into_iter().filter(|(_, m, _)| *m == "eth_call") {
        let result = node
            .raw_rpc_request(method, &build_params(&low_hex))
            .await?
            .map_err(|err| eyre::eyre!("{label}: {err}"))?;
        let gas = gas_left(&result)?;
        assert!(
            gas < U256::from(LOW_GAS_LIMIT),
            "{label}: the handler must apply the override; the call saw {gas} gas"
        );
    }

    Ok(())
}

#[tokio::test]
async fn eth_create_access_list_binds_the_gas_the_guard_reads() -> Result<()> {
    let node = start_node().await?;
    let low_hex = format!("{LOW_GAS_LIMIT:#x}");
    let looping = format!(r#"{{"from":"{CALLER}","input":"{LOOP_INIT_CODE}","gas":"{low_hex}"}}"#);

    for (label, params) in [
        ("positional", format!(r#"[{looping},"latest"]"#)),
        ("named", format!(r#"{{"request":{looping}}}"#)),
    ] {
        let result = node
            .raw_rpc_request("eth_createAccessList", &params)
            .await?
            .map_err(|err| eyre::eyre!("{label}: {err}"))?;
        assert_eq!(
            result["gasUsed"], low_hex,
            "{label}: the loop must run out of exactly the requested gas: {result}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn priced_eth_call_without_gas_stays_within_the_cap() -> Result<()> {
    let node = start_node().await?;

    let params = format!(r#"[{},"latest",{}]"#, priced_call(), state_overrides());
    let result = node
        .raw_rpc_request("eth_call", &params)
        .await?
        .map_err(|err| eyre::eyre!("{err}"))?;
    let gas = gas_left(&result)?;
    assert!(
        gas < U256::from(RPC_GAS_CAP),
        "a balance override alone must not lift the gas past the cap; the call saw {gas} gas"
    );

    Ok(())
}
