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

//! JSON types for commit certificates over HTTP or JSON-RPC.
//!
//! These structs match the minimal certificate payload returned by consensus
//! `GET /commit?height=…` and by JSON-RPC methods such as `arc_getCertificate`.

use malachitebft_core_types::{CommitCertificate, CommitSignature, Round};
use serde::{Deserialize, Serialize};
use serde_with::{base64::Base64, serde_as};

use crate::signing::Signature;
use crate::{Address, ArcContext, Height, ValueId};

/// Signature entry within a commit certificate (base64-encoded signature bytes).
#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpCommitSignature {
    pub address: Address,
    #[serde_as(as = "Base64")]
    pub signature: Vec<u8>,
}

impl From<CommitSignature<ArcContext>> for HttpCommitSignature {
    fn from(sig: CommitSignature<ArcContext>) -> Self {
        Self {
            address: sig.address,
            signature: sig.signature.to_bytes().to_vec(),
        }
    }
}

/// Minimal commit certificate JSON (height, round, value id, signatures).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpCommitCertificate {
    pub height: u64,
    pub round: i64,
    pub block_hash: ValueId,
    pub signatures: Vec<HttpCommitSignature>,
}

impl HttpCommitCertificate {
    /// Convert wire JSON into a [`CommitCertificate`].
    pub fn try_into_commit_certificate(self) -> eyre::Result<CommitCertificate<ArcContext>> {
        let signatures: eyre::Result<Vec<CommitSignature<ArcContext>>> = self
            .signatures
            .into_iter()
            .map(|sig| {
                let sig_bytes: [u8; 64] = sig.signature.try_into().map_err(|v: Vec<u8>| {
                    eyre::eyre!("Invalid signature length: {}", v.len())
                })?;
                Ok(CommitSignature {
                    address: sig.address,
                    signature: Signature::from_bytes(sig_bytes),
                })
            })
            .collect();

        let round_u32: u32 = self.round.try_into().map_err(|e| {
            eyre::eyre!(
                "Invalid round value {}, cannot convert to u32: {}",
                self.round,
                e
            )
        })?;

        Ok(CommitCertificate {
            height: Height::new(self.height),
            round: Round::new(round_u32),
            value_id: self.block_hash,
            commit_signatures: signatures?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Address, BlockHash};

    #[test]
    fn http_commit_certificate_json_roundtrip() {
        let orig = HttpCommitCertificate {
            height: 42,
            round: 3,
            block_hash: ValueId::new(BlockHash::ZERO),
            signatures: vec![],
        };
        let json = serde_json::to_string(&orig).unwrap();
        let back: HttpCommitCertificate = serde_json::from_str(&json).unwrap();
        assert_eq!(orig, back);
    }

    #[test]
    fn try_into_commit_certificate_empty_signatures() {
        let wire = HttpCommitCertificate {
            height: 10,
            round: 2,
            block_hash: ValueId::new(BlockHash::ZERO),
            signatures: vec![],
        };
        let cert = wire.try_into_commit_certificate().unwrap();
        assert_eq!(cert.height.as_u64(), 10);
        assert_eq!(cert.round.as_i64(), 2);
        assert_eq!(cert.value_id, ValueId::new(BlockHash::ZERO));
        assert!(cert.commit_signatures.is_empty());
    }

    #[test]
    fn try_into_rejects_short_signature() {
        let wire = HttpCommitCertificate {
            height: 1,
            round: 0,
            block_hash: ValueId::new(BlockHash::ZERO),
            signatures: vec![HttpCommitSignature {
                address: Address::default(),
                signature: vec![0u8; 32],
            }],
        };
        assert!(wire.try_into_commit_certificate().is_err());
    }

    #[test]
    fn http_commit_signature_json_roundtrip() {
        let mut b = [0u8; 64];
        b[0] = 1;
        b[63] = 2;
        let cs = CommitSignature {
            address: Address::default(),
            signature: Signature::from_bytes(b),
        };
        let http = HttpCommitSignature::from(cs);
        let json = serde_json::to_string(&http).unwrap();
        let back: HttpCommitSignature = serde_json::from_str(&json).unwrap();
        assert_eq!(http, back);
    }
}
