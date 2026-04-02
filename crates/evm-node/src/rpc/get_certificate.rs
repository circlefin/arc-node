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

//! getCertificate RPC API implementation

use crate::rpc::common::{codes, invalid_params};
use arc_consensus_types::{Address, ValueId};
use async_trait::async_trait;
use backon::{ExponentialBuilder, Retryable};
use jsonrpsee::{
    core::RpcResult,
    types::{ErrorCode, ErrorObjectOwned},
};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_with::{base64::Base64, serde_as};
use std::time::Duration;
use tracing::error;

/// Default maximum retry attempts for upstream certificate HTTP fetches.
const HTTP_MAX_RETRIES: usize = 3;

//TODO: These are copied from malachite-app, should we move them to types?
/// Signature entry within a commit certificate (base64 encoded signature bytes).
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcCommitSignature {
    pub address: Address,
    #[serde_as(as = "Base64")]
    pub signature: Vec<u8>,
}

/// Commit certificate returned by `arc_getCertificate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcCommitCertificate {
    pub height: u64,
    pub round: i64,
    pub block_hash: ValueId,
    #[serde(rename = "signatures")]
    pub signatures: Vec<RpcCommitSignature>,
}

/// Core logic for the `arc_getCertificate` method: validates params, performs fetch, maps errors.
pub async fn rpc_get_certificate(
    source: &dyn CertificateSource,
    height: u64,
) -> RpcResult<RpcCommitCertificate> {
    if height < 1 {
        return Err(invalid_params(format!(
            "height must be >= 1, got {}",
            height
        )));
    }
    match source.fetch(height).await {
        Ok(Some(cert)) => Ok(cert),
        Ok(None) | Err(FetchError::NotFound) => Err(ErrorObjectOwned::owned(
            codes::NOT_FOUND,
            "Certificate not found",
            None::<()>,
        )),
        Err(FetchError::Timeout) => Err(ErrorObjectOwned::owned(
            ErrorCode::InternalError.code(),
            "Timeout fetching certificate",
            None::<()>,
        )),
        Err(FetchError::Connect) => Err(ErrorObjectOwned::owned(
            codes::UPSTREAM_UNREACHABLE,
            "Upstream certificate server unreachable",
            None::<()>,
        )),
        Err(FetchError::Network(status)) => Err(ErrorObjectOwned::owned(
            ErrorCode::InternalError.code(),
            format!("Upstream HTTP error: {status}"),
            None::<()>,
        )),
        Err(FetchError::Decode(e)) => Err(ErrorObjectOwned::owned(
            ErrorCode::InternalError.code(),
            format!("Failed to decode upstream response: {e}"),
            None::<()>,
        )),
    }
}

#[async_trait]
pub trait CertificateSource: Send + Sync {
    async fn fetch(&self, height: u64) -> Result<Option<RpcCommitCertificate>, FetchError>;
}

#[derive(thiserror::Error, Debug)]
pub enum FetchError {
    #[error("not found")]
    NotFound,
    #[error("timeout")]
    Timeout,
    #[error("connect error")]
    Connect,
    #[error("network status {0}")]
    Network(StatusCode),
    #[error("decode error: {0}")]
    Decode(String),
}

#[derive(Clone)]
pub struct HttpCertificateSource {
    client: reqwest::Client,
    base_url: String,
    max_retries: usize,
}

impl HttpCertificateSource {
    pub fn new(base_url: impl Into<String>) -> eyre::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| eyre::eyre!("Failed to build HTTP client: {}", e))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            max_retries: HTTP_MAX_RETRIES,
        })
    }
}

#[async_trait]
impl CertificateSource for HttpCertificateSource {
    async fn fetch(&self, height: u64) -> Result<Option<RpcCommitCertificate>, FetchError> {
        let url = format!(
            "{}/commit?height={}",
            self.base_url.trim_end_matches('/'),
            height
        );
        let builder = ExponentialBuilder::default()
            .with_min_delay(Duration::from_millis(50))
            .with_max_delay(Duration::from_secs(5))
            .with_max_times(self.max_retries);
        let operation = || async {
            let resp = match self.client.get(&url).send().await {
                Ok(r) => r,
                Err(e) if e.is_timeout() => {
                    error!(height=?height, "Timeout fetching certificate");
                    return Err(FetchError::Timeout);
                }
                Err(e) if e.is_connect() => {
                    error!(height=?height, "Connect error fetching certificate");
                    return Err(FetchError::Connect);
                }
                Err(e) => {
                    error!(height=%height, error=?e, "General network error fetching certificate");
                    return Err(FetchError::Network(StatusCode::INTERNAL_SERVER_ERROR));
                }
            };
            let status = resp.status();
            if status == StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !status.is_success() {
                error!(height=%height, status=?status, "Upstream non-success status");
                return Err(FetchError::Network(status));
            }
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| FetchError::Decode(e.to_string()))?;
            let cert = serde_json::from_slice::<RpcCommitCertificate>(&bytes)
                .map_err(|e| FetchError::Decode(e.to_string()))?;
            Ok(Some(cert))
        };
        operation
            .retry(builder)
            .sleep(tokio::time::sleep)
            .when(|e: &FetchError| {
                matches!(
                    e,
                    FetchError::Timeout | FetchError::Connect | FetchError::Network(_)
                )
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{
        arc::{ArcApiImpl, ArcApiServer},
        common::codes::{NOT_FOUND, UPSTREAM_UNREACHABLE},
        get_certificate::{CertificateSource, RpcCommitCertificate},
    };
    use arc_consensus_types::{BlockHash, ValueId};
    use async_trait::async_trait;
    use jsonrpsee::types::ErrorCode;
    use reqwest::StatusCode;
    struct MockCertSource;
    #[async_trait]
    impl CertificateSource for MockCertSource {
        async fn fetch(&self, h: u64) -> Result<Option<RpcCommitCertificate>, FetchError> {
            Ok(Some(RpcCommitCertificate {
                height: h,
                round: 3,
                block_hash: ValueId::new(BlockHash::ZERO),
                signatures: vec![],
            }))
        }
    }

    #[tokio::test]
    async fn rejects_invalid_height() {
        let api = ArcApiImpl::new(MockCertSource);
        let err = ArcApiServer::get_certificate(&api, 0).await.unwrap_err();
        assert!(err.message().contains("height must be"));
    }

    #[tokio::test]
    async fn not_found_maps_to_error() {
        struct EmptySource;
        #[async_trait]
        impl CertificateSource for EmptySource {
            async fn fetch(&self, _h: u64) -> Result<Option<RpcCommitCertificate>, FetchError> {
                Ok(None)
            }
        }
        let api = ArcApiImpl::new(EmptySource);
        let err = ArcApiServer::get_certificate(&api, 5).await.unwrap_err();
        assert_eq!(err.code(), NOT_FOUND);
    }
    struct ConnectFail;
    #[async_trait]
    impl CertificateSource for ConnectFail {
        async fn fetch(&self, _h: u64) -> Result<Option<RpcCommitCertificate>, FetchError> {
            Err(FetchError::Connect)
        }
    }

    #[tokio::test]
    async fn connect_failure_maps_to_custom_code() {
        let api = ArcApiImpl::new(ConnectFail);
        let err = ArcApiServer::get_certificate(&api, 10).await.unwrap_err();
        assert_eq!(err.code(), UPSTREAM_UNREACHABLE);
        assert!(err.message().contains("unreachable"));
    }

    #[tokio::test]
    async fn returns_real_cert_from_source() {
        let api = ArcApiImpl::new(MockCertSource);
        let cert = ArcApiServer::get_certificate(&api, 7).await.unwrap();
        assert_eq!(cert.height, 7);
        assert_eq!(cert.round, 3);
    }
    struct TimeoutSource;
    #[async_trait]
    impl CertificateSource for TimeoutSource {
        async fn fetch(&self, _h: u64) -> Result<Option<RpcCommitCertificate>, FetchError> {
            Err(FetchError::Timeout)
        }
    }

    #[tokio::test]
    async fn timeout_maps_to_internal_error() {
        let api = ArcApiImpl::new(TimeoutSource);
        let err = ArcApiServer::get_certificate(&api, 9).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::InternalError.code());
        assert!(err.message().contains("Timeout"));
    }

    struct DecodeFailSource;
    #[async_trait]
    impl CertificateSource for DecodeFailSource {
        async fn fetch(&self, _h: u64) -> Result<Option<RpcCommitCertificate>, FetchError> {
            Err(FetchError::Decode("boom".into()))
        }
    }

    #[tokio::test]
    async fn decode_error_maps_to_internal_error() {
        let api = ArcApiImpl::new(DecodeFailSource);
        let err = ArcApiServer::get_certificate(&api, 11).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::InternalError.code());
        assert!(err.message().contains("Failed to decode"));
    }

    struct NetworkFailSource;
    #[async_trait]
    impl CertificateSource for NetworkFailSource {
        async fn fetch(&self, _h: u64) -> Result<Option<RpcCommitCertificate>, FetchError> {
            Err(FetchError::Network(StatusCode::INTERNAL_SERVER_ERROR))
        }
    }

    #[tokio::test]
    async fn network_status_maps_to_internal_error() {
        let api = ArcApiImpl::new(NetworkFailSource);
        let err = ArcApiServer::get_certificate(&api, 13).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::InternalError.code());
        assert!(err.message().contains("Upstream HTTP error"));
        assert!(err.message().contains("500"));
    }
}
