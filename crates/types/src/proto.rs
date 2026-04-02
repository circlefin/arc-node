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

//! Auto-generated types from protobuf definitions.

/// Top-level module for Arc protobuf types.
pub mod arc {
    /// Module for consensus-related protobuf types.
    pub mod consensus {
        /// Version 1 of the consensus protobuf types.
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/arc.consensus.v1.rs"));
        }
    }
    /// Module for liveness-related protobuf types.
    pub mod liveness {
        /// Version 1 of the liveness protobuf types.
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/arc.liveness.v1.rs"));
        }
    }
    /// Module for store-related protobuf types.
    pub mod store {
        /// Version 1 of the store protobuf types.
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/arc.store.v1.rs"));
        }
    }
    /// Module for sync-related protobuf types.
    pub mod sync {
        /// Version 1 of the sync protobuf types.
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/arc.sync.v1.rs"));
        }
    }
}

// Re-export v1 types at the top level for convenience and backward compatibility
pub use arc::consensus::v1::*;

// Also re-export types from other modules for backward compatibility
pub use arc::liveness::v1::{
    liveness_message, LivenessMessage, PolkaCertificate, PolkaSignature, RoundCertificate,
    RoundCertificateType, RoundSignature,
};
pub use arc::store::v1::{
    DoubleProposal as ProtoDoubleProposal, DoubleVote as ProtoDoubleVote,
    InvalidPayload as ProtoInvalidPayload, InvalidPayloads as ProtoInvalidPayloads,
    MisbehaviorEvidence as ProtoMisbehaviorEvidence,
    ProposalMonitorData as ProtoProposalMonitorData, ProposalParts,
    ValidatorEvidence as ProtoValidatorEvidence,
};
pub use arc::sync::v1::{
    sync_request, sync_response, CommitCertificate, CommitSignature, PeerId, ProposedValue, Status,
    SyncRequest, SyncResponse, SyncedValue, ValueRequest, ValueResponse,
};

// Keep module exports for those who want to use them explicitly
pub use arc::liveness::v1 as liveness;
pub use arc::store::v1 as store;
pub use arc::sync::v1 as sync;
