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
use prost::Message;
use ssz::Decode;
use std::time::{Duration, UNIX_EPOCH};

use arc_consensus_types::block::ConsensusBlock;
use arc_consensus_types::codec::proto as proto_funcs;
use arc_consensus_types::evidence::{
    DoubleProposal, DoubleVote, StoredMisbehaviorEvidence, ValidatorEvidence,
};
use arc_consensus_types::proposal_monitor::ProposalMonitor;
use arc_consensus_types::ssz::SszBlock;
use arc_consensus_types::{
    proto, Address, Height, ProposalParts, StoredCommitCertificate, ValueId,
};
use malachitebft_app_channel::app::types::core::{Round, Validity};
use malachitebft_proto::Protobuf;

use crate::invalid_payloads::{InvalidPayload, StoredInvalidPayloads};
use crate::versions::{
    CommitCertificateVersion, ConsensusBlockVersion, ExecutionPayloadVersion,
    InvalidPayloadsVersion, MisbehaviorEvidenceVersion, ProposalMonitorDataVersion,
    ProposalPartsVersion,
};

/// Error while decoding a value from the database.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The version of the data is not supported.
    #[error(
        "Unsupported version: {0}; please upgrade the database by running `malachite-app upgrade`"
    )]
    UnsupportedVersion(u8),

    /// The data is empty.
    #[error("Empty version")]
    EmptyVersion,

    /// The SSZ decoding error.
    #[error("SSZ decoding error: `{0:?}`")]
    Ssz(ssz::DecodeError),

    /// The Protobuf decoding error.
    #[error("Protobuf decoding error: `{0:?}`")]
    Protobuf(prost::DecodeError),

    /// The Protobuf error (local types).
    #[error("Protobuf error: `{0:?}`")]
    Proto(malachitebft_proto::Error),
}

impl From<ssz::DecodeError> for DecodeError {
    fn from(err: ssz::DecodeError) -> Self {
        DecodeError::Ssz(err)
    }
}

impl From<prost::DecodeError> for DecodeError {
    fn from(err: prost::DecodeError) -> Self {
        DecodeError::Protobuf(err)
    }
}

/// Decodes an execution payload from its byte representation.
pub fn decode_execution_payload(bytes: &[u8]) -> Result<ExecutionPayloadV3, DecodeError> {
    let version = bytes.first().ok_or(DecodeError::EmptyVersion)?;

    match ExecutionPayloadVersion::try_from(*version) {
        Ok(ExecutionPayloadVersion::V3) => {
            ExecutionPayloadV3::from_ssz_bytes(&bytes[1..]).map_err(DecodeError::Ssz)
        }
        Err(version) => Err(DecodeError::UnsupportedVersion(version)),
    }
}

/// Decodes a proposal parts from its byte representation.
pub fn decode_proposal_parts(bytes: &[u8]) -> Result<ProposalParts, DecodeError> {
    let version = bytes.first().ok_or(DecodeError::EmptyVersion)?;

    match ProposalPartsVersion::try_from(*version) {
        Ok(ProposalPartsVersion::V1) => {
            let proto = proto::ProposalParts::decode(&bytes[1..])?;
            proto_funcs::decode_proposal_parts(proto).map_err(DecodeError::Proto)
        }
        Err(version) => Err(DecodeError::UnsupportedVersion(version)),
    }
}

/// Decodes a commit certificate from its byte representation
pub fn decode_certificate(bytes: &[u8]) -> Result<StoredCommitCertificate, DecodeError> {
    let version = bytes.first().ok_or(DecodeError::EmptyVersion)?;

    match CommitCertificateVersion::try_from(*version) {
        Ok(CommitCertificateVersion::V1) => {
            let proto = proto::store::CommitCertificate::decode(&bytes[1..])?;
            proto_funcs::decode_store_commit_certificate(proto).map_err(DecodeError::Proto)
        }
        Err(version) => Err(DecodeError::UnsupportedVersion(version)),
    }
}

/// Decodes a block from its byte representation.
pub fn decode_block(bytes: &[u8]) -> Result<ConsensusBlock, DecodeError> {
    let version = bytes.first().ok_or(DecodeError::EmptyVersion)?;

    match ConsensusBlockVersion::try_from(*version) {
        Ok(ConsensusBlockVersion::V1) => {
            let (height, round, valid_round, proposer, is_valid, execution_payload, signature) =
                SszBlock::<ExecutionPayloadV3>::from_ssz_bytes(&bytes[1..])?;
            Ok(ConsensusBlock {
                height: Height::new(height),
                round: Round::from(round),
                valid_round: Round::from(valid_round),
                proposer: Address::from(proposer),
                validity: Validity::from_bool(is_valid),
                execution_payload,
                signature: signature.map(|s| s.0),
            })
        }
        Err(version) => Err(DecodeError::UnsupportedVersion(version)),
    }
}

/// Decodes proposal monitor data from its byte representation.
pub fn decode_proposal_monitor_data(bytes: &[u8]) -> Result<ProposalMonitor, DecodeError> {
    use arc_consensus_types::proposal_monitor::ProposalSuccessState;

    let version = bytes.first().ok_or(DecodeError::EmptyVersion)?;

    match ProposalMonitorDataVersion::try_from(*version) {
        Ok(ProposalMonitorDataVersion::V1) => {
            let proto_data = proto::ProtoProposalMonitorData::decode(&bytes[1..])?;

            let proposer = proto_data
                .proposer
                .ok_or_else(|| proto_error("Missing proposer in proposal monitor data"))
                .and_then(|a| Address::from_proto(a).map_err(DecodeError::Proto))?;

            // Convert times from milliseconds since UNIX_EPOCH
            let start_time = UNIX_EPOCH + Duration::from_millis(proto_data.start_time_ms);

            // Convert times from milliseconds; 0 = not present
            let proposal_receive_time = if proto_data.receive_time_ms > 0 {
                Some(UNIX_EPOCH + Duration::from_millis(proto_data.receive_time_ms))
            } else {
                None
            };

            // Convert value_id from bytes (empty = not present)
            let value_id = if proto_data.value_id.len() == 32 {
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&proto_data.value_id);
                Some(ValueId::new(hash.into()))
            } else {
                None
            };

            // Convert successful_state back to Option<bool>
            let successful = ProposalSuccessState::from(proto_data.successful_state);

            Ok(ProposalMonitor {
                height: Height::new(proto_data.height),
                proposer,
                start_time,
                proposal_receive_time,
                value_id,
                successful,
                synced: proto_data.synced,
            })
        }
        Err(version) => Err(DecodeError::UnsupportedVersion(version)),
    }
}

/// Decodes misbehavior evidence from its byte representation.
pub fn decode_misbehavior_evidence(bytes: &[u8]) -> Result<StoredMisbehaviorEvidence, DecodeError> {
    let version = bytes.first().ok_or(DecodeError::EmptyVersion)?;

    match MisbehaviorEvidenceVersion::try_from(*version) {
        Ok(MisbehaviorEvidenceVersion::V1) => {
            let proto_evidence = proto::ProtoMisbehaviorEvidence::decode(&bytes[1..])?;
            decode_proto_misbehavior_evidence(proto_evidence)
        }
        Err(version) => Err(DecodeError::UnsupportedVersion(version)),
    }
}

/// Decode a proto misbehavior evidence into domain type.
fn decode_proto_misbehavior_evidence(
    proto_evidence: proto::ProtoMisbehaviorEvidence,
) -> Result<StoredMisbehaviorEvidence, DecodeError> {
    let validators = proto_evidence
        .validators
        .into_iter()
        .map(decode_proto_validator_evidence)
        .collect::<Result<Vec<_>, DecodeError>>()?;

    Ok(StoredMisbehaviorEvidence {
        height: Height::new(proto_evidence.height),
        validators,
    })
}

/// Decode a proto validator evidence into domain type.
fn decode_proto_validator_evidence(
    v: proto::ProtoValidatorEvidence,
) -> Result<ValidatorEvidence, DecodeError> {
    let address = v
        .address
        .ok_or_else(|| proto_error("Missing address in validator evidence"))
        .and_then(|a| Address::from_proto(a).map_err(DecodeError::Proto))?;

    let double_votes = v
        .double_votes
        .into_iter()
        .map(decode_proto_double_vote)
        .collect::<Result<Vec<_>, DecodeError>>()?;

    let double_proposals = v
        .double_proposals
        .into_iter()
        .map(decode_proto_double_proposal)
        .collect::<Result<Vec<_>, DecodeError>>()?;

    Ok(ValidatorEvidence {
        address,
        double_votes,
        double_proposals,
    })
}

/// Decode a proto double vote into domain type.
fn decode_proto_double_vote(dv: proto::ProtoDoubleVote) -> Result<DoubleVote, DecodeError> {
    let first = dv
        .first
        .ok_or_else(|| proto_error("Missing first vote in double vote"))
        .and_then(|m| proto_funcs::decode_vote(m).map_err(DecodeError::Proto))?;

    let second = dv
        .second
        .ok_or_else(|| proto_error("Missing second vote in double vote"))
        .and_then(|m| proto_funcs::decode_vote(m).map_err(DecodeError::Proto))?;

    Ok(DoubleVote { first, second })
}

/// Decode a proto double proposal into domain type.
fn decode_proto_double_proposal(
    dp: proto::ProtoDoubleProposal,
) -> Result<DoubleProposal, DecodeError> {
    let first = dp
        .first
        .ok_or_else(|| proto_error("Missing first proposal in double proposal"))
        .and_then(|m| proto_funcs::decode_signed_proposal(m).map_err(DecodeError::Proto))?;

    let second = dp
        .second
        .ok_or_else(|| proto_error("Missing second proposal in double proposal"))
        .and_then(|m| proto_funcs::decode_signed_proposal(m).map_err(DecodeError::Proto))?;

    Ok(DoubleProposal { first, second })
}

/// Decodes invalid payloads from their byte representation.
pub fn decode_invalid_payloads(bytes: &[u8]) -> Result<StoredInvalidPayloads, DecodeError> {
    let version = bytes.first().ok_or(DecodeError::EmptyVersion)?;

    match InvalidPayloadsVersion::try_from(*version) {
        Ok(InvalidPayloadsVersion::V1) => {
            let proto = proto::ProtoInvalidPayloads::decode(&bytes[1..])?;
            decode_proto_invalid_payloads(proto)
        }
        Err(version) => Err(DecodeError::UnsupportedVersion(version)),
    }
}

/// Decode proto invalid payloads into domain type.
fn decode_proto_invalid_payloads(
    proto: proto::ProtoInvalidPayloads,
) -> Result<StoredInvalidPayloads, DecodeError> {
    let payloads = proto
        .payloads
        .into_iter()
        .map(decode_proto_invalid_payload)
        .collect::<Result<Vec<_>, DecodeError>>()?;

    Ok(StoredInvalidPayloads {
        height: Height::new(proto.height),
        payloads,
    })
}

/// Decode a single proto invalid payload into domain type.
fn decode_proto_invalid_payload(
    p: proto::ProtoInvalidPayload,
) -> Result<InvalidPayload, DecodeError> {
    let address = p
        .proposer_address
        .ok_or_else(|| proto_error("Missing proposer_address in invalid payload"))
        .and_then(|a| Address::from_proto(a).map_err(DecodeError::Proto))?;

    let payload = p
        .payload
        .as_ref()
        .map(|bytes| ExecutionPayloadV3::from_ssz_bytes(bytes).map_err(DecodeError::Ssz))
        .transpose()?;

    Ok(InvalidPayload {
        height: Height::new(p.height),
        round: Round::new(p.round),
        proposer_address: address,
        payload,
        reason: p.reason,
    })
}

fn proto_error(msg: &str) -> DecodeError {
    DecodeError::Proto(malachitebft_proto::Error::Other(msg.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder;
    use arbitrary::Unstructured;
    use arc_consensus_types::proposal_monitor::{ProposalMonitor, ProposalSuccessState};
    use arc_consensus_types::{
        signing::Signature, Address, ArcContext, CommitCertificateType, Height, ProposalData,
        ProposalFin, ProposalInit, ProposalPart, ProposalParts, StoredCommitCertificate, ValueId,
        B256,
    };
    use bytes::Bytes;
    use malachitebft_app_channel::app::types::core::{
        CommitCertificate, CommitSignature, Round, Validity,
    };

    use std::time::{Duration, UNIX_EPOCH};

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
    fn test_decode_execution_payload_valid() {
        let payload = create_test_execution_payload();
        let encoded = encoder::encode_execution_payload(&payload);
        let decoded = decode_execution_payload(&encoded).expect("should decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_decode_execution_payload_empty() {
        let result = decode_execution_payload(&[]);
        assert!(matches!(result, Err(DecodeError::EmptyVersion)));
    }

    #[test]
    fn test_decode_execution_payload_unsupported_version() {
        let mut bytes = vec![0xFF]; // Unsupported version
        bytes.extend_from_slice(&[1, 2, 3]);
        let result = decode_execution_payload(&bytes);
        assert!(matches!(result, Err(DecodeError::UnsupportedVersion(0xFF))));
    }

    #[test]
    fn test_decode_proposal_parts_valid() {
        let parts = create_test_proposal_parts();
        let encoded = encoder::encode_proposal_parts(&parts).expect("should encode");
        let decoded = decode_proposal_parts(&encoded).expect("should decode");
        assert_eq!(decoded, parts);
    }

    #[test]
    fn test_decode_proposal_parts_empty() {
        let result = decode_proposal_parts(&[]);
        assert!(matches!(result, Err(DecodeError::EmptyVersion)));
    }

    #[test]
    fn test_decode_proposal_parts_unsupported_version() {
        let mut bytes = vec![0x99]; // Unsupported version
        bytes.extend_from_slice(&[1, 2, 3]);
        let result = decode_proposal_parts(&bytes);
        assert!(matches!(result, Err(DecodeError::UnsupportedVersion(0x99))));
    }

    #[test]
    fn test_decode_certificate_valid() {
        let cert = create_test_commit_certificate();

        for tpe in [
            CommitCertificateType::Unknown,
            CommitCertificateType::Minimal,
            CommitCertificateType::Extended,
        ] {
            let cert = StoredCommitCertificate {
                certificate: cert.clone(),
                certificate_type: tpe,
                proposer: Some(Address::new([0u8; 20])),
            };

            let encoded = encoder::encode_certificate(&cert).expect("should encode");
            let stored = decode_certificate(&encoded).expect("should decode");

            assert_eq!(stored, cert);
        }
    }

    #[test]
    fn test_decode_certificate_empty() {
        let result = decode_certificate(&[]);
        assert!(matches!(result, Err(DecodeError::EmptyVersion)));
    }

    #[test]
    fn test_decode_certificate_unsupported_version() {
        let mut bytes = vec![0x42]; // Unsupported version
        bytes.extend_from_slice(&[1, 2, 3]);
        let result = decode_certificate(&bytes);
        assert!(matches!(result, Err(DecodeError::UnsupportedVersion(0x42))));
    }

    #[test]
    fn test_decode_block_valid() {
        let block = create_test_consensus_block();
        let encoded = encoder::encode_block(&block);
        let decoded = decode_block(&encoded).expect("should decode");
        assert_eq!(decoded, block);
    }

    #[test]
    fn test_decode_block_empty() {
        let result = decode_block(&[]);
        assert!(matches!(result, Err(DecodeError::EmptyVersion)));
    }

    #[test]
    fn test_decode_block_unsupported_version() {
        let mut bytes = vec![0xAB]; // Unsupported version
        bytes.extend_from_slice(&[1, 2, 3]);
        let result = decode_block(&bytes);
        assert!(matches!(result, Err(DecodeError::UnsupportedVersion(0xAB))));
    }

    fn create_test_proposal_monitor(
        height: u64,
        with_proposal: bool,
        successful: ProposalSuccessState,
        synced: bool,
    ) -> ProposalMonitor {
        let start_time = UNIX_EPOCH + Duration::from_secs(1000000);
        let proposer = Address::new([0x42; 20]);

        let mut monitor = ProposalMonitor::new(Height::new(height), proposer, start_time);

        if with_proposal {
            let receive_time = start_time + Duration::from_millis(150);
            let value_id = ValueId::new(B256::repeat_byte(0xAB));
            monitor.proposal_receive_time = Some(receive_time);
            monitor.value_id = Some(value_id);
        }

        monitor.successful = successful;
        monitor.synced = synced;

        monitor
    }

    #[test]
    fn test_decode_proposal_monitor_data_empty() {
        let result = decode_proposal_monitor_data(&[]);
        assert!(matches!(result, Err(DecodeError::EmptyVersion)));
    }

    #[test]
    fn test_decode_proposal_monitor_data_unsupported_version() {
        let mut bytes = vec![0xFF]; // Unsupported version
        bytes.extend_from_slice(&[1, 2, 3]);
        let result = decode_proposal_monitor_data(&bytes);
        assert!(matches!(result, Err(DecodeError::UnsupportedVersion(0xFF))));
    }

    #[test]
    fn test_decode_proposal_monitor_data_valid_full() {
        let monitor =
            create_test_proposal_monitor(42, true, ProposalSuccessState::Successful, false);
        let encoded = encoder::encode_proposal_monitor_data(&monitor).expect("should encode");
        let decoded = decode_proposal_monitor_data(&encoded).expect("should decode");

        assert_eq!(decoded.height, monitor.height);
        assert_eq!(decoded.proposer, monitor.proposer);
        assert_eq!(decoded.value_id, monitor.value_id);
        assert_eq!(decoded.successful, monitor.successful);
        assert_eq!(decoded.synced, monitor.synced);
        // Times are converted to/from milliseconds, so we compare at that precision
        assert!(
            decoded
                .start_time
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                == monitor
                    .start_time
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
        );
        assert!(decoded.proposal_receive_time.is_some());
    }

    #[test]
    fn test_decode_proposal_monitor_data_valid_no_proposal() {
        let monitor =
            create_test_proposal_monitor(100, false, ProposalSuccessState::Unknown, false);
        let encoded = encoder::encode_proposal_monitor_data(&monitor).expect("should encode");
        let decoded = decode_proposal_monitor_data(&encoded).expect("should decode");

        assert_eq!(decoded.height, monitor.height);
        assert_eq!(decoded.proposer, monitor.proposer);
        assert!(decoded.proposal_receive_time.is_none());
        assert!(decoded.value_id.is_none());
        assert!(decoded.successful.is_unknown());
        assert!(!decoded.synced);
    }

    #[test]
    fn test_decode_proposal_monitor_data_synced() {
        let monitor =
            create_test_proposal_monitor(50, false, ProposalSuccessState::Unsuccessful, true);
        let encoded = encoder::encode_proposal_monitor_data(&monitor).expect("should encode");
        let decoded = decode_proposal_monitor_data(&encoded).expect("should decode");

        assert_eq!(decoded.height, monitor.height);
        assert!(decoded.synced);
        assert_eq!(decoded.successful, ProposalSuccessState::Unsuccessful);
    }

    #[test]
    fn test_decode_invalid_payloads_valid() {
        let payload = create_test_execution_payload();
        let stored = StoredInvalidPayloads {
            height: Height::new(5),
            payloads: vec![InvalidPayload {
                height: Height::new(5),
                round: Round::new(0),
                proposer_address: Address::new([1u8; 20]),
                payload: Some(payload),
                reason: "bad payload".to_string(),
            }],
        };
        let encoded = encoder::encode_invalid_payloads(&stored).expect("should encode");
        let decoded = decode_invalid_payloads(&encoded).expect("should decode");
        assert_eq!(decoded, stored);
    }

    #[test]
    fn test_decode_invalid_payloads_without_payload() {
        let stored = StoredInvalidPayloads {
            height: Height::new(3),
            payloads: vec![InvalidPayload {
                height: Height::new(3),
                round: Round::new(0),
                proposer_address: Address::new([5u8; 20]),
                payload: None,
                reason: "engine error".to_string(),
            }],
        };
        let encoded = encoder::encode_invalid_payloads(&stored).expect("should encode");
        let decoded = decode_invalid_payloads(&encoded).expect("should decode");
        assert_eq!(decoded, stored);
    }

    #[test]
    fn test_decode_invalid_payloads_empty() {
        let result = decode_invalid_payloads(&[]);
        assert!(matches!(result, Err(DecodeError::EmptyVersion)));
    }

    #[test]
    fn test_decode_invalid_payloads_unsupported_version() {
        let mut bytes = vec![0xFE];
        bytes.extend_from_slice(&[1, 2, 3]);
        let result = decode_invalid_payloads(&bytes);
        assert!(matches!(result, Err(DecodeError::UnsupportedVersion(0xFE))));
    }
}
