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

// adapted from https://github.com/informalsystems/malachite/tree/v0.4.0/code/crates/test
use bytes::Bytes;
use malachitebft_core_types::{Context, LinearTimeouts, NilOrVal, Round};

#[cfg(feature = "byzantine")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "byzantine")]
use malachitebft_engine_byzantine::{Amnesia, Trigger};
#[cfg(feature = "byzantine")]
use rand::rngs::StdRng;
#[cfg(feature = "byzantine")]
use rand::SeedableRng;

use crate::address::*;
use crate::height::*;
use crate::proposal::*;
use crate::proposal_part::*;
use crate::proposer::{ProposerSelector, RoundRobin};
use crate::signing::*;
use crate::validator_set::*;
use crate::value::*;
use crate::vote::*;

/// Byzantine state bundle attached to `ArcContext` when the `byzantine`
/// feature is enabled. Owns the context-generic amnesia state machine
/// plus the `force_precommit_nil` trigger/RNG. Only non-`None` for tests
/// that opt into byzantine behavior via config.
#[cfg(feature = "byzantine")]
pub struct ByzantineState {
    /// Amnesia state machine (`ignore_locks`).
    pub amnesia: Amnesia<ArcContext>,
    /// When to rewrite non-nil precommits into nil precommits.
    pub force_precommit_nil: Trigger,
    /// The node's own validator address. `new_precommit` only rewrites for
    /// this address so certificate-verification reconstructions for other
    /// validators are left intact.
    pub self_address: Address,
    /// RNG for evaluating `force_precommit_nil`. Amnesia owns its own RNG.
    rng: Mutex<StdRng>,
}

#[cfg(feature = "byzantine")]
impl ByzantineState {
    pub fn new(
        ignore_locks: Trigger,
        force_precommit_nil: Trigger,
        self_address: Address,
        seed: Option<u64>,
    ) -> Self {
        let rng = match seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_entropy(),
        };
        Self {
            amnesia: Amnesia::new(ignore_locks, seed),
            force_precommit_nil,
            self_address,
            rng: Mutex::new(rng),
        }
    }

    fn should_force_precommit_nil(&self, height: Height, round: Round) -> bool {
        self.force_precommit_nil
            .fires(height, round, &mut self.rng.lock().expect("poisoned rng"))
    }
}

#[cfg(feature = "byzantine")]
impl std::fmt::Debug for ByzantineState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ByzantineState")
            .field("force_precommit_nil", &self.force_precommit_nil)
            .field("self_address", &self.self_address)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
// Preserve the pre-byzantine `Copy` impl for non-byzantine builds. When the
// feature is enabled, `Option<Arc<ByzantineState>>` is not `Copy`, so the
// derive is gated off.
#[cfg_attr(not(feature = "byzantine"), derive(Copy))]
pub struct ArcContext {
    pub proposer_selector: RoundRobin,
    #[cfg(feature = "byzantine")]
    pub byzantine: Option<Arc<ByzantineState>>,
}

impl ArcContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a [`ByzantineState`] bundle; used by arc-node's startup to
    /// wire amnesia + force-precommit-nil into the context when the
    /// `[byzantine]` config section is active.
    #[cfg(feature = "byzantine")]
    pub fn with_byzantine(mut self, byzantine: Arc<ByzantineState>) -> Self {
        self.byzantine = Some(byzantine);
        self
    }
}

impl Context for ArcContext {
    type Address = Address;
    type ProposalPart = ProposalPart;
    type Height = Height;
    type Proposal = Proposal;
    type ValidatorSet = ValidatorSet;
    type Validator = Validator;
    type Value = Value;
    type Vote = Vote;
    type SigningScheme = Ed25519;
    type Extension = Bytes;
    type Timeouts = LinearTimeouts;

    fn select_proposer<'a>(
        &self,
        validator_set: &'a Self::ValidatorSet,
        height: Self::Height,
        round: Round,
    ) -> &'a Self::Validator {
        self.proposer_selector
            .select_proposer(validator_set, height, round)
    }

    fn new_proposal(
        &self,
        height: Height,
        round: Round,
        value: Value,
        pol_round: Round,
        address: Address,
    ) -> Proposal {
        Proposal::new(height, round, value, pol_round, address)
    }

    fn new_prevote(
        &self,
        height: Height,
        round: Round,
        value_id: NilOrVal<ValueId>,
        address: Address,
    ) -> Vote {
        #[cfg(feature = "byzantine")]
        let value_id = match (&value_id, &self.byzantine) {
            (NilOrVal::Nil, Some(byz)) => byz
                .amnesia
                .try_override_nil_prevote(height, round)
                .map(NilOrVal::Val)
                .unwrap_or(value_id),
            _ => value_id,
        };
        Vote::new_prevote(height, round, value_id, address)
    }

    fn new_precommit(
        &self,
        height: Height,
        round: Round,
        value_id: NilOrVal<ValueId>,
        address: Address,
    ) -> Vote {
        #[cfg(feature = "byzantine")]
        if let Some(byz) = &self.byzantine {
            if address == byz.self_address
                && matches!(value_id, NilOrVal::Val(_))
                && byz.should_force_precommit_nil(height, round)
            {
                tracing::warn!(
                    %height, %round,
                    "BYZANTINE: Forcing precommit nil (rewriting non-nil precommit)"
                );
                return Vote::new_precommit(height, round, NilOrVal::Nil, address);
            }
        }
        Vote::new_precommit(height, round, value_id, address)
    }
}

#[cfg(all(test, feature = "byzantine"))]
mod byzantine_tests {
    use super::*;
    use crate::BlockHash;

    fn addr(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    fn value_id(byte: u8) -> ValueId {
        ValueId::new(BlockHash::repeat_byte(byte))
    }

    fn ctx_with_byzantine(
        ignore_locks: Trigger,
        force_precommit_nil: Trigger,
        self_address: Address,
    ) -> ArcContext {
        let state = Arc::new(ByzantineState::new(
            ignore_locks,
            force_precommit_nil,
            self_address,
            Some(42),
        ));
        ArcContext::default().with_byzantine(state)
    }

    #[test]
    fn new_prevote_amnesia_overrides_nil_with_recorded_value() {
        let self_addr = addr(0x01);
        let ctx = ctx_with_byzantine(Trigger::Always, Trigger::Never, self_addr);
        let h = Height::new(1);
        let r = Round::new(0);
        let vid = value_id(0xAB);

        ctx.byzantine
            .as_ref()
            .unwrap()
            .amnesia
            .record_proposed_value(h, r, vid);

        let vote = ctx.new_prevote(h, r, NilOrVal::Nil, self_addr);
        assert!(matches!(vote.value, NilOrVal::Val(v) if v == vid));
    }

    #[test]
    fn new_prevote_amnesia_leaves_non_nil_unchanged() {
        let self_addr = addr(0x01);
        let ctx = ctx_with_byzantine(Trigger::Always, Trigger::Never, self_addr);
        let h = Height::new(1);
        let r = Round::new(0);
        let vid = value_id(0xCD);

        let vote = ctx.new_prevote(h, r, NilOrVal::Val(vid), self_addr);
        assert!(matches!(vote.value, NilOrVal::Val(v) if v == vid));
    }

    #[test]
    fn new_prevote_without_byzantine_state_leaves_nil_unchanged() {
        let ctx = ArcContext::default();
        let vote = ctx.new_prevote(Height::new(1), Round::new(0), NilOrVal::Nil, addr(0x02));
        assert!(matches!(vote.value, NilOrVal::Nil));
    }

    #[test]
    fn new_precommit_force_nil_rewrites_val_to_nil_for_self() {
        let self_addr = addr(0x01);
        let ctx = ctx_with_byzantine(Trigger::Never, Trigger::Always, self_addr);
        let vote = ctx.new_precommit(
            Height::new(1),
            Round::new(0),
            NilOrVal::Val(value_id(0xAB)),
            self_addr,
        );
        assert!(matches!(vote.value, NilOrVal::Nil));
    }

    #[test]
    fn new_precommit_force_nil_leaves_other_validators_unchanged() {
        // Certificate reconstruction synthesises precommits for other validators;
        // those must never be rewritten.
        let self_addr = addr(0x01);
        let other_addr = addr(0x02);
        let ctx = ctx_with_byzantine(Trigger::Never, Trigger::Always, self_addr);
        let vid = value_id(0xAB);

        let vote = ctx.new_precommit(
            Height::new(1),
            Round::new(0),
            NilOrVal::Val(vid),
            other_addr,
        );
        assert!(matches!(vote.value, NilOrVal::Val(v) if v == vid));
    }

    #[test]
    fn new_precommit_force_nil_leaves_nil_unchanged() {
        let self_addr = addr(0x01);
        let ctx = ctx_with_byzantine(Trigger::Never, Trigger::Always, self_addr);
        let vote = ctx.new_precommit(Height::new(1), Round::new(0), NilOrVal::Nil, self_addr);
        assert!(matches!(vote.value, NilOrVal::Nil));
    }

    #[test]
    fn new_precommit_trigger_never_leaves_val_unchanged() {
        let self_addr = addr(0x01);
        let ctx = ctx_with_byzantine(Trigger::Never, Trigger::Never, self_addr);
        let vid = value_id(0xAB);

        let vote = ctx.new_precommit(Height::new(1), Round::new(0), NilOrVal::Val(vid), self_addr);
        assert!(matches!(vote.value, NilOrVal::Val(v) if v == vid));
    }
}
