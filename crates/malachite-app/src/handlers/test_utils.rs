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

//! Shared helpers for handler unit tests.

use sha3::Digest;

use malachitebft_app_channel::app::streaming::{StreamContent, StreamId, StreamMessage};

use arc_consensus_types::{
    Address, Height, ProposalFin, ProposalInit, ProposalPart, ProposalParts, Round,
};
use arc_signer::local::{LocalSigningProvider, PrivateKey};
use arc_signer::SigningProvider;

/// Builds signed `ProposalParts` with only `Init` and `Fin` (no data chunks).
/// Useful for exercising assembly-failure paths: the empty data makes
/// `assemble_block_from_parts` fail on SSZ decode of zero bytes, but the
/// signature over `height + round` still verifies against the signer.
pub(super) async fn signed_parts_without_data(
    height: Height,
    round: Round,
    signing_key: &PrivateKey,
) -> ProposalParts {
    let proposer = Address::from_public_key(&signing_key.public_key());
    let init = ProposalInit::new(height, round, Round::Nil, proposer);

    let mut hasher = sha3::Keccak256::new();
    hasher.update(height.as_u64().to_be_bytes());
    hasher.update(round.as_i64().to_be_bytes());
    let hash = hasher.finalize().to_vec();

    let provider = LocalSigningProvider::new(signing_key.clone());
    let signature = provider.sign_bytes(&hash).await.unwrap();

    ProposalParts::new(vec![
        ProposalPart::Init(init),
        ProposalPart::Fin(ProposalFin::new(signature)),
    ])
    .unwrap()
}

/// Same data-less proposal as [`signed_parts_without_data`], but emitted as the
/// stream of [`StreamMessage`]s a peer would gossip, ready to feed through
/// `on_received_proposal_part`. The completed stream assembles to zero bytes, so
/// `assemble_block_from_parts` fails — exercising the assembly-failure path.
pub(super) async fn signed_stream_without_data(
    stream_id: StreamId,
    height: Height,
    round: Round,
    signing_key: &PrivateKey,
) -> Vec<StreamMessage<ProposalPart>> {
    let proposer = Address::from_public_key(&signing_key.public_key());
    let init = ProposalInit::new(height, round, Round::Nil, proposer);

    let mut hasher = sha3::Keccak256::new();
    hasher.update(height.as_u64().to_be_bytes());
    hasher.update(round.as_i64().to_be_bytes());
    let hash = hasher.finalize().to_vec();

    let provider = LocalSigningProvider::new(signing_key.clone());
    let signature = provider.sign_bytes(&hash).await.unwrap();

    vec![
        StreamMessage::new(
            stream_id.clone(),
            0,
            StreamContent::Data(ProposalPart::Init(init)),
        ),
        StreamMessage::new(
            stream_id.clone(),
            1,
            StreamContent::Data(ProposalPart::Fin(ProposalFin::new(signature))),
        ),
        StreamMessage::new(stream_id, 2, StreamContent::Fin),
    ]
}
