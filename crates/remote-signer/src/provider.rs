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

//! Remote signing provider for Malachite consensus

use std::sync::Arc;

use async_trait::async_trait;
use ed25519_dalek::{Signature as Ed25519Signature, VerifyingKey};
use eyre::eyre;

use malachitebft_core_types::{Context, SignedExtension, SignedProposal, SignedVote};
use malachitebft_signing::{Error as SigningError, SigningProvider, VerificationResult};

use arc_consensus_types::signing::{PublicKey, Signature as ConsensusSignature};
use arc_consensus_types::{ArcContext, Proposal, Vote};
use tokio::sync::RwLock;

use crate::client::RemoteSignerClient;
use crate::config::RemoteSigningConfig;
use crate::error::RemoteSigningError;
use crate::metrics::RemoteSigningMetrics;

/// Remote signing provider for Malachite consensus
///
/// This provider bridges between the async RemoteSignerClient and the sync SigningProvider trait.
pub struct RemoteSigningProvider {
    pub(crate) client: RemoteSignerClient,
    public_key_cache: Arc<RwLock<Option<PublicKey>>>,
}

/// Validate a signature using a specific public key (synchronous)
pub fn validate_signature_with_key(
    message: &[u8],
    signature: &[u8],
    public_key: &[u8],
) -> Result<VerificationResult, SigningError> {
    // Ensure public key is 32 bytes
    if public_key.len() != 32 {
        return Err(SigningError::from_source(eyre!(
            "Invalid public key length: expected 32 bytes, got {}",
            public_key.len()
        )));
    }

    // Ensure signature is 64 bytes
    if signature.len() != 64 {
        return Err(SigningError::from_source(eyre!(
            "Invalid signature length: expected 64 bytes, got {}",
            signature.len()
        )));
    }

    // Convert to fixed-size arrays
    let public_key_bytes: [u8; 32] = public_key.try_into().expect("Checked length above");
    let signature_bytes: [u8; 64] = signature.try_into().expect("Checked length above");

    // Parse the public key
    let verifying_key = VerifyingKey::from_bytes(&public_key_bytes)
        .map_err(|_| SigningError::from_source(eyre!("Invalid public key")))?;

    // Parse the signature
    let ed25519_signature = Ed25519Signature::from_bytes(&signature_bytes);

    // Verify the signature
    let result = verifying_key
        .verify_strict(message, &ed25519_signature)
        .inspect_err(|e| {
            tracing::error!(
                signature = %hex::encode(signature),
                public_key = %hex::encode(public_key),
                "Signature verification failed: {e}"
            );
        })
        .is_ok();

    Ok(VerificationResult::from_bool(result))
}

/// Convert raw signature bytes to Ed25519 consensus signature
pub fn bytes_to_signature(signature_bytes: &[u8]) -> Result<ConsensusSignature, SigningError>
where
{
    if signature_bytes.len() == 64 {
        let mut sig_array = [0u8; 64];
        sig_array.copy_from_slice(signature_bytes);
        Ok(ConsensusSignature::from_bytes(sig_array))
    } else {
        Err(SigningError::from_source(eyre::eyre!(
            "Invalid signature length: expected 64 bytes, got {}",
            signature_bytes.len()
        )))
    }
}

// Async methods implementation
impl RemoteSigningProvider {
    /// Create a new consensus remote signing provider
    pub async fn new(config: RemoteSigningConfig) -> Result<Self, RemoteSigningError> {
        let client = RemoteSignerClient::new(config).await?;

        Ok(Self {
            client,
            public_key_cache: Arc::new(RwLock::new(None)),
        })
    }

    /// Get the public key, using cache if available
    pub async fn public_key(&self) -> Result<PublicKey, RemoteSigningError> {
        // Check cache first
        {
            let cache = self.public_key_cache.read().await;
            if let Some(cached_key) = cache.as_ref() {
                return Ok(*cached_key);
            }
        }

        // Take the lock on the cache
        let mut cache = self.public_key_cache.write().await;

        // Double-check after acquiring write lock
        if let Some(cached_key) = cache.as_ref() {
            return Ok(*cached_key);
        }

        // Fetch from external service using the async method
        let public_key_bytes = self.client.get_public_key().await?;
        let public_key = PublicKey::from_bytes(public_key_bytes.try_into().map_err(|pk| {
            RemoteSigningError::InvalidResponse(format!(
                "Invalid public key bytes from signer: expected 32 bytes, got 0x{}",
                hex::encode(pk)
            ))
        })?);

        // Update cache
        *cache = Some(public_key);

        Ok(public_key)
    }

    /// Clear the public key cache
    pub async fn clear_cache(&self) {
        let mut cache = self.public_key_cache.write().await;
        *cache = None;
    }

    /// Get the client configuration
    pub fn config(&self) -> &RemoteSigningConfig {
        self.client.config()
    }

    /// Get the client metrics
    pub fn metrics(&self) -> &RemoteSigningMetrics {
        self.client.metrics()
    }
}

impl Clone for RemoteSigningProvider {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            public_key_cache: Arc::clone(&self.public_key_cache),
        }
    }
}

// SigningProvider trait implementation
#[async_trait]
impl SigningProvider<ArcContext> for RemoteSigningProvider {
    async fn sign_bytes(&self, bytes: &[u8]) -> Result<ConsensusSignature, SigningError> {
        let signature_bytes = self
            .client
            .sign_message(bytes)
            .await
            .map_err(SigningError::from_source)?;

        bytes_to_signature(&signature_bytes)
    }

    async fn verify_signed_bytes(
        &self,
        bytes: &[u8],
        signature: &ConsensusSignature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        let signature_bytes = signature.to_bytes();
        let public_key_bytes = public_key.as_bytes();

        validate_signature_with_key(bytes, &signature_bytes, public_key_bytes)
    }

    async fn sign_vote(&self, vote: Vote) -> Result<SignedVote<ArcContext>, SigningError> {
        let vote_bytes = vote.to_sign_bytes();
        let signature = self.sign_bytes(&vote_bytes).await?;

        Ok(SignedVote::new(vote, signature))
    }

    async fn verify_signed_vote(
        &self,
        vote: &Vote,
        signature: &ConsensusSignature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        self.verify_signed_bytes(&vote.to_sign_bytes(), signature, public_key)
            .await
    }

    async fn sign_proposal(
        &self,
        proposal: Proposal,
    ) -> Result<SignedProposal<ArcContext>, SigningError> {
        let proposal_bytes = proposal.to_sign_bytes();

        let signature = self.sign_bytes(&proposal_bytes).await?;
        Ok(SignedProposal::new(proposal, signature))
    }

    async fn verify_signed_proposal(
        &self,
        proposal: &Proposal,
        signature: &ConsensusSignature,
        public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        self.verify_signed_bytes(&proposal.to_sign_bytes(), signature, public_key)
            .await
    }

    async fn sign_vote_extension(
        &self,
        _extension: <ArcContext as Context>::Extension,
    ) -> Result<SignedExtension<ArcContext>, SigningError> {
        unreachable!("Vote extensions are not supported in Arc");
    }

    async fn verify_signed_vote_extension(
        &self,
        _extension: &<ArcContext as Context>::Extension,
        _signature: &ConsensusSignature,
        _public_key: &PublicKey,
    ) -> Result<VerificationResult, SigningError> {
        unreachable!("Vote extensions are not supported in Arc");
    }
}

#[cfg(test)]
mod unit_tests {
    use arc_consensus_types::{Address, BlockHash, Height, Round, Value, ValueId};
    use malachitebft_core_types::{NilOrVal, VoteType};

    use super::*;

    #[test]
    fn test_validate_signature_with_key_invalid_public_key_length() {
        let message = b"test message";
        let signature = [0u8; 64];

        // Test with public key too short
        let short_key = [0u8; 31];
        assert!(validate_signature_with_key(message, &signature, &short_key).is_err());

        // Test with public key too long
        let long_key = [0u8; 33];
        assert!(validate_signature_with_key(message, &signature, &long_key).is_err());

        // Test with empty public key
        let empty_key = [];
        assert!(validate_signature_with_key(message, &signature, &empty_key).is_err());
    }

    #[test]
    fn test_validate_signature_with_key_invalid_signature_length() {
        let message = b"test message";
        let public_key = [0u8; 32];

        // Test with signature too short
        let short_sig = [0u8; 63];
        assert!(validate_signature_with_key(message, &short_sig, &public_key).is_err());

        // Test with signature too long
        let long_sig = [0u8; 65];
        assert!(validate_signature_with_key(message, &long_sig, &public_key).is_err());

        // Test with empty signature
        let empty_sig = [];
        assert!(validate_signature_with_key(message, &empty_sig, &public_key).is_err());
    }

    #[test]
    fn test_validate_signature_with_key_invalid_public_key_parsing() {
        let message = b"test message";
        let signature = [0u8; 64];

        // Test with invalid public key bytes (all 0xFF which might be invalid for Ed25519)
        let invalid_key = [0xFFu8; 32];
        // This should return false due to invalid public key parsing
        let result = validate_signature_with_key(message, &signature, &invalid_key);
        // Note: This test might pass or fail depending on ed25519-dalek's validation
        // The important thing is that it doesn't panic
        let _ = result; // Should not panic
    }

    #[test]
    fn test_bytes_to_signature_valid_length() {
        let valid_sig_bytes = [1u8; 64];

        let signature = bytes_to_signature(&valid_sig_bytes).unwrap();
        assert_eq!(signature.to_bytes(), valid_sig_bytes);
    }

    #[test]
    fn test_bytes_to_signature_invalid_length() {
        // Test with signature too short
        let short_sig = [1u8; 32];
        let result = bytes_to_signature(&short_sig);
        assert!(result.is_err()); // Should return error

        // Test with signature too long
        let long_sig = [1u8; 128];
        let result = bytes_to_signature(&long_sig);
        assert!(result.is_err()); // Should return error

        // Test with empty signature
        let empty_sig = [];
        let result = bytes_to_signature(&empty_sig);
        assert!(result.is_err()); // Should return error
    }

    #[test]
    fn signing_provider_verify_flows() {
        use ed25519_dalek::Signer;
        use ed25519_dalek::SigningKey;

        // Deterministic keypair
        let secret_bytes = [7u8; 32];
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();
        let public_key_bytes = verifying_key.to_bytes();

        // Vote
        let vote = Vote {
            typ: VoteType::Prevote,
            height: Height::new(1),
            round: Round::new(0),
            value: NilOrVal::Val(ValueId::new(BlockHash::new([1u8; 32]))),
            validator_address: Address::new([2u8; 20]),
            extension: None,
        };
        let vote_bytes = vote.to_sign_bytes();
        let vote_sig = signing_key.sign(&vote_bytes).to_bytes();
        assert!(
            validate_signature_with_key(&vote_bytes, &vote_sig, &public_key_bytes)
                .is_ok_and(|r| r.is_valid())
        );

        // Proposal
        let proposal = Proposal {
            height: Height::new(1),
            round: Round::new(0),
            value: Value::new(BlockHash::new([3u8; 32])),
            pol_round: Round::Nil,
            validator_address: Address::new([4u8; 20]),
        };
        let proposal_bytes = proposal.to_sign_bytes();
        let proposal_sig = signing_key.sign(&proposal_bytes).to_bytes();
        assert!(
            validate_signature_with_key(&proposal_bytes, &proposal_sig, &public_key_bytes)
                .is_ok_and(|r| r.is_valid())
        );

        // Vote extension
        // NOTE: Disabled due to vote extensions not being supported in Arc at this time
        // let ext = MockExtension;
        // let ext_bytes = serialization::extension_to_bytes::<MockContext>(&ext);
        // let ext_sig = signing_key.sign(&ext_bytes).to_bytes();
        // assert!(
        //     validate_signature_with_key(&ext_bytes, &ext_sig, &public_key_bytes)
        //         .is_ok_and(|r| r.is_valid())
        // );

        // Negative case: flip a byte in the vote signature
        let mut bad_sig = vote_sig;
        bad_sig[0] ^= 0x01;
        assert!(
            validate_signature_with_key(&vote_bytes, &bad_sig, &public_key_bytes)
                .is_ok_and(|r| r.is_invalid())
        );
    }
}

#[cfg(all(test, feature = "integration-remote-signer"))]
mod integration_tests {
    use std::thread;

    use arc_consensus_types::{Address, BlockHash, Height, Round, Value, ValueId};
    use malachitebft_core_types::{NilOrVal, VoteType};

    use crate::RemoteSigningError;

    use super::*;

    /// Validate a signature using the public key from the external service
    /// This is a test helper method that fetches the public key and validates the signature
    async fn validate_signature(
        provider: &RemoteSigningProvider,
        message: &[u8],
        signature: &[u8],
    ) -> Result<bool, RemoteSigningError> {
        // Get the public key for validation
        let public_key = provider.public_key().await?;

        // Validate signature is 64 bytes
        if signature.len() != 64 {
            return Err(RemoteSigningError::InvalidResponse(format!(
                "Invalid signature length: expected 64 bytes, got {}",
                signature.len()
            )));
        }

        // Use the synchronous validation method with raw bytes
        let result = validate_signature_with_key(message, signature, public_key.as_bytes())
            .is_ok_and(|r| r.is_valid());

        Ok(result)
    }

    async fn create_provider() -> Result<RemoteSigningProvider, RemoteSigningError> {
        let config = RemoteSigningConfig::default();
        RemoteSigningProvider::new(config).await
    }

    fn create_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime")
    }

    #[tokio::test]
    async fn provider_creation_success() {
        let config = RemoteSigningConfig::default();
        let result = RemoteSigningProvider::new(config.clone()).await;

        assert!(
            result.is_ok(),
            "Provider creation should succeed in integration tests. \
             Please ensure the remote signer service is running at {}",
            config.endpoint
        );
    }

    #[tokio::test]
    async fn provider_creation_failure() {
        let config = RemoteSigningConfig::new("http://localhost:9999".to_string());
        let result = RemoteSigningProvider::new(config).await;

        // With lazy connection, provider creation may succeed but operations will fail
        if result.is_ok() {
            let provider = result.unwrap();
            // Try to get public key - this should fail
            let public_key_result = provider.public_key().await;
            assert!(
                public_key_result.is_err(),
                "Public key retrieval should fail with bad endpoint"
            );
        } else {
            // If provider creation failed, that's also acceptable
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn provider_config_access() {
        let config = RemoteSigningConfig::default();
        let provider = RemoteSigningProvider::new(config)
            .await
            .expect("Failed to create provider");

        let provider_config = provider.config();
        assert_eq!(provider_config.endpoint, "http://0.0.0.0:10340");
    }

    #[tokio::test]
    async fn cache_operations() {
        let provider = create_provider().await.expect("Failed to create provider");

        // Test cache clearing - should not panic
        provider.clear_cache().await;

        // Verify cache is cleared by checking public key retrieval still works
        let public_key_result = provider.public_key().await;
        assert!(
            public_key_result.is_ok(),
            "Public key retrieval should work after cache clear"
        );
    }

    #[tokio::test]
    async fn public_key_caching() {
        let provider = create_provider().await.expect("Failed to create provider");

        // First call should fetch from external service
        let public_key1 = provider.public_key().await.unwrap();

        // Second call should use cache (we can't directly verify this in integration test,
        // but we can verify the keys are the same)
        let public_key2 = provider.public_key().await.unwrap();

        assert_eq!(public_key1, public_key2, "Cached key should match original");

        // Clear cache and fetch again
        provider.clear_cache().await;
        let public_key3 = provider.public_key().await.unwrap();

        assert_eq!(
            public_key1, public_key3,
            "Key after cache clear should match original"
        );
    }

    #[tokio::test]
    async fn signature_validation_workflow() {
        let provider = create_provider().await.expect("Failed to create provider");

        let message = b"test message for validation";

        // Sign the message
        let signature = provider.client.sign_message(message).await.unwrap();

        // Validate using the async method
        let is_valid = validate_signature(&provider, message, &signature)
            .await
            .unwrap();

        assert!(is_valid, "Self-signed signature should be valid");
    }

    #[tokio::test]
    async fn tls_configuration() {
        let config = RemoteSigningConfig::default().with_tls(Some("/path/to/cert.pem".to_string()));

        assert!(config.enable_tls, "TLS should be enabled");
        assert_eq!(config.tls_cert_path, Some("/path/to/cert.pem".to_string()));
    }

    // Sync facade tests - testing the sync wrapper functionality
    #[test]
    fn sync_provider_creation() {
        let rt = create_runtime();
        let config = RemoteSigningConfig::default();

        let provider = rt.block_on(async { RemoteSigningProvider::new(config).await });

        assert!(
            provider.is_ok(),
            "Provider creation should succeed in integration tests"
        );
    }

    #[test]
    fn sync_public_key_retrieval() {
        let rt = create_runtime();
        let config = RemoteSigningConfig::default();

        let provider = rt
            .block_on(async { RemoteSigningProvider::new(config).await })
            .expect("Failed to create provider");

        // Test the async method through runtime
        let public_key = rt.block_on(provider.public_key());

        assert!(public_key.is_ok());
    }

    #[test]
    fn sync_message_signing() {
        let rt = create_runtime();
        let config = RemoteSigningConfig::default();

        let provider = rt
            .block_on(RemoteSigningProvider::new(config))
            .expect("Failed to create provider");

        let message = b"test message to sign";

        // Use the client's async method through runtime
        let signature = rt
            .block_on(provider.client.sign_message(message))
            .expect("Failed to sign message");

        assert!(!signature.is_empty(), "Signature should not be empty");
        assert_eq!(signature.len(), 64, "Signature should be exactly 64 bytes");
    }

    #[test]
    fn sync_signature_validation() {
        let rt = create_runtime();
        let config = RemoteSigningConfig::default();

        let provider = rt
            .block_on(async { RemoteSigningProvider::new(config).await })
            .expect("Failed to create provider");

        let message = b"test message for validation";

        // Sign the message
        let signature = rt
            .block_on(async { provider.client.sign_message(message).await })
            .expect("Failed to sign message");

        // Validate the signature using async method
        let is_valid = rt
            .block_on(async { validate_signature(&provider, message, &signature).await })
            .expect("Failed to validate signature");

        assert!(is_valid, "Self-signed signature should be valid");
    }

    #[test]
    fn sync_validate_with_specific_key() {
        let rt = create_runtime();
        let config = RemoteSigningConfig::default();

        let provider = rt
            .block_on(RemoteSigningProvider::new(config))
            .expect("Failed to create provider");

        // Get the public key
        let public_key = rt
            .block_on(provider.public_key())
            .expect("Failed to get public key");

        let message = b"test message";

        // Sign the message
        let signature = rt
            .block_on(async { provider.client.sign_message(message).await })
            .expect("Failed to sign message");

        // Validate signature is 64 bytes
        assert_eq!(signature.len(), 64, "Signature should be 64 bytes");

        // Validate using the synchronous method (with raw bytes)
        let is_valid = validate_signature_with_key(message, &signature, public_key.as_bytes())
            .is_ok_and(|r| r.is_valid());

        assert!(is_valid, "Signature validation should succeed");
    }

    #[test]
    fn sync_concurrent_operations() {
        let rt = create_runtime();
        let config = RemoteSigningConfig::default();

        let provider = rt
            .block_on(RemoteSigningProvider::new(config))
            .expect("Failed to create provider");

        // Test concurrent operations from multiple threads
        let handles: Vec<_> = (0..5)
            .map(|i| {
                let provider_clone = provider.clone();
                let rt_handle = rt.handle().clone();
                thread::spawn(move || {
                    let message = format!("test message {i}");
                    rt_handle.block_on(async move {
                        provider_clone.client.sign_message(message.as_bytes()).await
                    })
                })
            })
            .collect();

        // Collect all results
        let results: Vec<Result<Vec<u8>, _>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Verify all signatures were successful
        for (i, result) in results.iter().enumerate() {
            assert!(result.is_ok(), "Signing should succeed for message {i}");
            let signature = result.as_ref().unwrap();
            assert!(!signature.is_empty(), "Signature {i} should not be empty");
        }
    }

    /// Test the signing/verify flows for the remote signing provider via the SigningProvider trait
    #[tokio::test]
    async fn remote_signing_provider_sign_verify_flows() {
        use super::SigningProvider;

        let provider = create_provider().await.expect("Failed to create provider");

        let public_key = provider
            .public_key()
            .await
            .expect("Failed to get public key");

        // Vote
        let vote = Vote {
            typ: VoteType::Prevote,
            height: Height::new(1),
            round: Round::new(0),
            value: NilOrVal::Val(ValueId::new(BlockHash::new([1u8; 32]))),
            validator_address: Address::new([2u8; 20]),
            extension: None,
        };
        let signed_vote = provider.sign_vote(vote).await.unwrap();
        let result = provider
            .verify_signed_vote(&signed_vote.message, &signed_vote.signature, &public_key)
            .await
            .unwrap();
        assert!(result.is_valid());

        // Proposal
        let proposal = Proposal {
            height: Height::new(1),
            round: Round::new(0),
            value: Value::new(BlockHash::new([3u8; 32])),
            pol_round: Round::Nil,
            validator_address: Address::new([4u8; 20]),
        };
        let signed_proposal = provider.sign_proposal(proposal).await.unwrap();
        let result = provider
            .verify_signed_proposal(
                &signed_proposal.message,
                &signed_proposal.signature,
                &public_key,
            )
            .await
            .unwrap();
        assert!(result.is_valid());

        // Raw bytes
        let bytes = b"test message";
        let signature = provider.sign_bytes(bytes).await.unwrap();
        let result = provider
            .verify_signed_bytes(bytes, &signature, &public_key)
            .await
            .unwrap();
        assert!(result.is_valid());

        // Vote extensions
        // NOTE: Disabled due to vote extensions not being supported in Arc at this time
        // let ext = MockExtension;
        // let signed_ext = provider.sign_vote_extension(ext).await;
        // let result = provider
        //     .verify_signed_vote_extension(&signed_ext.message, &signed_ext.signature, &public_key)
        //     .await;
        // assert!(result);
    }
}
