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

//! E2E tests for ADR-0003: Dynamic Block Gas Limit Configuration Validation.
//!
//! These tests exercise the gas limit validation pipeline end-to-end:
//! stateless bounds checking (consensus.rs) and stateful ProtocolConfig
//! conformance (executor.rs apply_pre_execution_changes).

use alloy_rpc_types_engine::PayloadStatusEnum;
use alloy_rpc_types_eth::BlockNumberOrTag;
use arc_execution_e2e::{
    chainspec::{
        localdev_with_block_gas_limit, localdev_with_protocol_config_reverts,
        BlockGasLimitProvider, LOCAL_DEV,
    },
    ArcSetup, ArcTestNode,
};
use eyre::Result;

use super::helpers::payload::{forkchoice_state, mutate_payload, next_payload_attributes};

/// Helper: build a block with the given gas_limit, then submit via Engine API.
async fn submit_with_gas_limit(setup: ArcSetup, gas_limit: u64) -> Result<PayloadStatusEnum> {
    let node = ArcTestNode::start(setup).await?;
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
    mutate_payload(&mut payload, |payload| {
        payload.as_v1_mut().gas_limit = gas_limit;
    })?;

    node.new_payload(payload).await
}

fn assert_valid(status: &PayloadStatusEnum) {
    assert!(
        matches!(status, PayloadStatusEnum::Valid),
        "Expected VALID, got {:?}",
        status
    );
}

fn assert_invalid_gas_limit(status: &PayloadStatusEnum) {
    assert!(
        matches!(
            status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.contains("block gas limit")
        ),
        "Expected INVALID with gas limit validation error, got {:?}",
        status
    );
}

/// Block built with the correct gas limit from ProtocolConfig is accepted.
#[tokio::test]
async fn test_correct_gas_limit_accepted() -> Result<()> {
    reth_tracing::init_test_tracing();
    let config = LOCAL_DEV.block_gas_limit_config(0);
    let status = submit_with_gas_limit(ArcSetup::new(), config.default()).await?;
    assert_valid(&status);
    Ok(())
}

/// Block with gas limit off-by-one from ProtocolConfig is rejected.
#[tokio::test]
async fn test_gas_limit_off_by_one_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let config = LOCAL_DEV.block_gas_limit_config(0);
    let status = submit_with_gas_limit(ArcSetup::new(), config.default() + 1).await?;
    assert_invalid_gas_limit(&status);
    Ok(())
}

/// When ProtocolConfig reverts, expected_gas_limit falls back to the chainspec
/// default. A block using that default is accepted.
#[tokio::test]
async fn test_protocol_config_reverts_default_gas_limit_accepted() -> Result<()> {
    reth_tracing::init_test_tracing();
    let spec = localdev_with_protocol_config_reverts();
    let config = spec.block_gas_limit_config(0);
    let status =
        submit_with_gas_limit(ArcSetup::new().with_chain_spec(spec), config.default()).await?;
    assert_valid(&status);
    Ok(())
}

/// When ProtocolConfig reverts, a block whose gas limit differs from the
/// chainspec default is rejected.
#[tokio::test]
async fn test_protocol_config_reverts_wrong_gas_limit_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let spec = localdev_with_protocol_config_reverts();
    let config = spec.block_gas_limit_config(0);
    let status =
        submit_with_gas_limit(ArcSetup::new().with_chain_spec(spec), config.default() + 1).await?;
    assert_invalid_gas_limit(&status);
    Ok(())
}

// ---------------------------------------------------------------------------
// ProtocolConfig returns a blockGasLimit below the chainspec minimum
// ---------------------------------------------------------------------------

/// ProtocolConfig returns a value below the minimum. The value is out of bounds,
/// so expected_gas_limit falls back to the default. A block using the default
/// is accepted.
#[tokio::test]
async fn test_protocol_config_below_min_default_gas_limit_accepted() -> Result<()> {
    reth_tracing::init_test_tracing();
    let config = LOCAL_DEV.block_gas_limit_config(0);
    let spec = localdev_with_block_gas_limit(config.min() - 1);
    let status =
        submit_with_gas_limit(ArcSetup::new().with_chain_spec(spec), config.default()).await?;
    assert_valid(&status);
    Ok(())
}

/// ProtocolConfig returns a value below the minimum. A block whose gas limit is
/// off-by-one from the default is rejected.
#[tokio::test]
async fn test_protocol_config_below_min_wrong_gas_limit_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let config = LOCAL_DEV.block_gas_limit_config(0);
    let spec = localdev_with_block_gas_limit(config.min() - 1);
    let status =
        submit_with_gas_limit(ArcSetup::new().with_chain_spec(spec), config.default() + 1).await?;
    assert_invalid_gas_limit(&status);
    Ok(())
}

/// ProtocolConfig returns a value above the maximum. The value is out of
/// bounds, so expected_gas_limit falls back to the default. A block using the
/// default is accepted.
#[tokio::test]
async fn test_protocol_config_above_max_default_gas_limit_accepted() -> Result<()> {
    reth_tracing::init_test_tracing();
    let config = LOCAL_DEV.block_gas_limit_config(0);
    let spec = localdev_with_block_gas_limit(config.max() + 1);
    let status =
        submit_with_gas_limit(ArcSetup::new().with_chain_spec(spec), config.default()).await?;
    assert_valid(&status);
    Ok(())
}

/// ProtocolConfig returns a value above the maximum. A block whose gas limit
/// is off-by-one from the default is rejected.
#[tokio::test]
async fn test_protocol_config_above_max_wrong_gas_limit_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();
    let config = LOCAL_DEV.block_gas_limit_config(0);
    let spec = localdev_with_block_gas_limit(config.max() + 1);
    let status =
        submit_with_gas_limit(ArcSetup::new().with_chain_spec(spec), config.default() - 1).await?;
    assert_invalid_gas_limit(&status);
    Ok(())
}
