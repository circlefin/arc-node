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

//! gRPC client for external signing service
//!
//! This client provides a fully asynchronous interface for remote signing operations.
//! It acts as the transport layer between the provider and the gRPC service.
//!
//! ## Data Format Convention
//!
//! All data is transmitted as raw bytes:
//!
//! ### Outgoing (to gRPC service):
//! - Messages: Raw bytes
//!
//! ### Incoming (from gRPC service):
//! - Public keys: Raw 32 bytes (Ed25519)
//! - Signatures: Raw 64 bytes (Ed25519)
//!
//! ### Client Interface (to provider):
//! - `get_public_key()`: Returns raw 32-byte Ed25519 public key
//! - `sign_message()`: Returns raw 64-byte Ed25519 signature

use backon::RetryableWithContext;
use tokio::fs;
use tonic::transport::{Certificate, ClientTlsConfig};
use tonic::{transport::Channel, Request};
use tracing::{debug, error, trace, warn};

use crate::metrics::RemoteSigningMetrics;
use crate::{config::RemoteSigningConfig, error::RemoteSigningError};

// Protobuf definitions for the external signing service
pub mod proto {
    tonic::include_proto!("arc.signer.v1");
}

use proto::signer_service_client::SignerServiceClient;

/// gRPC client for external signing service
#[derive(Clone)]
pub struct RemoteSignerClient {
    /// The gRPC client
    client: SignerServiceClient<Channel>,
    /// Configuration
    config: RemoteSigningConfig,
    /// Metrics
    metrics: RemoteSigningMetrics,
}

/// Ed25519 public key size in bytes
const ED25519_PUBLIC_KEY_SIZE_BYTES: usize = 32;

/// Ed25519 signature size in bytes
const ED25519_SIGNATURE_SIZE_BYTES: usize = 64;

impl RemoteSignerClient {
    /// Create a new remote signer client
    pub async fn new(config: RemoteSigningConfig) -> Result<Self, RemoteSigningError> {
        config
            .validate()
            .map_err(RemoteSigningError::Configuration)?;

        let mut channel_builder = Channel::from_shared(config.endpoint.clone())
            .map_err(|e| RemoteSigningError::Configuration(format!("Invalid endpoint: {e}")))?
            .timeout(config.timeout);

        // Configure TLS if enabled and certificate path is provided
        if config.enable_tls
            && let Some(cert_path) = &config.tls_cert_path
        {
            let cert = fs::read(cert_path).await.map_err(|e| {
                RemoteSigningError::Configuration(format!(
                    "Failed to read TLS certificate from {cert_path}: {e}"
                ))
            })?;

            let tls_config = ClientTlsConfig::new().ca_certificate(Certificate::from_pem(cert));
            channel_builder = channel_builder.tls_config(tls_config)?;
        }

        let channel = channel_builder.connect().await?;
        let client = SignerServiceClient::new(channel);

        Ok(Self {
            client,
            config,
            metrics: RemoteSigningMetrics::new(),
        })
    }

    /// Sign a message
    #[tracing::instrument(name = "remote_signer", skip_all)]
    pub async fn sign_message(&self, message: &[u8]) -> Result<Vec<u8>, RemoteSigningError> {
        let start = std::time::Instant::now();

        self.metrics.inc_sign_requests();

        let client = self.client.clone();

        let signature =
            Self::sign_message_with_retry(client, &self.config, &self.metrics, message).await?;

        let duration = start.elapsed();
        self.metrics.observe_sign_request_latency_total(duration);

        Ok(signature)
    }

    /// Get the public key from the external signing service.
    ///
    /// Returns the public key as a raw 32-byte Ed25519 key.
    #[tracing::instrument(name = "remote_signer", skip_all)]
    pub async fn get_public_key(&self) -> Result<Vec<u8>, RemoteSigningError> {
        let mut client = self.client.clone();
        Self::get_public_key_internal(&mut client, &self.config).await
    }

    /// Internal async method to get public key
    async fn get_public_key_internal(
        grpc_client: &mut SignerServiceClient<Channel>,
        config: &RemoteSigningConfig,
    ) -> Result<Vec<u8>, RemoteSigningError> {
        debug!(
            endpoint = %config.endpoint,
            "Requesting public key from remote signer"
        );

        let request = Request::new(proto::PublicKeyRequest {});

        let start = std::time::Instant::now();
        let response = grpc_client
            .public_key(request)
            .await
            .map_err(|e| RemoteSigningError::Status(Box::new(e)))?;
        let duration = start.elapsed();

        let public_key = response.into_inner().public_key;

        // Validate Ed25519 public key length
        if public_key.len() != ED25519_PUBLIC_KEY_SIZE_BYTES {
            return Err(RemoteSigningError::InvalidResponse(format!(
                "Invalid public key length: expected {} bytes, got {}",
                ED25519_PUBLIC_KEY_SIZE_BYTES,
                public_key.len()
            )));
        }

        debug!(
            endpoint = %config.endpoint,
            duration = ?duration,
            key_len = public_key.len(),
            "Successfully retrieved public key from remote signer"
        );

        Ok(public_key)
    }

    /// Internal async method to sign a message with retry
    async fn sign_message_with_retry(
        grpc_client: SignerServiceClient<Channel>,
        config: &RemoteSigningConfig,
        metrics: &RemoteSigningMetrics,
        message: &[u8],
    ) -> Result<Vec<u8>, RemoteSigningError> {
        let task = |mut client: SignerServiceClient<Channel>| async {
            let result = Self::sign_message_once(&mut client, config, metrics, message).await;
            (client, result)
        };

        let mut attempt = 0;

        let (_client, result) = task
            .retry(config.retry_config)
            .context(grpc_client)
            .notify(|error, backoff| {
                attempt += 1;
                metrics.inc_sign_request_retries();

                warn!(
                    %attempt, max_retries = %config.retry_config.max_retries,
                    "Failed to sign message, retrying in {backoff:?}: {error}"
                );
            })
            .await;

        match result {
            Ok(signature) => Ok(signature),

            Err(e) => {
                error!("Failed to sign message after {attempt} retries: {e}");

                Err(RemoteSigningError::RetryExhausted {
                    retries: config.retry_config.max_retries,
                })
            }
        }
    }

    /// Sign a message once (without retry)
    async fn sign_message_once(
        grpc_client: &mut SignerServiceClient<Channel>,
        config: &RemoteSigningConfig,
        metrics: &RemoteSigningMetrics,
        message: &[u8],
    ) -> Result<Vec<u8>, RemoteSigningError> {
        trace!(
            endpoint = %config.endpoint,
            message_len = message.len(),
            "Requesting signature from remote signer"
        );

        let request = Request::new(proto::SignRequest {
            message: message.to_vec(),
        });

        let start = std::time::Instant::now();
        let result = grpc_client
            .sign(request)
            .await
            .inspect_err(|_| {
                metrics.inc_sign_request_errors();
            })
            .map_err(|e| RemoteSigningError::Status(Box::new(e)));
        let duration = start.elapsed();

        metrics.observe_sign_request_latency_single(duration);

        let signature = result?.into_inner().signature;

        // Validate Ed25519 signature length (raw bytes, no hex decoding needed)
        if signature.len() != ED25519_SIGNATURE_SIZE_BYTES {
            return Err(RemoteSigningError::InvalidResponse(format!(
                "Invalid signature length: expected {} bytes, got {}",
                ED25519_SIGNATURE_SIZE_BYTES,
                signature.len()
            )));
        }

        trace!(
            endpoint = %config.endpoint,
            duration = ?duration,
            message_len = message.len(),
            signature_len = signature.len(),
            "Successfully signed message with remote signer"
        );

        Ok(signature)
    }

    /// Get the configuration
    pub fn config(&self) -> &RemoteSigningConfig {
        &self.config
    }

    /// Get the metrics
    pub fn metrics(&self) -> &RemoteSigningMetrics {
        &self.metrics
    }
}

#[cfg(all(test, feature = "integration-remote-signer"))]
mod integration_tests {
    use super::*;
    use crate::RetryConfig;
    use std::time::Duration;

    #[tokio::test]
    async fn client_creation_success() {
        let config = RemoteSigningConfig::default();
        let result = RemoteSignerClient::new(config.clone()).await;

        assert!(
            result.is_ok(),
            "Client creation should succeed in integration tests. \
             Please ensure the remote signer service is running at {}",
            config.endpoint
        );
    }

    #[tokio::test]
    async fn client_creation_failure() {
        let config = RemoteSigningConfig::new("http://localhost:9999".to_string());
        let result = RemoteSignerClient::new(config).await;

        // With lazy connection, client creation may succeed but operations will fail
        if result.is_ok() {
            let client = result.unwrap();
            // Try to get public key - this should fail
            let public_key_result = client.get_public_key().await;
            assert!(
                public_key_result.is_err(),
                "Public key retrieval should fail with bad endpoint"
            );
        }
    }

    #[tokio::test]
    async fn public_key_retrieval() {
        let config = RemoteSigningConfig::default();
        let client = RemoteSignerClient::new(config)
            .await
            .expect("Failed to create client");

        let public_key_result = client.get_public_key().await;

        match public_key_result {
            Ok(public_key) => {
                assert!(!public_key.is_empty(), "Public key should not be empty");
                // Should always be 32 raw bytes
                assert_eq!(
                    public_key.len(),
                    ED25519_PUBLIC_KEY_SIZE_BYTES,
                    "Public key should be exactly {} bytes, got {}",
                    ED25519_PUBLIC_KEY_SIZE_BYTES,
                    public_key.len()
                );
            }
            Err(e) => {
                panic!("Failed to retrieve public key in integration test: {e:?}");
            }
        }
    }

    #[tokio::test]
    async fn message_signing() {
        let config = RemoteSigningConfig::default();
        let client = RemoteSignerClient::new(config)
            .await
            .expect("Failed to create client");

        let message = b"test message to sign";
        let sign_result = client.sign_message(message).await;

        match sign_result {
            Ok(signature) => {
                assert!(!signature.is_empty(), "Signature should not be empty");
                // Should always be 64 raw bytes
                assert_eq!(
                    signature.len(),
                    ED25519_SIGNATURE_SIZE_BYTES,
                    "Signature should be exactly {} bytes, got {}",
                    ED25519_SIGNATURE_SIZE_BYTES,
                    signature.len()
                );
            }
            Err(e) => {
                panic!("Failed to sign message in integration test: {e:?}");
            }
        }
    }

    #[tokio::test]
    async fn retry_behavior() {
        // Test with a config that will fail (bad endpoint)
        let config = RemoteSigningConfig::new("http://localhost:9999".to_string())
            .with_retry_config(RetryConfig::new(
                2,                          // max_retries
                Duration::from_millis(10),  // short initial backoff for testing
                Duration::from_millis(100), // short max backoff
            ));

        let result = RemoteSignerClient::new(config).await;

        // With lazy connection, client creation may succeed but operations will fail
        if result.is_ok() {
            let client = result.unwrap();
            // Try to sign a message - this should fail with retry exhaustion
            let sign_result = client.sign_message(b"test").await;
            assert!(
                sign_result.is_err(),
                "Signing should fail with bad endpoint"
            );

            if let Err(RemoteSigningError::RetryExhausted { retries }) = sign_result {
                assert_eq!(retries, 2, "Should have exhausted exactly 2 retries");
            }
        }
    }

    #[tokio::test]
    async fn config_builder_pattern() {
        let config = RemoteSigningConfig::default()
            .with_timeout(Duration::from_secs(10))
            .with_retry_config(RetryConfig::new(
                2,
                Duration::from_millis(50),
                Duration::from_secs(1),
            ));

        assert_eq!(config.endpoint, "http://0.0.0.0:10340");
        assert_eq!(config.timeout, Duration::from_secs(10));
        assert_eq!(config.retry_config.max_retries, 2);
    }

    #[tokio::test]
    async fn retry_config_validation() {
        let retry_config = RetryConfig::new(5, Duration::from_millis(100), Duration::from_secs(10))
            .with_backoff_multiplier(1.5);

        assert_eq!(retry_config.max_retries, 5);
        assert_eq!(retry_config.initial_backoff, Duration::from_millis(100));
        assert_eq!(retry_config.max_backoff, Duration::from_secs(10));
        assert_eq!(retry_config.backoff_multiplier, 1.5);
    }
}
