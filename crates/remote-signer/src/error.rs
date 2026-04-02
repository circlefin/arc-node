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

//! Error types for consensus remote signing operations

use thiserror::Error;

/// Error types for consensus remote signing operations
#[derive(Debug, Error)]
pub enum RemoteSigningError {
    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("gRPC status error: {0}")]
    Status(Box<tonic::Status>),

    #[error("Authentication error: {0}")]
    Authentication(String),

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("Timeout error: {0}")]
    Timeout(String),

    #[error("Retry exhausted after {retries} attempts")]
    RetryExhausted { retries: usize },

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),
}
