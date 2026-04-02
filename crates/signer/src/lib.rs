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

use async_trait::async_trait;
use bytes::Bytes;

pub use arc_consensus_types::signing::{
    PublicKey, Signature, SigningError, SigningProvider, VerificationResult,
};
pub use arc_consensus_types::signing::{SignedExtension, SignedProposal, SignedVote};

use arc_consensus_types::{ArcContext, Proposal, Vote};

#[cfg(not(any(feature = "local", feature = "remote")))]
compile_error!("At least one signing provider feature must be enabled");

#[cfg(feature = "local")]
pub mod local;

#[cfg(feature = "remote")]
pub mod remote;

// QUESTION:
// Define `ArcSigningProvider` as a trait object?
// e.g. `type ArcSigningProvider = Arc<dyn SigningProvider<ArcContext> + Send + Sync>`
// Requires changes in Malachite.

/// Signing provider implementations for Arc consensus.
///
/// This enum abstracts over different signing backends, allowing the consensus
/// layer to work with local keys, remote signers, or HSMs without code changes.
///
/// # Variants
///
/// - `Local`: Signs using an in-memory Ed25519 private key. Suitable for testing
///   and non-production deployments. Enabled by the `local` feature.
/// - `Remote`: Signs by delegating to a remote signing service over the network.
///   Suitable for production deployments. Enabled by the `remote` feature.
#[derive(Clone)]
pub enum ArcSigningProvider {
    #[cfg(feature = "local")]
    Local(local::LocalSigningProvider),

    #[cfg(feature = "remote")]
    Remote(remote::RemoteSigningProvider),
}

#[async_trait]
impl SigningProvider<ArcContext> for ArcSigningProvider {
    async fn sign_bytes(&self, bytes: &[u8]) -> Result<Signature, SigningError> {
        match self {
            #[cfg(feature = "local")]
            Self::Local(provider) => provider.sign_bytes(bytes).await,

            #[cfg(feature = "remote")]
            Self::Remote(provider) => provider.sign_bytes(bytes).await,
        }
    }

    async fn verify_signed_bytes(
        &self,
        bytes: &[u8],
        signature: &Signature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        match self {
            #[cfg(feature = "local")]
            Self::Local(provider) => {
                provider
                    .verify_signed_bytes(bytes, signature, public_key)
                    .await
            }

            #[cfg(feature = "remote")]
            Self::Remote(provider) => {
                provider
                    .verify_signed_bytes(bytes, signature, public_key)
                    .await
            }
        }
    }

    async fn sign_vote(&self, vote: Vote) -> Result<SignedVote<ArcContext>, SigningError> {
        match self {
            #[cfg(feature = "local")]
            Self::Local(provider) => provider.sign_vote(vote).await,

            #[cfg(feature = "remote")]
            Self::Remote(provider) => provider.sign_vote(vote).await,
        }
    }

    async fn verify_signed_vote(
        &self,
        vote: &Vote,
        signature: &Signature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        match self {
            #[cfg(feature = "local")]
            Self::Local(provider) => {
                provider
                    .verify_signed_vote(vote, signature, public_key)
                    .await
            }

            #[cfg(feature = "remote")]
            Self::Remote(provider) => {
                provider
                    .verify_signed_vote(vote, signature, public_key)
                    .await
            }
        }
    }

    async fn sign_proposal(
        &self,
        proposal: Proposal,
    ) -> Result<SignedProposal<ArcContext>, SigningError> {
        match self {
            #[cfg(feature = "local")]
            Self::Local(provider) => provider.sign_proposal(proposal).await,

            #[cfg(feature = "remote")]
            Self::Remote(provider) => provider.sign_proposal(proposal).await,
        }
    }

    async fn verify_signed_proposal(
        &self,
        proposal: &Proposal,
        signature: &Signature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        match self {
            #[cfg(feature = "local")]
            Self::Local(provider) => {
                provider
                    .verify_signed_proposal(proposal, signature, public_key)
                    .await
            }

            #[cfg(feature = "remote")]
            Self::Remote(provider) => {
                provider
                    .verify_signed_proposal(proposal, signature, public_key)
                    .await
            }
        }
    }

    async fn sign_vote_extension(
        &self,
        _extension: Bytes,
    ) -> Result<SignedExtension<ArcContext>, SigningError> {
        unreachable!("Vote extensions are not supported in Arc at the moment");
    }

    async fn verify_signed_vote_extension(
        &self,
        _extension: &Bytes,
        _signature: &Signature,
        _public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        unreachable!("Vote extensions are not supported in Arc at the moment");
    }
}
