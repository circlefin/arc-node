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

//! Configuration types for consensus remote signing

use std::time::Duration;

use backon::{BackoffBuilder, ExponentialBuilder};

/// Configuration for the consensus remote signing client
#[derive(Debug, Clone)]
pub struct RemoteSigningConfig {
    pub endpoint: String,
    pub timeout: Duration,
    pub retry_config: RetryConfig,
    pub enable_tls: bool,
    pub tls_cert_path: Option<String>,
}

impl Default for RemoteSigningConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://0.0.0.0:10340".to_string(),
            timeout: Duration::from_secs(30),
            retry_config: RetryConfig::default(),
            enable_tls: false,
            tls_cert_path: None,
        }
    }
}

impl RemoteSigningConfig {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            ..Default::default()
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    pub fn with_tls(mut self, cert_path: Option<impl ToString>) -> Self {
        self.enable_tls = true;
        self.tls_cert_path = cert_path.map(|p| p.to_string());
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        // Validate endpoint is a valid URL
        url::Url::parse(&self.endpoint).map_err(|e| format!("Invalid endpoint URL: {e}"))?;

        // Validate TLS config consistency
        if self.enable_tls && self.tls_cert_path.is_none() {
            return Err("TLS enabled but no certificate path provided".to_string());
        }

        Ok(())
    }
}

/// Retry configuration for gRPC calls
#[derive(Debug, Copy, Clone)]
pub struct RetryConfig {
    pub max_retries: usize,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f32,
}

impl RetryConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.initial_backoff >= self.max_backoff {
            return Err("initial_backoff must be less than max_backoff".to_string());
        }

        if self.backoff_multiplier < 1.0 {
            return Err("backoff_multiplier must be at least 1.0".to_string());
        }

        Ok(())
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            backoff_multiplier: 2.0,
        }
    }
}

impl BackoffBuilder for RetryConfig {
    type Backoff = <ExponentialBuilder as BackoffBuilder>::Backoff;

    fn build(self) -> Self::Backoff {
        ExponentialBuilder::new()
            .with_max_times(self.max_retries)
            .with_min_delay(self.initial_backoff)
            .with_max_delay(self.max_backoff)
            .with_factor(self.backoff_multiplier)
            .build()
    }
}

impl RetryConfig {
    pub fn new(max_retries: usize, initial_backoff: Duration, max_backoff: Duration) -> Self {
        Self {
            max_retries,
            initial_backoff,
            max_backoff,
            backoff_multiplier: 2.0,
        }
    }

    pub fn with_backoff_multiplier(mut self, multiplier: f32) -> Self {
        self.backoff_multiplier = multiplier;
        self
    }
}

impl From<arc_consensus_types::RemoteSigningConfig> for RemoteSigningConfig {
    fn from(config: arc_consensus_types::RemoteSigningConfig) -> Self {
        Self {
            endpoint: config.endpoint,
            timeout: config.timeout,
            retry_config: config.retry.into(),
            enable_tls: config.enable_tls,
            tls_cert_path: config.tls_cert_path,
        }
    }
}

impl From<arc_consensus_types::RetryConfig> for RetryConfig {
    fn from(config: arc_consensus_types::RetryConfig) -> Self {
        Self {
            max_retries: config.max_retries,
            initial_backoff: config.initial_backoff,
            max_backoff: config.max_backoff,
            backoff_multiplier: config.backoff_multiplier,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_validation() {
        let config = RemoteSigningConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_endpoint() {
        let config = RemoteSigningConfig {
            endpoint: "invalid_url".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_tls_without_cert_path() {
        let config = RemoteSigningConfig {
            enable_tls: true,
            tls_cert_path: None,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_valid_tls_config() {
        let config = RemoteSigningConfig {
            enable_tls: true,
            tls_cert_path: Some("path/to/cert.pem".to_string()),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }
}
