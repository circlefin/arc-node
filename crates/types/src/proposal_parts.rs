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

use thiserror::Error;

use malachitebft_proto::{Error as ProtoError, Protobuf};

use crate::{Address, Height, ProposalData, ProposalFin, ProposalInit, ProposalPart, Round};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Error)]
pub enum ProposalPartsError {
    #[error("missing part: {0}")]
    MissingPart(&'static str),

    #[error("duplicate part: {0}")]
    DuplicatePart(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalParts {
    init: ProposalInit,
    data: Vec<ProposalData>,
    fin: ProposalFin,
}

impl ProposalParts {
    /// Create a new `ProposalParts` from its components
    ///
    /// ## Errors
    /// - The parts must include a single `Init` part and a single `Fin` part.
    ///
    /// ## Important
    /// - The caller must guarantee that the data parts are provided in ascending sequence order.
    pub fn new(parts: Vec<ProposalPart>) -> Result<Self, ProposalPartsError> {
        let mut init = None;
        let mut fin = None;
        let mut data = Vec::new();

        for part in parts {
            match part {
                ProposalPart::Init(p) => {
                    if init.is_some() {
                        return Err(ProposalPartsError::DuplicatePart("Init"));
                    }
                    init = Some(p);
                }
                ProposalPart::Fin(p) => {
                    if fin.is_some() {
                        return Err(ProposalPartsError::DuplicatePart("Fin"));
                    }
                    fin = Some(p);
                }
                ProposalPart::Data(p) => data.push(p),
            }
        }

        Ok(Self {
            init: init.ok_or(ProposalPartsError::MissingPart("Init"))?,
            data,
            fin: fin.ok_or(ProposalPartsError::MissingPart("Fin"))?,
        })
    }

    /// Return the height of these proposal parts
    pub const fn height(&self) -> Height {
        self.init.height
    }

    /// Return the round of these proposal parts
    pub const fn round(&self) -> Round {
        self.init.round
    }

    /// Return the adderss of the proposer of these proposal parts
    pub const fn proposer(&self) -> Address {
        self.init.proposer
    }

    /// Return the init part of these proposal parts
    pub const fn init(&self) -> &ProposalInit {
        &self.init
    }

    /// Return the data parts of these proposal parts
    pub fn data(&self) -> &[ProposalData] {
        &self.data
    }

    /// Return the fin part of these proposal parts
    pub const fn fin(&self) -> &ProposalFin {
        &self.fin
    }

    /// Return the total size in bytes of all data parts
    pub fn data_size(&self) -> usize {
        self.data.iter().map(|d| d.bytes.len()).sum()
    }

    /// Generate a unique hash from these proposal parts
    ///
    /// ## Important
    /// ⚠️ This must be kept in sync with `state::make_proposal_parts`
    pub fn hash(&self) -> [u8; 32] {
        use sha3::{Digest, Keccak256};

        let mut hasher = Keccak256::new();

        // Hash height and round
        hasher.update(self.height().as_u64().to_be_bytes());
        hasher.update(self.round().as_i64().to_be_bytes());

        // Hash all the data parts
        for data in &self.data {
            hasher.update(&data.bytes);
        }

        hasher.finalize().into()
    }
}

impl Protobuf for ProposalParts {
    type Proto = crate::proto::ProposalParts;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        Ok(Self {
            init: ProposalInit::from_proto(
                proto
                    .init
                    .ok_or(ProtoError::missing_field::<Self::Proto>("init"))?,
            )?,
            data: proto
                .data
                .into_iter()
                .map(ProposalData::from_proto)
                .collect::<Result<_, _>>()?,
            fin: ProposalFin::from_proto(
                proto
                    .fin
                    .ok_or(ProtoError::missing_field::<Self::Proto>("fin"))?,
            )?,
        })
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(crate::proto::ProposalParts {
            init: Some(self.init.to_proto()?),
            data: self
                .data
                .iter()
                .map(ProposalData::to_proto)
                .collect::<Result<_, _>>()?,
            fin: Some(self.fin.to_proto()?),
        })
    }
}
