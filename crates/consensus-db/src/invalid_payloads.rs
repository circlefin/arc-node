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

//! Types for storing and representing invalid proposals.

use std::fmt;

use alloy_rpc_types_engine::ExecutionPayloadV3;
use arc_consensus_types::{Address, Height, ProposalParts, Round};

use arc_consensus_types::block::ConsensusBlock;

/// Maximum number of invalid-payload records retained per height.
///
/// Each append rewrites the whole per-height value, so an uncapped vector lets
/// repeated invalid payloads at one height grow storage without bound and make
/// appends increasingly expensive. Retaining the earliest records at a height
/// bounds a single stored value while preserving the onset of any anomaly.
pub const MAX_INVALID_PAYLOADS_PER_HEIGHT: usize = 64;

/// Invalid payloads collected during a height.
/// Stored in the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredInvalidPayloads {
    pub height: Height,
    pub payloads: Vec<InvalidPayload>,
}

impl StoredInvalidPayloads {
    /// Create an empty invalid-payloads struct for a given height.
    pub fn empty(height: Height) -> Self {
        Self {
            height,
            payloads: Vec::new(),
        }
    }

    /// Appends an invalid payload to this collection, enforcing the per-height
    /// cap.
    ///
    /// Returns `true` if the payload was stored, or `false` if the collection
    /// was already at [`MAX_INVALID_PAYLOADS_PER_HEIGHT`] and the payload was
    /// dropped. The earliest records at a height are kept.
    #[must_use]
    pub fn add_invalid_payload(&mut self, payload: InvalidPayload) -> bool {
        if self.payloads.len() >= MAX_INVALID_PAYLOADS_PER_HEIGHT {
            return false;
        }
        self.payloads.push(payload);
        true
    }
}

/// An invalid payload that was submitted to the network.
///
/// An invalid payload is a payload that didn't pass the Engine API validation via
/// a call to `engine.newPayload`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidPayload {
    pub height: Height,
    pub round: Round,
    pub proposer_address: Address,
    pub payload: Option<ExecutionPayloadV3>,
    pub reason: String,
}

impl InvalidPayload {
    /// Creates an invalid payload record from an assembled block, including a copy
    /// of the execution payload.
    ///
    /// Use this when the full block is available (e.g. after successful assembly
    /// but failed Engine API validation).
    pub fn new_from_block(block: &ConsensusBlock, with_reason: &str) -> Self {
        Self {
            height: block.height,
            round: block.round,
            proposer_address: block.proposer,
            payload: Some(block.execution_payload.clone()),
            reason: with_reason.to_string(),
        }
    }

    /// Creates an invalid payload record from raw proposal parts, without an
    /// execution payload.
    ///
    /// Use this when the block could not be assembled from its parts (e.g. SSZ
    /// decoding failure), so no execution payload is available.
    pub fn new_from_parts(parts: &ProposalParts, with_reason: &str) -> Self {
        Self {
            height: parts.height(),
            round: parts.round(),
            proposer_address: parts.proposer(),
            payload: None,
            reason: with_reason.to_string(),
        }
    }

    /// Creates an invalid payload record from individual fields, without an
    /// execution payload.
    ///
    /// Use this when neither a [`ConsensusBlock`] nor [`ProposalParts`] is
    /// available (e.g. when raw bytes failed SSZ decoding before a block could
    /// be assembled).
    pub fn new_without_payload(
        height: Height,
        round: Round,
        proposer_address: Address,
        reason: &str,
    ) -> Self {
        Self {
            height,
            round,
            proposer_address,
            payload: None,
            reason: reason.to_string(),
        }
    }
}

impl fmt::Display for InvalidPayload {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let block_hash: &dyn fmt::Display = match &self.payload {
            Some(p) => &p.payload_inner.payload_inner.block_hash,
            None => &"<missing>",
        };

        write!(
            f,
            "{{ height: {}, round: {}, proposer_address: {}, block_hash: {}, reason: {} }}",
            self.height, self.round, self.proposer_address, block_hash, self.reason,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{InvalidPayload, StoredInvalidPayloads, MAX_INVALID_PAYLOADS_PER_HEIGHT};
    use arc_consensus_types::{Address, Height, Round};

    fn payload_with_reason(height: Height, reason: &str) -> InvalidPayload {
        InvalidPayload::new_without_payload(height, Round::new(0), Address::new([0u8; 20]), reason)
    }

    #[test]
    fn add_invalid_payload_accepts_up_to_the_cap() {
        let height = Height::new(1);
        let mut stored = StoredInvalidPayloads::empty(height);

        for i in 0..MAX_INVALID_PAYLOADS_PER_HEIGHT {
            assert!(
                stored.add_invalid_payload(payload_with_reason(height, &i.to_string())),
                "append below the cap must be accepted",
            );
        }

        assert_eq!(stored.payloads.len(), MAX_INVALID_PAYLOADS_PER_HEIGHT);
    }

    #[test]
    fn add_invalid_payload_drops_beyond_the_cap_and_keeps_the_earliest() {
        let height = Height::new(1);
        let mut stored = StoredInvalidPayloads::empty(height);

        for i in 0..MAX_INVALID_PAYLOADS_PER_HEIGHT {
            let _ = stored.add_invalid_payload(payload_with_reason(height, &i.to_string()));
        }

        // Further appends at a full height are rejected and do not grow the vector.
        for i in 0..5 {
            assert!(
                !stored.add_invalid_payload(payload_with_reason(height, &format!("overflow-{i}"))),
                "append at a full height must be rejected",
            );
        }

        assert_eq!(stored.payloads.len(), MAX_INVALID_PAYLOADS_PER_HEIGHT);
        // The earliest records are the ones retained.
        assert_eq!(stored.payloads.first().unwrap().reason, "0");
        assert!(
            stored
                .payloads
                .iter()
                .all(|p| !p.reason.starts_with("overflow")),
            "no dropped record should have been stored",
        );
    }
}
