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

use eyre::{eyre, Context};
use tracing::{error, info, warn};

use malachitebft_app_channel::app::consensus::Role;
use malachitebft_app_channel::app::types::ProposedValue;
use malachitebft_app_channel::Reply;

use arc_consensus_types::{Address, ArcContext, Height, ProposalParts, Round, Validator};
use arc_eth_engine::engine::Engine;
use arc_signer::ArcSigningProvider;

use crate::block::ConsensusBlock;
use crate::metrics::AppMetrics;
use crate::payload::{validate_consensus_block, EnginePayloadValidator};
use crate::proposal_parts::{assemble_block_from_parts, validate_proposal_parts};
use crate::state::State;
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

    state.current_round = round;
    state.current_proposer = Some(proposer);

    let proposer = state
        .validator_set()
        .get_by_address(&proposer)
        .ok_or_else(|| {
            eyre!(
                "Current proposer not found in validator set for height {height}, \
                 cannot process pending proposal parts"
            )
        })?;

    fetch_and_process_pending_proposals(
        height,
        round,
        proposer,
        state.store(),
        engine,
        state.signing_provider(),
        state.metrics(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn fetch_and_process_pending_proposals(
    height: Height,
    round: Round,
    proposer: &Validator,
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
        proposer,
        signing_provider,
    )
    .await
    .wrap_err("Failed to validate pending proposal parts")?;

    let blocks = validate_undecided_blocks(height, round, store, engine, metrics)
        .await
        .wrap_err("failed to validate undecided blocks")?;

    Ok(blocks.iter().map(ProposedValue::from).collect())
}

/// Process the pending proposal parts for the current height, assembling them
/// into blocks and moving the blocks to the undecided table.
///
/// ## Important
/// This function assumes that the pending parts are for the current height and round.
async fn process_pending_proposal_parts(
    store: &Store,
    pending_parts: Vec<ProposalParts>,
    current_height: Height,
    current_round: Round,
    current_proposer: &Validator,
    signing_provider: &ArcSigningProvider,
) -> eyre::Result<()> {
    for parts in pending_parts {
        let (height, round, proposer) = (parts.height(), parts.round(), parts.proposer());

        debug_assert_eq!(height, current_height, "Pending parts height mismatch");
        debug_assert_eq!(round, current_round, "Pending parts round mismatch");

        // Validate the proposal parts, ensuring they are from the expected proposer
        // for their height/round, and have a valid signature.
        if !validate_proposal_parts(&parts, current_proposer, signing_provider).await {
            continue;
        }

        // NOTE: The block is initially assigned a default validity status
        // (i.e., `Validity::VALID`), even though it has not yet been validated
        // by the execution client.
        // By inserting this block into the undecided blocks table, we are
        // temporarily violating the assumption that all blocks in that table
        // have been validated at least once by the execution client.
        // This temporary inconsistency is acceptable here because all blocks
        // in the undecided table are immediately validated right after this
        // call, in `AppMsg::StartedRound` (see `app.rs`).
        match assemble_block_from_parts(&parts) {
            Ok(block) => {
                info!(%height, %round, %proposer, "Added pending block to undecided");

                // Atomically remove from pending and store as undecided
                // This ensures that if the process fails, the parts are not lost
                remove_pending_parts_and_store_undecided_block(store, parts, block).await?;
            }
            Err(e) => {
                let invalid_payload = InvalidPayload::new_from_parts(&parts, &e.to_string());
                store.append_invalid_payload(invalid_payload).await.wrap_err_with(|| {
                    format!(
                        "Failed to store invalid payload after assembling block from pending parts (height={height}, round={round}, proposer={proposer})",
                    )
                })?;
                warn!(%height, %round, %proposer, "Failed to assemble block from pending parts: {e}");
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
async fn validate_undecided_blocks(
    height: Height,
    round: Round,
    store: &Store,
    engine: &Engine,
    metrics: &AppMetrics,
) -> eyre::Result<Vec<ConsensusBlock>> {
    let undecided_blocks = store
        .get_undecided_blocks(height, round)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to fetch undecided blocks for height {height} and round {round} \
                 from the state before sending them to execution client for validation"
            )
        })?;

    // Holds all blocks that were validated (either valid or invalid)
    let mut validated_blocks = Vec::with_capacity(undecided_blocks.len());
    let validator = EnginePayloadValidator::new(engine, metrics);

    for mut block in undecided_blocks {
        let block_hash = block.block_hash();

        info!(%height, %round, %block_hash, "Validating undecided block");

        let validity = match validate_consensus_block(&validator, &block, store).await {
            Ok(validity) => validity,
            Err(e) => {
                error!(%height, %round, %block_hash, "Failed to validate undecided block: {e}");
                continue;
            }
        };

        // Update the block validity
        block.validity = validity;

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
