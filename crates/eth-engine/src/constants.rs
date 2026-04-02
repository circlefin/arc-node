// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

use std::time::Duration;

use alloy_primitives::{address, Address as AlloyAddress};

// Engine API method names
pub const ENGINE_NEW_PAYLOAD_V1: &str = "engine_newPayloadV1";
pub const ENGINE_NEW_PAYLOAD_V2: &str = "engine_newPayloadV2";
pub const ENGINE_NEW_PAYLOAD_V3: &str = "engine_newPayloadV3";
pub const ENGINE_NEW_PAYLOAD_V4: &str = "engine_newPayloadV4";

pub const ENGINE_GET_PAYLOAD_V1: &str = "engine_getPayloadV1";
pub const ENGINE_GET_PAYLOAD_V2: &str = "engine_getPayloadV2";
pub const ENGINE_GET_PAYLOAD_V3: &str = "engine_getPayloadV3";
pub const ENGINE_GET_PAYLOAD_V4: &str = "engine_getPayloadV4";
pub const ENGINE_GET_PAYLOAD_V5: &str = "engine_getPayloadV5";

pub const ENGINE_FORKCHOICE_UPDATED_V1: &str = "engine_forkchoiceUpdatedV1";
pub const ENGINE_FORKCHOICE_UPDATED_V2: &str = "engine_forkchoiceUpdatedV2";
pub const ENGINE_FORKCHOICE_UPDATED_V3: &str = "engine_forkchoiceUpdatedV3";

pub const ENGINE_GET_PAYLOAD_BODIES_BY_HASH_V1: &str = "engine_getPayloadBodiesByHashV1";
pub const ENGINE_GET_PAYLOAD_BODIES_BY_RANGE_V1: &str = "engine_getPayloadBodiesByRangeV1";

pub const ENGINE_EXCHANGE_CAPABILITIES: &str = "engine_exchangeCapabilities";

pub const ENGINE_GET_CLIENT_VERSION_V1: &str = "engine_getClientVersionV1";

pub const ENGINE_GET_BLOBS_V1: &str = "engine_getBlobsV1";

// Engine API timeouts
pub const ENGINE_NEW_PAYLOAD_TIMEOUT: Duration = Duration::from_secs(8);
pub const ENGINE_GET_PAYLOAD_TIMEOUT: Duration = Duration::from_secs(2);
pub const ENGINE_FORKCHOICE_UPDATED_TIMEOUT: Duration = Duration::from_secs(8);
// pub const ENGINE_GET_PAYLOAD_BODIES_TIMEOUT: Duration = Duration::from_secs(10);
pub const ENGINE_EXCHANGE_CAPABILITIES_TIMEOUT: Duration = Duration::from_secs(1);
// pub const ENGINE_GET_CLIENT_VERSION_TIMEOUT: Duration = Duration::from_secs(1);
// pub const ENGINE_GET_BLOBS_TIMEOUT: Duration = Duration::from_secs(1);

// Ethereum API timeouts
pub const ETH_DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);
pub const ETH_BATCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// IPC client-level timeout (upper-bound safety net set at jsonrpsee Client construction)
pub const IPC_CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

// Retry policy for eth_call requests (validator set, consensus params).
pub const ETH_CALL_RETRY: backon::FibonacciBuilder = backon::FibonacciBuilder::new()
    .with_max_times(5)
    .with_min_delay(Duration::from_millis(100))
    .with_max_delay(Duration::from_secs(1));

pub const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(3);

// Engine API retries for IPC -- No need to keep trying forever (already done when connecting to IPC socket).
pub const ENGINE_EXCHANGE_CAPABILITIES_RETRY_IPC: backon::FibonacciBuilder =
    backon::FibonacciBuilder::new()
        .with_min_delay(Duration::from_millis(100))
        .with_max_delay(Duration::from_secs(1))
        .with_max_times(30);

// Engine API retries for RPC -- First call to `reth`, keep retrying indefinitely.
pub const ENGINE_EXCHANGE_CAPABILITIES_RETRY_RPC: backon::ConstantBuilder =
    backon::ConstantBuilder::new()
        .with_delay(INITIAL_RETRY_DELAY)
        .without_max_times();

// Engine API methods supported by this implementation
pub static NODE_CAPABILITIES: &[&str] = &[
    // ENGINE_NEW_PAYLOAD_V1,
    // ENGINE_NEW_PAYLOAD_V2,
    // ENGINE_NEW_PAYLOAD_V3,
    ENGINE_NEW_PAYLOAD_V4,
    // ENGINE_GET_PAYLOAD_V1,
    // ENGINE_GET_PAYLOAD_V2,
    // ENGINE_GET_PAYLOAD_V3,
    ENGINE_GET_PAYLOAD_V4,
    ENGINE_GET_PAYLOAD_V5,
    // ENGINE_FORKCHOICE_UPDATED_V1,
    // ENGINE_FORKCHOICE_UPDATED_V2,
    ENGINE_FORKCHOICE_UPDATED_V3,
    // ENGINE_GET_PAYLOAD_BODIES_BY_HASH_V1,
    // ENGINE_GET_PAYLOAD_BODIES_BY_RANGE_V1,
    // ENGINE_GET_CLIENT_VERSION_V1,
    // ENGINE_GET_BLOBS_V1,
];

/// ProtocolConfig, ValidatorRegistry and PermissionedValidatorManager are
/// AdminUpgradeableProxy contracts. It means that their addresses will always
/// stay the same. The underlying contract addresses might change, but the
/// client code doesn't need to know about that.
///
/// see scripts/genesis/addresses.ts
pub(crate) const VALIDATOR_REGISTRY_ADDRESS: AlloyAddress =
    address!("0x3600000000000000000000000000000000000002");

pub(crate) const PROTOCOL_CONFIG_ADDRESS: AlloyAddress =
    address!("0x3600000000000000000000000000000000000001");
