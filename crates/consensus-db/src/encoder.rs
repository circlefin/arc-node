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

use alloy_rpc_types_engine::ExecutionPayloadV3;
use bytes::Bytes;
use malachitebft_proto::Protobuf;
use prost::Message;
use ssz::Encode;
use std::time::UNIX_EPOCH;

use arc_consensus_types::block::{block_as_ssz_data, ConsensusBlock};
use arc_consensus_types::codec::proto as proto_funcs;
use arc_consensus_types::evidence::StoredMisbehaviorEvidence;
use arc_consensus_types::proposal_monitor::ProposalMonitor;
use arc_consensus_types::{proto, ProposalParts, StoredCommitCertificate};

use crate::invalid_payloads::StoredInvalidPayloads;
use crate::versions::{
    CommitCertificateVersion, ConsensusBlockVersion, ExecutionPayloadVersion,
    InvalidPayloadsVersion, MisbehaviorEvidenceVersion, ProposalMonitorDataVersion,
    ProposalPartsVersion,
};

pub fn encode_execution_payload(payload: &ExecutionPayloadV3) -> Vec<u8> {
    let ssz_bytes = payload.as_ssz_bytes();
    let mut bytes = Vec::with_capacity(1 + ssz_bytes.len());
    bytes.push(ExecutionPayloadVersion::V3 as u8);
    bytes.extend_from_slice(&ssz_bytes);
    bytes
}

pub fn encode_proposal_parts(parts: &ProposalParts) -> Result<Vec<u8>, malachitebft_proto::Error> {
    let proto = proto_funcs::encode_proposal_parts(parts)?;
    let proto_bytes = proto.encode_to_vec();
    // version byte + encoded protobuf
    let mut bytes = Vec::with_capacity(1 + proto_bytes.len());
    bytes.push(ProposalPartsVersion::V1 as u8);
    bytes.extend_from_slice(&proto_bytes);
    Ok(bytes)
}

/// Encodes a commit certificate into its byte representation
pub fn encode_certificate(
    certificate: &StoredCommitCertificate,
) -> Result<Vec<u8>, malachitebft_proto::Error> {
    let proto = proto_funcs::encode_store_commit_certificate(certificate)?;
    let proto_bytes = proto.encode_to_vec();
    // version byte + encoded protobuf
    let mut bytes = Vec::with_capacity(1 + proto_bytes.len());
    bytes.push(CommitCertificateVersion::V1 as u8);
    bytes.extend_from_slice(&proto_bytes);
    Ok(bytes)
}

/// Encodes a block into its byte representation.
pub fn encode_block(block: &ConsensusBlock) -> Bytes {
    let data = block_as_ssz_data(block);
    let ssz_bytes = data.as_ssz_bytes();
    // version byte + encoded SSZ
    let mut bytes = Vec::with_capacity(1 + ssz_bytes.len());
    bytes.push(ConsensusBlockVersion::V1 as u8);
    bytes.extend_from_slice(&ssz_bytes);
    Bytes::from(bytes)
}

/// Encodes proposal monitor data into its byte representation.
pub fn encode_proposal_monitor_data(
    data: &ProposalMonitor,
) -> Result<Vec<u8>, malachitebft_proto::Error> {
    // SystemTime is converted to milliseconds since UNIX_EPOCH
    let start_time_ms = {
        let duration = data
            .start_time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        duration.as_millis() as u64
    };

    let receive_time_ms = data
        .proposal_receive_time
        .map(|t| {
            let duration = t.duration_since(UNIX_EPOCH).unwrap_or_default();
            duration.as_millis() as u64
        })
        .unwrap_or(0);

    // Convert Option<ValueId> to bytes (empty = not present)
    let value_id_bytes = data
        .value_id
        .map(|v| Bytes::from(v.block_hash().to_vec()))
        .unwrap_or_default();

    let successful_state = data.successful.as_u32();

    let proto_data = proto::ProtoProposalMonitorData {
        height: data.height.as_u64(),
        proposer: Some(data.proposer.to_proto()?),
        start_time_ms,
        receive_time_ms,
        value_id: value_id_bytes,
        successful_state,
        synced: data.synced,
    };

    let proto_bytes = proto_data.encode_to_vec();
    // version byte + encoded protobuf
    let mut bytes = Vec::with_capacity(1 + proto_bytes.len());
    bytes.push(ProposalMonitorDataVersion::V1 as u8);
    bytes.extend_from_slice(&proto_bytes);
    Ok(bytes)
}

/// Encodes misbehavior evidence into its byte representation.
pub fn encode_misbehavior_evidence(
    evidence: &StoredMisbehaviorEvidence,
) -> Result<Vec<u8>, malachitebft_proto::Error> {
    let validators = evidence
        .validators
        .iter()
        .map(|v| {
            let double_votes = v
                .double_votes
                .iter()
                .map(|dv| {
                    Ok(proto::ProtoDoubleVote {
                        first: Some(proto_funcs::encode_vote(&dv.first)?),
                        second: Some(proto_funcs::encode_vote(&dv.second)?),
                    })
                })
                .collect::<Result<Vec<_>, malachitebft_proto::Error>>()?;

            let double_proposals = v
                .double_proposals
                .iter()
                .map(|dp| {
                    Ok(proto::ProtoDoubleProposal {
                        first: Some(proto_funcs::encode_signed_proposal(&dp.first)?),
                        second: Some(proto_funcs::encode_signed_proposal(&dp.second)?),
                    })
                })
                .collect::<Result<Vec<_>, malachitebft_proto::Error>>()?;

            Ok(proto::ProtoValidatorEvidence {
                address: Some(v.address.to_proto()?),
                double_votes,
                double_proposals,
            })
        })
        .collect::<Result<Vec<_>, malachitebft_proto::Error>>()?;

    let proto_evidence = proto::ProtoMisbehaviorEvidence {
        height: evidence.height.as_u64(),
        validators,
    };

    let proto_bytes = proto_evidence.encode_to_vec();
    // version byte + encoded protobuf
    let mut bytes = Vec::with_capacity(1 + proto_bytes.len());
    bytes.push(MisbehaviorEvidenceVersion::V1 as u8);
    bytes.extend_from_slice(&proto_bytes);
    Ok(bytes)
}

/// Encodes invalid payloads into their byte representation.
pub fn encode_invalid_payloads(
    stored: &StoredInvalidPayloads,
) -> Result<Vec<u8>, malachitebft_proto::Error> {
    let payloads = stored
        .payloads
        .iter()
        .map(|p| {
            Ok(proto::ProtoInvalidPayload {
                height: p.height.as_u64(),
                round: p.round.as_u32().ok_or_else(|| {
                    malachitebft_proto::Error::Other(format!(
                        "stored invalid payload {p} is missing the round",
                    ))
                })?,
                proposer_address: Some(p.proposer_address.to_proto()?),
                payload: p.payload.as_ref().map(|pl| pl.as_ssz_bytes().into()),
                reason: p.reason.clone(),
            })
        })
        .collect::<Result<Vec<_>, malachitebft_proto::Error>>()?;

    let proto_payloads = proto::ProtoInvalidPayloads {
        height: stored.height.as_u64(),
        payloads,
    };

    let proto_bytes = proto_payloads.encode_to_vec();
    // version byte + encoded protobuf
    let mut bytes = Vec::with_capacity(1 + proto_bytes.len());
    bytes.push(InvalidPayloadsVersion::V1 as u8);
    bytes.extend_from_slice(&proto_bytes);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder;
    use arbitrary::Unstructured;
    use arc_consensus_types::{
        signing::Signature, Address, Height, ProposalData, ProposalFin, ProposalInit, ProposalPart,
        ValueId,
    };
    use arc_consensus_types::{ArcContext, CommitCertificate, CommitCertificateType};
    use malachitebft_app_channel::app::types::core::{CommitSignature, Round, Validity};

    fn create_test_execution_payload() -> ExecutionPayloadV3 {
        // Use arbitrary to generate a valid ExecutionPayloadV3
        Unstructured::new(&[0xab; 1024])
            .arbitrary::<ExecutionPayloadV3>()
            .unwrap()
    }

    fn create_test_proposal_parts() -> ProposalParts {
        let signature = Signature::from_bytes([0u8; 64]);

        let parts = vec![
            ProposalPart::Init(ProposalInit::new(
                Height::new(1),
                Round::new(0),
                Round::Nil,
                Address::new([0u8; 20]),
            )),
            ProposalPart::Data(ProposalData::new(Bytes::from_static(b"test data"))),
            ProposalPart::Fin(ProposalFin::new(signature)),
        ];

        ProposalParts::new(parts).unwrap()
    }

    fn create_test_commit_certificate() -> CommitCertificate<ArcContext> {
        let signature = Signature::from_bytes([0u8; 64]);
        let address = Address::new([0u8; 20]);
        let commit_sig = CommitSignature::new(address, signature);

        // Get a valid block hash from a test payload
        let payload = create_test_execution_payload();
        let block_hash = payload.payload_inner.payload_inner.block_hash;

        CommitCertificate {
            height: Height::new(1),
            round: Round::new(0),
            value_id: ValueId::new(block_hash),
            commit_signatures: vec![commit_sig],
        }
    }

    fn create_test_consensus_block() -> ConsensusBlock {
        let signature = Signature::from_bytes([0u8; 64]);

        ConsensusBlock {
            height: Height::new(1),
            round: Round::new(0),
            valid_round: Round::Nil,
            proposer: Address::new([0u8; 20]),
            validity: Validity::Valid,
            execution_payload: create_test_execution_payload(),
            signature: Some(signature),
        }
    }

    #[test]
    fn test_encode_execution_payload() {
        let payload = create_test_execution_payload();
        let encoded = encode_execution_payload(&payload);

        // Check that version byte is correct
        assert_eq!(encoded[0], ExecutionPayloadVersion::V3 as u8);

        // Check that decoding works without error
        let decoded = decoder::decode_execution_payload(&encoded).expect("should decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_encode_proposal_parts() {
        let parts = create_test_proposal_parts();
        let encoded = encode_proposal_parts(&parts).expect("should encode");

        // Check that version byte is correct
        assert_eq!(encoded[0], ProposalPartsVersion::V1 as u8);

        // Check that decoding works without error
        let decoded = decoder::decode_proposal_parts(&encoded).expect("should decode");
        assert_eq!(decoded, parts);
    }

    #[test]
    fn test_encode_certificate() {
        let cert = create_test_commit_certificate();

        for tpe in [
            CommitCertificateType::Unknown,
            CommitCertificateType::Minimal,
            CommitCertificateType::Extended,
        ] {
            let stored = StoredCommitCertificate {
                certificate: cert.clone(),
                certificate_type: tpe,
                proposer: Some(Address::new([0u8; 20])),
            };
            let encoded = encode_certificate(&stored).expect("should encode");

            // Check that version byte is correct
            assert_eq!(encoded[0], CommitCertificateVersion::V1 as u8);

            // Check that decoding works without error
            let decoded = decoder::decode_certificate(&encoded).expect("should decode");

            assert_eq!(decoded, stored);
        }
    }

    #[test]
    fn test_encode_block() {
        let block = create_test_consensus_block();
        let encoded = encode_block(&block);

        // Check that version byte is correct
        assert_eq!(encoded[0], ConsensusBlockVersion::V1 as u8);

        // Check that decoding works without error
        let decoded = decoder::decode_block(&encoded).expect("should decode");
        assert_eq!(decoded, block);
    }

    #[test]
    fn test_encode_invalid_payloads() {
        use crate::invalid_payloads::InvalidPayload;

        let payload = create_test_execution_payload();
        let stored = StoredInvalidPayloads {
            height: Height::new(5),
            payloads: vec![InvalidPayload {
                height: Height::new(5),
                round: Round::new(0),
                proposer_address: Address::new([1u8; 20]),
                payload: Some(payload),
                reason: "bad block".to_string(),
            }],
        };

        let encoded = encode_invalid_payloads(&stored).expect("should encode");

        assert_eq!(encoded[0], InvalidPayloadsVersion::V1 as u8,);

        let decoded = decoder::decode_invalid_payloads(&encoded).expect("should decode");
        assert_eq!(decoded, stored);
    }

    #[test]
    fn test_encode_invalid_payloads_without_payload() {
        use crate::invalid_payloads::InvalidPayload;

        let stored = StoredInvalidPayloads {
            height: Height::new(7),
            payloads: vec![InvalidPayload {
                height: Height::new(7),
                round: Round::new(1),
                proposer_address: Address::new([2u8; 20]),
                payload: None,
                reason: "engine error".to_string(),
            }],
        };

        let encoded = encode_invalid_payloads(&stored).expect("should encode");

        let decoded = decoder::decode_invalid_payloads(&encoded).expect("should decode");
        assert_eq!(decoded, stored);
    }

    #[test]
    fn test_encode_invalid_payloads_empty() {
        let stored = StoredInvalidPayloads {
            height: Height::new(1),
            payloads: vec![],
        };

        let encoded = encode_invalid_payloads(&stored).expect("should encode");

        let decoded = decoder::decode_invalid_payloads(&encoded).expect("should decode");
        assert_eq!(decoded, stored);
    }
}
