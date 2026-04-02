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

use eyre::Context as _;
use tracing::{debug, error, info, warn};

use malachitebft_app_channel::app::streaming::{StreamContent, StreamMessage};
use malachitebft_app_channel::app::types::core::Validity;
use malachitebft_app_channel::app::types::{PeerId, ProposedValue};
use malachitebft_app_channel::Reply;
use malachitebft_core_types::Height as _;

use arc_consensus_types::proposer::ProposerSelector;
use arc_consensus_types::{ArcContext, Height, ProposalPart, ProposalParts, Round, ValidatorSet};
use arc_eth_engine::engine::Engine;
use arc_signer::ArcSigningProvider;

use crate::block::ConsensusBlock;
use crate::metrics::AppMetrics;
use crate::payload::{validate_consensus_block, EnginePayloadValidator};
use crate::proposal_parts::{assemble_block_from_parts, validate_proposal_parts};
use crate::state::State;
use crate::store::Store;
use crate::streaming::PartStreamsMap;
use arc_consensus_db::invalid_payloads::InvalidPayload;

/// Handles the `ReceivedProposalPart` message from the consensus engine.
///
/// This is called when a proposal part is received from a peer.
/// The application processes the received part, and if the full proposal
/// has been reconstructed, it validates the payload.
/// If the block is valid, it returns the `ProposedValue` to the consensus engine.
/// If the block is invalid, it logs an error.
/// In both cases, the complete block is stored for future use once consensus
/// reaches that height.
pub async fn handle(
    state: &mut State,
    engine: &Engine,
    from: PeerId,
    part: StreamMessage<ProposalPart>,
    reply: Reply<Option<ProposedValue<ArcContext>>>,
) {
    let max_pending_proposals = state.max_pending_proposals();
    let current_height = state.current_height;
    let current_round = state.current_round;
    let current_validator_set = state.validator_set().clone();
    let proposer_selector = state.ctx.proposer_selector;

    let context = HandlerContext {
        engine,
        store: state.store().clone(),
        metrics: state.metrics().clone(),
        signing_provider: state.signing_provider().clone(),
        streams_map: state.streams_map_mut(),
        current_height,
        current_round,
        current_validator_set,
        proposer_selector: &proposer_selector,
        max_pending_proposals,
    };

    let response = on_received_proposal_part(context, from, part)
        .await
        .inspect_err(|e| {
            error!(%from, "🔴 Error processing proposal part: {e:#}");
        })
        .unwrap_or(None);

    if let Some(proposed_value) = &response {
        record_proposal_in_monitor(state, proposed_value);
    }

    if let Err(e) = reply.send(response) {
        error!("🔴 ReceivedProposalPart: Failed to send reply: {e:?}");
    }
}

/// Records the proposal receipt in the proposal monitor.
fn record_proposal_in_monitor(state: &mut State, proposed_value: &ProposedValue<ArcContext>) {
    let current_round = state.current_round;
    if current_round.as_i64() != 0 {
        // We only monitor round 0
        return;
    }

    let Some(monitor) = &mut state.proposal_monitor else {
        warn!(
            %proposed_value.height,
            %proposed_value.round,
            %proposed_value.proposer,
            "No proposal monitor present",
        );
        return;
    };

    // Sanity checks - should always hold
    if monitor.height != proposed_value.height || monitor.proposer != proposed_value.proposer {
        warn!(
            monitor.height = %monitor.height,
            monitor.proposer = %monitor.proposer,
            %proposed_value.height,
            %proposed_value.proposer,
            "Proposal monitor mismatch, skipping recording",
        );
        return;
    }

    if proposed_value.round.as_i64() != 0 {
        warn!(
            proposed_value.round = %proposed_value.round,
            "Received proposed value not in round 0",
        );
        return;
    }

    monitor.record_proposal(proposed_value.value.id());
}

struct HandlerContext<'a, 'b> {
    engine: &'a Engine,
    store: Store,
    metrics: AppMetrics,
    signing_provider: ArcSigningProvider,
    streams_map: &'b mut PartStreamsMap,
    current_height: Height,
    current_round: Round,
    current_validator_set: ValidatorSet,
    proposer_selector: &'a dyn ProposerSelector,
    max_pending_proposals: usize,
}

async fn on_received_proposal_part(
    context: HandlerContext<'_, '_>,
    from: PeerId,
    part: StreamMessage<ProposalPart>,
) -> eyre::Result<Option<ProposedValue<ArcContext>>> {
    let (part_type, part_size) = match &part.content {
        StreamContent::Data(part) => (part.get_type(), part.size_bytes()),
        StreamContent::Fin => ("end of stream", 0),
    };

    info!(
        %from, %part.sequence, part.type = %part_type, part.size = %part_size, stream_id = %part.stream_id,
        "Received proposal part"
    );

    // Check if we have a full proposal
    let Some(parts) = context.streams_map.insert(from, part) else {
        return Ok(None);
    };

    // Process complete proposal parts, validate and assemble them into a block.
    let block = process_proposal_parts((&context).into(), parts).await?;

    let Some(mut block) = block else {
        return Ok(None);
    };

    // Validate the block
    validate_block(
        context.engine,
        &context.metrics,
        &context.store,
        &mut block,
        from,
    )
    .await?;

    let proposed_value = ProposedValue::from(&block);

    debug!(
        block_size = %block.size_bytes(),
        payload_size = %block.payload_size(),
        "🎁 Received complete proposal: {proposed_value:?}",
    );

    // Store the full undecided block in the store
    let block_hash = block.block_hash();

    context.store.store_undecided_block(block).await.wrap_err_with(||
        format!(
            "Failed to store undecided block {} built from parts received from {} for height={}, round={}, proposer={}",
            block_hash, from, proposed_value.height, proposed_value.round, proposed_value.proposer,
        )
    )?;

    Ok(Some(proposed_value))
}

/// Validates a block received from a peer via the Engine API and records
/// the result. If the engine rejects the payload, an [`InvalidPayload`]
/// record is persisted by [`validate_consensus_block`] and the block's
/// validity is set to [`Validity::Invalid`]. The block is kept either way
/// so that consensus can proceed with the correct validity information.
async fn validate_block(
    engine: &Engine,
    metrics: &AppMetrics,
    store: &Store,
    block: &mut ConsensusBlock,
    from: PeerId,
) -> eyre::Result<()> {
    let validator = EnginePayloadValidator::new(engine, metrics);
    let validity = validate_consensus_block(&validator, block, store)
        .await
        .wrap_err_with(|| {
            format!(
                "Payload validation failed on block built after \
                 receiving proposal part at height={}, round={} from {}",
                block.height, block.round, from,
            )
        })?;

    match validity {
        Validity::Invalid => {
            error!("❌ Received invalid block: {}", block.block_hash());
        }
        Validity::Valid => {
            debug!("✅ Received valid block: {}", block.block_hash());
        }
    }

    // Update the block validity
    block.validity = validity;

    Ok(())
}

struct ProcessingContext<'a> {
    store: &'a Store,
    metrics: &'a AppMetrics,
    signing_provider: &'a ArcSigningProvider,
    current_height: Height,
    current_round: Round,
    current_validator_set: &'a ValidatorSet,
    proposer_selector: &'a dyn ProposerSelector,
    max_pending_proposals: usize,
}

impl<'a> From<&'a HandlerContext<'_, '_>> for ProcessingContext<'a> {
    fn from(handler_ctx: &'a HandlerContext<'_, '_>) -> Self {
        Self {
            store: &handler_ctx.store,
            metrics: &handler_ctx.metrics,
            signing_provider: &handler_ctx.signing_provider,
            current_height: handler_ctx.current_height,
            current_round: handler_ctx.current_round,
            current_validator_set: &handler_ctx.current_validator_set,
            proposer_selector: handler_ctx.proposer_selector,
            max_pending_proposals: handler_ctx.max_pending_proposals,
        }
    }
}

/// Process complete proposal parts, validating and assembling them into a block.
///
/// - If the parts are for a past height, they are ignored.
/// - If the parts are for a future height, they are stored in pending without validation.
/// - If the parts are for the current height, they are validated and assembled into a block.
///
/// See [`validate_proposal_parts`] for details on validation.
async fn process_proposal_parts(
    ctx: ProcessingContext<'_>,
    parts: ProposalParts,
) -> eyre::Result<Option<ConsensusBlock>> {
    let parts_height = parts.height();
    let parts_round = parts.round();
    let parts_proposer = parts.proposer();

    // Ignore the proposal if from past height
    if parts_height < ctx.current_height {
        debug!(
            height = %ctx.current_height,
            round = %ctx.current_round,
            parts.height = %parts_height,
            parts.round = %parts_round,
            parts.proposer = %parts_proposer,
            "Received proposal from a previous height, ignoring"
        );

        return Ok(None);
    }

    // Store future proposals parts in pending without validation
    if parts_height > ctx.current_height || parts_round > ctx.current_round {
        maybe_store_pending_proposal(
            ctx.store,
            ctx.metrics,
            ctx.current_height,
            ctx.current_round,
            ctx.max_pending_proposals,
            parts,
        )
        .await?;

        return Ok(None);
    }

    debug_assert_eq!(parts_height, ctx.current_height);

    // Proposal is for the current height, validate its proposer and signature
    let expected_proposer =
        ctx.proposer_selector
            .select_proposer(ctx.current_validator_set, parts_height, parts_round);

    if !validate_proposal_parts(&parts, expected_proposer, ctx.signing_provider).await {
        return Ok(None);
    }

    // Assemble the block
    let block = match assemble_block_from_parts(&parts) {
        Ok(block) => block,
        Err(e) => {
            let invalid = InvalidPayload::new_from_parts(&parts, &e.to_string());
            ctx.store.append_invalid_payload(invalid).await.wrap_err_with(|| {
                format!(
                    "Failed to store invalid payload after assembling block from parts (height={parts_height}, round={parts_round}, proposer={parts_proposer})",
                )
            })?;
            return Err(e.wrap_err(format!(
                "Failed to assemble block from parts (height={parts_height}, round={parts_round}, proposer={parts_proposer})",
            )));
        }
    };

    debug!("Block hash: {}", block.block_hash());

    Ok(Some(block))
}

/// Store a pending proposal if it's not too far in the future
async fn maybe_store_pending_proposal(
    store: &Store,
    metrics: &AppMetrics,
    current_height: Height,
    current_round: Round,
    max_pending_proposals: usize,
    parts: ProposalParts,
) -> eyre::Result<()> {
    let max_future_height = current_height.increment_by(max_pending_proposals as u64 - 1);

    // Check that proposal is not for a height too far in the future
    if parts.height() > max_future_height {
        debug!(
            height = %current_height,
            round = %current_round,
            parts.height = %parts.height(),
            parts.round = %parts.round(),
            parts.proposer = %parts.proposer(),
            max_height = %max_future_height,
            "Received proposal for a height too far in the future, ignoring"
        );
        return Ok(());
    }

    debug!(
        height = %current_height,
        round = %current_round,
        parts.height = %parts.height(),
        parts.round = %parts.round(),
        parts.proposer = %parts.proposer(),
        "Storing pending proposal for a future height/round"
    );

    // Store the parts for future processing
    store
        .store_pending_proposal_parts(parts, max_pending_proposals, current_height)
        .await
        .wrap_err("Failed to store pending proposal parts")?;

    // Update metrics
    let pending_count = store
        .get_pending_proposal_parts_count()
        .await
        .wrap_err("failed to get pending proposals count after storing new pending proposal")?;

    metrics.observe_pending_proposal_parts_count(pending_count);

    Ok(())
}
