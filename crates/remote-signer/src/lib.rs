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

//! Consensus Remote Signing (see docs/features/remote-signer.md)
//!
//! This module provides remote signing support for the consensus layer (Malachite)
//! by delegating signing operations to an external gRPC service.

pub mod client;
pub mod config;
pub mod error;
pub mod metrics;
pub mod provider;

// Re-export the main types for easy access
pub use client::RemoteSignerClient;
pub use config::{RemoteSigningConfig, RetryConfig};
pub use error::RemoteSigningError;
pub use metrics::RemoteSigningMetrics;
pub use provider::RemoteSigningProvider;

// Re-export signing types from malachitebft_signing
pub use malachitebft_signing::{Error as SigningError, SigningProvider, VerificationResult};

/// Result type for consensus remote signing operations
pub type Result<T> = std::result::Result<T, RemoteSigningError>;
