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

use arc_consensus_types::signing::{SignedExtension, SignedProposal, SignedVote, SigningError};
use arc_consensus_types::signing::{SigningProvider, VerificationResult};

use crate::{ArcContext, Proposal, Vote};

pub use malachitebft_signing_ed25519::*;

#[derive(Clone)]
pub struct LocalSigningProvider {
    private_key: PrivateKey,
}

impl std::fmt::Debug for LocalSigningProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSigningProvider")
            .field("private_key", &"<redacted>")
            .finish()
    }
}

impl LocalSigningProvider {
    pub fn new(private_key: PrivateKey) -> Self {
        Self { private_key }
    }

    pub fn public_key(&self) -> PublicKey {
        self.private_key.public_key()
    }

    pub fn private_key(&self) -> &PrivateKey {
        &self.private_key
    }

    pub fn sign(&self, data: &[u8]) -> Signature {
        self.private_key.sign(data)
    }

    pub fn verify(&self, data: &[u8], signature: &Signature, public_key: &PublicKey) -> bool {
        public_key
            .inner()
            .verify(signature.inner(), data)
            .inspect_err(|e| {
                tracing::error!(
                    signature = %hex::encode(signature.to_bytes()),
                    public_key = %hex::encode(public_key.as_bytes()),
                    "Signature verification failed: {e}"
                );
            })
            .is_ok()
    }
}

#[async_trait]
impl SigningProvider<ArcContext> for LocalSigningProvider {
    async fn sign_bytes(&self, bytes: &[u8]) -> Result<Signature, SigningError> {
        Ok(self.sign(bytes))
    }

    async fn verify_signed_bytes(
        &self,
        bytes: &[u8],
        signature: &Signature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        Ok(VerificationResult::from_bool(
            self.verify(bytes, signature, public_key),
        ))
    }

    async fn sign_vote(&self, vote: Vote) -> Result<SignedVote<ArcContext>, SigningError> {
        let signature = self.sign(&vote.to_sign_bytes());
        Ok(SignedVote::new(vote, signature))
    }

    async fn verify_signed_vote(
        &self,
        vote: &Vote,
        signature: &Signature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        Ok(VerificationResult::from_bool(
            public_key.verify(&vote.to_sign_bytes(), signature).is_ok(),
        ))
    }

    async fn sign_proposal(
        &self,
        proposal: Proposal,
    ) -> Result<SignedProposal<ArcContext>, SigningError> {
        let signature = self.private_key.sign(&proposal.to_sign_bytes());
        Ok(SignedProposal::new(proposal, signature))
    }

    async fn verify_signed_proposal(
        &self,
        proposal: &Proposal,
        signature: &Signature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        Ok(VerificationResult::from_bool(
            public_key
                .verify(&proposal.to_sign_bytes(), signature)
                .is_ok(),
        ))
    }

    async fn sign_vote_extension(
        &self,
        extension: Bytes,
    ) -> Result<SignedExtension<ArcContext>, SigningError> {
        let signature = self.private_key.sign(extension.as_ref());
        Ok(SignedExtension::new(extension, signature))
    }

    async fn verify_signed_vote_extension(
        &self,
        extension: &Bytes,
        signature: &Signature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        Ok(VerificationResult::from_bool(
            public_key.verify(extension.as_ref(), signature).is_ok(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> LocalSigningProvider {
        let private_key = PrivateKey::generate(rand::thread_rng());
        LocalSigningProvider::new(private_key)
    }

    #[test]
    fn debug_impl_redacts_private_key() {
        let provider = test_provider();
        let debug_output = format!("{:?}", provider);
        assert!(
            debug_output.contains("<redacted>"),
            "Debug output must contain <redacted> placeholder"
        );
        assert_eq!(
            debug_output,
            "LocalSigningProvider { private_key: \"<redacted>\" }"
        );
    }

    #[test]
    fn verify_rejects_invalid_signature() {
        let provider = test_provider();
        let other = test_provider();
        let data = b"test message";
        let signature = other.sign(data);
        assert!(!provider.verify(data, &signature, &provider.public_key()));
    }
}
