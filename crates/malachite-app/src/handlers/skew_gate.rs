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

//! Proposer clock-skew gate for received proposals.
//!
//! When a proposal's header timestamp is more than
//! [`ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS`] ahead of local time, the gate
//! downgrades the node's prevote to `Invalid`. It only ever changes the vote:
//! the block's persisted (execution-only) validity is untouched, so a value that
//! later carries a commit certificate is still adopted via sync. Because the
//! `Invalid` travels with the `ProposedValue`, it also keeps the node from
//! deciding directly on that certificate — adoption goes around through sync.
//! Both the live-arrival path (`received_proposal_part`) and the buffered/
//! early-arrival path (`started_round`) apply the gate here.
//!
//! The threshold, the timestamp predicate, and the local-clock read live in this
//! module: the clock-skew judgment is a consensus-layer concern (the execution
//! layer no longer performs it).

use std::time::{SystemTime, UNIX_EPOCH};

use malachitebft_app_channel::app::types::core::Validity;
use tracing::warn;

use crate::block::ConsensusBlock;
use crate::metrics::{AppMetrics, SkewNilVoteSource};

/// The maximum clock skew, in seconds, tolerated between a proposer's block
/// timestamp and a validating node's local clock.
const ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS: u64 = 30;

/// Returns `true` when `header_timestamp` is more than
/// [`ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS`] ahead of `local_time` (both in
/// seconds since the Unix epoch).
///
/// A timestamp at or before `local_time + threshold` — including any timestamp
/// in the past — is within tolerance.
fn header_timestamp_exceeds_skew(header_timestamp: u64, local_time: u64) -> bool {
    header_timestamp > local_time.saturating_add(ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS)
}

/// Local wall-clock time in whole seconds since the Unix epoch, or `None` if the
/// system clock is set before the epoch.
///
/// Callers that cannot read the clock should skip the skew check rather than
/// treat the proposal as skewed, so a clock read failure never forces a nil-vote.
pub(crate) fn local_time_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// Applies the clock-skew gate: the validity to vote with, given a block's
/// execution verdict, the proposer's header timestamp, and local time.
///
/// An execution-`Valid` block whose timestamp is more than the clock-skew
/// threshold ahead of `local_time` votes `Invalid` (nil). An execution-`Invalid`
/// block stays `Invalid` regardless of the clock.
pub(crate) fn apply(
    execution_validity: Validity,
    header_timestamp: u64,
    local_time: u64,
) -> Validity {
    if execution_validity.is_valid() && header_timestamp_exceeds_skew(header_timestamp, local_time)
    {
        Validity::Invalid
    } else {
        execution_validity
    }
}

/// The validity to vote with for `block`, applying the clock-skew gate against
/// `local_time`, recording and warning on a downgrade under `source`.
///
/// When `local_time` is `None` (the system clock could not be read) the gate is
/// skipped and the block's execution validity is used, so a clock read failure
/// never forces a nil-vote.
pub(crate) fn validity_for(
    block: &ConsensusBlock,
    local_time: Option<u64>,
    metrics: &AppMetrics,
    source: SkewNilVoteSource,
) -> Validity {
    let Some(now) = local_time else {
        warn!(
            height = %block.height,
            round = %block.round,
            "Local clock is before the Unix epoch; skipping the proposer clock-skew vote check",
        );
        return block.validity;
    };

    let header_timestamp = block.execution_payload.timestamp();
    let validity = apply(block.validity, header_timestamp, now);

    // A downgrade (execution-Valid -> vote-Invalid) is the skew nil-vote: count it
    // labelled by path and warn with the offset, so a skewed node is diagnosable
    // rather than looking like an unexplained slow node. Reading the downgrade off
    // `apply`'s output keeps the signal from drifting from the condition. This counts
    // the downgrade decision, not a confirmed prevote: a later store failure (received
    // path) or a consensus-side cap-drop can still discard the value.
    if block.validity.is_valid() && !validity.is_valid() {
        metrics.inc_clock_skew_nil_vote_count(source);
        warn!(
            height = %block.height,
            round = %block.round,
            header_timestamp,
            local_time = now,
            delta_secs = header_timestamp.saturating_sub(now),
            "Prevoting nil: proposer timestamp too far ahead of local clock",
        );
    }

    validity
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_rpc_types_engine::ExecutionPayloadV3;
    use arbitrary::{Arbitrary, Unstructured};
    use arc_consensus_types::{Address, Height, Round};

    fn block_with_timestamp(timestamp: u64, validity: Validity) -> ConsensusBlock {
        let mut u = Unstructured::new(&[0u8; 512]);
        let mut payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        payload.payload_inner.payload_inner.timestamp = timestamp;
        ConsensusBlock {
            height: Height::new(1),
            round: Round::new(0),
            valid_round: Round::Nil,
            proposer: Address::new([0u8; 20]),
            validity,
            execution_payload: payload,
            signature: None,
        }
    }

    #[test]
    fn apply_downgrades_execution_valid_when_skewed() {
        let local_time = 1_000_000;
        let skewed = local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS + 1;
        assert_eq!(
            apply(Validity::Valid, skewed, local_time),
            Validity::Invalid
        );
    }

    #[test]
    fn apply_keeps_execution_valid_within_tolerance() {
        let local_time = 1_000_000;
        let at_threshold = local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS;
        assert_eq!(
            apply(Validity::Valid, at_threshold, local_time),
            Validity::Valid,
        );
        assert_eq!(
            apply(Validity::Valid, local_time - 100, local_time),
            Validity::Valid,
        );
    }

    #[test]
    fn apply_leaves_execution_invalid_invalid_regardless_of_clock() {
        let local_time = 1_000_000;
        let skewed = local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS + 1;
        assert_eq!(
            apply(Validity::Invalid, local_time, local_time),
            Validity::Invalid,
        );
        assert_eq!(
            apply(Validity::Invalid, skewed, local_time),
            Validity::Invalid,
        );
    }

    #[test]
    fn apply_saturates_near_u64_max() {
        // local_time + threshold saturates at u64::MAX instead of overflowing, so
        // the comparison never panics in debug builds; a timestamp at the saturated
        // bound is within tolerance.
        assert_eq!(apply(Validity::Valid, u64::MAX, u64::MAX), Validity::Valid);
        assert_eq!(
            apply(Validity::Valid, u64::MAX, u64::MAX - 1),
            Validity::Valid,
        );
    }

    #[test]
    fn validity_for_skips_the_gate_when_local_time_is_unavailable() {
        let metrics = AppMetrics::default();
        let local_time = 1_000_000u64;
        let block = block_with_timestamp(
            local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS + 100,
            Validity::Valid,
        );
        // No clock reading → gate skipped, execution validity preserved, nothing counted.
        assert_eq!(
            validity_for(&block, None, &metrics, SkewNilVoteSource::ReceivedProposal),
            Validity::Valid,
        );
        // With a lagging clock reading → skewed proposal is downgraded.
        assert_eq!(
            validity_for(
                &block,
                Some(local_time),
                &metrics,
                SkewNilVoteSource::ReceivedProposal
            ),
            Validity::Invalid,
        );
        assert_eq!(
            metrics.get_clock_skew_nil_vote_count_by_source(SkewNilVoteSource::ReceivedProposal),
            1,
        );
    }

    #[test]
    fn skew_nil_vote_is_counted_and_labelled_by_path() {
        let metrics = AppMetrics::default();
        let local_time = 1_000_000u64;
        let skewed = block_with_timestamp(
            local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS + 1,
            Validity::Valid,
        );
        let within = block_with_timestamp(
            local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS,
            Validity::Valid,
        );
        let exec_invalid = block_with_timestamp(
            local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS + 1,
            Validity::Invalid,
        );

        // Within tolerance → Valid vote, nothing counted.
        assert_eq!(
            validity_for(
                &within,
                Some(local_time),
                &metrics,
                SkewNilVoteSource::ReceivedProposal
            ),
            Validity::Valid,
        );
        // Execution-invalid stays Invalid but is not a skew downgrade → not counted.
        assert_eq!(
            validity_for(
                &exec_invalid,
                Some(local_time),
                &metrics,
                SkewNilVoteSource::ReceivedProposal,
            ),
            Validity::Invalid,
        );
        // Too-far-future on the live path → nil-vote, counted under that path.
        assert_eq!(
            validity_for(
                &skewed,
                Some(local_time),
                &metrics,
                SkewNilVoteSource::ReceivedProposal
            ),
            Validity::Invalid,
        );
        // And on the buffered path → counted under the other label.
        assert_eq!(
            validity_for(
                &skewed,
                Some(local_time),
                &metrics,
                SkewNilVoteSource::StartedRound
            ),
            Validity::Invalid,
        );

        assert_eq!(
            metrics.get_clock_skew_nil_vote_count_by_source(SkewNilVoteSource::ReceivedProposal),
            1,
        );
        assert_eq!(
            metrics.get_clock_skew_nil_vote_count_by_source(SkewNilVoteSource::StartedRound),
            1,
        );
    }
}
