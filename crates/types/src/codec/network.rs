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

//! Network codec

use malachitebft_app::engine::util::streaming::StreamMessage;
use malachitebft_codec::HasEncodedLen;
use malachitebft_core_consensus::{LivenessMsg, SignedConsensusMsg};
use malachitebft_core_types::ValidatorProof;
use malachitebft_sync::{self as sync};

use crate::codec::error::CodecError;
use crate::codec::impl_versioned_codec;
use crate::codec::proto::ProtobufCodec;
use crate::codec::versions::{
    LivenessMsgVersion, ProposalPartVersion, SignedConsensusMsgVersion, StreamMessageVersion,
    SyncRequestVersion, SyncResponseVersion, SyncStatusVersion, ValidatorProofVersion,
};
use crate::{ArcContext, ProposalPart};

#[derive(Copy, Clone, Debug)]
pub struct NetCodec;

impl_versioned_codec!(
    NetCodec,
    SignedConsensusMsg<ArcContext>,
    SignedConsensusMsgVersion,
    SignedConsensusMsgVersion::V1
);
impl_versioned_codec!(
    NetCodec,
    LivenessMsg<ArcContext>,
    LivenessMsgVersion,
    LivenessMsgVersion::V1
);
impl_versioned_codec!(
    NetCodec,
    ProposalPart,
    ProposalPartVersion,
    ProposalPartVersion::V1
);
impl_versioned_codec!(
    NetCodec,
    StreamMessage<ProposalPart>,
    StreamMessageVersion,
    StreamMessageVersion::V1
);
impl_versioned_codec!(
    NetCodec,
    sync::Status<ArcContext>,
    SyncStatusVersion,
    SyncStatusVersion::V1
);
impl_versioned_codec!(
    NetCodec,
    sync::Request<ArcContext>,
    SyncRequestVersion,
    SyncRequestVersion::V1
);
impl_versioned_codec!(
    NetCodec,
    sync::Response<ArcContext>,
    SyncResponseVersion,
    SyncResponseVersion::V1
);
impl_versioned_codec!(
    NetCodec,
    ValidatorProof<ArcContext>,
    ValidatorProofVersion,
    ValidatorProofVersion::V1
);

impl HasEncodedLen<sync::Response<ArcContext>> for NetCodec {
    fn encoded_len(&self, response: &sync::Response<ArcContext>) -> Result<usize, Self::Error> {
        ProtobufCodec
            .encoded_len(response)
            .map_err(CodecError::Protobuf)
        // TODO: Phase 2 - Add 1 for the version byte once versioned encoding is enabled
        // .map(|len| len + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{BufMut, Bytes, BytesMut};
    use malachitebft_app::engine::util::streaming::{StreamContent, StreamId, StreamMessage};
    use malachitebft_codec::Codec;
    use malachitebft_core_consensus::{LivenessMsg, SignedConsensusMsg};
    use malachitebft_core_types::{NilOrVal, Proposal as _, Round, SignedVote, Vote as _};
    use malachitebft_sync as sync;

    use crate::{
        signing::Signature, Address, BlockHash, Height, ProposalData, ProposalFin, ProposalInit,
        Value, Vote,
    };

    use alloy_primitives::Address as AlloyAddress;
    use malachitebft_sync::PeerId;

    // Helper function to create a test SignedConsensusMsg (Vote variant)
    fn create_test_vote() -> SignedConsensusMsg<ArcContext> {
        let vote = Vote::new_prevote(
            Height::new(1),
            Round::new(0),
            NilOrVal::Nil,
            Address::from(AlloyAddress::from([1u8; 20])),
        );
        let signature = Signature::from_bytes([42u8; 64]);
        SignedConsensusMsg::Vote(SignedVote::new(vote, signature))
    }

    // Helper function to create a test SignedConsensusMsg (Proposal variant)
    fn create_test_proposal() -> SignedConsensusMsg<ArcContext> {
        use malachitebft_core_types::SignedProposal;

        let proposal = crate::Proposal::new(
            Height::new(1),
            Round::new(0),
            Value::new(BlockHash::from([0xa; 32])),
            Round::Nil,
            Address::from(AlloyAddress::from([2u8; 20])),
        );
        let signature = Signature::from_bytes([43u8; 64]);
        SignedConsensusMsg::Proposal(SignedProposal::new(proposal, signature))
    }

    #[test]
    fn test_encode_decode_roundtrip_vote() {
        let msg = create_test_vote();
        let codec = NetCodec;

        let encoded = codec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");

        // Verify all fields match
        match (&msg, &decoded) {
            (SignedConsensusMsg::Vote(orig), SignedConsensusMsg::Vote(dec)) => {
                assert_eq!(orig.message.height(), dec.message.height());
                assert_eq!(orig.message.round(), dec.message.round());
                assert_eq!(
                    orig.message.validator_address(),
                    dec.message.validator_address()
                );
                assert_eq!(orig.message.value(), dec.message.value());
                assert_eq!(orig.signature, dec.signature);
            }
            _ => panic!("Expected Vote variants"),
        }
    }

    #[test]
    fn test_encode_decode_roundtrip_proposal() {
        let msg = create_test_proposal();
        let codec = NetCodec;

        let encoded = codec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");

        // Verify all fields match
        match (&msg, &decoded) {
            (SignedConsensusMsg::Proposal(orig), SignedConsensusMsg::Proposal(dec)) => {
                assert_eq!(orig.message.height(), dec.message.height());
                assert_eq!(orig.message.round(), dec.message.round());
                assert_eq!(
                    orig.message.validator_address(),
                    dec.message.validator_address()
                );
                assert_eq!(orig.signature, dec.signature);
            }
            _ => panic!("Expected Proposal variants"),
        }
    }

    #[test]
    fn test_decode_empty_bytes_fails() {
        let codec = NetCodec;
        let empty_bytes = Bytes::new();

        let result: Result<SignedConsensusMsg<ArcContext>, _> = codec.decode(empty_bytes);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Empty bytes, expected version byte"));
    }

    #[test]
    fn test_decode_invalid_version_fails() {
        let codec = NetCodec;

        // Create a message with an invalid version byte (0x02)
        let mut bytes = BytesMut::new();
        bytes.put_u8(0x02); // Invalid version
        bytes.put_slice(b"some data"); // Some dummy data

        let result: Result<SignedConsensusMsg<ArcContext>, _> = codec.decode(bytes.freeze());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unsupported version"));
    }

    #[test]
    fn test_decode_only_version_byte_fails() {
        let codec = NetCodec;

        // Create a message with only the version byte, no actual message data
        let mut bytes = BytesMut::new();
        bytes.put_u8(SignedConsensusMsgVersion::V1 as u8);

        let result: Result<SignedConsensusMsg<ArcContext>, _> = codec.decode(bytes.freeze());
        // This should fail because there's no protobuf data after the version byte
        assert!(result.is_err());
    }

    // Helper function to create a test LivenessMsg (Vote variant)
    fn create_test_liveness_msg() -> LivenessMsg<ArcContext> {
        let vote = Vote::new_prevote(
            Height::new(2),
            Round::new(1),
            NilOrVal::Nil,
            Address::from(AlloyAddress::from([3u8; 20])),
        );
        let signature = Signature::from_bytes([44u8; 64]);
        LivenessMsg::Vote(SignedVote::new(vote, signature))
    }

    // Helper function to create a test ProposalPart (Init variant)
    fn create_test_proposal_part_init() -> ProposalPart {
        ProposalPart::Init(ProposalInit::new(
            Height::new(3),
            Round::new(2),
            Round::Nil,
            Address::from(AlloyAddress::from([4u8; 20])),
        ))
    }

    // Helper function to create a test ProposalPart (Data variant)
    fn create_test_proposal_part_data() -> ProposalPart {
        ProposalPart::Data(ProposalData::new(Bytes::from_static(b"test data")))
    }

    // Helper function to create a test ProposalPart (Fin variant)
    fn create_test_proposal_part_fin() -> ProposalPart {
        ProposalPart::Fin(ProposalFin::new(Signature::from_bytes([45u8; 64])))
    }

    // Helper function to create a test StreamMessage (Data variant)
    fn create_test_stream_message_data() -> StreamMessage<ProposalPart> {
        StreamMessage {
            stream_id: StreamId::new(Bytes::from_static(b"test_stream")),
            sequence: 1,
            content: StreamContent::Data(create_test_proposal_part_data()),
        }
    }

    // Helper function to create a test StreamMessage (Fin variant)
    fn create_test_stream_message_fin() -> StreamMessage<ProposalPart> {
        StreamMessage {
            stream_id: StreamId::new(Bytes::from_static(b"test_stream")),
            sequence: 2,
            content: StreamContent::Fin,
        }
    }

    // Helper function to create a test sync::Status
    fn create_test_sync_status() -> sync::Status<ArcContext> {
        // Create a valid PeerId by encoding it properly first and then decoding
        // A valid multihash for identity (code 0x00) with 32 bytes looks like:
        // [0x00, 0x20, ...32 bytes...]
        // where 0x00 is the identity code and 0x20 (32 in decimal) is the length
        let mut peer_id_bytes = vec![0x00, 0x20]; // identity multihash code + length
        peer_id_bytes.extend_from_slice(&[5u8; 32]); // 32 bytes of data

        sync::Status {
            peer_id: PeerId::from_bytes(&peer_id_bytes).expect("Valid multihash"),
            tip_height: Height::new(100),
            history_min_height: Height::new(1),
        }
    }

    // Helper function to create a test sync::Request
    fn create_test_sync_request() -> sync::Request<ArcContext> {
        sync::Request::ValueRequest(sync::ValueRequest::new(Height::new(10)..=Height::new(20)))
    }

    // Helper function to create a test sync::Response
    fn create_test_sync_response() -> sync::Response<ArcContext> {
        sync::Response::ValueResponse(sync::ValueResponse {
            start_height: Height::new(10),
            values: vec![],
        })
    }

    #[test]
    fn test_encode_decode_roundtrip_liveness_msg() {
        let msg = create_test_liveness_msg();
        let codec = NetCodec;

        let encoded = codec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");

        // Verify the message matches
        match (&msg, &decoded) {
            (LivenessMsg::Vote(orig), LivenessMsg::Vote(dec)) => {
                assert_eq!(orig.message.height(), dec.message.height());
                assert_eq!(orig.message.round(), dec.message.round());
                assert_eq!(
                    orig.message.validator_address(),
                    dec.message.validator_address()
                );
                assert_eq!(orig.signature, dec.signature);
            }
            _ => panic!("Expected Vote variants"),
        }
    }

    #[test]
    fn test_encode_decode_roundtrip_proposal_part_init() {
        let part = create_test_proposal_part_init();
        let codec = NetCodec;

        let encoded = codec.encode(&part).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");

        assert_eq!(part, decoded);
    }

    #[test]
    fn test_encode_decode_roundtrip_proposal_part_data() {
        let part = create_test_proposal_part_data();
        let codec = NetCodec;

        let encoded = codec.encode(&part).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");

        assert_eq!(part, decoded);
    }

    #[test]
    fn test_encode_decode_roundtrip_proposal_part_fin() {
        let part = create_test_proposal_part_fin();
        let codec = NetCodec;

        let encoded = codec.encode(&part).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");

        assert_eq!(part, decoded);
    }

    #[test]
    fn test_encode_decode_roundtrip_stream_message_data() {
        let msg = create_test_stream_message_data();
        let codec = NetCodec;

        let encoded = codec.encode(&msg).expect("Failed to encode");
        let decoded: StreamMessage<ProposalPart> = codec.decode(encoded).expect("Failed to decode");

        assert_eq!(msg.stream_id, decoded.stream_id);
        assert_eq!(msg.sequence, decoded.sequence);
        match (&msg.content, &decoded.content) {
            (StreamContent::Data(orig), StreamContent::Data(dec)) => {
                assert_eq!(orig, dec);
            }
            _ => panic!("Expected Data variants"),
        }
    }

    #[test]
    fn test_encode_decode_roundtrip_stream_message_fin() {
        let msg = create_test_stream_message_fin();
        let codec = NetCodec;

        let encoded = codec.encode(&msg).expect("Failed to encode");
        let decoded: StreamMessage<ProposalPart> = codec.decode(encoded).expect("Failed to decode");

        assert_eq!(msg.stream_id, decoded.stream_id);
        assert_eq!(msg.sequence, decoded.sequence);
        assert!(matches!(decoded.content, StreamContent::Fin));
    }

    #[test]
    fn test_encode_decode_roundtrip_sync_status() {
        let status = create_test_sync_status();
        let codec = NetCodec;

        let encoded = codec.encode(&status).expect("Failed to encode");
        let decoded: sync::Status<ArcContext> = codec.decode(encoded).expect("Failed to decode");

        assert_eq!(status.peer_id, decoded.peer_id);
        assert_eq!(status.tip_height, decoded.tip_height);
        assert_eq!(status.history_min_height, decoded.history_min_height);
    }

    #[test]
    fn test_encode_decode_roundtrip_sync_request() {
        let request = create_test_sync_request();
        let codec = NetCodec;

        let encoded = codec.encode(&request).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");

        match (&request, &decoded) {
            (sync::Request::ValueRequest(orig), sync::Request::ValueRequest(dec)) => {
                assert_eq!(orig.range, dec.range);
            }
        }
    }

    #[test]
    fn test_encode_decode_roundtrip_sync_response() {
        let response = create_test_sync_response();
        let codec = NetCodec;

        let encoded = codec.encode(&response).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");

        match (&response, &decoded) {
            (sync::Response::ValueResponse(orig), sync::Response::ValueResponse(dec)) => {
                assert_eq!(orig.start_height, dec.start_height);
                assert_eq!(orig.values.len(), dec.values.len());
            }
        }
    }

    /// XXX: remove after all nodes are upgraded to use versioning
    #[test]
    fn test_previous_codec_compatibility() {
        let codec = NetCodec;

        let msg = create_test_vote();
        // NOTE: ProtobufCodec is used here because it is the previous codec used for encoding and decoding messages.
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        // LivenessMsg (Vote)
        let msg = create_test_liveness_msg();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        // Proposal (SignedConsensusMsg::Proposal)
        let msg = create_test_proposal();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        // ProposalPart messages (Init, Data, Fin variants)
        let msg = create_test_proposal_part_init();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        let msg = create_test_proposal_part_data();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        let msg = create_test_proposal_part_fin();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        // StreamMessage (Data variant)
        let msg = create_test_stream_message_data();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        // StreamMessage (Fin variant)
        let msg = create_test_stream_message_fin();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        // sync::Status
        let msg = create_test_sync_status();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded: sync::Status<ArcContext> = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        // sync::Request
        let msg = create_test_sync_request();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        // sync::Response
        let msg = create_test_sync_response();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_decode_corrupted_legacy_message() {
        let codec = NetCodec;

        // A corrupted protobuf message that doesn't start with a valid version byte (e.g., 0x02)
        let corrupted_bytes = Bytes::from_static(b"\x02corrupted_data");
        let result: Result<SignedConsensusMsg<ArcContext>, _> = codec.decode(corrupted_bytes);
        assert!(result.is_err());

        // The logic should fall back to versioned decoding and fail with an "UnsupportedVersion" error.
        let err = result.unwrap_err();
        assert!(
            matches!(err, CodecError::UnsupportedVersion(2)),
            "Expected UnsupportedVersion error, got {:?}",
            err
        );

        // A corrupted protobuf message that happens to start with a valid version byte (0x01)
        let corrupted_bytes_v1 = Bytes::from_static(b"\x01corrupted_data");
        let result_v1: Result<SignedConsensusMsg<ArcContext>, _> = codec.decode(corrupted_bytes_v1);
        assert!(result_v1.is_err());

        // The logic should fall back, see the valid version byte, and then fail on protobuf decoding.
        let err_v1 = result_v1.unwrap_err();
        assert!(
            matches!(err_v1, CodecError::Protobuf(_)),
            "Expected Protobuf error, got {:?}",
            err_v1
        );
    }
}
