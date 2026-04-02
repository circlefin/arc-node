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

use bytes::Bytes;
use prost::Message;

use malachitebft_app::engine::util::streaming::{StreamContent, StreamId, StreamMessage};
use malachitebft_core_consensus::{LivenessMsg, ProposedValue, SignedConsensusMsg};
use malachitebft_core_types::{
    CommitCertificate, CommitSignature, NilOrVal, PolkaCertificate, PolkaSignature, Round,
    RoundCertificate, RoundCertificateType, RoundSignature, SignedExtension, SignedProposal,
    SignedVote, ValidatorProof, Validity,
};
use malachitebft_proto::{Error as ProtoError, Protobuf};
use malachitebft_signing_ed25519::Signature;
use malachitebft_sync::{self as sync, PeerId};

use crate::{decode_votetype, encode_votetype, proto, StoredCommitCertificate};
use crate::{
    Address, ArcContext, CommitCertificateType, Height, Proposal, ProposalPart, ProposalParts,
    Value, ValueId, Vote,
};

use super::{Codec, HasEncodedLen};

#[derive(Copy, Clone, Debug)]
pub struct ProtobufCodec;

impl Codec<Value> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<Value, Self::Error> {
        Protobuf::from_bytes(&bytes)
    }

    fn encode(&self, msg: &Value) -> Result<Bytes, Self::Error> {
        Protobuf::to_bytes(msg)
    }
}

impl Codec<ProposalPart> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<ProposalPart, Self::Error> {
        Protobuf::from_bytes(&bytes)
    }

    fn encode(&self, msg: &ProposalPart) -> Result<Bytes, Self::Error> {
        Protobuf::to_bytes(msg)
    }
}

impl Codec<ProposalParts> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<ProposalParts, Self::Error> {
        Protobuf::from_bytes(&bytes)
    }

    fn encode(&self, msg: &ProposalParts) -> Result<Bytes, Self::Error> {
        Protobuf::to_bytes(msg)
    }
}

impl Codec<Signature> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<Signature, Self::Error> {
        let proto = proto::Signature::decode(bytes.as_ref())?;
        decode_signature(proto)
    }

    fn encode(&self, msg: &Signature) -> Result<Bytes, Self::Error> {
        Ok(Bytes::from(
            proto::Signature {
                bytes: Bytes::copy_from_slice(msg.to_bytes().as_ref()),
            }
            .encode_to_vec(),
        ))
    }
}

impl Codec<SignedConsensusMsg<ArcContext>> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<SignedConsensusMsg<ArcContext>, Self::Error> {
        let proto = proto::SignedMessage::decode(bytes.as_ref())?;

        let signature = proto
            .signature
            .ok_or_else(|| ProtoError::missing_field::<proto::SignedMessage>("signature"))
            .and_then(decode_signature)?;

        let proto_message = proto
            .message
            .ok_or_else(|| ProtoError::missing_field::<proto::SignedMessage>("message"))?;

        match proto_message {
            proto::signed_message::Message::Proposal(proto) => {
                let proposal = Proposal::from_proto(proto)?;
                Ok(SignedConsensusMsg::Proposal(SignedProposal::new(
                    proposal, signature,
                )))
            }
            proto::signed_message::Message::Vote(vote) => {
                let vote = Vote::from_proto(vote)?;
                Ok(SignedConsensusMsg::Vote(SignedVote::new(vote, signature)))
            }
        }
    }

    fn encode(&self, msg: &SignedConsensusMsg<ArcContext>) -> Result<Bytes, Self::Error> {
        match msg {
            SignedConsensusMsg::Vote(vote) => {
                let proto = proto::SignedMessage {
                    message: Some(proto::signed_message::Message::Vote(
                        vote.message.to_proto()?,
                    )),
                    signature: Some(encode_signature(&vote.signature)),
                };
                Ok(Bytes::from(proto.encode_to_vec()))
            }
            SignedConsensusMsg::Proposal(proposal) => {
                let proto = proto::SignedMessage {
                    message: Some(proto::signed_message::Message::Proposal(
                        proposal.message.to_proto()?,
                    )),
                    signature: Some(encode_signature(&proposal.signature)),
                };
                Ok(Bytes::from(proto.encode_to_vec()))
            }
        }
    }
}

pub fn encode_round_certificate(
    certificate: &RoundCertificate<ArcContext>,
) -> Result<proto::RoundCertificate, ProtoError> {
    Ok(proto::RoundCertificate {
        height: certificate.height.as_u64(),
        round: certificate.round.as_u32().expect("round should not be nil"),
        cert_type: match certificate.cert_type {
            RoundCertificateType::Precommit => proto::RoundCertificateType::Precommit.into(),
            RoundCertificateType::Skip => proto::RoundCertificateType::Skip.into(),
        },
        signatures: certificate
            .round_signatures
            .iter()
            .map(|sig| -> Result<proto::RoundSignature, ProtoError> {
                let value_id = match sig.value_id {
                    NilOrVal::Nil => None,
                    NilOrVal::Val(value_id) => Some(value_id.to_proto()?),
                };
                Ok(proto::RoundSignature {
                    vote_type: encode_votetype(sig.vote_type).into(),
                    validator_address: Some(sig.address.to_proto()?),
                    signature: Some(encode_signature(&sig.signature)),
                    value_id,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub fn decode_round_certificate(
    certificate: proto::RoundCertificate,
) -> Result<RoundCertificate<ArcContext>, ProtoError> {
    Ok(RoundCertificate {
        height: Height::new(certificate.height),
        round: Round::new(certificate.round),
        cert_type: match proto::RoundCertificateType::try_from(certificate.cert_type)
            .map_err(|_| ProtoError::Other("Unknown RoundCertificateType".into()))?
        {
            proto::RoundCertificateType::Precommit => RoundCertificateType::Precommit,
            proto::RoundCertificateType::Skip => RoundCertificateType::Skip,
        },
        round_signatures: certificate
            .signatures
            .into_iter()
            .map(|sig| -> Result<RoundSignature<ArcContext>, ProtoError> {
                let vote_type = decode_votetype(sig.vote_type());
                let address = sig.validator_address.ok_or_else(|| {
                    ProtoError::missing_field::<proto::RoundCertificate>("validator_address")
                })?;

                let signature = sig.signature.ok_or_else(|| {
                    ProtoError::missing_field::<proto::RoundCertificate>("signature")
                })?;

                let value_id = match sig.value_id {
                    None => NilOrVal::Nil,
                    Some(value_id) => NilOrVal::Val(ValueId::from_proto(value_id)?),
                };

                let signature = decode_signature(signature)?;
                let address = Address::from_proto(address)?;
                Ok(RoundSignature::new(vote_type, value_id, address, signature))
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

impl Codec<LivenessMsg<ArcContext>> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<LivenessMsg<ArcContext>, Self::Error> {
        let msg = proto::LivenessMessage::decode(bytes.as_ref())?;
        match msg.message {
            Some(proto::liveness_message::Message::Vote(vote)) => {
                Ok(LivenessMsg::Vote(decode_vote(vote)?))
            }
            Some(proto::liveness_message::Message::PolkaCertificate(cert)) => Ok(
                LivenessMsg::PolkaCertificate(decode_polka_certificate(cert)?),
            ),
            Some(proto::liveness_message::Message::RoundCertificate(cert)) => Ok(
                LivenessMsg::SkipRoundCertificate(decode_round_certificate(cert)?),
            ),
            None => Err(ProtoError::missing_field::<proto::LivenessMessage>(
                "message",
            )),
        }
    }

    fn encode(&self, msg: &LivenessMsg<ArcContext>) -> Result<Bytes, Self::Error> {
        match msg {
            LivenessMsg::Vote(vote) => {
                let message = encode_vote(vote)?;
                Ok(Bytes::from(
                    proto::LivenessMessage {
                        message: Some(proto::liveness_message::Message::Vote(message)),
                    }
                    .encode_to_vec(),
                ))
            }
            LivenessMsg::PolkaCertificate(cert) => {
                let message = encode_polka_certificate(cert)?;
                Ok(Bytes::from(
                    proto::LivenessMessage {
                        message: Some(proto::liveness_message::Message::PolkaCertificate(message)),
                    }
                    .encode_to_vec(),
                ))
            }
            LivenessMsg::SkipRoundCertificate(cert) => {
                let message = encode_round_certificate(cert)?;
                Ok(Bytes::from(
                    proto::LivenessMessage {
                        message: Some(proto::liveness_message::Message::RoundCertificate(message)),
                    }
                    .encode_to_vec(),
                ))
            }
        }
    }
}

impl Codec<StreamMessage<ProposalPart>> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<StreamMessage<ProposalPart>, Self::Error> {
        let proto = proto::StreamMessage::decode(bytes.as_ref())?;

        let proto_content = proto
            .content
            .ok_or_else(|| ProtoError::missing_field::<proto::StreamMessage>("content"))?;

        let content = match proto_content {
            proto::stream_message::Content::Data(data) => {
                StreamContent::Data(ProposalPart::from_bytes(&data)?)
            }
            proto::stream_message::Content::Fin(_) => StreamContent::Fin,
        };

        Ok(StreamMessage {
            stream_id: StreamId::new(proto.stream_id),
            sequence: proto.sequence,
            content,
        })
    }

    fn encode(&self, msg: &StreamMessage<ProposalPart>) -> Result<Bytes, Self::Error> {
        let proto = proto::StreamMessage {
            stream_id: msg.stream_id.to_bytes(),
            sequence: msg.sequence,
            content: match &msg.content {
                StreamContent::Data(data) => {
                    Some(proto::stream_message::Content::Data(data.to_bytes()?))
                }
                StreamContent::Fin => Some(proto::stream_message::Content::Fin(true)),
            },
        };

        Ok(Bytes::from(proto.encode_to_vec()))
    }
}

impl Codec<ProposedValue<ArcContext>> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<ProposedValue<ArcContext>, Self::Error> {
        let proto = proto::ProposedValue::decode(bytes.as_ref())?;

        let proposer = proto
            .proposer
            .ok_or_else(|| ProtoError::missing_field::<proto::ProposedValue>("proposer"))?;

        let value = proto
            .value
            .ok_or_else(|| ProtoError::missing_field::<proto::ProposedValue>("value"))?;

        Ok(ProposedValue {
            height: Height::new(proto.height),
            round: Round::new(proto.round),
            valid_round: proto.valid_round.map(Round::new).unwrap_or(Round::Nil),
            proposer: Address::from_proto(proposer)?,
            value: Value::from_proto(value)?,
            validity: Validity::from_bool(proto.validity),
        })
    }

    fn encode(&self, msg: &ProposedValue<ArcContext>) -> Result<Bytes, Self::Error> {
        let proto = proto::ProposedValue {
            height: msg.height.as_u64(),
            round: msg
                .round
                .as_u32()
                .expect("round is always Some in a valid proposal"),
            valid_round: msg.valid_round.as_u32(),
            proposer: Some(msg.proposer.to_proto()?),
            value: Some(msg.value.to_proto()?),
            validity: msg.validity.to_bool(),
        };

        Ok(Bytes::from(proto.encode_to_vec()))
    }
}

impl Codec<sync::Status<ArcContext>> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<sync::Status<ArcContext>, Self::Error> {
        let proto = proto::Status::decode(bytes.as_ref())?;

        let proto_peer_id = proto
            .peer_id
            .ok_or_else(|| ProtoError::missing_field::<proto::Status>("peer_id"))?;

        Ok(sync::Status {
            peer_id: PeerId::from_bytes(proto_peer_id.id.as_ref())
                .map_err(|e| ProtoError::Other(format!("Invalid peer ID: {e}")))?,
            tip_height: Height::new(proto.height),
            history_min_height: Height::new(proto.earliest_height),
        })
    }

    fn encode(&self, msg: &sync::Status<ArcContext>) -> Result<Bytes, Self::Error> {
        let proto = proto::Status {
            peer_id: Some(proto::PeerId {
                id: Bytes::from(msg.peer_id.to_bytes()),
            }),
            height: msg.tip_height.as_u64(),
            earliest_height: msg.history_min_height.as_u64(),
        };

        Ok(Bytes::from(proto.encode_to_vec()))
    }
}

impl Codec<sync::Request<ArcContext>> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<sync::Request<ArcContext>, Self::Error> {
        let proto = proto::SyncRequest::decode(bytes.as_ref())?;
        let request = proto
            .request
            .ok_or_else(|| ProtoError::missing_field::<proto::SyncRequest>("request"))?;

        match request {
            proto::sync_request::Request::ValueRequest(req) => Ok(sync::Request::ValueRequest(
                sync::ValueRequest::new(Height::new(req.range_start)..=Height::new(req.range_end)),
            )),
        }
    }

    fn encode(&self, msg: &sync::Request<ArcContext>) -> Result<Bytes, Self::Error> {
        let proto = match msg {
            sync::Request::ValueRequest(req) => proto::SyncRequest {
                request: Some(proto::sync_request::Request::ValueRequest(
                    proto::ValueRequest {
                        range_start: req.range.start().as_u64(),
                        range_end: req.range.end().as_u64(),
                    },
                )),
            },
        };

        Ok(Bytes::from(proto.encode_to_vec()))
    }
}

impl Codec<sync::Response<ArcContext>> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<sync::Response<ArcContext>, Self::Error> {
        decode_sync_response(proto::SyncResponse::decode(bytes)?)
    }

    fn encode(&self, response: &sync::Response<ArcContext>) -> Result<Bytes, Self::Error> {
        encode_sync_response(response).map(|proto| proto.encode_to_vec().into())
    }
}

impl HasEncodedLen<sync::Response<ArcContext>> for ProtobufCodec {
    fn encoded_len(&self, response: &sync::Response<ArcContext>) -> Result<usize, Self::Error> {
        let proto = encode_sync_response(response)?;
        Ok(proto.encoded_len())
    }
}

pub fn decode_sync_response(
    proto_response: proto::SyncResponse,
) -> Result<sync::Response<ArcContext>, ProtoError> {
    let response = proto_response
        .response
        .ok_or_else(|| ProtoError::missing_field::<proto::SyncResponse>("messages"))?;

    let response = match response {
        proto::sync_response::Response::ValueResponse(value_response) => {
            sync::Response::ValueResponse(sync::ValueResponse::new(
                Height::new(value_response.start_height),
                value_response
                    .values
                    .into_iter()
                    .map(decode_synced_value)
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
    };

    Ok(response)
}

pub fn encode_sync_response(
    response: &sync::Response<ArcContext>,
) -> Result<proto::SyncResponse, ProtoError> {
    let proto = match response {
        sync::Response::ValueResponse(value_response) => proto::SyncResponse {
            response: Some(proto::sync_response::Response::ValueResponse(
                proto::ValueResponse {
                    start_height: value_response.start_height.as_u64(),
                    values: value_response
                        .values
                        .iter()
                        .map(encode_synced_value)
                        .collect::<Result<Vec<_>, _>>()?,
                },
            )),
        },
    };

    Ok(proto)
}

pub fn encode_synced_value(
    synced_value: &sync::RawDecidedValue<ArcContext>,
) -> Result<proto::SyncedValue, ProtoError> {
    let certificate = encode_sync_commit_certificate(&synced_value.certificate)?;

    Ok(proto::SyncedValue {
        value_bytes: synced_value.value_bytes.clone(),
        certificate: Some(certificate),
    })
}

pub fn decode_synced_value(
    proto: proto::SyncedValue,
) -> Result<sync::RawDecidedValue<ArcContext>, ProtoError> {
    let proto_certificate = proto
        .certificate
        .ok_or_else(|| ProtoError::missing_field::<proto::SyncedValue>("certificate"))?;

    let certificate = decode_sync_commit_certificate(proto_certificate)?;

    Ok(sync::RawDecidedValue {
        value_bytes: proto.value_bytes,
        certificate,
    })
}

// NOTE: Will be used again in #997
#[allow(dead_code)]
pub(crate) fn encode_polka_certificate(
    polka_certificate: &PolkaCertificate<ArcContext>,
) -> Result<proto::PolkaCertificate, ProtoError> {
    Ok(proto::PolkaCertificate {
        height: polka_certificate.height.as_u64(),
        round: polka_certificate
            .round
            .as_u32()
            .expect("round should not be nil"),
        value_id: Some(polka_certificate.value_id.to_proto()?),
        signatures: polka_certificate
            .polka_signatures
            .iter()
            .map(|sig| -> Result<proto::PolkaSignature, ProtoError> {
                let address = sig.address.to_proto()?;
                let signature = encode_signature(&sig.signature);
                Ok(proto::PolkaSignature {
                    validator_address: Some(address),
                    signature: Some(signature),
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

// NOTE: Will be used again in #997
#[allow(dead_code)]
pub(crate) fn decode_polka_certificate(
    certificate: proto::PolkaCertificate,
) -> Result<PolkaCertificate<ArcContext>, ProtoError> {
    let value_id = certificate
        .value_id
        .ok_or_else(|| ProtoError::missing_field::<proto::PolkaCertificate>("value_id"))
        .and_then(ValueId::from_proto)?;

    Ok(PolkaCertificate {
        height: Height::new(certificate.height),
        round: Round::new(certificate.round),
        value_id,
        polka_signatures: certificate
            .signatures
            .into_iter()
            .map(|sig| -> Result<PolkaSignature<ArcContext>, ProtoError> {
                let address = sig.validator_address.ok_or_else(|| {
                    ProtoError::missing_field::<proto::PolkaCertificate>("validator_address")
                })?;
                let signature = sig.signature.ok_or_else(|| {
                    ProtoError::missing_field::<proto::PolkaCertificate>("signature")
                })?;
                let signature = decode_signature(signature)?;
                let address = Address::from_proto(address)?;
                Ok(PolkaSignature::new(address, signature))
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub fn decode_store_commit_certificate(
    proto: proto::store::CommitCertificate,
) -> Result<StoredCommitCertificate, ProtoError> {
    let certificate = decode_commit_certificate_fields(
        proto.height,
        proto.round,
        proto.value_id,
        proto.signatures,
    )?;

    let certificate_type = CommitCertificateType::from(proto.extended);
    let proposer = proto.proposer.map(Address::from_proto).transpose()?;

    Ok(StoredCommitCertificate {
        certificate,
        certificate_type,
        proposer,
    })
}

pub fn encode_store_commit_certificate(
    certificate: &StoredCommitCertificate,
) -> Result<proto::store::CommitCertificate, ProtoError> {
    let StoredCommitCertificate {
        certificate,
        certificate_type,
        proposer,
    } = certificate;

    Ok(proto::store::CommitCertificate {
        height: certificate.height.as_u64(),
        round: certificate.round.as_u32().expect("round should not be nil"),
        value_id: Some(certificate.value_id.to_proto()?),
        signatures: encode_commit_signatures(&certificate.commit_signatures)?,
        proposer: proposer.map(|p| p.to_proto()).transpose()?,
        extended: certificate_type.as_bool(),
    })
}

pub fn decode_sync_commit_certificate(
    proto: proto::sync::CommitCertificate,
) -> Result<CommitCertificate<ArcContext>, ProtoError> {
    decode_commit_certificate_fields(proto.height, proto.round, proto.value_id, proto.signatures)
}

pub fn encode_sync_commit_certificate(
    certificate: &CommitCertificate<ArcContext>,
) -> Result<proto::sync::CommitCertificate, ProtoError> {
    Ok(proto::sync::CommitCertificate {
        height: certificate.height.as_u64(),
        round: certificate.round.as_u32().expect("round should not be nil"),
        value_id: Some(certificate.value_id.to_proto()?),
        signatures: encode_commit_signatures(&certificate.commit_signatures)?,
    })
}

fn decode_commit_certificate_fields(
    height: u64,
    round: u32,
    value_id: Option<proto::ValueId>,
    signatures: Vec<proto::sync::CommitSignature>,
) -> Result<CommitCertificate<ArcContext>, ProtoError> {
    let value_id = value_id
        .ok_or_else(|| ProtoError::missing_field::<proto::sync::CommitCertificate>("value_id"))
        .and_then(ValueId::from_proto)?;

    let commit_signatures = signatures
        .into_iter()
        .map(|sig| -> Result<CommitSignature<ArcContext>, ProtoError> {
            let address = sig.validator_address.ok_or_else(|| {
                ProtoError::missing_field::<proto::sync::CommitSignature>("validator_address")
            })?;
            let signature = sig.signature.ok_or_else(|| {
                ProtoError::missing_field::<proto::sync::CommitSignature>("signature")
            })?;
            let signature = decode_signature(signature)?;
            let address = Address::from_proto(address)?;
            Ok(CommitSignature::new(address, signature))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CommitCertificate {
        height: Height::new(height),
        round: Round::new(round),
        value_id,
        commit_signatures,
    })
}

fn encode_commit_signatures(
    commit_signatures: &[CommitSignature<ArcContext>],
) -> Result<Vec<proto::sync::CommitSignature>, ProtoError> {
    commit_signatures
        .iter()
        .map(|sig| -> Result<proto::sync::CommitSignature, ProtoError> {
            let address = sig.address.to_proto()?;
            let signature = encode_signature(&sig.signature);
            Ok(proto::sync::CommitSignature {
                validator_address: Some(address),
                signature: Some(signature),
            })
        })
        .collect()
}

pub fn decode_extension(ext: proto::Extension) -> Result<SignedExtension<ArcContext>, ProtoError> {
    let signature = ext
        .signature
        .ok_or_else(|| ProtoError::missing_field::<proto::Extension>("signature"))
        .and_then(decode_signature)?;

    Ok(SignedExtension::new(ext.data, signature))
}

pub fn encode_extension(ext: &SignedExtension<ArcContext>) -> Result<proto::Extension, ProtoError> {
    Ok(proto::Extension {
        data: ext.message.clone(),
        signature: Some(encode_signature(&ext.signature)),
    })
}

pub fn encode_vote(vote: &SignedVote<ArcContext>) -> Result<proto::SignedMessage, ProtoError> {
    Ok(proto::SignedMessage {
        message: Some(proto::signed_message::Message::Vote(
            vote.message.to_proto()?,
        )),
        signature: Some(encode_signature(&vote.signature)),
    })
}

pub fn decode_vote(msg: proto::SignedMessage) -> Result<SignedVote<ArcContext>, ProtoError> {
    let signature = msg
        .signature
        .ok_or_else(|| ProtoError::missing_field::<proto::SignedMessage>("signature"))?;

    let vote = match msg.message {
        Some(proto::signed_message::Message::Vote(v)) => Ok(v),
        _ => Err(ProtoError::Other(
            "Invalid message type: not a vote".to_string(),
        )),
    }?;

    let signature = decode_signature(signature)?;
    let vote = Vote::from_proto(vote)?;
    Ok(SignedVote::new(vote, signature))
}

pub fn encode_signature(signature: &Signature) -> proto::Signature {
    proto::Signature {
        bytes: Bytes::copy_from_slice(signature.to_bytes().as_ref()),
    }
}

pub fn decode_signature(signature: proto::Signature) -> Result<Signature, ProtoError> {
    let bytes = <[u8; 64]>::try_from(signature.bytes.as_ref())
        .map_err(|_| ProtoError::Other("Invalid signature length".to_string()))?;
    Ok(Signature::from_bytes(bytes))
}

impl Codec<ValidatorProof<ArcContext>> for ProtobufCodec {
    type Error = ProtoError;

    fn decode(&self, bytes: Bytes) -> Result<ValidatorProof<ArcContext>, Self::Error> {
        let proto = proto::ValidatorProof::decode(bytes.as_ref())?;
        let signature = proto
            .signature
            .ok_or_else(|| ProtoError::missing_field::<proto::ValidatorProof>("signature"))?;
        let signature = decode_signature(signature)?;

        Ok(ValidatorProof::new(
            proto.public_key.to_vec(),
            proto.peer_id.to_vec(),
            signature,
        ))
    }

    fn encode(&self, msg: &ValidatorProof<ArcContext>) -> Result<Bytes, Self::Error> {
        let proto = proto::ValidatorProof {
            public_key: Bytes::from(msg.public_key.clone()),
            peer_id: Bytes::from(msg.peer_id.clone()),
            signature: Some(encode_signature(&msg.signature)),
        };
        Ok(Bytes::from(proto.encode_to_vec()))
    }
}

pub fn decode_proposal_parts(
    proposal_parts: proto::ProposalParts,
) -> Result<ProposalParts, ProtoError> {
    ProposalParts::from_proto(proposal_parts)
}

pub fn encode_proposal_parts(
    proposal_parts: &ProposalParts,
) -> Result<proto::ProposalParts, ProtoError> {
    ProposalParts::to_proto(proposal_parts)
}

pub fn encode_signed_proposal(
    proposal: &SignedProposal<ArcContext>,
) -> Result<proto::SignedMessage, ProtoError> {
    Ok(proto::SignedMessage {
        message: Some(proto::signed_message::Message::Proposal(
            proposal.message.to_proto()?,
        )),
        signature: Some(encode_signature(&proposal.signature)),
    })
}

pub fn decode_signed_proposal(
    msg: proto::SignedMessage,
) -> Result<SignedProposal<ArcContext>, ProtoError> {
    let signature = msg
        .signature
        .ok_or_else(|| ProtoError::missing_field::<proto::SignedMessage>("signature"))?;

    let proposal = match msg.message {
        Some(proto::signed_message::Message::Proposal(p)) => Ok(p),
        _ => Err(ProtoError::Other(
            "Invalid message type: not a proposal".to_string(),
        )),
    }?;

    let signature = decode_signature(signature)?;
    let proposal = Proposal::from_proto(proposal)?;
    Ok(SignedProposal::new(proposal, signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Address, Height};
    use malachitebft_core_types::{NilOrVal, Round, RoundSignature, VoteType};
    use malachitebft_signing_ed25519::Signature;

    #[test]
    fn test_round_certificate_encode_decode() {
        // Create test data
        let height = Height::new(1);
        let round = Round::new(2);
        let address = Address::new([1; 20]);
        let signature = Signature::from_bytes([2; 64]);
        let cert_type = RoundCertificateType::Skip;

        // Create a round signature
        let round_sig = RoundSignature::new(VoteType::Prevote, NilOrVal::Nil, address, signature);

        // Create the round certificate
        let certificate = RoundCertificate {
            height,
            round,
            cert_type,
            round_signatures: vec![round_sig],
        };

        // Encode the certificate
        let encoded = encode_round_certificate(&certificate).unwrap();

        // Decode the certificate
        let decoded = decode_round_certificate(encoded).unwrap();

        // Verify the decoded data matches the original
        assert_eq!(decoded.height, certificate.height);
        assert_eq!(decoded.round, certificate.round);
        assert_eq!(
            decoded.round_signatures.len(),
            certificate.round_signatures.len()
        );

        // Verify the signature details
        let decoded_sig = &decoded.round_signatures[0];
        let original_sig = &certificate.round_signatures[0];
        assert_eq!(decoded_sig.vote_type, original_sig.vote_type);
        assert_eq!(decoded_sig.value_id, original_sig.value_id);
        assert_eq!(decoded_sig.address, original_sig.address);
        assert_eq!(
            decoded_sig.signature.to_bytes(),
            original_sig.signature.to_bytes()
        );
    }

    #[test]
    fn test_validator_proof_encode_decode() {
        use malachitebft_core_types::ValidatorProof;

        let codec = ProtobufCodec;

        let public_key = vec![0x01; 32];
        let peer_id = vec![0x02; 38];
        let signature = Signature::from_bytes([0x03; 64]);

        let proof =
            ValidatorProof::<ArcContext>::new(public_key.clone(), peer_id.clone(), signature);

        let encoded = codec.encode(&proof).expect("encoding should succeed");
        let decoded: ValidatorProof<ArcContext> =
            codec.decode(encoded).expect("decoding should succeed");

        assert_eq!(decoded.public_key, public_key);
        assert_eq!(decoded.peer_id, peer_id);
        assert_eq!(decoded.signature.to_bytes(), signature.to_bytes());
    }

    #[test]
    fn test_validator_proof_decode_missing_signature_fails() {
        use prost::Message;

        let codec = ProtobufCodec;

        // Create a proto message with no signature field
        let proto_without_sig = proto::ValidatorProof {
            public_key: Bytes::from(vec![0x01; 32]),
            peer_id: Bytes::from(vec![0x02; 38]),
            signature: None,
        };

        let encoded = Bytes::from(proto_without_sig.encode_to_vec());
        let result: Result<ValidatorProof<ArcContext>, _> = codec.decode(encoded);

        assert!(
            result.is_err(),
            "decoding should fail when signature is missing"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("signature"),
            "error should mention missing signature field"
        );
    }

    #[test]
    fn test_decode_sync_status_with_invalid_peer_id_returns_error() {
        use prost::Message;

        let codec = ProtobufCodec;

        // Valid protobuf Status with peer_id bytes that are not a valid multihash.
        let proto_msg = proto::Status {
            peer_id: Some(proto::PeerId {
                id: Bytes::from(vec![0xFF, 0xFF, 0xFF]),
            }),
            height: 1,
            earliest_height: 0,
        };

        let encoded = Bytes::from(proto_msg.encode_to_vec());
        let result: Result<sync::Status<ArcContext>, _> = codec.decode(encoded);

        assert!(
            result.is_err(),
            "decoding a sync::Status with invalid peer_id bytes must return Err, not panic"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Invalid peer ID"),
            "error should mention invalid peer ID, got: {err}"
        );
    }
}
