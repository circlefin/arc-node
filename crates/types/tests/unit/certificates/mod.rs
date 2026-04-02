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
#![allow(dead_code)]

mod commit;
mod polka;
mod round;

use std::marker::PhantomData;

pub mod types {
    pub use arc_consensus_types::{
        Address, ArcContext, BlockHash, Height, Validator, ValidatorSet, ValueId, Vote,
    };
    pub use arc_signer::local::LocalSigningProvider;
    pub use malachitebft_core_types::{
        CertificateError, Context, NilOrVal, Round, RoundCertificateType, SignedVote,
        ThresholdParams, VoteType, VotingPower,
    };
    pub use malachitebft_signing::{SigningProvider, SigningProviderExt};
    pub use malachitebft_signing_ed25519::{PrivateKey, Signature};
}

use types::*;

use rand::{rngs::StdRng, Rng, SeedableRng};

const DEFAULT_SEED: u64 = 0xfeedbeef;

pub fn make_validators_seeded<const N: usize>(
    voting_powers: [VotingPower; N],
    seed: u64,
) -> [(Validator, PrivateKey); N] {
    let mut rng = StdRng::seed_from_u64(seed);

    let mut validators = Vec::with_capacity(N);

    for vp in voting_powers {
        let sk = PrivateKey::generate(&mut rng);
        let val = Validator::new(sk.public_key(), vp);
        validators.push((val, sk));
    }

    validators.try_into().expect("N validators")
}

pub fn make_validators<const N: usize>(
    voting_powers: [VotingPower; N],
    seed: u64,
) -> ([Validator; N], [LocalSigningProvider; N]) {
    let (validators, private_keys): (Vec<_>, Vec<_>) = make_validators_seeded(voting_powers, seed)
        .into_iter()
        .map(|(v, pk)| (v, LocalSigningProvider::new(pk)))
        .unzip();

    (
        validators.try_into().unwrap(),
        private_keys.try_into().unwrap(),
    )
}

pub fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

pub trait CertificateBuilder {
    type Certificate;

    fn build_certificate(
        height: Height,
        round: Round,
        value_id: Option<ValueId>,
        votes: Vec<SignedVote<ArcContext>>,
    ) -> Self::Certificate;

    async fn verify_certificate(
        ctx: &ArcContext,
        signer: &LocalSigningProvider,
        certificate: &Self::Certificate,
        validator_set: &ValidatorSet,
        threshold_params: ThresholdParams,
    ) -> Result<(), CertificateError<ArcContext>>;

    fn make_vote(
        ctx: &ArcContext,
        height: Height,
        round: Round,
        value_id: NilOrVal<ValueId>,
        vote_type: VoteType,
        validator_address: Address,
    ) -> Vote {
        match vote_type {
            VoteType::Prevote => ctx.new_prevote(height, round, value_id, validator_address),
            VoteType::Precommit => ctx.new_precommit(height, round, value_id, validator_address),
        }
    }
}

/// A fluent builder for certificate testing
pub struct CertificateTest<C> {
    ctx: ArcContext,
    height: Height,
    round: Round,
    value_id: ValueId,
    validators: Vec<Validator>,
    signers: Vec<LocalSigningProvider>,
    votes: Vec<SignedVote<ArcContext>>,
    marker: PhantomData<C>,
}

impl<C> CertificateTest<C>
where
    C: CertificateBuilder,
{
    /// Create a new certificate test with default settings
    pub fn new() -> Self {
        Self {
            ctx: ArcContext::new(),
            height: Height::new(1),
            round: Round::new(0),
            value_id: ValueId::new(BlockHash::from([0xa; 32])),
            validators: Vec::new(),
            signers: Vec::new(),
            votes: Vec::new(),
            marker: PhantomData,
        }
    }

    /// Set the height for the certificate
    pub fn with_height(mut self, height: u64) -> Self {
        self.height = Height::new(height);
        self
    }

    /// Set the round for the certificate
    pub fn with_round(mut self, round: i64) -> Self {
        self.round = Round::from(round);
        self
    }

    /// Set the value ID for the certificate
    pub fn for_value(mut self, value_id: BlockHash) -> Self {
        self.value_id = ValueId::new(value_id);
        self
    }

    /// Set up validators with the given voting powers using default seed
    pub fn with_validators<const N: usize>(self, voting_powers: [VotingPower; N]) -> Self {
        self.with_validators_seeded(voting_powers, DEFAULT_SEED)
    }

    /// Set up validators with the given voting powers and seed
    pub fn with_validators_seeded<const N: usize>(
        mut self,
        voting_powers: [VotingPower; N],
        seed: u64,
    ) -> Self {
        let (validators, signers) = make_validators(voting_powers, seed);
        self.validators = Vec::from(validators);
        self.signers = Vec::from(signers);
        self
    }

    /// Add votes to include in the certificate
    pub fn with_votes(
        mut self,
        indices: impl IntoIterator<Item = usize>,
        vote_type: VoteType,
    ) -> Self {
        for idx in indices {
            if idx < self.validators.len() {
                let vote = block_on(self.signers[idx].sign_vote(C::make_vote(
                    &self.ctx,
                    self.height,
                    self.round,
                    NilOrVal::Val(self.value_id),
                    vote_type,
                    self.validators[idx].address,
                )));

                self.votes.push(vote.unwrap());
            }
        }
        self
    }

    /// Add nil votes to include in the certificate
    pub fn with_nil_votes(
        mut self,
        indices: impl IntoIterator<Item = usize>,
        vote_type: VoteType,
    ) -> Self {
        for idx in indices {
            if idx < self.validators.len() {
                let vote = block_on(self.signers[idx].sign_vote(C::make_vote(
                    &self.ctx,
                    self.height,
                    self.round,
                    NilOrVal::Nil,
                    vote_type,
                    self.validators[idx].address,
                )));

                self.votes.push(vote.unwrap());
            }
        }
        self
    }

    /// Add a vote with different value to include in the certificate
    pub fn with_different_value_vote(mut self, index: usize, vote_type: VoteType) -> Self {
        if index < self.validators.len() {
            let vote = block_on(self.signers[index].sign_vote(C::make_vote(
                &self.ctx,
                self.height,
                self.round,
                NilOrVal::Val(ValueId::new(BlockHash::from([0xb; 32]))),
                vote_type,
                self.validators[index].address,
            )));

            self.votes.push(vote.unwrap());
        }
        self
    }

    /// Add votes to include in the certificate with random types and values
    /// If vote_type_opt is Some, uses that vote type; otherwise picks one at random.
    pub fn with_random_votes(
        mut self,
        indices: impl IntoIterator<Item = usize>,
        vote_type_opt: Option<VoteType>,
    ) -> Self {
        let mut rng = rand::thread_rng();

        for idx in indices {
            if idx < self.validators.len() {
                let vote_type = match vote_type_opt {
                    Some(vt) => vt,
                    None => {
                        // Randomly pick vote type
                        if rng.gen_range(0..2) == 0 {
                            VoteType::Prevote
                        } else {
                            VoteType::Precommit
                        }
                    }
                };

                // Randomly pick value kind: 0 = nil, 1 = same value, 2 = different value
                match rng.gen_range(0..3) {
                    0 => self = self.with_nil_votes([idx], vote_type),
                    1 => self = self.with_votes([idx], vote_type),
                    2 => self = self.with_different_value_vote(idx, vote_type),
                    _ => unreachable!(),
                };
            }
        }

        self
    }

    /// Add a vote with invalid height to include in the certificate
    pub fn with_invalid_height_vote(mut self, index: usize, vote_type: VoteType) -> Self {
        if index < self.validators.len() {
            let vote = block_on(self.signers[index].sign_vote(C::make_vote(
                &self.ctx,
                self.height.increment(),
                self.round,
                NilOrVal::Val(self.value_id),
                vote_type,
                self.validators[index].address,
            )));

            self.votes.push(vote.unwrap());
        }
        self
    }

    /// Add a vote with invalid round to include in the certificate
    pub fn with_invalid_round_vote(mut self, index: usize, vote_type: VoteType) -> Self {
        if index < self.validators.len() {
            let vote = block_on(self.signers[index].sign_vote(C::make_vote(
                &self.ctx,
                self.height,
                self.round.increment(),
                NilOrVal::Val(self.value_id),
                vote_type,
                self.validators[index].address,
            )));

            self.votes.push(vote.unwrap());
        }
        self
    }

    /// Add a vote with invalid signature to include in the certificate
    pub fn with_invalid_signature_vote(mut self, index: usize, vote_type: VoteType) -> Self {
        if index < self.validators.len() {
            let mut vote = block_on(self.signers[index].sign_vote(C::make_vote(
                &self.ctx,
                self.height,
                self.round,
                NilOrVal::Val(self.value_id),
                vote_type,
                self.validators[index].address,
            )))
            .unwrap();
            vote.signature = Signature::test(); // Set an invalid signature
            self.votes.push(vote);
        }
        self
    }

    /// Add a vote from external validator to include in the certificate
    pub fn with_non_validator_vote(mut self, seed: u64, vote_type: VoteType) -> Self {
        let ([validator], [signer]) = make_validators([0], seed);
        let vote = block_on(signer.sign_vote(C::make_vote(
            &self.ctx,
            self.height,
            self.round,
            NilOrVal::Val(self.value_id),
            vote_type,
            validator.address,
        )));
        self.votes.push(vote.unwrap());
        self
    }

    /// Add a duplicate last vote to include in the certificate
    pub fn with_duplicate_last_vote(mut self) -> Self {
        if let Some(last_vote) = self.votes.last().cloned() {
            self.votes.push(last_vote);
        }
        self
    }

    /// Build the certificate based on the configured settings
    fn build_certificate(&self) -> (C::Certificate, ValidatorSet) {
        let validator_set = ValidatorSet::new(self.validators.clone());
        let certificate = C::build_certificate(
            self.height,
            self.round,
            Some(self.value_id),
            self.votes.clone(),
        );
        (certificate, validator_set)
    }

    /// Verify that the certificate is valid
    pub fn expect_valid(self) {
        let (certificate, validator_set) = self.build_certificate();

        for signer in &self.signers {
            let result = block_on(C::verify_certificate(
                &self.ctx,
                signer,
                &certificate,
                &validator_set,
                ThresholdParams::default(),
            ));

            assert!(
                result.is_ok(),
                "Expected valid certificate, but got error: {:?}",
                result.unwrap_err()
            );
        }
    }

    /// Verify that the certificate is invalid with the expected error
    pub fn expect_error(self, expected_error: CertificateError<ArcContext>) {
        let (certificate, validator_set) = self.build_certificate();

        for signer in &self.signers {
            let result = block_on(C::verify_certificate(
                &self.ctx,
                signer,
                &certificate,
                &validator_set,
                ThresholdParams::default(),
            ));

            assert_eq!(
                result.as_ref(),
                Err(&expected_error),
                "Expected certificate error {expected_error:?}, but got: {result:?}",
            );
        }
    }
}
