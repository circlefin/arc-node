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

use crate::{Height, Round, Validator, ValidatorSet};

/// A trait for selecting proposers from a validator set.
pub trait ProposerSelector: Send + Sync {
    /// Select a proposer in the validator set for the given height and round.
    fn select_proposer<'a>(
        &self,
        validator_set: &'a ValidatorSet,
        height: Height,
        round: Round,
    ) -> &'a Validator;
}

/// A simple round-robin proposer selector.
///
/// The proposer is selected based on the formula: `(height - 1 + round) % validator_set.count()`.
///
/// # Panics
/// - If the validator set is empty
/// - If the round is nil
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct RoundRobin;

impl ProposerSelector for RoundRobin {
    fn select_proposer<'a>(
        &self,
        validator_set: &'a ValidatorSet,
        height: Height,
        round: Round,
    ) -> &'a Validator {
        assert!(!validator_set.is_empty(), "validator set cannot be empty");
        assert!(round != Round::Nil && round.as_i64() >= 0);

        let proposer_index = {
            let height = height.as_u64() as usize;
            let round = round.as_i64() as usize;

            (height - 1 + round) % validator_set.len()
        };

        validator_set
            .get_by_index(proposer_index)
            .expect("proposer_index is valid")
    }
}
