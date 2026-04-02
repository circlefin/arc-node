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

//! Types for storing and representing misbehavior evidence.

use malachitebft_app_channel::app::types::MisbehaviorEvidence;
use malachitebft_core_types::{SignedProposal, SignedVote};

use crate::{Address, ArcContext, Height};

/// A pair of conflicting votes from the same validator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoubleVote {
    pub first: SignedVote<ArcContext>,
    pub second: SignedVote<ArcContext>,
}

/// A pair of conflicting proposals from the same validator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoubleProposal {
    pub first: SignedProposal<ArcContext>,
    pub second: SignedProposal<ArcContext>,
}

/// Evidence of misbehavior for a single validator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatorEvidence {
    pub address: Address,
    pub double_votes: Vec<DoubleVote>,
    pub double_proposals: Vec<DoubleProposal>,
}

/// Misbehavior evidence collected during a height.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMisbehaviorEvidence {
    pub height: Height,
    pub validators: Vec<ValidatorEvidence>,
}

impl StoredMisbehaviorEvidence {
    pub fn from_misbehavior_evidence(
        height: Height,
        evidence: &MisbehaviorEvidence<ArcContext>,
    ) -> Self {
        use std::collections::BTreeMap;

        // Merge proposal and vote evidence per validator
        let mut validator_map: BTreeMap<Address, ValidatorEvidence> = BTreeMap::new();

        // Process vote evidence
        for addr in evidence.votes.iter() {
            if let Some(double_votes) = evidence.votes.get(addr) {
                let double_votes = double_votes
                    .iter()
                    .map(|(first, second)| DoubleVote {
                        first: first.to_owned(),
                        second: second.to_owned(),
                    })
                    .collect();

                validator_map
                    .entry(addr.to_owned())
                    .or_insert_with(|| ValidatorEvidence {
                        address: addr.to_owned(),
                        double_votes: Vec::new(),
                        double_proposals: Vec::new(),
                    })
                    .double_votes = double_votes;
            }
        }

        // Process proposal evidence
        for addr in evidence.proposals.iter() {
            if let Some(double_proposals) = evidence.proposals.get(addr) {
                let double_proposals = double_proposals
                    .iter()
                    .map(|(first, second)| DoubleProposal {
                        first: first.to_owned(),
                        second: second.to_owned(),
                    })
                    .collect();

                validator_map
                    .entry(addr.to_owned())
                    .or_insert_with(|| ValidatorEvidence {
                        address: addr.to_owned(),
                        double_votes: Vec::new(),
                        double_proposals: Vec::new(),
                    })
                    .double_proposals = double_proposals;
            }
        }

        Self {
            height,
            validators: validator_map.into_values().collect(),
        }
    }

    /// Create an empty evidence struct for a given height.
    pub fn empty(height: Height) -> Self {
        Self {
            height,
            validators: Vec::new(),
        }
    }

    /// Check if there is any evidence.
    pub fn is_empty(&self) -> bool {
        self.validators.is_empty()
    }
}
