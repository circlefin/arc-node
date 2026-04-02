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

/// Engine capabilities
#[derive(Clone, Copy, Debug)]
pub struct EngineCapabilities {
    pub new_payload_v1: bool,
    pub new_payload_v2: bool,
    pub new_payload_v3: bool,
    pub new_payload_v4: bool,
    pub forkchoice_updated_v1: bool,
    pub forkchoice_updated_v2: bool,
    pub forkchoice_updated_v3: bool,
    pub get_payload_bodies_by_hash_v1: bool,
    pub get_payload_bodies_by_range_v1: bool,
    pub get_payload_v1: bool,
    pub get_payload_v2: bool,
    pub get_payload_v3: bool,
    pub get_payload_v4: bool,
    pub get_payload_v5: bool,
    pub get_client_version_v1: bool,
    pub get_blobs_v1: bool,
}

impl EngineCapabilities {
    /// Create EngineCapabilities from a set of capability strings
    pub fn from_capabilities(capabilities: &std::collections::HashSet<String>) -> Self {
        use crate::constants::*;

        Self {
            new_payload_v1: capabilities.contains(ENGINE_NEW_PAYLOAD_V1),
            new_payload_v2: capabilities.contains(ENGINE_NEW_PAYLOAD_V2),
            new_payload_v3: capabilities.contains(ENGINE_NEW_PAYLOAD_V3),
            new_payload_v4: capabilities.contains(ENGINE_NEW_PAYLOAD_V4),
            forkchoice_updated_v1: capabilities.contains(ENGINE_FORKCHOICE_UPDATED_V1),
            forkchoice_updated_v2: capabilities.contains(ENGINE_FORKCHOICE_UPDATED_V2),
            forkchoice_updated_v3: capabilities.contains(ENGINE_FORKCHOICE_UPDATED_V3),
            get_payload_bodies_by_hash_v1: capabilities
                .contains(ENGINE_GET_PAYLOAD_BODIES_BY_HASH_V1),
            get_payload_bodies_by_range_v1: capabilities
                .contains(ENGINE_GET_PAYLOAD_BODIES_BY_RANGE_V1),
            get_payload_v1: capabilities.contains(ENGINE_GET_PAYLOAD_V1),
            get_payload_v2: capabilities.contains(ENGINE_GET_PAYLOAD_V2),
            get_payload_v3: capabilities.contains(ENGINE_GET_PAYLOAD_V3),
            get_payload_v4: capabilities.contains(ENGINE_GET_PAYLOAD_V4),
            get_payload_v5: capabilities.contains(ENGINE_GET_PAYLOAD_V5),
            get_client_version_v1: capabilities.contains(ENGINE_GET_CLIENT_VERSION_V1),
            get_blobs_v1: capabilities.contains(ENGINE_GET_BLOBS_V1),
        }
    }

    /// Create EngineCapabilities with all capabilities set to true
    pub fn all() -> Self {
        Self {
            new_payload_v1: true,
            new_payload_v2: true,
            new_payload_v3: true,
            new_payload_v4: true,
            forkchoice_updated_v1: true,
            forkchoice_updated_v2: true,
            forkchoice_updated_v3: true,
            get_payload_bodies_by_hash_v1: true,
            get_payload_bodies_by_range_v1: true,
            get_payload_v1: true,
            get_payload_v2: true,
            get_payload_v3: true,
            get_payload_v4: true,
            get_payload_v5: true,
            get_client_version_v1: true,
            get_blobs_v1: true,
        }
    }
}

use crate::engine::EngineAPI;

pub async fn check_capabilities(api: impl EngineAPI) -> eyre::Result<()> {
    let caps = api.exchange_capabilities().await?;

    if !caps.forkchoice_updated_v3 {
        eyre::bail!("Engine does not support forkchoiceUpdatedV3");
    }

    if !caps.get_payload_v4 {
        eyre::bail!("Engine does not support getPayloadV4");
    }

    if !caps.new_payload_v4 {
        eyre::bail!("Engine does not support newPayloadV4");
    }

    // V5 is only used when Osaka is active (decided per-block by `use_v5()`).
    // Warn instead of bail so that chains without Osaka can start even if the
    // EL hasn't been upgraded to advertise V5 yet (e.g. during rolling upgrades).
    if !caps.get_payload_v5 {
        tracing::warn!(
            "Engine does not advertise getPayloadV5 — \
             Osaka blocks will fail until the EL is upgraded"
        );
    }

    Ok(())
}
