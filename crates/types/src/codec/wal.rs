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

//! WAL codec

use malachitebft_core_consensus::{ProposedValue, SignedConsensusMsg};

use crate::codec::impl_versioned_codec;
use crate::codec::versions::{ProposedValueVersion, SignedConsensusMsgVersion};
use crate::ArcContext;

#[derive(Copy, Clone, Debug)]
pub struct WalCodec;

impl_versioned_codec!(
    WalCodec,
    SignedConsensusMsg<ArcContext>,
    SignedConsensusMsgVersion,
    SignedConsensusMsgVersion::V1
);
impl_versioned_codec!(
    WalCodec,
    ProposedValue<ArcContext>,
    ProposedValueVersion,
    ProposedValueVersion::V1
);

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{BufMut, Bytes, BytesMut};
    use malachitebft_codec::Codec;
    use malachitebft_core_consensus::SignedConsensusMsg;
    use malachitebft_core_types::{
        NilOrVal, Proposal as _, Round, SignedVote, Validity, Vote as _,
    };

    use crate::codec::error::CodecError;
    use crate::codec::proto::ProtobufCodec;
    use crate::{signing::Signature, Address, BlockHash, Height, Value, Vote};

    use alloy_primitives::Address as AlloyAddress;

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

    // Helper function to create a test ProposedValue
    fn create_test_proposed_value() -> ProposedValue<ArcContext> {
        ProposedValue {
            height: Height::new(5),
            round: Round::new(2),
            valid_round: Round::Nil,
            proposer: Address::from(AlloyAddress::from([5u8; 20])),
            value: Value::new(BlockHash::from([0xb; 32])),
            validity: Validity::Valid,
        }
    }

    #[test]
    fn test_encode_decode_roundtrip_vote() {
        let msg = create_test_vote();
        let codec = WalCodec;

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
        let codec = WalCodec;

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
    fn test_encode_decode_roundtrip_proposed_value() {
        let value = create_test_proposed_value();
        let codec = WalCodec;

        let encoded = codec.encode(&value).expect("Failed to encode");
        let decoded: ProposedValue<ArcContext> = codec.decode(encoded).expect("Failed to decode");

        assert_eq!(value.height, decoded.height);
        assert_eq!(value.round, decoded.round);
        assert_eq!(value.valid_round, decoded.valid_round);
        assert_eq!(value.proposer, decoded.proposer);
        assert_eq!(value.value, decoded.value);
        assert_eq!(value.validity, decoded.validity);
    }

    #[test]
    fn test_decode_signed_consensus_msg_empty_bytes_fails() {
        let codec = WalCodec;
        let empty_bytes = Bytes::new();

        let result: Result<SignedConsensusMsg<ArcContext>, _> = codec.decode(empty_bytes);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Empty bytes, expected version byte"));
    }

    #[test]
    fn test_decode_signed_consensus_msg_invalid_version_fails() {
        let codec = WalCodec;

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
    fn test_decode_signed_consensus_msg_only_version_byte_fails() {
        let codec = WalCodec;

        // Create a message with only the version byte, no actual message data
        let mut bytes = BytesMut::new();
        bytes.put_u8(SignedConsensusMsgVersion::V1 as u8);

        let result: Result<SignedConsensusMsg<ArcContext>, _> = codec.decode(bytes.freeze());
        // This should fail because there's no protobuf data after the version byte
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_proposed_value_empty_bytes_fails() {
        let codec = WalCodec;
        let empty_bytes = Bytes::new();

        let result: Result<ProposedValue<ArcContext>, _> = codec.decode(empty_bytes);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Empty bytes, expected version byte"));
    }

    #[test]
    fn test_decode_proposed_value_invalid_version_fails() {
        let codec = WalCodec;

        // Create a message with an invalid version byte (0x02)
        let mut bytes = BytesMut::new();
        bytes.put_u8(0x02); // Invalid version
        bytes.put_slice(b"some data"); // Some dummy data

        let result: Result<ProposedValue<ArcContext>, _> = codec.decode(bytes.freeze());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unsupported version"));
    }

    #[test]
    fn test_decode_proposed_value_only_version_byte_fails() {
        let codec = WalCodec;

        // Create a message with only the version byte, no actual message data
        let mut bytes = BytesMut::new();
        bytes.put_u8(ProposedValueVersion::V1 as u8);

        let result: Result<ProposedValue<ArcContext>, _> = codec.decode(bytes.freeze());
        // This should fail because there's no protobuf data after the version byte
        assert!(result.is_err());
    }

    /// XXX: remove after all nodes are upgraded to use versioning
    #[test]
    fn test_previous_codec_compatibility() {
        let codec = WalCodec;

        let msg = create_test_vote();
        // NOTE: ProtobufCodec is used here because it is the previous codec used for encoding and decoding messages.
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        let msg = create_test_proposal();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);

        let msg = create_test_proposed_value();
        let encoded = ProtobufCodec.encode(&msg).expect("Failed to encode");
        let decoded = codec.decode(encoded).expect("Failed to decode");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_decode_corrupted_legacy_message() {
        let codec = WalCodec;

        // A corrupted protobuf message that doesn't start with a valid version byte (e.g., 0x02)
        let corrupted_bytes = Bytes::from_static(b"\x02corrupted_data");
        let result: Result<ProposedValue<ArcContext>, _> = codec.decode(corrupted_bytes);
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
        let result_v1: Result<ProposedValue<ArcContext>, _> = codec.decode(corrupted_bytes_v1);
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
