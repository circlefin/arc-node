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

use eyre::Context;
use tracing::{error, info, warn};

use malachitebft_app_channel::app::consensus::Role;
use malachitebft_app_channel::app::types::core::Validity;
use malachitebft_app_channel::app::types::ProposedValue;
use malachitebft_app_channel::Reply;

use arc_consensus_types::proposer::ProposerSelector;
use arc_consensus_types::{Address, ArcContext, Height, ProposalParts, Round, ValidatorSet};
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_signer::ArcSigningProvider;

use super::skew_gate;
use crate::block::ConsensusBlock;
use crate::metrics::{
    AppMetrics, InvalidPayloadSource, SkewNilVoteSource, TransientValidationSource,
};
use crate::payload::{
    check_payload_binding, establish_block_validity, persist_invalid_payload_best_effort,
    validate_consensus_block, EnginePayloadValidator, PayloadValidator,
};
use crate::proposal_parts::{
    assemble_block_from_parts, resolve_expected_proposer, validate_proposal_parts,
};
use crate::state::{attach_valid_proposal_value_ids, State};
use crate::store::repositories::{InvalidPayloadsRepository, UndecidedBlocksRepository};
use crate::store::Store;
use arc_consensus_db::invalid_payloads::InvalidPayload;

/// Handles the `StartedRound` message from the consensus engine.
///
/// This is called when the consensus engine starts a new round for a given height.
/// The application performs the following steps:
/// 1. If it's the first round of a new height, it resets the height timer
/// 2. Updates the current round and proposer in the state
/// 3. Retrieves any pending proposal parts for the current height and round
/// 4. Processes the pending proposal parts to reconstruct any complete proposals,
///    adding them to the undecided blocks table
/// 5. Validates all undecided blocks for the current height and round by sending them
///    to the execution client, and updating their validity status
/// 6. Returns the valid proposed values to the consensus engine
pub async fn handle(
    state: &mut State,
    engine: &Engine,
    height: Height,
    round: Round,
    proposer: Address,
    role: Role,
    reply: Reply<Vec<ProposedValue<ArcContext>>>,
) {
    let proposals = match on_started_round(state, engine, height, round, proposer, role).await {
        Ok(proposals) => {
            info!(%height, %round, "StartedRound: sending {} undecided proposals to consensus", proposals.len());
            proposals
        }
        Err(e) => {
            error!(%height, %round, "StartedRound: failed to process pending proposal parts: {e}");

            // In case of error, we send an empty list of proposals to consensus
            Vec::new()
        }
    };

    if let Err(e) = reply.send(proposals) {
        error!("🔴 StartedRound: Failed to send reply: {e:?}");
    }
}

async fn on_started_round(
    state: &mut State,
    engine: &Engine,
    height: Height,
    round: Round,
    proposer: Address,
    role: Role,
) -> eyre::Result<Vec<ProposedValue<ArcContext>>> {
    // If we are starting a new height, reset the height timer
    if round.as_i64() == 0 {
        let network_id = state.started_height(height, round, proposer);
        info!(%height, %network_id, "🦋 Started height");
    }

    info!(%height, %round, ?role, %proposer, "🔮 Started round");

    assert_eq!(state.current_height, height, "Consensus height mismatch");
    assert!(round != Round::Nil, "Round cannot be Nil");
    assert!(round >= state.current_round, "Round cannot go backwards");

    record_missed_rounds(
        state.metrics(),
        &state.ctx.proposer_selector,
        state.validator_set(),
        height,
        state.current_round,
        round,
    );

    state.current_round = round;
    state.current_proposer = Some(proposer);

    let proposals = fetch_and_process_pending_proposals(
        height,
        round,
        state.validator_set(),
        &state.ctx.proposer_selector,
        state.store(),
        engine,
        state.signing_provider(),
        state.metrics(),
        state.previous_block.as_ref(),
    )
    .await?;

    // Second half of the pending-parts recording: record the value id the
    // early-arrived parts carry (engine-invalid blocks skipped).
    attach_valid_proposal_value_ids(&mut state.proposal_monitor, height, round, &proposals);

    Ok(proposals)
}

/// Increments the `consensus_round_missed` counter once for every round that was
/// started but skipped over between `prev_round` and `new_round`, attributing each
/// skipped round to the validator that round-robin would have made its proposer.
///
/// `prev_round` is the height's previously started round (`state.current_round`),
/// with `Round::Nil` treated as round 0; the skipped rounds are the half-open
/// range `prev_round..new_round`, so the round that actually started is never
/// counted as missed.
///
/// This is an at-least-once alerting signal, not an exact ledger. On a mid-height
/// restart `state.current_round` resets to `Round::Nil`, so a replayed
/// `StartedRound` for a round the node had already advanced past will re-count
/// rounds that were counted before the restart. The counter therefore never
/// under-counts missed rounds but may over-count across restarts — acceptable for
/// the alerting use case it serves.
fn record_missed_rounds(
    metrics: &AppMetrics,
    proposer_selector: &dyn ProposerSelector,
    validator_set: &ValidatorSet,
    height: Height,
    prev_round: Round,
    new_round: Round,
) {
    let mut missed_round = prev_round.or(Round::Some(0));

    while missed_round < new_round {
        let missed_proposer = proposer_selector
            .select_proposer(validator_set, height, missed_round)
            .address;

        warn!(%missed_proposer, %height, %missed_round, "Consensus round missed");

        metrics.inc_consensus_round_missed(missed_proposer);

        missed_round = missed_round.increment();
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch_and_process_pending_proposals(
    height: Height,
    round: Round,
    validator_set: &ValidatorSet,
    proposer_selector: &dyn ProposerSelector,
    store: &Store,
    engine: &Engine,
    signing_provider: &ArcSigningProvider,
    metrics: &AppMetrics,
    previous_block: Option<&ExecutionBlock>,
) -> eyre::Result<Vec<ProposedValue<ArcContext>>> {
    let pending_parts = store
        .get_pending_proposal_parts(height, round)
        .await
        .wrap_err("failed to fetch pending proposal parts")?;

    info!(%height, %round, "StartedRound: Found {} pending proposal parts", pending_parts.len());

    let payload_validator = EnginePayloadValidator::new(engine, metrics);

    // Validate each pending block with the engine and insert the row into
    // the undecided table with the engine's verdict. Rows always reflect a
    // real verdict; on a transient engine error we skip the insert. The
    // parts are pruned when height advances past them.
    process_pending_proposal_parts(
        store,
        pending_parts,
        height,
        round,
        validator_set,
        proposer_selector,
        &payload_validator,
        store,
        signing_provider,
        metrics,
        previous_block,
    )
    .await
    .wrap_err("Failed to validate pending proposal parts")?;

    // Re-validate undecided blocks that already exist in the store. This is
    // needed after a restart, when the execution client may have lost the
    // payloads from its in-memory tree and must be re-fed them.
    let blocks = validate_undecided_blocks(
        height,
        round,
        previous_block,
        store,
        &payload_validator,
        store,
        metrics,
    )
    .await
    .wrap_err("failed to validate undecided blocks")?;

    // Apply the same vote-time clock-skew gate as the live-arrival path: a proposal
    // whose parts arrived before its round started — or an undecided block that was
    // stored, survived a crash, and is re-validated on recovery — must still be
    // prevoted nil when its timestamp is too far ahead of local time. The stored
    // block keeps its execution-only validity — only the vote is downgraded.
    let local_time = skew_gate::local_time_secs();
    Ok(blocks
        .iter()
        .map(|block| {
            let validity = skew_gate::validity_for(
                block,
                local_time,
                metrics,
                SkewNilVoteSource::StartedRound,
            );
            block.to_proposed_value_with_validity(validity)
        })
        .collect())
}

/// Process the pending proposal parts for the current height, assembling them
/// into blocks, validating each block against the execution client, and
/// moving the validated blocks to the undecided table with their engine
/// verdict.
///
/// A row in the undecided table always reflects the engine's verdict at
/// insertion time: no placeholder `Valid` ever leaks. If the engine cannot
/// be reached (transport error, `SYNCING`/`ACCEPTED`, …) the pending parts
/// are left in place and pruned when height advances past them. BFT
/// liveness covers the gap: the round times out and a later proposer's
/// block gets decided — this specific proposal does not need to land.
///
/// ## Important
/// This function assumes that the pending parts are for the current height and round.
#[allow(clippy::too_many_arguments)]
async fn process_pending_proposal_parts(
    store: &Store,
    pending_parts: Vec<ProposalParts>,
    current_height: Height,
    current_round: Round,
    validator_set: &ValidatorSet,
    proposer_selector: &dyn ProposerSelector,
    payload_validator: &impl PayloadValidator,
    invalid_payloads: &impl InvalidPayloadsRepository,
    signing_provider: &ArcSigningProvider,
    metrics: &AppMetrics,
    previous_block: Option<&ExecutionBlock>,
) -> eyre::Result<()> {
    for parts in pending_parts {
        let (height, round, proposer) = (parts.height(), parts.round(), parts.proposer());

        debug_assert_eq!(height, current_height, "Pending parts height mismatch");
        debug_assert_eq!(round, current_round, "Pending parts round mismatch");

        let expected_proposer = resolve_expected_proposer(proposer_selector, validator_set, &parts);

        if !validate_proposal_parts(&parts, expected_proposer, signing_provider).await {
            continue;
        }

        let mut block = match assemble_block_from_parts(&parts) {
            Ok(block) => block,
            Err(e) => {
                warn!(%height, %round, %proposer, "Failed to assemble block from pending parts: {e}");
                metrics.inc_invalid_payloads_count(InvalidPayloadSource::AssemblyFailure);
                let invalid_payload = InvalidPayload::new_from_parts(&parts, &e.to_string());
                persist_invalid_payload_best_effort(
                    store,
                    invalid_payload,
                    height,
                    round,
                    proposer,
                )
                .await;
                continue;
            }
        };

        // Engine verdict before insert: a transient engine error must not be
        // recorded as a permanent `Invalid` verdict against this block.
        let validity = match establish_block_validity(
            payload_validator,
            &block,
            previous_block,
            invalid_payloads,
            metrics,
        )
        .await
        {
            Ok(verdict) => verdict.validity(),
            Err(e) => {
                warn!(
                    %height, %round, %proposer,
                    "Skipping pending parts: transient engine validation error: {e:#}"
                );
                metrics.inc_transient_validation_errors_count(
                    TransientValidationSource::StartedRoundPending,
                );
                continue;
            }
        };

        block.validity = validity;

        info!(%height, %round, %proposer, ?validity, "Added pending block to undecided");

        // Atomically remove from pending and store as undecided.
        // This ensures that if the process fails, the parts are not lost.
        remove_pending_parts_and_store_undecided_block(store, parts, block).await?;
    }

    Ok(())
}

/// Re-sends every undecided block for the given height and round to the
/// execution client. This is the **restart recovery** path: after a crash,
/// the EL may have lost the in-memory tree state for these blocks and must
/// be re-fed them so subsequent `forkchoice_updated` calls can succeed.
///
/// The stored row's `validity` was set by `process_pending_proposal_parts`
/// at insertion time, so this function does not need to *establish* a verdict
/// — it only refreshes the EL's view and reconciles any change.
///
/// On a transient engine error (`Err`), the existing verdict is kept: the row
/// already reflects a real engine call, so a momentary engine outage must not
/// flip it.
///
/// A row that breaks the binding rules is invalid whatever the engine answers,
/// and the engine answers `Valid` for a genuine block that sits at another
/// height. Such a row never reaches the engine, so the engine cannot revive it
/// and its verdict is not read back as an engine verdict change.
async fn validate_undecided_blocks(
    height: Height,
    round: Round,
    previous_block: Option<&ExecutionBlock>,
    undecided_blocks: &impl UndecidedBlocksRepository,
    payload_validator: &impl PayloadValidator,
    invalid_payloads: &impl InvalidPayloadsRepository,
    metrics: &AppMetrics,
) -> eyre::Result<Vec<ConsensusBlock>> {
    let blocks = undecided_blocks
        .get_by_round(height, round)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to fetch undecided blocks for height {height} and round {round} \
                 from the state before sending them to execution client for validation"
            )
        })?;

    let mut validated_blocks = Vec::with_capacity(blocks.len());

    for mut block in blocks {
        let block_hash = block.self_reported_block_hash();
        let existing_validity = block.validity;

        info!(%height, %round, %block_hash, ?existing_validity, "Re-validating undecided block");

        if let Err(error) =
            check_payload_binding(&block.execution_payload, block.height, previous_block)
        {
            warn!(
                %height, %round, %block_hash, ?existing_validity, reason = %error,
                "Undecided block is not bound to its place in the chain; keeping it invalid",
            );

            // A row inserted by this binary already carries the verdict and its
            // forensic record. One that still reads `Valid` predates the rules.
            if existing_validity.is_valid() {
                block.validity = Validity::Invalid;

                metrics.inc_invalid_payloads_count(error.invalid_payload_source());

                persist_invalid_payload_best_effort(
                    invalid_payloads,
                    InvalidPayload::new_from_block(&block, &error.to_string()),
                    height,
                    round,
                    block.proposer,
                )
                .await;

                undecided_blocks
                    .store_undecided_block(block.clone())
                    .await
                    .wrap_err_with(|| {
                        format!(
                            "Failed to persist the binding verdict for undecided block \
                             {block_hash} at height={height} round={round}"
                        )
                    })?;
            }

            validated_blocks.push(block);
            continue;
        }

        match validate_consensus_block(payload_validator, &block, invalid_payloads, metrics).await {
            Ok(new_validity) => {
                if new_validity != existing_validity {
                    warn!(
                        %height, %round, %block_hash,
                        from = ?existing_validity, to = ?new_validity,
                        "Engine verdict changed on re-validation",
                    );
                    block.validity = new_validity;
                    undecided_blocks
                        .store_undecided_block(block.clone())
                        .await
                        .wrap_err_with(|| {
                            format!(
                                "Failed to persist re-validated undecided block {block_hash} \
                                 at height={height} round={round}"
                            )
                        })?;
                }
            }
            Err(e) => {
                warn!(
                    %height, %round, %block_hash, ?existing_validity,
                    "Re-validation failed transiently; keeping existing verdict: {e:#}",
                );
                metrics.inc_transient_validation_errors_count(
                    TransientValidationSource::StartedRoundRevalidation,
                );
            }
        }

        if !block.validity.is_valid() {
            warn!(%height, %round, %block_hash, "Undecided block is invalid");
        }

        validated_blocks.push(block);
    }

    Ok(validated_blocks)
}

/// Atomically removes pending proposal parts and stores the undecided block.
/// This ensures that if the process fails, the parts are not lost.
async fn remove_pending_parts_and_store_undecided_block(
    store: &Store,
    parts: ProposalParts,
    block: ConsensusBlock,
) -> eyre::Result<()> {
    let height = block.height;
    let round = block.round;
    let block_hash = block.self_reported_block_hash();

    store
        .remove_pending_parts_and_store_undecided_block(parts, block)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to atomically remove pending parts and store undecided block at height={}, round={}, block_hash={}",
                height, round, block_hash
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::payload::{MockPayloadValidator, PayloadValidationResult};
    use crate::store::repositories::mocks::{
        MockInvalidPayloadsRepository, MockUndecidedBlocksRepository,
    };
    use alloy_rpc_types_engine::{PayloadStatus, PayloadStatusEnum};
    use arc_eth_engine::engine::{MockEngineAPI, MockEthereumAPI};

    use std::sync::{Arc, Mutex};

    use alloy_rpc_types_engine::ExecutionPayloadV3;
    use arbitrary::{Arbitrary, Unstructured};
    use arc_consensus_db::{DbMetrics, DbUpgrade};
    use arc_consensus_types::proposer::RoundRobin;
    use arc_consensus_types::Validator;
    use arc_signer::local::{LocalSigningProvider, PrivateKey};
    use bytesize::ByteSize;
    use malachitebft_core_types::Validity;
    use tempfile::tempdir;

    use crate::handlers::test_utils::signed_parts_without_data;
    use crate::proposal_parts::make_proposal_parts;

    fn create_dummy_block(height: Height, round: Round, seed: u8) -> ConsensusBlock {
        let bytes = [seed; 1024];
        let mut u = Unstructured::new(&bytes);

        let proposer = Address::arbitrary(&mut u).unwrap();

        // The block number carries the height, so the block is bound to it.
        let mut execution_payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        execution_payload.payload_inner.payload_inner.block_number = height.as_u64();

        ConsensusBlock {
            height,
            round,
            valid_round: Round::Nil,
            proposer,
            validity: Validity::Valid,
            execution_payload,
            signature: None,
        }
    }

    async fn test_store() -> (Store, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let store = Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            ByteSize::mib(64),
        )
        .await
        .unwrap();
        (store, dir)
    }

    #[test]
    fn record_missed_rounds_counts_each_skipped_round() {
        // Advancing from a fresh height (previous round Nil) straight to round 3
        // means rounds 0, 1 and 2 were started but never decided: three missed
        // rounds, each attributed to that round's round-robin proposer. The round
        // that actually started (3) is not itself a miss.
        let keys: Vec<PrivateKey> = (0..4)
            .map(|_| PrivateKey::generate(rand::rngs::OsRng))
            .collect();
        let validator_set =
            ValidatorSet::new(keys.iter().map(|k| Validator::new(k.public_key(), 1)));
        let selector = RoundRobin;
        let height = Height::new(1);
        let metrics = AppMetrics::default();

        record_missed_rounds(
            &metrics,
            &selector,
            &validator_set,
            height,
            Round::Nil,
            Round::new(3),
        );

        // At height 1 the round-robin index is `round % 4`, so rounds 0..=3 map to
        // four distinct proposers; assert exactly one increment per skipped round.
        for missed in [Round::new(0), Round::new(1), Round::new(2)] {
            let proposer = selector
                .select_proposer(&validator_set, height, missed)
                .address;
            assert_eq!(
                metrics.get_consensus_round_missed_count(proposer),
                1,
                "round {missed} should be counted once against its proposer",
            );
        }

        let started = selector
            .select_proposer(&validator_set, height, Round::new(3))
            .address;
        assert_eq!(
            metrics.get_consensus_round_missed_count(started),
            0,
            "the round that actually started is not a missed round",
        );
    }

    #[test]
    fn record_missed_rounds_starting_round_zero_is_noop() {
        // Starting round 0 of a fresh height (previous round Nil) skips nothing.
        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator_set = ValidatorSet::new(vec![Validator::new(signing_key.public_key(), 1)]);
        let selector = RoundRobin;
        let height = Height::new(1);
        let metrics = AppMetrics::default();

        record_missed_rounds(
            &metrics,
            &selector,
            &validator_set,
            height,
            Round::Nil,
            Round::new(0),
        );

        let proposer = selector
            .select_proposer(&validator_set, height, Round::new(0))
            .address;
        assert_eq!(
            metrics.get_consensus_round_missed_count(proposer),
            0,
            "no round is skipped when starting at round 0",
        );
    }

    #[test]
    fn record_missed_rounds_same_round_is_noop() {
        // prev_round == new_round exercises the `missed_round < new_round` loop
        // boundary: re-entering the current round counts nothing.
        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator_set = ValidatorSet::new(vec![Validator::new(signing_key.public_key(), 1)]);
        let selector = RoundRobin;
        let height = Height::new(1);
        let metrics = AppMetrics::default();

        record_missed_rounds(
            &metrics,
            &selector,
            &validator_set,
            height,
            Round::new(2),
            Round::new(2),
        );

        let proposer = selector
            .select_proposer(&validator_set, height, Round::new(2))
            .address;
        assert_eq!(
            metrics.get_consensus_round_missed_count(proposer),
            0,
            "re-entering the same round is not a miss",
        );
    }

    #[tokio::test]
    async fn validate_undecided_blocks_all_valid() {
        // Stored rows already reflect a real Valid verdict from
        // process_pending_proposal_parts, so re-validation that returns Valid
        // does not write anything back.
        let height = Height::new(1);
        let round = Round::new(0);

        let block1 = create_dummy_block(height, round, 0x11);
        let block2 = create_dummy_block(height, round, 0x22);
        let blocks = vec![block1, block2];

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .returning(move |_, _| Ok(blocks.clone()));
        undecided.expect_store_undecided_block().times(0);

        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .times(2)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let result = validate_undecided_blocks(
            height, round, None, &undecided, &validator, &invalid, &metrics,
        )
        .await
        .expect("should succeed");

        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|b| b.validity == Validity::Valid));
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn validate_undecided_blocks_engine_flips_verdict_persists_change() {
        // If the engine's verdict on re-validation differs from the stored
        // row's validity, the new verdict is persisted. Verdicts that match
        // are not re-written.
        let height = Height::new(1);
        let round = Round::new(0);

        let block1 = create_dummy_block(height, round, 0x11);
        let block2 = create_dummy_block(height, round, 0x22);
        let blocks = vec![block1, block2];

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .returning(move |_, _| Ok(blocks.clone()));

        // Only block2 (flipped Valid -> Invalid) is persisted; block1 (Valid -> Valid) is not.
        let persisted = Arc::new(Mutex::new(Vec::<Validity>::new()));
        let persisted_clone = Arc::clone(&persisted);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(move |b| {
                persisted_clone.lock().unwrap().push(b.validity);
                Ok(())
            });

        let mut call_count = 0usize;
        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .times(2)
            .returning(move |_| {
                call_count += 1;
                if call_count == 1 {
                    Ok(PayloadValidationResult::Valid)
                } else {
                    Ok(PayloadValidationResult::Invalid {
                        reason: "bad block".into(),
                    })
                }
            });

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(1).returning(|_| Ok(()));

        let metrics = AppMetrics::default();
        let result = validate_undecided_blocks(
            height, round, None, &undecided, &validator, &invalid, &metrics,
        )
        .await
        .expect("should succeed");

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].validity, Validity::Valid);
        assert_eq!(result[1].validity, Validity::Invalid);
        assert_eq!(
            *persisted.lock().unwrap(),
            vec![Validity::Invalid],
            "only the block whose verdict changed should be persisted",
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn validate_undecided_blocks_empty() {
        let height = Height::new(1);
        let round = Round::new(0);

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided.expect_get_by_round().returning(|_, _| Ok(vec![]));

        let validator = MockPayloadValidator::new();
        let invalid = MockInvalidPayloadsRepository::new();

        let metrics = AppMetrics::default();
        let result = validate_undecided_blocks(
            height, round, None, &undecided, &validator, &invalid, &metrics,
        )
        .await
        .expect("should succeed");

        assert!(result.is_empty());
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn validate_undecided_blocks_repository_error() {
        let height = Height::new(1);
        let round = Round::new(0);

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .returning(|_, _| Err(std::io::Error::other("DB connection failed")));

        let validator = MockPayloadValidator::new();
        let invalid = MockInvalidPayloadsRepository::new();

        let metrics = AppMetrics::default();
        let err = validate_undecided_blocks(
            height, round, None, &undecided, &validator, &invalid, &metrics,
        )
        .await
        .expect_err("should propagate repository error");

        assert!(
            err.to_string().contains("Failed to fetch undecided blocks"),
            "error should describe the failure, got: {err}",
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn validate_undecided_blocks_validation_error_keeps_existing_verdict() {
        // When the engine call fails transiently during re-validation we keep
        // the stored row's existing verdict. The row already reflects a real
        // engine verdict set at insertion time by process_pending_proposal_parts,
        // so a momentary outage must not flip it.
        let height = Height::new(1);
        let round = Round::new(0);

        let block1 = create_dummy_block(height, round, 0x11);
        let block2 = create_dummy_block(height, round, 0x22);
        let blocks = vec![block1, block2];

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .returning(move |_, _| Ok(blocks.clone()));

        // Neither block triggers a store: the errored block keeps its verdict
        // (no write), the successful one re-validates to the same verdict (no write).
        undecided.expect_store_undecided_block().times(0);

        let mut call_count = 0usize;
        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .times(2)
            .returning(move |_| {
                call_count += 1;
                if call_count == 1 {
                    Err(eyre::eyre!("engine down"))
                } else {
                    Ok(PayloadValidationResult::Valid)
                }
            });

        // Engine `Err` is not a verdict, so no forensic record is written.
        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let result = validate_undecided_blocks(
            height, round, None, &undecided, &validator, &invalid, &metrics,
        )
        .await
        .expect("should succeed despite one block erroring");

        assert_eq!(result.len(), 2, "both blocks should be returned");
        assert_eq!(
            result[0].validity,
            Validity::Valid,
            "errored block keeps its existing Valid verdict — not flipped to Invalid",
        );
        assert_eq!(result[1].validity, Validity::Valid);
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
        assert_eq!(
            metrics.get_transient_validation_errors_count_by_source(
                TransientValidationSource::StartedRoundRevalidation
            ),
            1,
        );
    }

    #[tokio::test]
    async fn process_pending_proposal_parts_increments_on_assembly_failure() {
        let (store, _dir) = test_store().await;

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(1);
        let round = Round::new(0);
        let parts = signed_parts_without_data(height, round, &signing_key).await;

        let selector = RoundRobin;
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key));
        let metrics = AppMetrics::default();

        // Assembly fails before validation, so the engine is never called.
        let mut payload_validator = MockPayloadValidator::new();
        payload_validator.expect_validate_payload().times(0);
        let mut invalid_payloads = MockInvalidPayloadsRepository::new();
        invalid_payloads.expect_append().times(0);

        process_pending_proposal_parts(
            &store,
            vec![parts],
            height,
            round,
            &validator_set,
            &selector,
            &payload_validator,
            &invalid_payloads,
            &provider,
            &metrics,
            None,
        )
        .await
        .expect("should handle assembly failure gracefully");

        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    /// Builds a signed `ProposalParts` from a real `ConsensusBlock` whose
    /// proposer matches the single-validator set used by these tests.
    /// Returns the parts and the block hash so callers can query the store.
    async fn signed_parts_for_single_validator(
        height: Height,
        round: Round,
        signing_key: &PrivateKey,
        seed: u8,
    ) -> (ProposalParts, arc_consensus_types::BlockHash) {
        signed_parts_with_payload_height(height, height.as_u64(), round, signing_key, seed).await
    }

    /// Parts for `height` whose payload reports `payload_height` as its block
    /// number. The two differ only in a test of the binding rules.
    async fn signed_parts_with_payload_height(
        height: Height,
        payload_height: u64,
        round: Round,
        signing_key: &PrivateKey,
        seed: u8,
    ) -> (ProposalParts, arc_consensus_types::BlockHash) {
        let proposer = Address::from_public_key(&signing_key.public_key());
        let bytes = [seed; 1024];
        let mut u = Unstructured::new(&bytes);

        let mut execution_payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        execution_payload.payload_inner.payload_inner.block_number = payload_height;

        let block = ConsensusBlock {
            height,
            round,
            valid_round: Round::Nil,
            proposer,
            validity: Validity::Valid,
            execution_payload,
            signature: None,
        };
        let block_hash = block.self_reported_block_hash();

        let provider = LocalSigningProvider::new(signing_key.clone());
        let (raw_parts, _sig) = make_proposal_parts(&provider, &block).await.unwrap();
        (ProposalParts::new(raw_parts).unwrap(), block_hash)
    }

    #[tokio::test]
    async fn process_pending_proposal_parts_inserts_valid_block_and_clears_pending() {
        let (store, _dir) = test_store().await;

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(1);
        let round = Round::new(0);
        let (parts, block_hash) =
            signed_parts_for_single_validator(height, round, &signing_key, 0xAA).await;

        store
            .store_pending_proposal_parts(parts.clone(), 100, height)
            .await
            .unwrap();

        let selector = RoundRobin;
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key));
        let metrics = AppMetrics::default();

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .times(1)
            .returning(|_| Ok(PayloadValidationResult::Valid));
        let mut invalid_payloads = MockInvalidPayloadsRepository::new();
        invalid_payloads.expect_append().times(0);

        process_pending_proposal_parts(
            &store,
            vec![parts],
            height,
            round,
            &validator_set,
            &selector,
            &payload_validator,
            &invalid_payloads,
            &provider,
            &metrics,
            None,
        )
        .await
        .expect("validate-and-insert should succeed");

        let stored = store
            .get_undecided_block(height, round, block_hash)
            .await
            .unwrap()
            .expect("undecided block should be stored");
        assert_eq!(stored.validity, Validity::Valid);

        let remaining = store
            .get_pending_proposal_parts(height, round)
            .await
            .unwrap();
        assert!(remaining.is_empty(), "pending parts must be removed");
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    /// The whole round-start sequence, which is where a rejection has to survive:
    /// `process_pending_proposal_parts` writes the binding verdict, and
    /// `validate_undecided_blocks` reads the same row back two statements later.
    /// The engine answers `VALID` here, because the payload is a genuine block
    /// that sits at another height. Consensus must still receive `Invalid`.
    #[tokio::test]
    async fn fetch_and_process_pending_proposals_keeps_an_unbound_payload_invalid() {
        let (store, _dir) = test_store().await;

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(11);
        let round = Round::new(0);
        let (parts, _block_hash) =
            signed_parts_with_payload_height(height, 5, round, &signing_key, 0xAA).await;

        store
            .store_pending_proposal_parts(parts, 100, height)
            .await
            .unwrap();

        let mut engine_mock = MockEngineAPI::new();
        engine_mock.expect_new_payload().returning(|_, _, _, _| {
            Ok(PayloadStatus {
                status: PayloadStatusEnum::Valid,
                latest_valid_hash: None,
            })
        });
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        let selector = RoundRobin;
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key));
        let metrics = AppMetrics::default();

        let proposals = fetch_and_process_pending_proposals(
            height,
            round,
            &validator_set,
            &selector,
            &store,
            &engine,
            &provider,
            &metrics,
            None,
        )
        .await
        .expect("round start should succeed");

        assert_eq!(proposals.len(), 1);
        assert_eq!(
            proposals[0].validity,
            Validity::Invalid,
            "the engine verdict must not revive a binding rejection",
        );
    }

    /// A proposal whose parts arrived before its round started (buffered as
    /// pending) is still prevoted nil when its timestamp is too far ahead of local
    /// time, while the stored block keeps its execution-only `Valid` verdict — so it
    /// stays adoptable via a commit certificate. Mirrors the live-arrival path and
    /// closes the early-arrival bypass.
    #[tokio::test]
    async fn fetch_and_process_pending_proposals_votes_nil_on_future_timestamp_but_stores_valid() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let (store, _dir) = test_store().await;

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(11);
        let round = Round::new(0);

        // A block bound to the height (execution-valid) but stamped far ahead of
        // local time, streamed as pending parts.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let proposer = Address::from_public_key(&signing_key.public_key());
        let mut u = Unstructured::new(&[0xAB; 1024]);
        let mut execution_payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        execution_payload.payload_inner.payload_inner.block_number = height.as_u64();
        execution_payload.payload_inner.payload_inner.timestamp = now + 3600;
        let block = ConsensusBlock {
            height,
            round,
            valid_round: Round::Nil,
            proposer,
            validity: Validity::Valid,
            execution_payload,
            signature: None,
        };
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key.clone()));
        let (raw_parts, _sig) = make_proposal_parts(&provider, &block).await.unwrap();
        let parts = ProposalParts::new(raw_parts).unwrap();

        store
            .store_pending_proposal_parts(parts, 100, height)
            .await
            .unwrap();

        let mut engine_mock = MockEngineAPI::new();
        engine_mock.expect_new_payload().returning(|_, _, _, _| {
            Ok(PayloadStatus {
                status: PayloadStatusEnum::Valid,
                latest_valid_hash: None,
            })
        });
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        let selector = RoundRobin;
        let metrics = AppMetrics::default();

        let proposals = fetch_and_process_pending_proposals(
            height,
            round,
            &validator_set,
            &selector,
            &store,
            &engine,
            &provider,
            &metrics,
            None,
        )
        .await
        .expect("round start should succeed");

        assert_eq!(proposals.len(), 1);
        assert_eq!(
            proposals[0].validity,
            Validity::Invalid,
            "a too-far-future pending proposal must be prevoted nil",
        );

        let stored = store.get_by_round(height, round).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].validity,
            Validity::Valid,
            "the stored block keeps its execution-only Valid verdict",
        );
    }

    /// A row that predates the rules reads `Valid` and breaks them. The engine
    /// answers `VALID`, and the verdict is still corrected and persisted.
    #[tokio::test]
    async fn validate_undecided_blocks_corrects_a_stored_valid_unbound_row() {
        let height = Height::new(11);
        let round = Round::new(0);

        let mut block = create_dummy_block(height, round, 0xCC);
        block
            .execution_payload
            .payload_inner
            .payload_inner
            .block_number = 5;

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .return_once(move |_, _| Ok(vec![block.clone()]));
        undecided
            .expect_store_undecided_block()
            .times(1)
            .withf(|b: &ConsensusBlock| b.validity == Validity::Invalid)
            .returning(|_| Ok(()));

        let mut validator = MockPayloadValidator::new();
        validator.expect_validate_payload().times(0);

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid
            .expect_append()
            .times(1)
            .withf(|ip: &InvalidPayload| ip.reason.contains("does not match consensus height"))
            .returning(|_| Ok(()));

        let metrics = AppMetrics::default();
        let blocks = validate_undecided_blocks(
            height, round, None, &undecided, &validator, &invalid, &metrics,
        )
        .await
        .expect("should succeed");

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].validity, Validity::Invalid);
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    /// The parent rule through the whole round-start sequence. This is the branch
    /// where `previous_block` threading into the re-validation step matters.
    #[tokio::test]
    async fn fetch_and_process_pending_proposals_keeps_a_parent_mismatch_invalid() {
        let (store, _dir) = test_store().await;

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(11);
        let round = Round::new(0);
        // Bound to the height, so only the parent rule can reject it.
        let (parts, _block_hash) =
            signed_parts_with_payload_height(height, height.as_u64(), round, &signing_key, 0xDD)
                .await;

        store
            .store_pending_proposal_parts(parts, 100, height)
            .await
            .unwrap();

        let mut engine_mock = MockEngineAPI::new();
        engine_mock.expect_new_payload().times(0);
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        // The immediate predecessor, and not the block the payload extends.
        let previous_block = ExecutionBlock {
            block_hash: arc_consensus_types::B256::repeat_byte(0xAB),
            block_number: 10,
            parent_hash: arc_consensus_types::B256::ZERO,
            timestamp: 0,
        };

        let selector = RoundRobin;
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key));
        let metrics = AppMetrics::default();

        let proposals = fetch_and_process_pending_proposals(
            height,
            round,
            &validator_set,
            &selector,
            &store,
            &engine,
            &provider,
            &metrics,
            Some(&previous_block),
        )
        .await
        .expect("round start should succeed");

        assert_eq!(proposals.len(), 1);
        assert_eq!(
            proposals[0].validity,
            Validity::Invalid,
            "the engine verdict must not revive a parent-rule rejection",
        );
        assert_eq!(
            metrics.get_invalid_payloads_count_by_source(InvalidPayloadSource::PayloadParent),
            1,
            "the parent rule must be the rule that rejected it",
        );
    }

    /// Pending parts whose payload belongs at another height: the row lands as
    /// `Invalid` and the engine never sees the payload.
    #[tokio::test]
    async fn process_pending_proposal_parts_rejects_a_payload_from_another_height() {
        let (store, _dir) = test_store().await;

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(11);
        let round = Round::new(0);
        let (parts, block_hash) =
            signed_parts_with_payload_height(height, 5, round, &signing_key, 0xAA).await;

        store
            .store_pending_proposal_parts(parts.clone(), 100, height)
            .await
            .unwrap();

        let selector = RoundRobin;
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key));
        let metrics = AppMetrics::default();

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator.expect_validate_payload().times(0);

        let mut invalid_payloads = MockInvalidPayloadsRepository::new();
        invalid_payloads
            .expect_append()
            .times(1)
            .withf(|ip: &InvalidPayload| ip.reason.contains("does not match consensus height"))
            .returning(|_| Ok(()));

        process_pending_proposal_parts(
            &store,
            vec![parts],
            height,
            round,
            &validator_set,
            &selector,
            &payload_validator,
            &invalid_payloads,
            &provider,
            &metrics,
            None,
        )
        .await
        .expect("a binding error is a verdict, not a failure");

        let stored = store
            .get_undecided_block(height, round, block_hash)
            .await
            .unwrap()
            .expect("undecided block should be stored");
        assert_eq!(stored.validity, Validity::Invalid);
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn process_pending_proposal_parts_inserts_invalid_block_and_clears_pending() {
        let (store, _dir) = test_store().await;

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(1);
        let round = Round::new(0);
        let (parts, block_hash) =
            signed_parts_for_single_validator(height, round, &signing_key, 0xBB).await;

        store
            .store_pending_proposal_parts(parts.clone(), 100, height)
            .await
            .unwrap();

        let selector = RoundRobin;
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key));
        let metrics = AppMetrics::default();

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .times(1)
            .returning(|_| {
                Ok(PayloadValidationResult::Invalid {
                    reason: "engine rejected".into(),
                })
            });
        let mut invalid_payloads = MockInvalidPayloadsRepository::new();
        invalid_payloads
            .expect_append()
            .times(1)
            .returning(|_| Ok(()));

        process_pending_proposal_parts(
            &store,
            vec![parts],
            height,
            round,
            &validator_set,
            &selector,
            &payload_validator,
            &invalid_payloads,
            &provider,
            &metrics,
            None,
        )
        .await
        .expect("validate-and-insert should succeed with Invalid verdict");

        let stored = store
            .get_undecided_block(height, round, block_hash)
            .await
            .unwrap()
            .expect("undecided block should be stored");
        assert_eq!(stored.validity, Validity::Invalid);

        let remaining = store
            .get_pending_proposal_parts(height, round)
            .await
            .unwrap();
        assert!(remaining.is_empty(), "pending parts must be removed");
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn process_pending_proposal_parts_skips_insert_on_transient_engine_error() {
        let (store, _dir) = test_store().await;

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(1);
        let round = Round::new(0);
        let (parts, block_hash) =
            signed_parts_for_single_validator(height, round, &signing_key, 0xCC).await;

        store
            .store_pending_proposal_parts(parts.clone(), 100, height)
            .await
            .unwrap();

        let selector = RoundRobin;
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key));
        let metrics = AppMetrics::default();

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .times(1)
            .returning(|_| Err(eyre::eyre!("engine unreachable")));
        let mut invalid_payloads = MockInvalidPayloadsRepository::new();
        invalid_payloads.expect_append().times(0);

        process_pending_proposal_parts(
            &store,
            vec![parts],
            height,
            round,
            &validator_set,
            &selector,
            &payload_validator,
            &invalid_payloads,
            &provider,
            &metrics,
            None,
        )
        .await
        .expect("transient engine error must not fail the handler");

        let stored = store
            .get_undecided_block(height, round, block_hash)
            .await
            .unwrap();
        assert!(
            stored.is_none(),
            "no undecided row should be inserted when the engine errors",
        );

        let remaining = store
            .get_pending_proposal_parts(height, round)
            .await
            .unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "pending parts must stay in place for cleanup on commit",
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
        assert_eq!(
            metrics.get_transient_validation_errors_count_by_source(
                TransientValidationSource::StartedRoundPending
            ),
            1,
        );
    }

    #[tokio::test]
    async fn validate_undecided_blocks_propagates_persist_error() {
        let height = Height::new(1);
        let round = Round::new(0);

        // Stored block is Valid; re-validation returns Invalid (verdict
        // change) so persist is attempted and fails — the error must propagate.
        let block = create_dummy_block(height, round, 0x33);
        let blocks = vec![block];

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .returning(move |_, _| Ok(blocks.clone()));
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(|_| Err(std::io::Error::other("disk full")));

        let mut validator = MockPayloadValidator::new();
        validator.expect_validate_payload().times(1).returning(|_| {
            Ok(PayloadValidationResult::Invalid {
                reason: "engine rejected".into(),
            })
        });

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(1).returning(|_| Ok(()));

        let metrics = AppMetrics::default();
        let err = validate_undecided_blocks(
            height, round, None, &undecided, &validator, &invalid, &metrics,
        )
        .await
        .expect_err("persist error should propagate");

        assert!(
            err.to_string()
                .contains("Failed to persist re-validated undecided block"),
            "error should describe the persist failure, got: {err}",
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }
}
