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
use arc_signer::ArcSigningProvider;

use crate::block::ConsensusBlock;
use crate::metrics::{AppMetrics, InvalidPayloadSource};
use crate::payload::{validate_consensus_block, EnginePayloadValidator, PayloadValidator};
use crate::proposal_parts::{
    assemble_block_from_parts, resolve_expected_proposer, validate_proposal_parts,
};
use crate::state::State;
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

    fetch_and_process_pending_proposals(
        height,
        round,
        state.validator_set(),
        &state.ctx.proposer_selector,
        state.store(),
        engine,
        state.signing_provider(),
        state.metrics(),
    )
    .await
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
) -> eyre::Result<Vec<ProposedValue<ArcContext>>> {
    let pending_parts = store
        .get_pending_proposal_parts(height, round)
        .await
        .wrap_err("failed to fetch pending proposal parts")?;

    info!(%height, %round, "StartedRound: Found {} pending proposal parts", pending_parts.len());

    // Convert the pending proposal parts for the current round,
    // into blocks and add them to undecided blocks table.
    process_pending_proposal_parts(
        store,
        pending_parts,
        height,
        round,
        validator_set,
        proposer_selector,
        signing_provider,
        metrics,
    )
    .await
    .wrap_err("Failed to validate pending proposal parts")?;

    let blocks = validate_undecided_blocks(
        height,
        round,
        store,
        &EnginePayloadValidator::new(engine, metrics),
        store,
        metrics,
    )
    .await
    .wrap_err("failed to validate undecided blocks")?;

    Ok(blocks.iter().map(ProposedValue::from).collect())
}

/// Process the pending proposal parts for the current height, assembling them
/// into blocks and moving the blocks to the undecided table.
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
    signing_provider: &ArcSigningProvider,
    metrics: &AppMetrics,
) -> eyre::Result<()> {
    for parts in pending_parts {
        let (height, round, proposer) = (parts.height(), parts.round(), parts.proposer());

        debug_assert_eq!(height, current_height, "Pending parts height mismatch");
        debug_assert_eq!(round, current_round, "Pending parts round mismatch");

        let expected_proposer = resolve_expected_proposer(proposer_selector, validator_set, &parts);

        if !validate_proposal_parts(&parts, expected_proposer, signing_provider).await {
            continue;
        }

        // NOTE: The block is initially assigned a default validity status
        // (i.e., `Validity::Valid`), even though it has not yet been validated
        // by the execution client.
        // By inserting this block into the undecided blocks table, we are
        // temporarily violating the assumption that all blocks in that table
        // have been validated at least once by the execution client.
        // This temporary inconsistency is acceptable here because all blocks
        // in the undecided table are immediately validated by the subsequent
        // `validate_undecided_blocks` in `AppMsg::StartedRound` handler.
        match assemble_block_from_parts(&parts) {
            Ok(block) => {
                info!(%height, %round, %proposer, "Added pending block to undecided");

                // Atomically remove from pending and store as undecided
                // This ensures that if the process fails, the parts are not lost
                remove_pending_parts_and_store_undecided_block(store, parts, block).await?;
            }
            Err(e) => {
                warn!(%height, %round, %proposer, "Failed to assemble block from pending parts: {e}");
                metrics.inc_invalid_payloads_count(InvalidPayloadSource::AssemblyFailure);
                let invalid_payload = InvalidPayload::new_from_parts(&parts, &e.to_string());
                store.append_invalid_payload(invalid_payload).await.wrap_err_with(|| {
                    format!(
                        "Failed to store invalid payload after assembling block from pending parts (height={height}, round={round}, proposer={proposer})",
                    )
                })?;
            }
        }
    }

    Ok(())
}

/// Sends all undecided blocks for the given height and round to the execution
/// client, ensuring the client has the corresponding payloads locally.
/// This is important in two scenarios:
/// 1. when validating newly created undecided blocks reconstructed from proposal
///    parts.
/// 2. when re-validating undecided blocks after a crash or restart.
///
/// The second case addresses the EL "amnesia" issue, where the execution client may
/// have forgotten previously validated payloads that were only stored in memory and
/// lost after a restart.
///
/// After each block is validated, the engine's verdict is persisted back to the
/// undecided blocks table.
async fn validate_undecided_blocks(
    height: Height,
    round: Round,
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

    // Holds all blocks that were validated (either valid or invalid)
    let mut validated_blocks = Vec::with_capacity(blocks.len());

    for mut block in blocks {
        let block_hash = block.block_hash();

        info!(%height, %round, %block_hash, "Validating undecided block");

        let validity = match validate_consensus_block(
            payload_validator,
            &block,
            invalid_payloads,
            metrics,
        )
        .await
        {
            Ok(validity) => validity,
            Err(e) => {
                error!(%height, %round, %block_hash, "Failed to validate undecided block, marking Invalid: {e}");
                Validity::Invalid
            }
        };

        block.validity = validity;

        // Persist the engine's verdict before returning the block to consensus.
        undecided_blocks
            .store_undecided_block(block.clone())
            .await
            .wrap_err_with(|| {
                format!(
                    "Failed to persist validated undecided block {block_hash} \
                     at height={height} round={round}"
                )
            })?;

        validated_blocks.push(block);

        if !validity.is_valid() {
            // It is possible that we had multiple blocks before restart,
            // and one or more of them are invalid. We continue to the next block.
            warn!(%height, %round, %block_hash, "Undecided block is invalid");
        }
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
    let block_hash = block.block_hash();

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

    fn create_dummy_block(height: Height, round: Round, seed: u8) -> ConsensusBlock {
        let bytes = [seed; 1024];
        let mut u = Unstructured::new(&bytes);

        ConsensusBlock {
            height,
            round,
            valid_round: Round::Nil,
            proposer: Address::arbitrary(&mut u).unwrap(),
            validity: Validity::Valid,
            execution_payload: ExecutionPayloadV3::arbitrary(&mut u).unwrap(),
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
        let height = Height::new(1);
        let round = Round::new(0);

        let block1 = create_dummy_block(height, round, 0x11);
        let block2 = create_dummy_block(height, round, 0x22);
        let blocks = vec![block1, block2];

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .returning(move |_, _| Ok(blocks.clone()));
        undecided
            .expect_store_undecided_block()
            .times(2)
            .withf(|b| b.validity == Validity::Valid)
            .returning(|_| Ok(()));

        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .times(2)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let result =
            validate_undecided_blocks(height, round, &undecided, &validator, &invalid, &metrics)
                .await
                .expect("should succeed");

        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|b| b.validity == Validity::Valid));
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn validate_undecided_blocks_mixed_validity() {
        let height = Height::new(1);
        let round = Round::new(0);

        let block1 = create_dummy_block(height, round, 0x11);
        let block2 = create_dummy_block(height, round, 0x22);
        let blocks = vec![block1, block2];

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .returning(move |_, _| Ok(blocks.clone()));

        // Record the validity of every block persisted, in call order.
        let persisted = Arc::new(Mutex::new(Vec::<Validity>::new()));
        let persisted_clone = Arc::clone(&persisted);
        undecided
            .expect_store_undecided_block()
            .times(2)
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
        let result =
            validate_undecided_blocks(height, round, &undecided, &validator, &invalid, &metrics)
                .await
                .expect("should succeed");

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].validity, Validity::Valid);
        assert_eq!(result[1].validity, Validity::Invalid);
        assert_eq!(
            *persisted.lock().unwrap(),
            vec![Validity::Valid, Validity::Invalid],
            "persisted validity should match engine verdict, not the placeholder"
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
        let result =
            validate_undecided_blocks(height, round, &undecided, &validator, &invalid, &metrics)
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
        let err =
            validate_undecided_blocks(height, round, &undecided, &validator, &invalid, &metrics)
                .await
                .expect_err("should propagate repository error");

        assert!(
            err.to_string().contains("Failed to fetch undecided blocks"),
            "error should describe the failure, got: {err}",
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn validate_undecided_blocks_validation_error_marks_invalid() {
        // When the engine call fails (transport error, SYNCING/ACCEPTED, etc.)
        // we treat the block as `Invalid` for the current round so the placeholder
        // `Valid` written by `process_pending_proposal_parts` does not leak.
        // Malachite's `FullProposalKeeper::handle_validity_change` rejects any
        // subsequent `Valid -> Invalid` flip on the same WAL entry.
        let height = Height::new(1);
        let round = Round::new(0);

        let block1 = create_dummy_block(height, round, 0x11);
        let block2 = create_dummy_block(height, round, 0x22);
        let blocks = vec![block1, block2];

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round()
            .returning(move |_, _| Ok(blocks.clone()));

        // Both blocks must be persisted: the errored block with `Invalid`,
        // the successful one with `Valid`. Order matches the input order.
        let persisted = Arc::new(Mutex::new(Vec::<Validity>::new()));
        let persisted_clone = Arc::clone(&persisted);
        undecided
            .expect_store_undecided_block()
            .times(2)
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
                    Err(eyre::eyre!("engine down"))
                } else {
                    Ok(PayloadValidationResult::Valid)
                }
            });

        // Engine `Err` is not a verdict, so no forensic record is written.
        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let result =
            validate_undecided_blocks(height, round, &undecided, &validator, &invalid, &metrics)
                .await
                .expect("should succeed despite one block erroring");

        assert_eq!(result.len(), 2, "both blocks should be returned");
        assert_eq!(result[0].validity, Validity::Invalid);
        assert_eq!(result[1].validity, Validity::Valid);
        assert_eq!(
            *persisted.lock().unwrap(),
            vec![Validity::Invalid, Validity::Valid],
            "errored block must be persisted as Invalid, not left with placeholder Valid",
        );
        // Engine failure is not counted as an engine rejection; the metric
        // tracks `EngineReject` and `AssemblyFailure`, not transport errors.
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
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

        process_pending_proposal_parts(
            &store,
            vec![parts],
            height,
            round,
            &validator_set,
            &selector,
            &provider,
            &metrics,
        )
        .await
        .expect("should handle assembly failure gracefully");

        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn validate_undecided_blocks_propagates_persist_error() {
        let height = Height::new(1);
        let round = Round::new(0);

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
        validator
            .expect_validate_payload()
            .times(1)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let invalid = MockInvalidPayloadsRepository::new();

        let metrics = AppMetrics::default();
        let err =
            validate_undecided_blocks(height, round, &undecided, &validator, &invalid, &metrics)
                .await
                .expect_err("persist error should propagate");

        assert!(
            err.to_string()
                .contains("Failed to persist validated undecided block"),
            "error should describe the persist failure, got: {err}",
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }
}
