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

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashSet};
use std::time::{Duration, Instant};

use arc_consensus_types::{Height, ProposalPart, ProposalParts};

use malachitebft_app_channel::app::streaming::{Sequence, StreamId, StreamMessage};
use malachitebft_app_channel::app::types::PeerId;
use tracing::{error, warn};

/// Maximum number of messages allowed per stream
///
/// Maximum block size
/// = MAX_MESSAGES_PER_STREAM * CHUNK_SIZE
/// = 128 * 128 KiB = 16 MiB
const MAX_MESSAGES_PER_STREAM: usize = 128;

/// Maximum number of concurrent streams allowed per peer
///
/// Maximum memory per peer (if all streams at full capacity)
/// = MAX_STREAMS_PER_PEER * MAX_MESSAGES_PER_STREAM * CHUNK_SIZE
/// = 64 * 128 * 128 KiB = 1024 MiB
const MAX_STREAMS_PER_PEER: usize = 64;

/// Maximum total number of concurrent streams across all peers
///
/// Maximum total memory across all peers (if all streams at full capacity)
/// = MAX_TOTAL_STREAMS * MAX_MESSAGES_PER_STREAM * CHUNK_SIZE
/// = 100 * 128 * 128 KiB = 1600 MiB total memory
const MAX_TOTAL_STREAMS: usize = 100;

/// Size of chunks in which proposal data is split for streaming
pub(crate) const CHUNK_SIZE: usize = 128 * 1024;

/// Maximum age for a stream before it's evicted
const MAX_STREAM_AGE: Duration = Duration::from_secs(60);

struct MinSeq<T>(StreamMessage<T>);

impl<T> PartialEq for MinSeq<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.sequence == other.0.sequence
    }
}

impl<T> Eq for MinSeq<T> {}

impl<T> Ord for MinSeq<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.sequence.cmp(&self.0.sequence)
    }
}

impl<T> PartialOrd for MinSeq<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct MinHeap<T>(BinaryHeap<MinSeq<T>>);

impl<T> Default for MinHeap<T> {
    fn default() -> Self {
        Self(BinaryHeap::new())
    }
}

impl<T> MinHeap<T> {
    fn push(&mut self, msg: StreamMessage<T>) {
        self.0.push(MinSeq(msg));
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn drain(&mut self) -> Vec<T> {
        let mut vec = Vec::with_capacity(self.0.len());
        while let Some(MinSeq(msg)) = self.0.pop() {
            if let Some(data) = msg.content.into_data() {
                vec.push(data);
            }
        }
        vec
    }
}

struct StreamState {
    buffer: MinHeap<ProposalPart>,
    seen_sequences: HashSet<Sequence>,
    expected_messages: usize,
    message_count: usize,
    height: Option<Height>,
    fin_received: bool,
    is_complete: bool,
    created_at: Instant,
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new()
    }
}

enum InsertResult {
    Duplicate,
    Incomplete(Option<Height>),
    ExceededMaxMessages,
    ExceededMaxChunkSize(usize),
    Complete(Vec<ProposalPart>),
}

impl StreamState {
    fn new() -> Self {
        Self {
            buffer: MinHeap::default(),
            seen_sequences: HashSet::default(),
            expected_messages: 0,
            message_count: 0,
            height: None,
            fin_received: false,
            is_complete: false,
            created_at: Instant::now(),
        }
    }

    fn insert(&mut self, msg: StreamMessage<ProposalPart>) -> InsertResult {
        // Reject oversized Data chunks before recording the sequence as seen
        if let Some(ProposalPart::Data(data)) = msg.content.as_data() {
            if data.bytes.len() > CHUNK_SIZE {
                return InsertResult::ExceededMaxChunkSize(data.bytes.len());
            }
        }

        if !self.seen_sequences.insert(msg.sequence) {
            // We have already seen a message with this sequence number, ignore it.
            return InsertResult::Duplicate;
        }

        // Check if we've exceeded the maximum number of messages per stream
        if self.message_count >= MAX_MESSAGES_PER_STREAM {
            return InsertResult::ExceededMaxMessages;
        }

        // Increment message count
        self.message_count += 1;

        // This is the `Init` message.
        if let Some(init) = msg.content.as_data().and_then(|part| part.as_init()) {
            self.height = Some(init.height);
        }

        // This is the `Fin` message.
        if msg.is_fin() {
            self.fin_received = true;

            // If we have received the fin message, we can determine when we will be done.
            // We are done if we have already received all messages from 0 to fin.sequence,
            // included. That is to say, if we have received `fin.sequence + 1` messages.
            self.expected_messages = msg.sequence as usize + 1;
        }

        // Add the message to the buffer.
        self.buffer.push(msg);

        // Check if we are done, ie. we have received Init, Fin, and all messages in between.
        self.is_complete = self.height.is_some()
            && self.fin_received
            && self.buffer.len() == self.expected_messages;

        // Otherwise, abort early.
        if !self.is_complete {
            return InsertResult::Incomplete(self.height);
        }

        // We are complete, drain the buffer and assemble the proposal parts.
        let parts = self.buffer.drain();

        // NOTE: The order of the parts is guaranteed by the MinHeap
        InsertResult::Complete(parts)
    }
}

/// Map to track active proposal part streams from peers
///
/// Enforces the following limits:
/// - [`MAX_STREAMS_PER_PEER`] streams per peer
/// - [`MAX_MESSAGES_PER_STREAM`] messages per stream
/// - [`CHUNK_SIZE`] per data chunk
/// - [`MAX_TOTAL_STREAMS`] total concurrent streams
/// - Evict streams older than [`MAX_STREAM_AGE`]
/// - Immediately evict streams that exceed message or size limits
/// - Immediately evict streams from previous heights
pub struct PartStreamsMap {
    current_height: Height,
    streams: BTreeMap<(PeerId, StreamId), StreamState>,
    evicted: BTreeSet<(PeerId, StreamId)>, // TODO: Change to HashSet when StreamId is Hashable
    last_eviction: Instant,
}

impl PartStreamsMap {
    /// Create a new empty PartStreamsMap
    pub fn new(current_height: Height) -> Self {
        Self {
            streams: BTreeMap::new(),
            last_eviction: Instant::now(),
            evicted: BTreeSet::new(),
            current_height,
        }
    }

    /// Update the current height
    pub fn set_current_height(&mut self, height: Height) {
        self.current_height = height;
    }

    /// Insert a new proposal part message into the map
    ///
    /// ## Parameters
    /// - `peer_id`: The ID of the peer sending the message
    /// - `msg`: The stream message containing the proposal part
    ///
    /// ## Returns
    /// Returns `Some(ProposalParts)` if the stream is complete after insertion,
    /// otherwise returns `None`.
    pub fn insert(
        &mut self,
        peer_id: PeerId,
        msg: StreamMessage<ProposalPart>,
    ) -> Option<ProposalParts> {
        // First, evict any streams that have exceeded MAX_STREAM_AGE
        self.evict_old_streams();

        let stream_id = msg.stream_id.clone();
        let key = (peer_id, stream_id.clone());

        if self.evicted.contains(&key) {
            // This stream has been evicted before, ignore further messages.
            return None;
        }

        // Check if this is a new stream
        let is_new_stream = !self.streams.contains_key(&key);

        // If it's a new stream, check if we've exceeded the per-peer limit
        if is_new_stream {
            let stream_count = self.peer_streams_count(peer_id);
            if stream_count >= MAX_STREAMS_PER_PEER {
                warn!(
                    %peer_id,
                    %stream_count,
                    max = MAX_STREAMS_PER_PEER,
                    "Peer exceeded maximum number of concurrent streams, rejecting new stream"
                );

                return None;
            }

            // Check if we've exceeded the total streams limit
            if self.streams.len() >= MAX_TOTAL_STREAMS {
                // Evict the oldest stream to make room
                self.evict_oldest_stream();
            }
        }

        let state = self.streams.entry(key.clone()).or_default();

        // Insert the message into the stream state.
        let result = state.insert(msg);

        let parts = match result {
            InsertResult::Duplicate => {
                // Duplicate message, ignore
                None
            }

            InsertResult::Incomplete(None) => {
                // Stream is not yet complete and height is unknown
                None
            }

            InsertResult::Incomplete(Some(height)) => {
                // Stream is not yet complete but the height is known
                if height < self.current_height {
                    // Stream is stale
                    self.evict(&key);
                }

                None
            }

            InsertResult::ExceededMaxMessages => {
                warn!(
                    %peer_id,
                    %stream_id,
                    message_count = state.message_count,
                    max = MAX_MESSAGES_PER_STREAM,
                    "Stream exceeded maximum message count, message rejected"
                );

                // Stream exceeded max messages
                self.evict(&key);
                None
            }

            InsertResult::ExceededMaxChunkSize(actual) => {
                warn!(
                    %peer_id,
                    %stream_id,
                    actual,
                    max = CHUNK_SIZE,
                    "Stream sent oversized data chunk, evicting"
                );

                self.evict(&key);
                None
            }

            InsertResult::Complete(parts) => {
                // Stream is complete, stop tracking
                self.streams.remove(&key);
                Some(parts)
            }
        }?;

        // Assemble and return the ProposalParts if complete.
        match ProposalParts::new(parts) {
            Ok(proposal_parts) => Some(proposal_parts),
            Err(e) => {
                error!("Failed to assemble proposal parts: {e}");
                None
            }
        }
    }

    /// Count active streams for a given peer
    fn peer_streams_count(&mut self, peer_id: PeerId) -> usize {
        self.streams
            .keys()
            .filter(|(pid, _)| *pid == peer_id)
            .count()
    }

    /// Evict a stream from the map and mark it as evicted
    fn evict(&mut self, key: &(PeerId, StreamId)) {
        self.streams.remove(key);
        self.evicted.insert(key.clone());
    }

    /// Evict streams that have exceeded MAX_STREAM_AGE
    fn evict_old_streams(&mut self) {
        let now = Instant::now();

        // Only perform eviction check periodically,
        // to avoid excessive overhead on every insert.
        if now.duration_since(self.last_eviction) < MAX_STREAM_AGE {
            return;
        }

        // Update last eviction time
        self.last_eviction = now;

        // Clear the evicted set to avoid unbounded growth
        self.evicted.clear();

        // Identify streams to evict, ie. those older than MAX_STREAM_AGE
        let keys_to_remove: Vec<_> = self
            .streams
            .iter()
            .filter(|(_, state)| now.duration_since(state.created_at) > MAX_STREAM_AGE)
            .map(|(key, _)| key.clone())
            .collect();

        // Evict the identified streams
        for key @ (peer_id, stream_id) in &keys_to_remove {
            warn!(%peer_id, %stream_id, "Evicting stream due to age timeout");
            self.evict(key);
        }
    }

    /// Evict the oldest stream to make room for a new one
    fn evict_oldest_stream(&mut self) {
        if let Some((oldest_key, _)) = self
            .streams
            .iter()
            .min_by_key(|(_, state)| state.created_at)
        {
            let ref oldest_key @ (ref peer_id, ref stream_id) = oldest_key.clone();

            warn!(%peer_id, %stream_id, "Evicting oldest stream due to total streams limit");
            self.evict(oldest_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use arc_consensus_types::signing::Signature;
    use arc_consensus_types::{Address, Height, ProposalData, ProposalFin, ProposalInit, Round};
    use malachitebft_app_channel::app::streaming::StreamContent;
    use proptest::prelude::*;

    use super::*;

    // Helper functions to easily create test messages
    fn make_message(
        stream_id: &StreamId,
        sequence: Sequence,
        part: ProposalPart,
    ) -> StreamMessage<ProposalPart> {
        StreamMessage {
            stream_id: stream_id.clone(),
            sequence,
            content: StreamContent::Data(part),
        }
    }

    fn make_fin_message(stream_id: &StreamId, sequence: Sequence) -> StreamMessage<ProposalPart> {
        StreamMessage {
            stream_id: stream_id.clone(),
            sequence,
            content: StreamContent::Fin,
        }
    }

    fn make_init_part() -> ProposalPart {
        ProposalPart::Init(ProposalInit {
            height: Height::new(1),
            round: Round::new(0),
            pol_round: Round::new(0),
            proposer: Address::new([0xa; 20]),
        })
    }

    fn make_data_part(data: u8) -> ProposalPart {
        ProposalPart::Data(ProposalData {
            bytes: vec![data].into(),
        })
    }

    fn make_data_part_with_size(len: usize) -> ProposalPart {
        ProposalPart::Data(ProposalData {
            bytes: vec![0xAB; len].into(),
        })
    }

    fn make_fin_part() -> ProposalPart {
        ProposalPart::Fin(ProposalFin {
            signature: Signature::test(),
        })
    }

    // --- Unit Tests ---

    #[test]
    fn test_insert_single_message_stream_not_complete() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());

        let mut map = PartStreamsMap::new(Height::new(1));
        let msg = make_message(&stream_1, 0, make_init_part());

        let result = map.insert(peer_1, msg);

        assert!(
            result.is_none(),
            "Stream should not be complete after one message"
        );
        assert_eq!(map.streams.len(), 1, "Map should contain one active stream");
    }

    #[test]
    fn test_insert_in_order_completes_and_removes_stream() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());

        let mut map = PartStreamsMap::new(Height::new(1));
        let init_msg = make_message(&stream_1, 0, make_init_part());
        let data_msg = make_message(&stream_1, 1, make_data_part(42));
        let data_fin_msg = make_message(&stream_1, 2, make_fin_part());
        let fin_msg = make_fin_message(&stream_1, 3);

        // Insert Init and Data parts
        assert!(map.insert(peer_1, init_msg).is_none());
        assert_eq!(map.streams.len(), 1);
        assert!(map.insert(peer_1, data_msg).is_none());
        assert_eq!(map.streams.len(), 1);
        assert!(map.insert(peer_1, data_fin_msg).is_none());
        assert_eq!(map.streams.len(), 1);

        // Insert final part
        let result = map.insert(peer_1, fin_msg);

        assert!(
            result.is_some(),
            "Stream should be complete and return ProposalParts"
        );
        assert!(
            map.streams.is_empty(),
            "Map should be empty after stream is complete"
        );
    }

    #[test]
    fn test_insert_out_of_order_completes_and_removes_stream() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());

        let init_msg = make_message(&stream_1, 0, make_init_part());
        let data_msg = make_message(&stream_1, 1, make_data_part(42));
        let data_fin_msg = make_message(&stream_1, 2, make_fin_part());
        let fin_msg = make_fin_message(&stream_1, 3);

        let parts = [
            init_msg.clone(),
            data_msg.clone(),
            data_fin_msg.clone(),
            fin_msg.clone(),
        ];

        use itertools::Itertools;

        // Test all permutations of message order
        for perm in parts.iter().permutations(parts.len()) {
            let mut map = PartStreamsMap::new(Height::new(1));

            // Insert all but the last message
            for msg in &perm[..3] {
                assert!(map.insert(peer_1, (*msg).clone()).is_none());
                assert_eq!(map.streams.len(), 1);
            }

            // Insert the last message, which should complete the stream
            let result = map.insert(peer_1, perm[3].clone());

            assert!(
                result.is_some(),
                "Stream should be complete and return ProposalParts"
            );
            assert!(
                map.streams.is_empty(),
                "Map should be empty after stream is complete"
            );
        }
    }

    #[test]
    fn test_insert_duplicate_sequence_is_ignored() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());

        let mut map = PartStreamsMap::new(Height::new(1));
        let init_msg = make_message(&stream_1, 0, make_init_part());
        let data_msg = make_message(&stream_1, 1, make_data_part(42));
        let data_msg_duplicate = make_message(&stream_1, 1, make_data_part(99)); // Same seq
        let data_fin_msg = make_message(&stream_1, 2, make_fin_part());
        let fin_msg = make_fin_message(&stream_1, 3);

        map.insert(peer_1, init_msg);
        map.insert(peer_1, data_msg);
        map.insert(peer_1, data_fin_msg);

        // Insert duplicate message
        let result_duplicate = map.insert(peer_1, data_msg_duplicate);
        assert!(
            result_duplicate.is_none(),
            "Duplicate message should be ignored and return None"
        );

        // The stream state should not be corrupted and should complete normally
        let result_final = map.insert(peer_1, fin_msg);
        assert!(
            result_final.is_some(),
            "Stream should complete successfully after ignoring a duplicate"
        );
        assert!(map.streams.is_empty(), "Completed stream should be removed");
    }

    #[test]
    fn test_stream_with_missing_part_is_not_completed() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());

        let mut map = PartStreamsMap::new(Height::new(1));
        let init_msg = make_message(&stream_1, 0, make_init_part());
        // Sequence 1 is missing
        let fin_msg = make_message(&stream_1, 2, make_fin_part());

        map.insert(peer_1, init_msg);
        let result = map.insert(peer_1, fin_msg);

        assert!(
            result.is_none(),
            "Stream should not complete if a part is missing"
        );
        assert_eq!(
            map.streams.len(),
            1,
            "Incomplete stream should remain in the map"
        );
    }

    #[test]
    fn test_multiple_interleaved_streams() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());
        let stream_2 = StreamId::new(vec![202].into());

        let mut map = PartStreamsMap::new(Height::new(1));

        // Messages for two different streams
        let s1_init = make_message(&stream_1, 0, make_init_part());
        let s1_data_fin = make_message(&stream_1, 1, make_fin_part());
        let s1_fin = make_fin_message(&stream_1, 2);
        let s2_init = make_message(&stream_2, 0, make_init_part());
        let s2_data = make_message(&stream_2, 1, make_data_part(10));
        let s2_data_fin = make_message(&stream_2, 2, make_fin_part());
        let s2_fin = make_fin_message(&stream_2, 3);

        // Interleave inserts
        map.insert(peer_1, s1_init);
        assert_eq!(map.streams.len(), 1);
        map.insert(peer_1, s2_init);
        assert_eq!(
            map.streams.len(),
            2,
            "Map should track two separate streams"
        );

        map.insert(peer_1, s1_data_fin);
        assert_eq!(map.streams.len(), 2);
        map.insert(peer_1, s2_data_fin);
        assert_eq!(map.streams.len(), 2);

        // Complete stream 1
        let s1_result = map.insert(peer_1, s1_fin);
        assert!(s1_result.is_some(), "Stream 1 should complete");
        assert_eq!(
            map.streams.len(),
            1,
            "Map should have one stream left after S1 completes"
        );

        // Continue and complete stream 2
        map.insert(peer_1, s2_data);
        let s2_result = map.insert(peer_1, s2_fin);
        assert!(s2_result.is_some(), "Stream 2 should complete");
        assert!(
            map.streams.is_empty(),
            "Map should be empty after all streams are complete"
        );
    }

    #[test]
    fn test_per_peer_stream_limit() {
        let peer_1 = PeerId::random();
        let mut map = PartStreamsMap::new(Height::new(1));

        // Create MAX_STREAMS_PER_PEER streams
        for i in 0..MAX_STREAMS_PER_PEER {
            let stream = StreamId::new(vec![i as u8].into());
            let msg = make_message(&stream, 0, make_init_part());
            assert!(
                map.insert(peer_1, msg).is_none(),
                "Should accept stream {i}"
            );
        }

        assert_eq!(
            map.streams.len(),
            MAX_STREAMS_PER_PEER,
            "Should have exactly MAX_STREAMS_PER_PEER streams"
        );

        // Try to create one more stream - should be rejected
        let overflow_stream = StreamId::new(vec![255].into());
        let overflow_msg = make_message(&overflow_stream, 0, make_init_part());
        let result = map.insert(peer_1, overflow_msg);

        assert!(
            result.is_none(),
            "Should reject stream exceeding per-peer limit"
        );
        assert_eq!(
            map.streams.len(),
            MAX_STREAMS_PER_PEER,
            "Stream count should remain unchanged after rejection"
        );

        // Complete one stream to free up a slot
        let stream_0 = StreamId::new(vec![0].into());
        let fin_msg = make_message(&stream_0, 1, make_fin_part());
        map.insert(peer_1, fin_msg);
        let fin = make_fin_message(&stream_0, 2);
        map.insert(peer_1, fin);

        assert_eq!(
            map.streams.len(),
            MAX_STREAMS_PER_PEER - 1,
            "Should have one less stream after completion"
        );

        // Now we should be able to add a new stream
        let new_stream = StreamId::new(vec![100].into());
        let new_msg = make_message(&new_stream, 0, make_init_part());
        assert!(
            map.insert(peer_1, new_msg).is_none(),
            "Should accept new stream after one completes"
        );
        assert_eq!(map.streams.len(), MAX_STREAMS_PER_PEER);
    }

    #[test]
    fn test_per_stream_message_limit() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());
        let mut map = PartStreamsMap::new(Height::new(1));

        // Send Init
        let init_msg = make_message(&stream_1, 0, make_init_part());
        assert!(map.insert(peer_1, init_msg).is_none());

        // Send MAX_MESSAGES_PER_STREAM - 1 data messages (accounting for init already sent)
        for i in 1..MAX_MESSAGES_PER_STREAM {
            let msg = make_message(&stream_1, i as u64, make_data_part(i as u8));
            let result = map.insert(peer_1, msg);
            assert!(
                result.is_none(),
                "Should accept message {i} of {MAX_MESSAGES_PER_STREAM}"
            );
        }

        assert_eq!(map.streams.len(), 1, "Stream should still be active");

        // Try to send one more message - should be rejected
        let overflow_msg = make_message(
            &stream_1,
            MAX_MESSAGES_PER_STREAM as u64,
            make_data_part(MAX_MESSAGES_PER_STREAM as u8),
        );
        let result = map.insert(peer_1, overflow_msg);

        assert!(
            result.is_none(),
            "Should reject message exceeding per-stream limit"
        );

        assert_eq!(map.streams.len(), 0, "Stream has been evicted");
    }

    #[test]
    fn test_per_peer_limit_independent_across_peers() {
        let peer_1 = PeerId::random();
        let peer_2 = PeerId::random();
        let mut map = PartStreamsMap::new(Height::new(1));

        // Peer 1 creates MAX_STREAMS_PER_PEER streams
        for i in 0..MAX_STREAMS_PER_PEER {
            let stream = StreamId::new(vec![i as u8].into());
            let msg = make_message(&stream, 0, make_init_part());
            map.insert(peer_1, msg);
        }

        // Peer 2 should also be able to create MAX_STREAMS_PER_PEER streams
        for i in 0..MAX_STREAMS_PER_PEER {
            let stream = StreamId::new(vec![i as u8].into());
            let msg = make_message(&stream, 0, make_init_part());
            let result = map.insert(peer_2, msg);
            assert!(
                result.is_none(),
                "Peer 2 should be able to create stream {i}"
            );
        }

        if MAX_STREAMS_PER_PEER * 2 <= MAX_TOTAL_STREAMS {
            // Both peers should have their streams accepted
            assert_eq!(
                map.streams.len(),
                MAX_STREAMS_PER_PEER * 2,
                "Should have streams from both peers"
            );
        } else {
            // Total streams limit should have been enforced
            assert_eq!(
                map.streams.len(),
                MAX_TOTAL_STREAMS,
                "Should have total streams limited to MAX_TOTAL_STREAMS"
            );
        }

        // Both peers should now be at their limit
        let overflow_stream = StreamId::new(vec![255].into());
        let overflow_msg_p1 = make_message(&overflow_stream, 0, make_init_part());
        assert!(
            map.insert(peer_1, overflow_msg_p1).is_none(),
            "Peer 1 should be rejected"
        );

        let overflow_msg_p2 = make_message(&overflow_stream, 0, make_init_part());
        assert!(
            map.insert(peer_2, overflow_msg_p2).is_none(),
            "Peer 2 should be rejected"
        );
    }

    #[test]
    fn test_stream_age_eviction() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());
        let stream_2 = StreamId::new(vec![102].into());
        let stream_3 = StreamId::new(vec![103].into());

        let mut map = PartStreamsMap::new(Height::new(1));

        // Create first stream
        let msg1 = make_message(&stream_1, 0, make_init_part());
        map.insert(peer_1, msg1);
        assert_eq!(map.streams.len(), 1);

        // Manually set the created_at time to be older than MAX_STREAM_AGE
        if let Some(state) = map.streams.get_mut(&(peer_1, stream_1.clone())) {
            state.created_at = Instant::now() - MAX_STREAM_AGE - Duration::from_secs(1);
        }

        // Create a second stream (this will not trigger eviction of the old one yet)
        let msg2 = make_message(&stream_2, 0, make_init_part());
        map.insert(peer_1, msg2);

        // The old stream should not have been evicted
        assert!(
            map.streams.contains_key(&(peer_1, stream_1.clone())),
            "Old stream should not have been evicted yet"
        );
        assert!(
            map.streams.contains_key(&(peer_1, stream_2.clone())),
            "New stream should be present"
        );

        // Set last_eviction far enough in the past to force eviction check
        map.last_eviction = Instant::now() - MAX_STREAM_AGE - Duration::from_secs(1);

        // Create a third stream to trigger eviction of the old one
        let msg3 = make_message(&stream_3, 0, make_init_part());
        map.insert(peer_1, msg3);

        // The old stream should have been evicted
        assert!(
            !map.streams.contains_key(&(peer_1, stream_1)),
            "Old stream should have been evicted"
        );
        assert!(
            map.streams.contains_key(&(peer_1, stream_2)),
            "Second stream should be present"
        );
        assert!(
            map.streams.contains_key(&(peer_1, stream_3)),
            "New stream should be present"
        );
    }

    #[test]
    fn test_total_streams_limit_eviction() {
        let mut map = PartStreamsMap::new(Height::new(1));
        let mut peers = Vec::new();

        // Create MAX_TOTAL_STREAMS streams from different peers
        for _ in 0..MAX_TOTAL_STREAMS {
            let peer = PeerId::random();
            peers.push(peer);
            let stream = StreamId::new(vec![0].into());
            let msg = make_message(&stream, 0, make_init_part());
            map.insert(peer, msg);
        }

        assert_eq!(
            map.streams.len(),
            MAX_TOTAL_STREAMS,
            "Should have MAX_TOTAL_STREAMS streams"
        );

        // Get the oldest stream's creation time to verify it gets evicted
        let oldest_peer = peers[0];
        let oldest_stream = StreamId::new(vec![0].into());
        let oldest_key = (oldest_peer, oldest_stream.clone());

        // Manually set the first stream to be the oldest
        if let Some(state) = map.streams.get_mut(&oldest_key) {
            state.created_at = Instant::now() - Duration::from_secs(100);
        }

        // Try to create one more stream from a new peer
        let new_peer = PeerId::random();
        let new_stream = StreamId::new(vec![0].into());
        let new_msg = make_message(&new_stream, 0, make_init_part());
        map.insert(new_peer, new_msg);

        // Should still have MAX_TOTAL_STREAMS (oldest evicted, new one added)
        assert_eq!(
            map.streams.len(),
            MAX_TOTAL_STREAMS,
            "Should still have MAX_TOTAL_STREAMS streams after eviction"
        );

        // The oldest stream should have been evicted
        assert!(
            !map.streams.contains_key(&oldest_key),
            "Oldest stream should have been evicted"
        );

        // The new stream should be present
        assert!(
            map.streams.contains_key(&(new_peer, new_stream)),
            "New stream should be present"
        );
    }

    #[test]
    fn test_completed_streams_dont_count_toward_limits() {
        let peer_1 = PeerId::random();
        let mut map = PartStreamsMap::new(Height::new(1));

        // Create and complete MAX_STREAMS_PER_PEER streams
        for i in 0..MAX_STREAMS_PER_PEER {
            let stream = StreamId::new(vec![i as u8].into());

            // Send complete stream
            let init = make_message(&stream, 0, make_init_part());
            let fin_part = make_message(&stream, 1, make_fin_part());
            let fin = make_fin_message(&stream, 2);

            map.insert(peer_1, init);
            map.insert(peer_1, fin_part);
            map.insert(peer_1, fin);
        }

        // All streams should be completed and removed
        assert_eq!(
            map.streams.len(),
            0,
            "All completed streams should be removed"
        );

        // Should be able to create MAX_STREAMS_PER_PEER new streams
        for i in 0..MAX_STREAMS_PER_PEER {
            let stream = StreamId::new(vec![(i + 100) as u8].into());
            let msg = make_message(&stream, 0, make_init_part());
            assert!(
                map.insert(peer_1, msg).is_none(),
                "Should accept new stream {i} after previous ones completed"
            );
        }

        assert_eq!(
            map.streams.len(),
            MAX_STREAMS_PER_PEER,
            "Should have MAX_STREAMS_PER_PEER new streams"
        );
    }

    #[test]
    fn test_evict_old_streams_removes_all_expired() {
        let mut map = PartStreamsMap::new(Height::new(1));
        let peer = PeerId::random();

        // Create 3 streams, 2 old and 1 new
        for i in 0..3 {
            let stream = StreamId::new(vec![i].into());
            let msg = make_message(&stream, 0, make_init_part());
            map.insert(peer, msg);
        }

        // Age first two streams
        for i in 0..2 {
            let stream = StreamId::new(vec![i].into());
            if let Some(state) = map.streams.get_mut(&(peer, stream)) {
                state.created_at = Instant::now() - MAX_STREAM_AGE - Duration::from_secs(1);
            }
        }

        // No eviction yet because `last_eviction` is recent
        map.last_eviction = Instant::now();

        let stream_2 = StreamId::new(vec![2].into());
        let msg = make_message(&stream_2, 1, make_data_part(1));
        map.insert(peer, msg);

        // Should still have all 3 streams
        assert_eq!(map.streams.len(), 3);

        // Now trigger eviction by setting `last_eviction` far in the past
        map.last_eviction = Instant::now() - MAX_STREAM_AGE - Duration::from_secs(1);

        // Trigger eviction by inserting new message into remaining stream
        let msg = make_message(&stream_2, 1, make_data_part(2));
        map.insert(peer, msg);

        // Should only have 1 stream left
        assert_eq!(map.streams.len(), 1);
        assert!(map.streams.contains_key(&(peer, stream_2)));
    }

    #[test]
    fn test_message_limit_independent_across_streams() {
        let peer = PeerId::random();
        let mut map = PartStreamsMap::new(Height::new(1));

        // Fill first stream to capacity
        let stream_1 = StreamId::new(vec![1].into());
        for i in 0..MAX_MESSAGES_PER_STREAM {
            let msg = make_message(&stream_1, i as u64, make_data_part(i as u8));
            map.insert(peer, msg);
        }

        // Second stream should still accept messages
        let stream_2 = StreamId::new(vec![2].into());
        let msg = make_message(&stream_2, 0, make_init_part());
        assert!(
            map.insert(peer, msg).is_none(),
            "Second stream should accept messages despite first being at limit"
        );
    }

    #[test]
    fn test_evicted_stream_rejects_new_messages() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());
        let mut map = PartStreamsMap::new(Height::new(1));

        // Send Init
        let init_msg = make_message(&stream_1, 0, make_init_part());
        map.insert(peer_1, init_msg);

        // Exceed message limit to trigger eviction
        for i in 1..=MAX_MESSAGES_PER_STREAM {
            let msg = make_message(&stream_1, i as u64, make_data_part(i as u8));
            map.insert(peer_1, msg);
        }

        // Verify stream was evicted
        assert_eq!(map.streams.len(), 0, "Stream should be evicted");

        // Try to send another message to the same stream
        let new_msg = make_message(
            &stream_1,
            (MAX_MESSAGES_PER_STREAM + 1) as u64,
            make_data_part(99),
        );
        let result = map.insert(peer_1, new_msg);

        assert!(
            result.is_none(),
            "Message to evicted stream should be rejected"
        );
        assert_eq!(
            map.streams.len(),
            0,
            "No new stream should be created for evicted stream"
        );
    }

    #[test]
    fn test_stale_height_streams_evicted() {
        let peer_1 = PeerId::random();
        let stream_1 = StreamId::new(vec![101].into());
        let mut map = PartStreamsMap::new(Height::new(5));

        // Send Init message for old height (height 3)
        let mut init_part = make_init_part();
        if let ProposalPart::Init(ref mut init) = init_part {
            init.height = Height::new(3);
        }
        let init_msg = make_message(&stream_1, 0, init_part);
        map.insert(peer_1, init_msg);

        // Send a data message
        let data_msg = make_message(&stream_1, 1, make_data_part(42));
        map.insert(peer_1, data_msg);

        // Verify stream was evicted
        assert_eq!(map.streams.len(), 0, "Stale stream should be evicted");
        assert!(
            map.evicted.contains(&(peer_1, stream_1)),
            "Stream should be marked as evicted"
        );
    }

    #[test]
    fn test_evicted_set_cleared_periodically() {
        let mut map = PartStreamsMap::new(Height::new(1));
        let peer = PeerId::random();

        // Create and evict a stream
        let stream = StreamId::new(vec![1].into());
        for i in 0..=MAX_MESSAGES_PER_STREAM {
            let msg = make_message(&stream, i as u64, make_data_part(i as u8));
            map.insert(peer, msg);
        }

        assert!(
            !map.evicted.is_empty(),
            "Evicted set should contain entries"
        );

        // Simulate time passing beyond MAX_STREAM_AGE
        map.last_eviction = Instant::now() - MAX_STREAM_AGE - Duration::from_secs(1);

        // Trigger eviction cycle
        map.evict_old_streams();

        assert!(
            map.evicted.is_empty(),
            "Evicted set should be cleared after eviction cycle"
        );
    }

    #[test]
    fn test_oversized_chunk_rejected() {
        let peer = PeerId::random();
        let stream = StreamId::new(vec![1].into());
        let mut map = PartStreamsMap::new(Height::new(1));

        let init_msg = make_message(&stream, 0, make_init_part());
        map.insert(peer, init_msg);

        // Send a data chunk exceeding CHUNK_SIZE
        let oversized = make_data_part_with_size(CHUNK_SIZE + 1);
        let msg = make_message(&stream, 1, oversized);
        let result = map.insert(peer, msg);

        assert!(result.is_none(), "Oversized chunk should be rejected");
        assert!(
            map.streams.is_empty(),
            "Stream should be evicted after oversized chunk"
        );
        assert!(
            map.evicted.contains(&(peer, stream)),
            "Stream should be marked as evicted"
        );
    }

    #[test]
    fn test_normal_chunk_accepted() {
        let peer = PeerId::random();
        let stream = StreamId::new(vec![1].into());
        let mut map = PartStreamsMap::new(Height::new(1));

        let init_msg = make_message(&stream, 0, make_init_part());
        map.insert(peer, init_msg);

        // CHUNK_SIZE - 1 should be accepted
        let under_limit = make_data_part_with_size(CHUNK_SIZE - 1);
        let msg = make_message(&stream, 1, under_limit);
        map.insert(peer, msg);
        assert_eq!(map.streams.len(), 1, "Under-limit chunk should be accepted");

        // Data chunk exactly at CHUNK_SIZE should be accepted
        let at_limit = make_data_part_with_size(CHUNK_SIZE);
        let msg = make_message(&stream, 2, at_limit);
        let result = map.insert(peer, msg);

        assert!(result.is_none(), "Stream should not be complete yet");
        assert_eq!(map.streams.len(), 1, "Stream should still be active");
    }

    #[test]
    fn test_non_data_parts_not_subject_to_size_check() {
        let peer = PeerId::random();
        let stream = StreamId::new(vec![1].into());
        let mut map = PartStreamsMap::new(Height::new(1));

        // Init and Fin are not Data variants, so they bypass the byte limit
        let init_msg = make_message(&stream, 0, make_init_part());
        map.insert(peer, init_msg);
        assert_eq!(map.streams.len(), 1, "Init should be accepted");

        let fin_part_msg = make_message(&stream, 1, make_fin_part());
        map.insert(peer, fin_part_msg);
        assert_eq!(map.streams.len(), 1, "Fin part should be accepted");

        let fin_msg = make_fin_message(&stream, 2);
        let result = map.insert(peer, fin_msg);

        assert!(
            result.is_some(),
            "Stream should complete — Init/Fin are not subject to chunk size limit"
        );
    }

    // --- Property-Based Tests ---

    proptest! {
        #[test]
        fn prop_per_peer_stream_limit_never_exceeded(
            stream_attempts in prop::collection::vec(any::<u8>(), 1..50)
        ) {
            let peer = PeerId::random();
            let mut map = PartStreamsMap::new(Height::new(1));

            // Try to create streams using different IDs
            for stream_id_byte in stream_attempts {
                let stream = StreamId::new(vec![stream_id_byte].into());
                let msg = make_message(&stream, 0, make_init_part());
                map.insert(peer, msg);

                // Count how many streams this peer has
                let peer_stream_count = map.peer_streams_count(peer);

                // Should never exceed the limit
                prop_assert!(
                    peer_stream_count <= MAX_STREAMS_PER_PEER,
                    "Peer stream count {} exceeded limit {}",
                    peer_stream_count,
                    MAX_STREAMS_PER_PEER
                );
            }
        }

        #[test]
        fn prop_per_stream_message_limit_never_exceeded(
            message_count in 1..500usize
        ) {
            let peer = PeerId::random();
            let stream = StreamId::new(vec![1].into());
            let mut map = PartStreamsMap::new(Height::new(1));

            // Try to send many messages to the same stream
            for i in 0..message_count {
                let msg = make_message(&stream, i as u64, make_data_part((i % 256) as u8));
                map.insert(peer, msg);

                // Check the stream state if it still exists
                if let Some(state) = map.streams.get(&(peer, stream.clone())) {
                    prop_assert!(
                        state.message_count <= MAX_MESSAGES_PER_STREAM,
                        "Stream message count {} exceeded limit {}",
                        state.message_count,
                        MAX_MESSAGES_PER_STREAM
                    );
                }
            }
        }

        #[test]
        fn prop_total_streams_limit_never_exceeded(
            peer_count in 1..50usize,
            streams_per_peer in 1..20usize
        ) {
            let mut map = PartStreamsMap::new(Height::new(1));
            let mut peers = Vec::new();

            // Generate unique peers
            for _ in 0..peer_count {
                peers.push(PeerId::random());
            }

            // Try to create multiple streams for each peer
            for peer in &peers {
                for stream_idx in 0..streams_per_peer {
                    let stream = StreamId::new(vec![(stream_idx % 256) as u8, (stream_idx / 256) as u8].into());
                    let msg = make_message(&stream, 0, make_init_part());
                    map.insert(*peer, msg);

                    // Total streams should never exceed the limit
                    prop_assert!(
                        map.streams.len() <= MAX_TOTAL_STREAMS,
                        "Total stream count {} exceeded limit {}",
                        map.streams.len(),
                        MAX_TOTAL_STREAMS
                    );
                }
            }
        }

        #[test]
        fn prop_stream_age_eviction_works(
            stream_count in 1..30usize
        ) {
            let peer = PeerId::random();
            let mut map = PartStreamsMap::new(Height::new(1));

            // Create streams
            for i in 0..stream_count {
                let stream = StreamId::new(vec![i as u8].into());
                let msg = make_message(&stream, 0, make_init_part());
                map.insert(peer, msg);
            }

            let initial_count = map.streams.len();

            // Age all streams beyond MAX_STREAM_AGE
            for state in map.streams.values_mut() {
                state.created_at = Instant::now() - MAX_STREAM_AGE - Duration::from_secs(1);
            }

            // Set last eviction time far in the past to force eviction on next insert
            map.last_eviction = Instant::now() - MAX_STREAM_AGE - Duration::from_secs(1);

            // Trigger eviction by inserting a new stream
            let new_stream = StreamId::new(vec![255].into());
            let msg = make_message(&new_stream, 0, make_init_part());
            map.insert(peer, msg);

            // All old streams should be evicted, only the new one should remain
            prop_assert!(
                map.streams.len() <= 1,
                "Expected at most 1 stream after aging {}, but found {}",
                initial_count,
                map.streams.len()
            );
        }

        #[test]
        fn prop_completed_streams_are_removed(
            completion_count in 1..20usize
        ) {
            let peer = PeerId::random();
            let mut map = PartStreamsMap::new(Height::new(1));

            // Complete multiple streams
            for i in 0..completion_count {
                let stream = StreamId::new(vec![i as u8].into());

                // Send complete stream: init, data, fin_part, fin
                let init = make_message(&stream, 0, make_init_part());
                let data = make_message(&stream, 1, make_data_part(42));
                let fin_part = make_message(&stream, 2, make_fin_part());
                let fin = make_fin_message(&stream, 3);

                map.insert(peer, init);
                map.insert(peer, data);
                map.insert(peer, fin_part);
                map.insert(peer, fin);
            }

            // All completed streams should be removed
            prop_assert_eq!(
                map.streams.len(),
                0,
                "Expected all completed streams to be removed, but {} remain",
                map.streams.len()
            );
        }

        #[test]
        fn prop_limits_independent_across_peers(
            peer_count in 2..10usize
        ) {
            let mut map = PartStreamsMap::new(Height::new(1));
            let mut peers = Vec::new();

            // Generate unique peers
            for _ in 0..peer_count {
                peers.push(PeerId::random());
            }

            // Each peer creates streams up to their limit
            for peer in &peers {
                for i in 0..MAX_STREAMS_PER_PEER {
                    let stream = StreamId::new(vec![i as u8].into());
                    let msg = make_message(&stream, 0, make_init_part());
                    map.insert(*peer, msg);
                }
            }

            // Verify each peer's stream count independently
            for peer in &peers {
                let stream_count = map.peer_streams_count(*peer);

                prop_assert!(
                    stream_count <= MAX_STREAMS_PER_PEER,
                    "Peer stream count {} exceeded limit {} for peer {:?}",
                    stream_count,
                    MAX_STREAMS_PER_PEER,
                    peer
                );
            }

            // Also verify total doesn't exceed global limit
            prop_assert!(
                map.streams.len() <= MAX_TOTAL_STREAMS,
                "Total stream count {} exceeded limit {}",
                map.streams.len(),
                MAX_TOTAL_STREAMS
            );
        }

        #[test]
        fn prop_incomplete_streams_remain_buffered(
            message_count in 1..10usize
        ) {
            let peer = PeerId::random();
            let stream = StreamId::new(vec![1].into());
            let mut map = PartStreamsMap::new(Height::new(1));

            // Send incomplete stream (no Fin message)
            for i in 0..message_count {
                let msg = make_message(&stream, i as u64, make_data_part(i as u8));
                let result = map.insert(peer, msg);

                // Should never complete without Fin
                prop_assert!(
                    result.is_none(),
                    "Stream should not complete without Fin message"
                );
            }

            // Stream should still be in the map
            prop_assert!(
                map.streams.contains_key(&(peer, stream)),
                "Incomplete stream should remain buffered"
            );
        }

        #[test]
        fn prop_out_of_order_messages_complete_correctly(
            // Generate a shuffled sequence of indices
            seed in any::<u64>()
        ) {
            use rand::{SeedableRng, seq::SliceRandom};
            use rand::rngs::StdRng;

            let peer = PeerId::random();
            let stream = StreamId::new(vec![1].into());
            let mut map = PartStreamsMap::new(Height::new(1));

            // Create messages in order
            let init = make_message(&stream, 0, make_init_part());
            let data = make_message(&stream, 1, make_data_part(42));
            let fin_part = make_message(&stream, 2, make_fin_part());
            let fin = make_fin_message(&stream, 3);

            let mut messages = [init, data, fin_part, fin];

            // Shuffle messages
            let mut rng = StdRng::seed_from_u64(seed);
            messages.shuffle(&mut rng);

            // Insert all but the last message
            for msg in &messages[..3] {
                let result = map.insert(peer, msg.clone());
                prop_assert!(
                    result.is_none(),
                    "Stream should not complete until all messages received"
                );
            }

            // Insert the last message, should complete
            let result = map.insert(peer, messages[3].clone());
            prop_assert!(
                result.is_some(),
                "Stream should complete when all messages received, regardless of order"
            );

            // Stream should be removed after completion
            prop_assert!(
                !map.streams.contains_key(&(peer, stream)),
                "Completed stream should be removed from map"
            );
        }

        #[test]
        fn prop_duplicate_sequences_ignored(
            message_count in 1..20usize,
            duplicate_indices in prop::collection::vec(0..20usize, 1..10)
        ) {
            let peer = PeerId::random();
            let stream = StreamId::new(vec![1].into());
            let mut map = PartStreamsMap::new(Height::new(1));

            // Send initial messages
            for i in 0..message_count {
                let msg = make_message(&stream, i as u64, make_data_part(i as u8));
                map.insert(peer, msg);
            }

            let state_before = map.streams.get(&(peer, stream.clone()))
                .map(|s| s.message_count);

            // Send duplicate messages
            for &idx in &duplicate_indices {
                if idx < message_count {
                    let duplicate = make_message(&stream, idx as u64, make_data_part(99));
                    map.insert(peer, duplicate);
                }
            }

            // Message count should not increase from duplicates
            if let Some(state) = map.streams.get(&(peer, stream)) {
                prop_assert_eq!(
                    state.message_count,
                    state_before.unwrap_or(0),
                    "Duplicate messages should not increase message count"
                );
            }
        }

        #[test]
        fn prop_missing_parts_prevent_completion(
            total_parts in 5..15usize, // Need at least 5 parts: init, data1, data2, fin_part, fin
        ) {
            let peer = PeerId::random();
            let stream = StreamId::new(vec![1].into());
            let mut map = PartStreamsMap::new(Height::new(1));

            // Choose a missing index in the middle of data parts (not init, not fin_part, not fin)
            // For total_parts=5: seq 0=init, 1=data, 2=data, 3=fin_part, 4=fin
            // We can skip seq 1 or 2
            let missing_index = 1 + (total_parts % 2); // Will be 1 or 2

            // Send init
            map.insert(peer, make_message(&stream, 0, make_init_part()));

            // Send data parts, skipping the missing one
            // Data parts go from seq 1 to seq (total_parts - 3)
            for i in 1..total_parts - 2 {
                if i != missing_index {
                    let msg = make_message(&stream, i as u64, make_data_part(i as u8));
                    map.insert(peer, msg);
                }
            }

            // Send fin_part and fin
            let fin_part = make_message(&stream, (total_parts - 2) as u64, make_fin_part());
            let fin = make_fin_message(&stream, (total_parts - 1) as u64);

            map.insert(peer, fin_part);
            let result = map.insert(peer, fin);

            // Should not complete with missing part
            prop_assert!(
                result.is_none(),
                "Stream should not complete with missing part at index {}",
                missing_index
            );

            // Stream should remain in map
            prop_assert!(
                map.streams.contains_key(&(peer, stream)),
                "Incomplete stream should remain in map"
            );
        }

        #[test]
        fn prop_multiple_interleaved_streams_independent(
            stream_count in 2..8usize,
            messages_per_stream in 2..10usize,
            seed in any::<u64>()
        ) {
            use rand::{SeedableRng, seq::SliceRandom};
            use rand::rngs::StdRng;

            let peer = PeerId::random();
            let mut map = PartStreamsMap::new(Height::new(1));
            let mut rng = StdRng::seed_from_u64(seed);

            // Create all messages for all streams
            let mut all_messages = Vec::new();

            for stream_idx in 0..stream_count {
                let stream = StreamId::new(vec![stream_idx as u8].into());

                // Init
                all_messages.push((stream_idx, make_message(&stream, 0, make_init_part())));

                // Data parts
                for msg_idx in 1..messages_per_stream {
                    all_messages.push((
                        stream_idx,
                        make_message(&stream, msg_idx as u64, make_data_part(msg_idx as u8))
                    ));
                }

                // Fin part
                all_messages.push((
                    stream_idx,
                    make_message(&stream, messages_per_stream as u64, make_fin_part())
                ));

                // Fin
                all_messages.push((
                    stream_idx,
                    make_fin_message(&stream, (messages_per_stream + 1) as u64)
                ));
            }

            // Shuffle to interleave messages from different streams
            all_messages.shuffle(&mut rng);

            let mut completed = vec![false; stream_count];

            // Insert all messages
            for (stream_idx, msg) in all_messages {
                let result = map.insert(peer, msg);

                // Mark stream as completed if it returns a result
                if result.is_some() {
                    completed[stream_idx] = true;
                }
            }

            // All streams should have completed
            prop_assert!(
                completed.iter().all(|&c| c),
                "All streams should complete independently: {:?}",
                completed
            );

            // Map should be empty after all streams complete
            prop_assert_eq!(
                map.streams.len(),
                0,
                "Map should be empty after all streams complete"
            );
        }

        #[test]
        fn prop_stream_completion_requires_init_and_fin(
            has_init in any::<bool>(),
            has_fin in any::<bool>(),
            data_count in 1..10usize
        ) {
            let peer = PeerId::random();
            let stream = StreamId::new(vec![1].into());
            let mut map = PartStreamsMap::new(Height::new(1));

            let mut seq = 0u64;

            // Conditionally send init
            if has_init {
                map.insert(peer, make_message(&stream, seq, make_init_part()));
                seq += 1;
            }

            // Send data parts
            for i in 0..data_count {
                map.insert(peer, make_message(&stream, seq, make_data_part(i as u8)));
                seq += 1;
            }

            // Conditionally send fin_part and fin
            let result = if has_fin {
                map.insert(peer, make_message(&stream, seq, make_fin_part()));
                seq += 1;
                map.insert(peer, make_fin_message(&stream, seq))
            } else {
                None
            };

            // Should only complete if both init and fin are present
            if has_init && has_fin {
                prop_assert!(
                    result.is_some(),
                    "Stream with init and fin should complete"
                );
            } else {
                prop_assert!(
                    result.is_none(),
                    "Stream without init={} or fin={} should not complete",
                    has_init,
                    has_fin
                );
            }
        }

        #[test]
        fn prop_message_limit_independent_across_streams(
            stream1_message_count in 1..MAX_MESSAGES_PER_STREAM,
            stream2_message_count in 1..(MAX_MESSAGES_PER_STREAM / 2),
        ) {
            let peer = PeerId::random();
            let mut map = PartStreamsMap::new(Height::new(1));

            // Fill first stream up to its message count
            let stream_1 = StreamId::new(vec![1].into());
            for i in 0..stream1_message_count {
                let msg = make_message(&stream_1, i as u64, make_data_part(i as u8));
                map.insert(peer, msg);
            }

            // Verify first stream exists and has the expected message count
            let stream1_state = map.streams.get(&(peer, stream_1.clone()));
            prop_assert!(
                stream1_state.is_some(),
                "Stream 1 should exist after inserting messages"
            );
            prop_assert_eq!(
                stream1_state.unwrap().message_count,
                stream1_message_count,
                "Stream 1 should have expected message count"
            );

            // Second stream should still accept messages independently
            let stream_2 = StreamId::new(vec![2].into());
            for i in 0..stream2_message_count {
                let msg = make_message(&stream_2, i as u64, make_data_part(i as u8));
                let result = map.insert(peer, msg);

                prop_assert!(
                    result.is_none(),
                    "Stream 2 message {} should be accepted despite stream 1 having {} messages",
                    i,
                    stream1_message_count
                );
            }

            // Verify second stream exists and has its own independent message count
            let stream2_state = map.streams.get(&(peer, stream_2));
            prop_assert!(
                stream2_state.is_some(),
                "Stream 2 should exist after inserting messages"
            );
            prop_assert_eq!(
                stream2_state.unwrap().message_count,
                stream2_message_count,
                "Stream 2 should have expected message count independent of stream 1"
            );

            // Verify first stream's message count hasn't changed
            let stream1_state_after = map.streams.get(&(peer, stream_1));
            prop_assert_eq!(
                stream1_state_after.unwrap().message_count,
                stream1_message_count,
                "Stream 1 message count should remain unchanged after stream 2 operations"
            );
        }
    }
}
