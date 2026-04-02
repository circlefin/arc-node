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

// adapted from https://github.com/informalsystems/malachite/tree/v0.4.0/code/crates/test

use core::fmt;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use malachitebft_core_types::Round;
use malachitebft_proto::{self as proto, Error as ProtoError, Protobuf};
use malachitebft_signing_ed25519::Signature;

use crate::codec::proto::{decode_signature, encode_signature};
use crate::{Address, ArcContext, Height};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalData {
    pub bytes: Bytes,
}

impl ProposalData {
    pub fn new(bytes: Bytes) -> Self {
        Self { bytes }
    }

    pub fn size_bytes(&self) -> usize {
        self.bytes.len()
    }
}

impl fmt::Debug for ProposalData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let debug_len = std::cmp::min(8, self.bytes.len());

        f.debug_struct("ProposalData")
            .field("bytes", &&self.bytes[..debug_len])
            .field("len", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalPart {
    Init(ProposalInit),
    Data(ProposalData),
    Fin(ProposalFin),
}

impl ProposalPart {
    pub fn get_type(&self) -> &'static str {
        match self {
            Self::Init(_) => "init",
            Self::Data(_) => "data",
            Self::Fin(_) => "fin",
        }
    }

    pub fn as_init(&self) -> Option<&ProposalInit> {
        match self {
            Self::Init(init) => Some(init),
            _ => None,
        }
    }

    pub fn to_sign_bytes(&self) -> Bytes {
        proto::Protobuf::to_bytes(self).expect("protobuf encode is infallible")
    }

    pub fn size_bytes(&self) -> usize {
        match self {
            Self::Init(init) => init.size_bytes(),
            Self::Data(data) => data.size_bytes(),
            Self::Fin(fin) => fin.size_bytes(),
        }
    }
}

/// A part of a value for a height, round. Identified in this scope by the sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalInit {
    pub height: Height,
    pub round: Round,
    pub pol_round: Round,
    pub proposer: Address,
}

impl ProposalInit {
    pub fn new(height: Height, round: Round, pol_round: Round, proposer: Address) -> Self {
        Self {
            height,
            round,
            pol_round,
            proposer,
        }
    }

    /// Approximate in-memory size, not wire size.
    pub fn size_bytes(&self) -> usize {
        std::mem::size_of_val(&self.height)
            + std::mem::size_of_val(&self.round)
            + std::mem::size_of_val(&self.pol_round)
            + std::mem::size_of_val(&self.proposer)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalFin {
    pub signature: Signature,
}

impl ProposalFin {
    pub fn new(signature: Signature) -> Self {
        Self { signature }
    }

    /// Approximate in-memory size, not wire size.
    pub fn size_bytes(&self) -> usize {
        std::mem::size_of_val(&self.signature)
    }
}

impl malachitebft_core_types::ProposalPart<ArcContext> for ProposalPart {
    fn is_first(&self) -> bool {
        matches!(self, Self::Init(_))
    }

    fn is_last(&self) -> bool {
        matches!(self, Self::Fin(_))
    }
}

impl Protobuf for ProposalInit {
    type Proto = crate::proto::ProposalInit;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        Ok(Self {
            height: Height::new(proto.height),
            round: Round::new(proto.round),
            pol_round: Round::from(proto.pol_round),
            proposer: proto
                .proposer
                .ok_or_else(|| ProtoError::missing_field::<Self::Proto>("proposer"))
                .and_then(Address::from_proto)?,
        })
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(Self::Proto {
            height: self.height.as_u64(),
            round: self
                .round
                .as_u32()
                .expect("round is always Some in a valid proposal"),
            pol_round: self.pol_round.as_u32(),
            proposer: Some(self.proposer.to_proto()?),
        })
    }
}

impl Protobuf for ProposalData {
    type Proto = crate::proto::ProposalData;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        Ok(Self { bytes: proto.bytes })
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(Self::Proto {
            bytes: self.bytes.clone(),
        })
    }
}

impl Protobuf for ProposalFin {
    type Proto = crate::proto::ProposalFin;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        Ok(Self {
            signature: proto
                .signature
                .ok_or_else(|| ProtoError::missing_field::<Self::Proto>("signature"))
                .and_then(decode_signature)?,
        })
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(Self::Proto {
            signature: Some(encode_signature(&self.signature)),
        })
    }
}

impl Protobuf for ProposalPart {
    type Proto = crate::proto::ProposalPart;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        use crate::proto::proposal_part::Part;

        let part = proto
            .part
            .ok_or_else(|| ProtoError::missing_field::<Self::Proto>("part"))?;

        match part {
            Part::Init(init) => Ok(Self::Init(ProposalInit::from_proto(init)?)),
            Part::Data(data) => Ok(Self::Data(ProposalData::from_proto(data)?)),
            Part::Fin(fin) => Ok(Self::Fin(ProposalFin::from_proto(fin)?)),
        }
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        use crate::proto::proposal_part::Part;

        match self {
            Self::Init(init) => Ok(Self::Proto {
                part: Some(Part::Init(init.to_proto()?)),
            }),
            Self::Data(data) => Ok(Self::Proto {
                part: Some(Part::Data(data.to_proto()?)),
            }),
            Self::Fin(fin) => Ok(Self::Proto {
                part: Some(Part::Fin(fin.to_proto()?)),
            }),
        }
    }
}
