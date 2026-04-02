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

use std::ops::RangeInclusive;

use alloy_rpc_types_engine::ExecutionPayloadV3;
use arc_eth_engine::engine::Engine;
use bytesize::ByteSize;
use eyre::{eyre, WrapErr};
use ssz::Encode;
use tracing::{debug, error, info, warn};

use malachitebft_app_channel::app::types::codec::HasEncodedLen;
use malachitebft_app_channel::app::types::sync::RawDecidedValue;
use malachitebft_app_channel::Reply;
use malachitebft_core_types::utils::height::{DisplayRange, HeightRangeExt};
use malachitebft_core_types::Height as _;

use arc_consensus_types::codec::proto::ProtobufCodec;
use arc_consensus_types::sync::{Response, ValueResponse};
use arc_consensus_types::{ArcContext, Height};

use crate::block::DecidedBlock;
use crate::metrics::AppMetrics;
use crate::state::State;
use crate::store::Store;

pub async fn handle(
    state: &mut State,
    engine: &Engine,
    range: RangeInclusive<Height>,
    reply: Reply<Vec<RawDecidedValue<ArcContext>>>,
) -> Result<(), eyre::Error> {
    let config = state.config().value_sync;

    if !config.enabled {
        warn!("GetDecidedValues: Sync is disabled in the configuration");
        let _ = reply.send(Vec::new());
        return Ok(());
    }

    let latest_height = state
        .store()
        .max_height()
        .await
        .wrap_err("GetDecidedValues: Failed to fetch the latest height from the state")?
        .unwrap_or_default();

    let earliest_height = state
        .store()
        .min_height()
        .await
        .wrap_err("GetDecidedValues: Failed to fetch the earliest height from the state")?
        .unwrap_or_default();

    let store = state.store().clone();
    let metrics = state.metrics().clone();
    let engine = engine.clone();

    // Spawn retrieval of decided values in a separate task to avoid blocking the main application loop.
    tokio::spawn(async move {
        let values = get_decided_values(
            range,
            earliest_height..=latest_height,
            config.batch_size,
            config.max_response_size,
            store,
            engine,
            metrics,
        )
        .await
        .inspect_err(|e| {
            error!("🔴 GetDecidedValues: Error while getting decided values: {e:?}");
        })
        .unwrap_or_default();

        if let Err(e) = reply.send(values) {
            error!("🔴 GetDecidedValues: Failed to send reply: {e:?}");
        }
    });

    Ok(())
}

async fn get_decided_values(
    requested_range: RangeInclusive<Height>,
    available_range: RangeInclusive<Height>,
    batch_size: usize,
    max_response_size: ByteSize,
    store: Store,
    engine: Engine,
    metrics: AppMetrics,
) -> Result<Vec<RawDecidedValue<ArcContext>>, eyre::Error> {
    let _guard = metrics.start_msg_process_timer("GetDecidedValues");

    let earliest_height = *available_range.start();
    let latest_height = *available_range.end();

    let Some(range) =
        get_clamped_request_range(requested_range, earliest_height, latest_height, batch_size)
    else {
        return Ok(Vec::new()); // Warn logged inside get_clamped_request_range
    };

    // Batch fetch all execution payloads in one RPC call
    let heights = range.clone().iter_heights().collect::<Vec<_>>();
    let block_numbers = heights
        .iter()
        .map(|height| format!("0x{:x}", height.as_u64()))
        .collect::<Vec<_>>();

    let execution_payloads = engine.eth.get_execution_payloads(&block_numbers).await?;

    let mut values = Vec::with_capacity(range.len());
    let mut total_bytes = ByteSize::b(0);

    for (height, execution_payload) in heights.into_iter().zip(execution_payloads.into_iter()) {
        let Some(execution_payload) = execution_payload else {
            debug!(%height, "No execution payload found at this height from EL, skipping");
            continue;
        };

        let (raw_value, raw_bytes_len) =
            match get_raw_decided_value(&store, execution_payload, height).await {
                Ok(result) => result,
                Err(e) => {
                    warn!(%height, "Failed to get decided value at height: {e}");
                    continue;
                }
            };

        // NOTE: This size estimate slightly over-approximates the true wire size.
        // These estimates assume each value is sent in its own SyncResponse message,
        // whereas in practice all values are batched into a single message.
        //
        // For 10 SyncedValues each with 10 signatures, batching all values into
        // one SyncResponse (~10X + 9,990 bytes) is about 90 bytes smaller than sending
        // 10 separate SyncResponses (~10X + 10,080 bytes). In other words, splitting
        // adds ~9 bytes of framing overhead per message (<1% overhead for typical payloads).
        //
        // This over-approximation is acceptable for our purpose of ensuring we are not going
        // over the max response size limit.
        //
        // Moreover, Malachite will perform a very similar over-approximation when checking
        // the response to GetDecidedValues, so this keeps our behavior consistent.
        if total_bytes + raw_bytes_len > max_response_size {
            warn!(
                %height, %max_response_size, %raw_bytes_len,
                "GetDecidedValues: Reached max total bytes limit for response, stopping here",
            );

            break;
        }

        total_bytes += raw_bytes_len;

        values.push(raw_value);
    }

    info!(
        values = %values.len(),
        %total_bytes,
        %max_response_size,
        "GetDecidedValues: Returning decided values"
    );

    Ok(values)
}

async fn get_raw_decided_value(
    store: &Store,
    execution_payload: ExecutionPayloadV3,
    height: Height,
) -> eyre::Result<(RawDecidedValue<ArcContext>, ByteSize)> {
    let stored = store
        .get_certificate(Some(height))
        .await?
        .ok_or_else(|| eyre!("No certificate found at height {height}"))?;

    let decided_block = DecidedBlock::new(execution_payload, stored.certificate);

    let raw_value = RawDecidedValue {
        certificate: decided_block.certificate,
        value_bytes: decided_block.execution_payload.as_ssz_bytes().into(),
    };

    let response = Response::ValueResponse(ValueResponse::new(height, vec![raw_value.clone()]));

    let Ok(raw_bytes_len) = ProtobufCodec.encoded_len(&response) else {
        return Err(eyre!(
            "Failed to determine encoded length of value at height {height}"
        ));
    };

    Ok((raw_value, ByteSize::b(raw_bytes_len as u64)))
}

fn get_clamped_request_range(
    range: RangeInclusive<Height>,
    earliest_height: Height,
    latest_height: Height,
    batch_size: usize,
) -> Option<RangeInclusive<Height>> {
    assert!(
        earliest_height <= latest_height,
        "Earliest height must always be less than or equal to latest height"
    );
    let mut start = *range.start();
    let mut end = *range.end();

    if end < start {
        warn!(requested_start = %start, requested_end = %end, "GetDecidedValues: Invalid inverted request range");
        return None;
    }

    if end < earliest_height || start > latest_height {
        warn!(
            requested_start = %start,
            requested_end = %end,
            %earliest_height,
            %latest_height,
            "GetDecidedValues: Requested range lies wholly outside available bounds",
        );
        return None;
    }

    if start < earliest_height {
        warn!(
            %earliest_height,
            requested_start = %start,
            "GetDecidedValues: Requested start is before earliest height; clamping",
        );
        start = earliest_height;
    }
    if end > latest_height {
        warn!(
            %latest_height,
            requested_end = %end,
            "GetDecidedValues: Requested end is beyond latest height; clamping",
        );
        end = latest_height;
    }

    debug_assert!(start <= end, "Post-clamp range must satisfy start <= end");

    if batch_size == 0 {
        warn!(
            requested = %DisplayRange(&(start..=end)),
            "GetDecidedValues: Batch size is zero; returning None",
        );
        return None;
    }

    let requested_count = end.as_u64() - start.as_u64() + 1;
    if requested_count > batch_size as u64 {
        end = start.increment_by((batch_size - 1) as u64);
        warn!(
            requested = %requested_count,
            max = %batch_size,
            clamped = %DisplayRange(&(start..=end)),
            "GetDecidedValues: Clamping request range to max batch size",
        );
    }

    Some(start..=end)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to easily create ranges
    fn h(start: u64, end: u64) -> RangeInclusive<Height> {
        Height::new(start)..=Height::new(end)
    }

    #[test]
    fn test_returns_range_unchanged_when_within_limits() {
        let range = h(10, 14);
        let batch_size = 10;
        let earliest = Height::new(5);
        let latest = Height::new(20);

        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, range);
    }

    #[test]
    fn test_clamps_when_range_exceeds_batch_size() {
        let range = h(10, 25); // 16 items total
        let batch_size = 10;
        let earliest = Height::new(5);
        let latest = Height::new(50);

        let result = get_clamped_request_range(range, earliest, latest, batch_size).unwrap();
        let expected = h(10, 19); // 10 total values: 10..=19

        assert_eq!(result, expected);
    }

    #[test]
    fn test_clamps_when_end_exceeds_latest_height() {
        let range = h(10, 25);
        let batch_size = 20;
        let earliest = Height::new(5);
        let latest = Height::new(15);

        let result = get_clamped_request_range(range, earliest, latest, batch_size).unwrap();
        let expected = h(10, 15);

        assert_eq!(result, expected);
    }

    #[test]
    fn test_clamps_for_both_batch_and_latest_height() {
        let range = h(10, 50);
        let batch_size = 10;
        let earliest = Height::new(5);
        let latest = Height::new(15);

        let result = get_clamped_request_range(range, earliest, latest, batch_size).unwrap();
        // First clamp: 10..=19 (batch)
        // Then clamp again: 10..=15 (latest)
        let expected = h(10, 15);

        assert_eq!(result, expected);
    }

    #[test]
    fn test_handles_range_equal_to_latest_height() {
        let range = h(10, 20);
        let batch_size = 15;
        let earliest = Height::new(5);
        let latest = Height::new(20);

        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, range);
    }

    #[test]
    fn test_handles_singleton_range() {
        // When start == end, still valid range
        let range = h(5, 5);
        let batch_size = 10;
        let earliest = Height::new(1);
        let latest = Height::new(100);

        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, range);
    }

    #[test]
    fn test_handles_zero_batch_size_is_none() {
        let range = h(10, 20);
        let batch_size = 0;
        let earliest = Height::new(5);
        let latest = Height::new(50);

        let res = get_clamped_request_range(range, earliest, latest, batch_size);
        assert!(res.is_none());
    }

    #[test]
    fn test_clamps_request_range_within_bounds() {
        let range = h(1, 10);
        let earliest = Height::new(1);
        let latest = Height::new(20);
        let batch_size = 100;
        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, range);
    }

    #[test]
    fn test_clamp_request_range_end_beyond_tip() {
        let range = h(5, 15);
        let earliest = Height::new(1);
        let latest = Height::new(10);
        let batch_size = 100;
        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, h(5, 10));
    }

    #[test]
    fn test_clamp_request_range_start_and_end_at_tip() {
        let range = h(10, 10);
        let earliest = Height::new(1);
        let latest = Height::new(10);
        let batch_size = 100;
        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, range);
    }

    #[test]
    fn test_clamp_request_range_start_above_tip() {
        let range = h(15, 20);
        let earliest = Height::new(1);
        let latest = Height::new(10);
        let batch_size = 10;
        let res = get_clamped_request_range(range.clone(), earliest, latest, batch_size);
        assert!(res.is_none());
    }

    #[test]
    fn test_inverted_above_bounds_is_none() {
        let range = h(20, 10);
        let earliest = Height::new(3);
        let latest = Height::new(8);
        let batch_size = 20;
        let res = get_clamped_request_range(range, earliest, latest, batch_size);
        assert!(res.is_none());
    }

    #[test]
    fn test_inverted_below_bounds_is_none() {
        let range = h(20, 19);
        let earliest = Height::new(40);
        let latest = Height::new(80);
        let batch_size = 10;
        let res = get_clamped_request_range(range, earliest, latest, batch_size);
        assert!(res.is_none());
    }

    #[test]
    fn test_inverted_within_bounds_is_none() {
        let range = h(20, 19);
        let earliest = Height::new(5);
        let latest = Height::new(50);
        let batch_size = 10;
        let res = get_clamped_request_range(range, earliest, latest, batch_size);
        assert!(res.is_none());
    }

    #[test]
    fn test_entirely_below_bounds_is_none() {
        let range = h(1, 2);
        let earliest = Height::new(5);
        let latest = Height::new(50);
        let batch_size = 10;
        let res = get_clamped_request_range(range, earliest, latest, batch_size);
        assert!(res.is_none());
    }

    #[test]
    fn test_entirely_above_bounds_is_none() {
        let range = h(60, 65);
        let earliest = Height::new(5);
        let latest = Height::new(50);
        let batch_size = 10;
        let res = get_clamped_request_range(range, earliest, latest, batch_size);
        assert!(res.is_none());
    }

    #[test]
    fn test_batch_cap_near_latest() {
        let range = h(95, 120);
        let earliest = Height::new(50);
        let latest = Height::new(100);
        let batch_size = 10;
        let result = get_clamped_request_range(range, earliest, latest, batch_size).unwrap();
        assert_eq!(result, h(95, 100));
    }

    #[test]
    fn test_bounds_first_then_batch() {
        let range = h(1, 100);
        let earliest = Height::new(20);
        let latest = Height::new(200);
        let batch_size = 5;
        let result = get_clamped_request_range(range, earliest, latest, batch_size).unwrap();
        assert_eq!(result, h(20, 24));
    }

    #[test]
    fn test_exact_batch_size_no_change() {
        let range = h(10, 19);
        let earliest = Height::new(1);
        let latest = Height::new(100);
        let batch_size = 10;
        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, range);
    }

    #[test]
    fn test_exact_limits() {
        let range = h(20, 29);
        let earliest = Height::new(20);
        let latest = Height::new(29);
        let batch_size = 10;
        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, range);
    }

    #[test]
    fn test_off_by_1() {
        let range = h(19, 29);
        let earliest = Height::new(20);
        let latest = Height::new(28);
        let batch_size = 8;
        let result =
            get_clamped_request_range(range.clone(), earliest, latest, batch_size).unwrap();
        assert_eq!(result, h(20, 27));
    }
}
