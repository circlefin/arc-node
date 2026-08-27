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

use arc_consensus_types::proposal_monitor::ProposalMonitor;
use arc_consensus_types::proposer::ProposerSelector;
use arc_consensus_types::{ArcContext, Height, ProposalPart, ProposalParts, Round, ValidatorSet};
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_signer::ArcSigningProvider;

use super::skew_gate;
use crate::block::ConsensusBlock;
use crate::metrics::{AppMetrics, InvalidPayloadSource, SkewNilVoteSource};
use crate::payload::{
    establish_block_validity, persist_invalid_payload_best_effort, EnginePayloadValidator,
};
use crate::proposal_parts::{
    assemble_block_from_parts, resolve_expected_proposer, validate_proposal_parts,
};
use crate::state::State;
use crate::store::Store;
use crate::streaming::{InsertResult, PartStreamsMap};
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
    let previous_block = state.previous_block;

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
        previous_block,
    };

    let outcome = on_received_proposal_part(context, from, part)
        .await
        .inspect_err(|e| {
            error!(%from, "🔴 Error processing proposal part: {e:#}");
        })
        .unwrap_or(ReceivedPart::None);

    let response = match outcome {
        ReceivedPart::Assembled(proposed_value) => {
            let current_round = state.current_round;
            record_proposal_in_monitor(&mut state.proposal_monitor, current_round, &proposed_value);

            // Feed the byzantine amnesia state machine with incoming proposals,
            // so a later nil-prevote at this (height, round) can be overridden
            // with `NilOrVal::Val(value_id)`. No-op when the byzantine feature
            // is off or the amnesia trigger is inactive.
            #[cfg(feature = "byzantine")]
            if let Some(byz) = &state.ctx.byzantine {
                byz.amnesia.record_proposed_value(
                    proposed_value.height,
                    proposed_value.round,
                    proposed_value.value.id(),
                );
            }

            Some(proposed_value)
        }
        ReceivedPart::BufferedPending(height) => {
            // Parts for a future height completed before this node entered it.
            // Record the receive time against that height so its monitor —
            // created later, at round-0 start — shows the early arrival
            // (negative `proposal_delay_ms`). Value id is attached when they reassemble.
            state.record_early_pending_parts_time(height);
            None
        }
        ReceivedPart::None => None,
    };

    if let Err(e) = reply.send(response) {
        error!("🔴 ReceivedProposalPart: Failed to send reply: {e:?}");
    }
}

/// What handling a received proposal part produced, from the perspective of
/// the proposal monitor and the consensus reply.
enum ReceivedPart {
    /// A complete current-height proposal was assembled, validated, and stored.
    Assembled(ProposedValue<ArcContext>),
    /// A complete proposal for a future height was buffered as pending.
    BufferedPending(Height),
    /// Nothing actionable for the monitor or the consensus reply.
    None,
}

/// Records a live-received round-0 proposal in the proposal monitor.
///
/// Fast path: parts arrived while the node was already at the proposal's
/// height, so the monitor exists and we record directly. The pending-parts
/// case (parts arrive before the node reaches the height) does not flow
/// through here.
fn record_proposal_in_monitor(
    monitor: &mut Option<ProposalMonitor>,
    current_round: Round,
    proposed_value: &ProposedValue<ArcContext>,
) {
    if !proposed_value.validity.is_valid() {
        warn!(
            %proposed_value.height,
            %proposed_value.proposer,
            "Not recording invalid proposal in monitor",
        );
        return;
    }

    if current_round.as_i64() != 0 {
        // We only monitor round 0
        return;
    }

    let Some(monitor) = monitor.as_mut() else {
        warn!(
            %proposed_value.height,
            %proposed_value.round,
            %proposed_value.proposer,
            "No proposal monitor present",
        );
        return;
    };

    // Sanity checks - should always hold on the live path
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
    /// Block finalized at the height before `current_height`, when the node has one.
    previous_block: Option<ExecutionBlock>,
}

async fn on_received_proposal_part(
    context: HandlerContext<'_, '_>,
    from: PeerId,
    part: StreamMessage<ProposalPart>,
) -> eyre::Result<ReceivedPart> {
    let (part_type, part_size) = match &part.content {
        StreamContent::Data(part) => (part.get_type(), part.size_bytes()),
        StreamContent::Fin => ("end of stream", 0),
    };

    info!(
        %from, %part.sequence, part.type = %part_type, part.size = %part_size, stream_id = %part.stream_id,
        "Received proposal part"
    );

    // Capture the stream key before `insert` consumes `part`, so a completed
    // stream can be marked closed once it leaves `streams`.
    let stream_id = part.stream_id.clone();

    // Check if we have a full proposal
    let parts = match context.streams_map.insert(from, part) {
        InsertResult::Complete(parts) => parts,
        InsertResult::Pending => return Ok(ReceivedPart::None),
        // Benign: an old proposal part re-circulating on the network. Dropped
        // without warning — not peer misbehaviour.
        InsertResult::Stale => return Ok(ReceivedPart::None),
        InsertResult::Invalid(e) => {
            warn!(%from, error = %e, "Rejecting stream message");
            return Ok(ReceivedPart::None);
        }
    };

    // The stream has now left `streams`. Process it, then close its key on every
    // disposition except a transient `Deferred` decline — including on error. A
    // resurfaced straggler of a stream we completed (whether retained, rejected,
    // or failed mid-validation/-storage) must not reopen a height=None slot held
    // until the age timer; a genuine retry restreams under a fresh stream id,
    // which `closed_keys` does not block.
    let disposition = handle_complete_parts(&context, parts, from).await;

    if !matches!(disposition, Ok(Disposition::Deferred)) {
        context.streams_map.mark_closed(from, stream_id);
    }

    match disposition? {
        Disposition::Assembled(value) => Ok(ReceivedPart::Assembled(*value)),
        Disposition::BufferedPending(height) => Ok(ReceivedPart::BufferedPending(height)),
        Disposition::Terminal | Disposition::Deferred => Ok(ReceivedPart::None),
    }
}

/// Outcome of fully handling a completed stream's parts.
enum Disposition {
    /// Current-height proposal assembled into a block, validated, and stored as
    /// an undecided block; carries the resulting `ProposedValue`.
    Assembled(Box<ProposedValue<ArcContext>>),
    /// A round-0 proposal for a future height was buffered as pending.
    /// Terminal for stream-key purposes: the key may be closed,
    /// exactly as for any other stored-pending stream.
    BufferedPending(Height),
    /// Terminal non-block disposition (stored pending but out of monitor scope,
    /// ignored past-height, rejected, or assembly-failed); the stream key may
    /// be closed.
    Terminal,
    /// Transient decline; the stream key must stay open for re-admission.
    Deferred,
}

/// Classifies a completed set of proposal parts and, for a current-height
/// proposal, validates and stores it as an undecided block.
///
/// Errors (engine unreachable, store failure) propagate to the caller, which
/// closes the stream key regardless of whether processing succeeded — the parts
/// already left `streams`, and a deterministic re-delivery would fail the same
/// way while a genuine retry restreams under a fresh stream id.
async fn handle_complete_parts(
    context: &HandlerContext<'_, '_>,
    parts: ProposalParts,
    from: PeerId,
) -> eyre::Result<Disposition> {
    // Captured before `parts` is consumed below.
    let parts_height = parts.height();
    let parts_round = parts.round();
    let current_height = context.current_height;

    let mut block = match process_proposal_parts(context.into(), parts).await? {
        PartsOutcome::Block(block) => *block,
        // The proposal monitor only cares about buffered future-height, round-0 proposals.
        // The rest fall through to `Terminal` below.
        PartsOutcome::StoredPending
            if parts_height > current_height && parts_round.as_i64() == 0 =>
        {
            return Ok(Disposition::BufferedPending(parts_height));
        }
        outcome if outcome.is_terminal() => return Ok(Disposition::Terminal),
        // The only non-terminal outcome is a transient `Deferred` decline.
        _ => return Ok(Disposition::Deferred),
    };

    // Validate the block
    validate_block(
        context.engine,
        &context.metrics,
        &context.store,
        &mut block,
        context.previous_block.as_ref(),
        from,
    )
    .await?;

    // Don't key a non-canonical block into the undecided table; its invalid
    // record was already persisted by `establish_block_validity`.
    if !block.may_be_stored_as_undecided() {
        warn!(
            height = %block.height,
            round = %block.round,
            block_hash = %block.self_reported_block_hash(),
            "Proposal block self-reported hash is not canonical; not storing as undecided",
        );
        return Ok(Disposition::Terminal);
    }

    // The block is stored with its execution-only validity below; only the prevote
    // is downgraded when the proposer's timestamp is too far ahead of local time.
    // This refuses a too-far-future proposal without ever persisting it Invalid, so
    // it stays adoptable via a commit certificate with no restart.
    //
    // The gate re-runs on every offer, including a restreamed re-proposal: the vote
    // must be recomputed against current local time each time (the verdict can relax
    // to Valid as the clock advances), so it is intentionally not restream-guarded
    // here or on the `started_round` path.
    let validity = skew_gate::validity_for(
        &block,
        skew_gate::local_time_secs(),
        &context.metrics,
        SkewNilVoteSource::ReceivedProposal,
    );
    let proposed_value = block.to_proposed_value_with_validity(validity);

    debug!(
        block_size = %block.size_bytes(),
        payload_size = %block.payload_size(),
        "🎁 Received complete proposal: {proposed_value:?}",
    );

    // Store the full undecided block in the store
    let block_hash = block.self_reported_block_hash();

    context.store.store_undecided_block(block).await.wrap_err_with(||
        format!(
            "Failed to store undecided block {} built from parts received from {} for height={}, round={}, proposer={}",
            block_hash, from, proposed_value.height, proposed_value.round, proposed_value.proposer,
        )
    )?;

    Ok(Disposition::Assembled(Box::new(proposed_value)))
}

/// Validates a block received from a peer via the Engine API and records
/// the result. If a binding rule or the engine rejects the payload, an
/// [`InvalidPayload`] record is persisted by [`establish_block_validity`] and the block's
/// validity is set to [`Validity::Invalid`]. The block is kept either way
/// so that consensus can proceed with the correct validity information.
async fn validate_block(
    engine: &Engine,
    metrics: &AppMetrics,
    store: &Store,
    block: &mut ConsensusBlock,
    previous_block: Option<&ExecutionBlock>,
    from: PeerId,
) -> eyre::Result<()> {
    let validator = EnginePayloadValidator::new(engine, metrics);
    let validity = establish_block_validity(&validator, block, previous_block, store, metrics)
        .await
        .map(|verdict| verdict.validity())
        .wrap_err_with(|| {
            format!(
                "Payload validation failed on block built after \
                 receiving proposal part at height={}, round={} from {}",
                block.height, block.round, from,
            )
        })?;

    match validity {
        Validity::Invalid => {
            error!(
                "❌ Received invalid block: {}",
                block.self_reported_block_hash()
            );
        }
        Validity::Valid => {
            debug!(
                "✅ Received valid block: {}",
                block.self_reported_block_hash()
            );
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

/// Disposition of a completed set of proposal parts.
///
/// Drives whether the originating stream key is recorded as closed: every
/// variant except [`PartsOutcome::Deferred`] is terminal and its key may be
/// closed so resurfaced duplicates cannot reopen a slot. `Deferred` is
/// transient — the proposal could become storable later — so its key must stay
/// open for re-admission.
enum PartsOutcome {
    /// Current-height proposal with a valid proposer and signature. Carries the
    /// block for the caller to validate and store as an undecided block. Boxed
    /// to keep the enum small (the block dwarfs the unit variants).
    Block(Box<ConsensusBlock>),
    /// Future-height/round proposal retained in the pending table.
    StoredPending,
    /// Proposal from a past height; ignored.
    IgnoredPastHeight,
    /// Current-height proposal rejected: wrong proposer or invalid signature.
    RejectedInvalid,
    /// Current-height proposal whose parts failed to assemble into a block. The
    /// bytes are malformed, so reassembly is deterministic — an `InvalidPayload`
    /// is recorded and the key may be closed; a resurfaced copy would only fail
    /// the same way.
    AssemblyFailed,
    /// Valid proposal not retained right now, but for a transient reason: it is
    /// too far in the future (the window will advance) or the pending table is
    /// full (room will free up). The key is left open for re-admission.
    Deferred,
}

impl PartsOutcome {
    /// Whether the originating stream key may be recorded as closed: true for
    /// every disposition except a transient [`PartsOutcome::Deferred`] decline.
    fn is_terminal(&self) -> bool {
        !matches!(self, PartsOutcome::Deferred)
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
) -> eyre::Result<PartsOutcome> {
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

        return Ok(PartsOutcome::IgnoredPastHeight);
    }

    // Store future proposals parts in pending without validation
    if parts_height > ctx.current_height || parts_round > ctx.current_round {
        let stored = maybe_store_pending_proposal(
            ctx.store,
            ctx.metrics,
            ctx.current_height,
            ctx.current_round,
            ctx.max_pending_proposals,
            parts,
        )
        .await?;

        if stored {
            return Ok(PartsOutcome::StoredPending);
        }

        return Ok(PartsOutcome::Deferred);
    }

    debug_assert_eq!(parts_height, ctx.current_height);

    // Proposal is for the current height, validate its proposer and signature.
    let expected_proposer =
        resolve_expected_proposer(ctx.proposer_selector, ctx.current_validator_set, &parts);

    if !validate_proposal_parts(&parts, expected_proposer, ctx.signing_provider).await {
        return Ok(PartsOutcome::RejectedInvalid);
    }

    // Assemble the block
    let block = match assemble_block_from_parts(&parts) {
        Ok(block) => block,
        Err(e) => {
            warn!(
                height = %parts_height,
                round = %parts_round,
                proposer = %parts_proposer,
                "Failed to assemble block from parts: {e}",
            );
            ctx.metrics
                .inc_invalid_payloads_count(InvalidPayloadSource::AssemblyFailure);
            let invalid = InvalidPayload::new_from_parts(&parts, &e.to_string());
            persist_invalid_payload_best_effort(
                ctx.store,
                invalid,
                parts_height,
                parts_round,
                parts_proposer,
            )
            .await;
            return Ok(PartsOutcome::AssemblyFailed);
        }
    };

    debug!("Block hash: {}", block.self_reported_block_hash());

    Ok(PartsOutcome::Block(Box::new(block)))
}

/// Store a pending proposal if it's not too far in the future.
///
/// Returns `true` if the proposal was retained in the pending table, `false` if
/// it was transiently declined — either too far in the future, or the pending
/// table was full. A declined proposal may become storable later (the window
/// advances, the table frees), so its stream key must be left open.
async fn maybe_store_pending_proposal(
    store: &Store,
    metrics: &AppMetrics,
    current_height: Height,
    current_round: Round,
    max_pending_proposals: usize,
    parts: ProposalParts,
) -> eyre::Result<bool> {
    // max_pending_proposals > 0 (asserted at construction); fits in u64 on 64-bit targets
    #[allow(clippy::cast_possible_truncation, clippy::arithmetic_side_effects)]
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
        return Ok(false);
    }

    debug!(
        height = %current_height,
        round = %current_round,
        parts.height = %parts.height(),
        parts.round = %parts.round(),
        parts.proposer = %parts.proposer(),
        "Storing pending proposal for a future height/round"
    );

    // Store the parts for future processing. `false` means the table was full.
    let stored = store
        .store_pending_proposal_parts(parts, max_pending_proposals, current_height)
        .await
        .wrap_err("Failed to store pending proposal parts")?;

    // Update metrics
    let pending_count = store
        .get_pending_proposal_parts_count()
        .await
        .wrap_err("failed to get pending proposals count after storing new pending proposal")?;

    metrics.observe_pending_proposal_parts_count(pending_count);

    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_rpc_types_engine::{ExecutionPayloadV3, PayloadStatus, PayloadStatusEnum};
    use arbitrary::{Arbitrary, Unstructured};
    use arc_consensus_db::{DbMetrics, DbUpgrade};
    use arc_consensus_types::proposer::RoundRobin;
    use arc_consensus_types::{Address, Validator};
    use arc_eth_engine::engine::{MockEngineAPI, MockEthereumAPI};
    use arc_signer::local::{LocalSigningProvider, PrivateKey};
    use bytesize::ByteSize;
    use tempfile::tempdir;

    use crate::handlers::test_utils::{signed_parts_without_data, signed_stream_without_data};
    use crate::proposal_parts::prepare_stream;
    use crate::streaming::new_stream_id;

    impl ReceivedPart {
        fn as_assembled(&self) -> Option<&ProposedValue<ArcContext>> {
            match self {
                ReceivedPart::Assembled(v) => Some(v),
                _ => None,
            }
        }

        fn is_none(&self) -> bool {
            matches!(self, ReceivedPart::None)
        }

        fn is_buffered_pending(&self) -> bool {
            matches!(self, ReceivedPart::BufferedPending(_))
        }
    }

    use arc_consensus_types::{BlockHash, Value};
    use malachitebft_app_channel::app::types::core::Validity;
    use std::time::SystemTime;

    fn proposed_value(
        height: Height,
        round: Round,
        proposer: Address,
        validity: Validity,
        seed: u8,
    ) -> ProposedValue<ArcContext> {
        ProposedValue {
            height,
            round,
            valid_round: Round::Nil,
            proposer,
            value: Value::new(BlockHash::repeat_byte(seed)),
            validity,
        }
    }

    #[test]
    fn record_proposal_in_monitor_records_matching_live_proposal() {
        let height = Height::new(2);
        let proposer = Address::new([0x42; 20]);
        let mut monitor = Some(ProposalMonitor::new(height, proposer, SystemTime::now()));

        let pv = proposed_value(height, Round::new(0), proposer, Validity::Valid, 0xAA);
        record_proposal_in_monitor(&mut monitor, Round::new(0), &pv);

        assert_eq!(monitor.unwrap().value_id, Some(pv.value.id()));
    }

    #[test]
    fn record_proposal_in_monitor_skips_invalid_proposal() {
        let height = Height::new(2);
        let proposer = Address::new([0x42; 20]);
        let mut monitor = Some(ProposalMonitor::new(height, proposer, SystemTime::now()));

        let pv = proposed_value(height, Round::new(0), proposer, Validity::Invalid, 0xAA);
        record_proposal_in_monitor(&mut monitor, Round::new(0), &pv);

        assert!(monitor.unwrap().value_id.is_none());
    }

    #[test]
    fn record_proposal_in_monitor_skips_off_round0() {
        let height = Height::new(2);
        let proposer = Address::new([0x42; 20]);
        let mut monitor = Some(ProposalMonitor::new(height, proposer, SystemTime::now()));

        // Node has advanced past round 0 (the "too-late" path).
        let pv = proposed_value(height, Round::new(0), proposer, Validity::Valid, 0xAA);
        record_proposal_in_monitor(&mut monitor, Round::new(1), &pv);

        assert!(monitor.unwrap().value_id.is_none());
    }

    #[test]
    fn record_proposal_in_monitor_noop_when_monitor_absent() {
        let height = Height::new(2);
        let proposer = Address::new([0x42; 20]);
        let mut monitor: Option<ProposalMonitor> = None;

        let pv = proposed_value(height, Round::new(0), proposer, Validity::Valid, 0xAA);
        record_proposal_in_monitor(&mut monitor, Round::new(0), &pv);

        assert!(monitor.is_none());
    }

    #[test]
    fn record_proposal_in_monitor_skips_on_height_mismatch() {
        let proposer = Address::new([0x42; 20]);
        let mut monitor = Some(ProposalMonitor::new(
            Height::new(2),
            proposer,
            SystemTime::now(),
        ));

        // Monitor is for height 2 but the proposal is for height 3.
        let pv = proposed_value(
            Height::new(3),
            Round::new(0),
            proposer,
            Validity::Valid,
            0xAA,
        );
        record_proposal_in_monitor(&mut monitor, Round::new(0), &pv);

        assert!(monitor.unwrap().value_id.is_none());
    }

    #[tokio::test]
    async fn process_proposal_parts_assembly_failure_is_terminal() {
        let dir = tempdir().unwrap();
        let store = Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            ByteSize::mib(64),
        )
        .await
        .unwrap();

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);

        let height = Height::new(1);
        let round = Round::new(0);
        let parts = signed_parts_without_data(height, round, &signing_key).await;

        let selector = RoundRobin;
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key));
        let metrics = AppMetrics::default();

        let ctx = ProcessingContext {
            store: &store,
            metrics: &metrics,
            signing_provider: &provider,
            current_height: height,
            current_round: round,
            current_validator_set: &validator_set,
            proposer_selector: &selector,
            max_pending_proposals: 10,
        };

        let outcome = process_proposal_parts(ctx, parts).await.unwrap();

        assert!(matches!(outcome, PartsOutcome::AssemblyFailed));
        assert!(
            outcome.is_terminal(),
            "assembly failure is deterministic — its key must be closed"
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    /// Owned fixtures backing a [`ProcessingContext`]; each test borrows these.
    struct Fixtures {
        _dir: tempfile::TempDir,
        store: Store,
        metrics: AppMetrics,
        provider: ArcSigningProvider,
        validator_set: ValidatorSet,
        signing_key: PrivateKey,
    }

    async fn fixtures() -> Fixtures {
        let dir = tempdir().unwrap();
        let store = Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            ByteSize::mib(64),
        )
        .await
        .unwrap();

        let signing_key = PrivateKey::generate(rand::rngs::OsRng);
        let validator = Validator::new(signing_key.public_key(), 1);
        let validator_set = ValidatorSet::new(vec![validator]);
        let provider = ArcSigningProvider::Local(LocalSigningProvider::new(signing_key.clone()));

        Fixtures {
            _dir: dir,
            store,
            metrics: AppMetrics::default(),
            provider,
            validator_set,
            signing_key,
        }
    }

    #[tokio::test]
    async fn process_proposal_parts_past_height_is_ignored() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let ctx = ProcessingContext {
            store: &f.store,
            metrics: &f.metrics,
            signing_provider: &f.provider,
            current_height: Height::new(5),
            current_round: Round::new(0),
            current_validator_set: &f.validator_set,
            proposer_selector: &selector,
            max_pending_proposals: 10,
        };

        let parts = signed_parts_without_data(Height::new(3), Round::new(0), &f.signing_key).await;
        let outcome = process_proposal_parts(ctx, parts).await.unwrap();

        assert!(matches!(outcome, PartsOutcome::IgnoredPastHeight));
        assert!(outcome.is_terminal());
        assert_eq!(f.store.get_pending_proposal_parts_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn process_proposal_parts_future_in_range_is_stored_pending() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let ctx = ProcessingContext {
            store: &f.store,
            metrics: &f.metrics,
            signing_provider: &f.provider,
            current_height: Height::new(5),
            current_round: Round::new(0),
            current_validator_set: &f.validator_set,
            proposer_selector: &selector,
            max_pending_proposals: 10,
        };

        let parts = signed_parts_without_data(Height::new(6), Round::new(0), &f.signing_key).await;
        let outcome = process_proposal_parts(ctx, parts).await.unwrap();

        assert!(matches!(outcome, PartsOutcome::StoredPending));
        assert!(outcome.is_terminal());
        assert_eq!(f.store.get_pending_proposal_parts_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn process_proposal_parts_too_far_future_is_deferred_and_not_stored() {
        let f = fixtures().await;
        let selector = RoundRobin;
        // max_pending_proposals = 2 → allowed heights are 5 and 6; 10 is too far.
        let ctx = ProcessingContext {
            store: &f.store,
            metrics: &f.metrics,
            signing_provider: &f.provider,
            current_height: Height::new(5),
            current_round: Round::new(0),
            current_validator_set: &f.validator_set,
            proposer_selector: &selector,
            max_pending_proposals: 2,
        };

        let parts = signed_parts_without_data(Height::new(10), Round::new(0), &f.signing_key).await;
        let outcome = process_proposal_parts(ctx, parts).await.unwrap();

        assert!(matches!(outcome, PartsOutcome::Deferred));
        assert!(
            !outcome.is_terminal(),
            "a too-far proposal is transient — its key must stay open"
        );
        assert_eq!(f.store.get_pending_proposal_parts_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn process_proposal_parts_pending_table_full_is_deferred_and_not_stored() {
        let f = fixtures().await;

        // Fill the pending table (capacity 2) with two in-range future proposals.
        let p1 = signed_parts_without_data(Height::new(6), Round::new(0), &f.signing_key).await;
        let p2 = signed_parts_without_data(Height::new(6), Round::new(1), &f.signing_key).await;
        assert!(f
            .store
            .store_pending_proposal_parts(p1, 2, Height::new(5))
            .await
            .unwrap());
        assert!(f
            .store
            .store_pending_proposal_parts(p2, 2, Height::new(5))
            .await
            .unwrap());
        assert_eq!(f.store.get_pending_proposal_parts_count().await.unwrap(), 2);

        let selector = RoundRobin;
        let ctx = ProcessingContext {
            store: &f.store,
            metrics: &f.metrics,
            signing_provider: &f.provider,
            current_height: Height::new(5),
            current_round: Round::new(0),
            current_validator_set: &f.validator_set,
            proposer_selector: &selector,
            max_pending_proposals: 2,
        };

        // Another in-range future proposal: valid, but the table is full.
        let p3 = signed_parts_without_data(Height::new(6), Round::new(2), &f.signing_key).await;
        let outcome = process_proposal_parts(ctx, p3).await.unwrap();

        assert!(matches!(outcome, PartsOutcome::Deferred));
        assert!(
            !outcome.is_terminal(),
            "a table-full decline is transient — its key must stay open"
        );
        assert_eq!(
            f.store.get_pending_proposal_parts_count().await.unwrap(),
            2,
            "a table-full proposal must not be stored"
        );
    }

    // --- on_received_proposal_part: full-handler tests (mock Engine API) ---
    //
    // These exercise the `mark_closed` glue end-to-end: a completed stream whose
    // proposal reaches a terminal disposition has its key closed (so resurfaced
    // duplicates are dropped), while a transiently-deferred proposal leaves the
    // key open. The current-height path also drives block validation through a
    // mocked Engine API.

    /// A small, deterministic execution payload that SSZ round-trips through
    /// `assemble_block_from_parts`.
    /// Builds a payload that carries `height` as its block number, so a block
    /// built from it is bound to that height.
    fn dummy_payload(height: Height) -> ExecutionPayloadV3 {
        let mut u = Unstructured::new(&[0u8; 1024]);
        let mut payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        payload.payload_inner.payload_inner.block_number = height.as_u64();
        payload
    }

    /// Build a [`ConsensusBlock`] proposed by the fixtures' single validator, so
    /// proposer + signature validation passes on the current-height path.
    fn block_from(f: &Fixtures, height: Height, round: Round) -> ConsensusBlock {
        ConsensusBlock {
            height,
            round,
            valid_round: Round::Nil,
            proposer: Address::from_public_key(&f.signing_key.public_key()),
            validity: Validity::Valid,
            execution_payload: dummy_payload(height),
            signature: None,
        }
    }

    fn make_ctx<'a, 'b>(
        f: &Fixtures,
        engine: &'a Engine,
        selector: &'a dyn ProposerSelector,
        streams_map: &'b mut PartStreamsMap,
        current_height: Height,
        max_pending_proposals: usize,
        previous_block: Option<ExecutionBlock>,
    ) -> HandlerContext<'a, 'b> {
        HandlerContext {
            engine,
            store: f.store.clone(),
            metrics: f.metrics.clone(),
            signing_provider: f.provider.clone(),
            streams_map,
            current_height,
            current_round: Round::new(0),
            current_validator_set: f.validator_set.clone(),
            proposer_selector: selector,
            max_pending_proposals,
            previous_block,
        }
    }

    /// Feed every message through `on_received_proposal_part`, rebuilding a fresh
    /// `HandlerContext` each time (it is consumed by value), and return the result
    /// of the completing (last) message.
    #[allow(clippy::too_many_arguments)]
    async fn feed_stream(
        f: &Fixtures,
        engine: &Engine,
        selector: &dyn ProposerSelector,
        streams_map: &mut PartStreamsMap,
        current_height: Height,
        max_pending: usize,
        from: PeerId,
        messages: &[StreamMessage<ProposalPart>],
    ) -> eyre::Result<ReceivedPart> {
        feed_stream_with_previous_block(
            f,
            engine,
            selector,
            streams_map,
            current_height,
            max_pending,
            from,
            messages,
            None,
        )
        .await
    }

    /// [`feed_stream`] with a predecessor in the context, so the parent rule applies.
    #[allow(clippy::too_many_arguments)]
    async fn feed_stream_with_previous_block(
        f: &Fixtures,
        engine: &Engine,
        selector: &dyn ProposerSelector,
        streams_map: &mut PartStreamsMap,
        current_height: Height,
        max_pending: usize,
        from: PeerId,
        messages: &[StreamMessage<ProposalPart>],
        previous_block: Option<ExecutionBlock>,
    ) -> eyre::Result<ReceivedPart> {
        let mut last = ReceivedPart::None;
        for msg in messages {
            let ctx = make_ctx(
                f,
                engine,
                selector,
                streams_map,
                current_height,
                max_pending,
                previous_block,
            );
            last = on_received_proposal_part(ctx, from, msg.clone()).await?;
        }
        Ok(last)
    }

    /// A proposal for the current height whose payload belongs at another height:
    /// the engine never sees it, and consensus gets an `Invalid` verdict.
    #[tokio::test]
    async fn on_received_proposal_part_rejects_a_payload_from_another_height() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let height = Height::new(5);
        let round = Round::new(0);

        let mut engine_mock = MockEngineAPI::new();
        engine_mock.expect_new_payload().times(0);
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        // The payload reports block number 2. Its self-reported hash is canonical,
        // so the row can still be keyed under it.
        let mut block = block_from(&f, height, round);
        block
            .execution_payload
            .payload_inner
            .payload_inner
            .block_number = 2;
        block
            .execution_payload
            .payload_inner
            .payload_inner
            .block_hash =
            arc_consensus_types::block::canonical_block_hash(&block.execution_payload)
                .expect("recompute canonical hash");

        let stream_id = new_stream_id(height, round, 0);
        let (messages, _sig) = prepare_stream(stream_id.clone(), &f.provider, &block)
            .await
            .unwrap();

        let mut streams_map = PartStreamsMap::new(height, 1);
        let result = feed_stream(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            height,
            10,
            from,
            &messages,
        )
        .await
        .unwrap();

        let proposed = result
            .as_assembled()
            .expect("the verdict must reach consensus");
        assert_eq!(proposed.validity, Validity::Invalid);
        assert_eq!(f.metrics.get_invalid_payloads_count(), 1);
    }

    /// A proposal for the current height whose payload reports that height but
    /// extends some other block. The height rule sees nothing wrong, so only the
    /// parent rule rejects it, and the engine still never sees it.
    #[tokio::test]
    async fn on_received_proposal_part_rejects_a_payload_that_extends_another_block() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let height = Height::new(5);
        let round = Round::new(0);

        let mut engine_mock = MockEngineAPI::new();
        engine_mock.expect_new_payload().times(0);
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        // The immediate predecessor, and not the block the payload extends.
        let previous_block = ExecutionBlock {
            block_hash: arc_consensus_types::B256::repeat_byte(0xAB),
            block_number: height.as_u64() - 1,
            parent_hash: arc_consensus_types::B256::ZERO,
            timestamp: 0,
        };

        let mut block = block_from(&f, height, round);
        block
            .execution_payload
            .payload_inner
            .payload_inner
            .block_number = height.as_u64();
        block
            .execution_payload
            .payload_inner
            .payload_inner
            .parent_hash = arc_consensus_types::B256::repeat_byte(0xCD);
        block
            .execution_payload
            .payload_inner
            .payload_inner
            .block_hash =
            arc_consensus_types::block::canonical_block_hash(&block.execution_payload)
                .expect("recompute canonical hash");

        let stream_id = new_stream_id(height, round, 0);
        let (messages, _sig) = prepare_stream(stream_id.clone(), &f.provider, &block)
            .await
            .unwrap();

        let mut streams_map = PartStreamsMap::new(height, 1);
        let result = feed_stream_with_previous_block(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            height,
            10,
            from,
            &messages,
            Some(previous_block),
        )
        .await
        .unwrap();

        let proposed = result
            .as_assembled()
            .expect("the verdict must reach consensus");
        assert_eq!(proposed.validity, Validity::Invalid);
        assert_eq!(
            f.metrics
                .get_invalid_payloads_count_by_source(InvalidPayloadSource::PayloadParent),
            1,
        );
    }

    #[tokio::test]
    async fn on_received_proposal_part_current_height_validates_stores_and_closes_key() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let height = Height::new(5);
        let round = Round::new(0);

        // Engine accepts the payload as valid.
        let mut engine_mock = MockEngineAPI::new();
        engine_mock.expect_new_payload().returning(|_, _, _, _| {
            Ok(PayloadStatus {
                status: PayloadStatusEnum::Valid,
                latest_valid_hash: None,
            })
        });
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        let block = block_from(&f, height, round);
        let stream_id = new_stream_id(height, round, 0);
        let (messages, _sig) = prepare_stream(stream_id.clone(), &f.provider, &block)
            .await
            .unwrap();

        let mut streams_map = PartStreamsMap::new(height, 1);
        let result = feed_stream(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            height,
            10,
            from,
            &messages,
        )
        .await
        .unwrap();

        let proposed = result
            .as_assembled()
            .expect("current-height proposal should yield a ProposedValue");
        assert_eq!(proposed.height, height);
        assert_eq!(proposed.round, round);
        assert!(
            streams_map.is_closed(from, &stream_id),
            "key must be closed once the undecided block is stored"
        );

        // A resurfaced straggler (a data part) for the same stream is dropped and
        // does not reopen a slot.
        let straggler = messages[1].clone();
        let ctx = make_ctx(&f, &engine, &selector, &mut streams_map, height, 10, None);
        assert!(on_received_proposal_part(ctx, from, straggler)
            .await
            .unwrap()
            .is_none());
    }

    /// A current-height proposal whose self-reported hash is not canonical and is
    /// rejected by the engine is not stored as an undecided block: the stream
    /// reaches a terminal disposition and its key is closed.
    #[tokio::test]
    async fn on_received_proposal_part_non_canonical_invalid_block_is_not_stored() {
        use crate::store::repositories::UndecidedBlocksRepository;

        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let height = Height::new(5);
        let round = Round::new(0);

        // The engine rejects the payload.
        let mut engine_mock = MockEngineAPI::new();
        engine_mock.expect_new_payload().returning(|_, _, _, _| {
            Ok(PayloadStatus {
                status: PayloadStatusEnum::Invalid {
                    validation_error: "block hash mismatch".into(),
                },
                latest_valid_hash: None,
            })
        });
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        // `block_from`'s payload carries a non-canonical self-reported hash.
        let block = block_from(&f, height, round);
        assert!(!block.self_reported_hash_is_canonical());
        let stream_id = new_stream_id(height, round, 0);
        let (messages, _sig) = prepare_stream(stream_id.clone(), &f.provider, &block)
            .await
            .unwrap();

        let mut streams_map = PartStreamsMap::new(height, 1);
        let result = feed_stream(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            height,
            10,
            from,
            &messages,
        )
        .await
        .unwrap();

        assert!(
            result.is_none(),
            "a non-canonical invalid proposal yields no ProposedValue"
        );
        assert!(
            streams_map.is_closed(from, &stream_id),
            "a terminal disposition must close the stream key"
        );
        assert!(
            f.store
                .get_by_round(height, round)
                .await
                .unwrap()
                .is_empty(),
            "a non-canonical invalid block must not be stored as undecided"
        );
        assert_eq!(f.metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn on_received_proposal_part_future_in_range_stores_pending_and_closes_key() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let current_height = Height::new(5);
        let future_height = Height::new(6); // within range for max_pending = 10

        // The future path stores without validation: the Engine must not be called.
        let engine = Engine::new(
            Box::new(MockEngineAPI::new()),
            Box::new(MockEthereumAPI::new()),
        );

        let block = block_from(&f, future_height, Round::new(0));
        let stream_id = new_stream_id(future_height, Round::new(0), 0);
        let (messages, _sig) = prepare_stream(stream_id.clone(), &f.provider, &block)
            .await
            .unwrap();

        let mut streams_map = PartStreamsMap::new(current_height, 1);
        let result = feed_stream(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            current_height,
            10,
            from,
            &messages,
        )
        .await
        .unwrap();

        assert!(
            result.is_buffered_pending(),
            "a round-0 future-height proposal buffered as pending must report \
             BufferedPending so its receive time can be recorded"
        );
        assert!(
            streams_map.is_closed(from, &stream_id),
            "a stored-pending (terminal) stream key must be closed"
        );
        assert_eq!(f.store.get_pending_proposal_parts_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn on_received_proposal_part_too_far_future_is_deferred_and_key_left_open() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let current_height = Height::new(5);
        let too_far_height = Height::new(10); // max_pending = 2 allows only heights 5, 6

        let engine = Engine::new(
            Box::new(MockEngineAPI::new()),
            Box::new(MockEthereumAPI::new()),
        );

        let block = block_from(&f, too_far_height, Round::new(0));
        let stream_id = new_stream_id(too_far_height, Round::new(0), 0);
        let (messages, _sig) = prepare_stream(stream_id.clone(), &f.provider, &block)
            .await
            .unwrap();

        let mut streams_map = PartStreamsMap::new(current_height, 1);
        let result = feed_stream(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            current_height,
            2,
            from,
            &messages,
        )
        .await
        .unwrap();

        assert!(result.is_none());
        assert!(
            !streams_map.is_closed(from, &stream_id),
            "a transiently-deferred (Deferred) stream key must stay open"
        );
        assert_eq!(
            f.store.get_pending_proposal_parts_count().await.unwrap(),
            0,
            "a too-far proposal must not be stored"
        );
    }

    #[tokio::test]
    async fn on_received_proposal_part_assembly_failure_closes_key() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let height = Height::new(5);
        let round = Round::new(0);

        // Assembly fails before validation, so the Engine must never be called:
        // an expectation-less mock panics if it is.
        let engine = Engine::new(
            Box::new(MockEngineAPI::new()),
            Box::new(MockEthereumAPI::new()),
        );

        let stream_id = new_stream_id(height, round, 0);
        let messages =
            signed_stream_without_data(stream_id.clone(), height, round, &f.signing_key).await;

        let mut streams_map = PartStreamsMap::new(height, 1);
        let result = feed_stream(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            height,
            10,
            from,
            &messages,
        )
        .await
        .unwrap();

        assert!(
            result.is_none(),
            "a stream that fails to assemble yields no ProposedValue"
        );
        assert!(
            streams_map.is_closed(from, &stream_id),
            "assembly failure is deterministic — its key must be closed"
        );
        assert_eq!(
            f.metrics.get_invalid_payloads_count(),
            1,
            "an InvalidPayload must be recorded for the malformed parts"
        );

        // A resurfaced straggler for the same stream is dropped and does not
        // reopen a slot.
        let straggler = messages[1].clone();
        let ctx = make_ctx(&f, &engine, &selector, &mut streams_map, height, 10, None);
        assert!(on_received_proposal_part(ctx, from, straggler)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn on_received_proposal_part_engine_error_closes_key() {
        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let height = Height::new(5);
        let round = Round::new(0);

        // The Engine is unreachable: validation errors instead of returning a
        // verdict.
        let mut engine_mock = MockEngineAPI::new();
        engine_mock
            .expect_new_payload()
            .returning(|_, _, _, _| Err(eyre::eyre!("engine unreachable")));
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        let block = block_from(&f, height, round);
        let stream_id = new_stream_id(height, round, 0);
        let (messages, _sig) = prepare_stream(stream_id.clone(), &f.provider, &block)
            .await
            .unwrap();

        let mut streams_map = PartStreamsMap::new(height, 1);
        let result = feed_stream(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            height,
            10,
            from,
            &messages,
        )
        .await;

        assert!(result.is_err(), "an engine error must propagate");
        assert!(
            streams_map.is_closed(from, &stream_id),
            "the key is closed even when validation errors, so a resurfaced \
             straggler cannot reopen a slot"
        );
    }

    /// A live current-height proposal whose timestamp is far ahead of local time
    /// is prevoted nil (Invalid vote), yet the block is still stored with its
    /// execution-only Valid verdict so it stays adoptable via a commit certificate.
    #[tokio::test]
    async fn on_received_proposal_part_future_timestamp_votes_nil_but_stores_valid() {
        use crate::store::repositories::UndecidedBlocksRepository;

        let f = fixtures().await;
        let selector = RoundRobin;
        let from = PeerId::random();
        let height = Height::new(5);
        let round = Round::new(0);

        // The engine accepts the payload: it is execution-valid.
        let mut engine_mock = MockEngineAPI::new();
        engine_mock.expect_new_payload().returning(|_, _, _, _| {
            Ok(PayloadStatus {
                status: PayloadStatusEnum::Valid,
                latest_valid_hash: None,
            })
        });
        let engine = Engine::new(Box::new(engine_mock), Box::new(MockEthereumAPI::new()));

        // Stamp the block well beyond the skew threshold ahead of local time.
        let now = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut block = block_from(&f, height, round);
        block
            .execution_payload
            .payload_inner
            .payload_inner
            .timestamp = now + 3600;

        let stream_id = new_stream_id(height, round, 0);
        let (messages, _sig) = prepare_stream(stream_id.clone(), &f.provider, &block)
            .await
            .unwrap();

        let mut streams_map = PartStreamsMap::new(height, 1);
        let result = feed_stream(
            &f,
            &engine,
            &selector,
            &mut streams_map,
            height,
            10,
            from,
            &messages,
        )
        .await
        .unwrap();

        let proposed = result
            .as_assembled()
            .expect("the verdict must reach consensus");
        assert_eq!(
            proposed.validity,
            Validity::Invalid,
            "a too-far-future proposal must be prevoted nil",
        );

        let stored = f.store.get_by_round(height, round).await.unwrap();
        assert_eq!(
            stored.len(),
            1,
            "the block must still be stored as undecided"
        );
        assert_eq!(
            stored[0].validity,
            Validity::Valid,
            "the stored block keeps its execution-only Valid verdict",
        );
    }
}
