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

//! Unified event type bridging consensus and execution layer events.

use alloy_primitives::B256;
use arc_consensus_types::{ArcContext, Height};

/// Unified event emitted by an Arc test node, covering both layers.
///
/// The [`NodeHandle`](crate::NodeHandle) exposes a broadcast channel of these events,
/// allowing step handlers and the framework sequencer to react to both consensus
/// and execution activity through a single stream.
#[derive(Clone, Debug)]
pub enum ArcEvent {
    // -- Consensus layer events --
    /// Consensus started processing a new height.
    ConsensusStartedHeight { height: Height },

    /// Consensus decided (committed) a block at the given height.
    ConsensusDecided {
        height: Height,
        certificate: malachitebft_core_types::CommitCertificate<ArcContext>,
    },

    /// Consensus finalized a block (post-commit, after evidence collection).
    ConsensusFinalized { height: Height },

    /// A value was locally proposed by this node.
    ConsensusProposedValue {
        height: Height,
        round: malachitebft_core_types::Round,
    },

    // -- Execution layer events --
    /// A new block was produced on the execution layer.
    BlockProduced { number: Height, hash: B256 },
}
