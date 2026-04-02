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

use std::fmt;

use crate::rpc::json_structs::JsonError;
use eyre::Report;
use jsonrpsee_types::error::ErrorObject;
use thiserror::Error;

/// Error codes taken from reth's code.
/// See https://github.com/paradigmxyz/reth/blob/7345e1e5b5b88e53c4b3f3152078653507d0d26f/crates/rpc/rpc-engine-api/src/error.rs
///
/// Code used by reth for EngineApiError::UnknownPayload.
const UNKNOWN_PAYLOAD_CODE: i32 = -38001;

/// Code used by reth for EngineApiError::UnsupportedFork.
const UNSUPPORTED_FORK_CODE: i32 = -38005;

/// Represents an error returned by the Engine API RPC.
/// Mirrors the structure of the error that reth returns.
#[derive(Debug, Clone, Error)]
pub struct EngineApiRpcError {
    code: i32,
    message: String,
    // To create this field, reth wraps the Display output of the JSON-RPC error
    // inside a struct, converts that struct into a String, and then inserts that
    // String into the JSON-RPC error's `data` field.
    // As a result, whatever ends up in the `data` field can be retrieved as a plain
    // String.
    data: Option<String>,
}

impl EngineApiRpcError {
    /// Creates a new `EngineApiRpcError`.
    pub fn new(code: i32, message: &str, data: Option<&str>) -> Self {
        Self {
            code,
            message: message.into(),
            data: data.map(|d| d.into()),
        }
    }

    /// Classifies the error into a specific kind.
    pub fn kind(&self) -> EngineRpcErrorKind {
        match self.code {
            UNKNOWN_PAYLOAD_CODE => EngineRpcErrorKind::UnknownPayload,
            UNSUPPORTED_FORK_CODE => EngineRpcErrorKind::UnsupportedFork,
            _ => EngineRpcErrorKind::Other,
        }
    }

    /// Checks if the error is of kind `UnknownPayload`.
    pub fn is_unknown_payload(&self) -> bool {
        self.kind() == EngineRpcErrorKind::UnknownPayload
    }

    /// Checks if the error is of kind `UnsupportedFork`.
    pub fn is_unsupported_fork(&self) -> bool {
        self.kind() == EngineRpcErrorKind::UnsupportedFork
    }
}

impl fmt::Display for EngineApiRpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.data {
            Some(data) => write!(f, "Code {}: {}: {}", self.code, self.message, data),
            None => write!(f, "Code {}: {}", self.code, self.message),
        }
    }
}

/// Convert a `JsonError` object to our engine api error type.
impl From<JsonError> for EngineApiRpcError {
    fn from(err: JsonError) -> Self {
        Self {
            code: err.code,
            message: err.message,
            data: err.data.map(|d| d.to_string()),
        }
    }
}

/// Convert a jsonrpsee `ErrorObject` into our Engine api error type.
impl From<ErrorObject<'_>> for EngineApiRpcError {
    fn from(err: ErrorObject<'_>) -> Self {
        Self {
            code: err.code(),
            message: err.message().to_owned(),
            data: err.data().map(|d| d.get().to_owned()),
        }
    }
}

/// Converts an `eyre::Report` into an `EngineApiRpcError` if possible.
impl TryFrom<&Report> for EngineApiRpcError {
    type Error = String;
    fn try_from(err: &Report) -> Result<Self, Self::Error> {
        for cause in err.chain() {
            if let Some(engine_api_error) = cause.downcast_ref::<Self>() {
                return Ok(engine_api_error.clone());
            }
        }
        Err("error object isn't of type EngineApiRpcError".to_string())
    }
}

/// Converts an `eyre::Report` into an `EngineApiRpcError` if possible.
impl TryFrom<Report> for EngineApiRpcError {
    type Error = String;
    fn try_from(err: Report) -> Result<Self, Self::Error> {
        EngineApiRpcError::try_from(&err)
    }
}

/// Classification of Engine API JSON-RPC errors we want to match specifically.
/// Can easily be extended with additional kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineRpcErrorKind {
    /// reth thinks that the payload does not exist or is not available locally.
    UnknownPayload,
    /// reth does not support the requested Engine API version for the current fork.
    UnsupportedFork,
    /// Any JSON-RPC error we are not interested in.
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use eyre::{eyre, Report};
    use serde_json::json;

    #[test]
    fn display_includes_data() {
        let err = EngineApiRpcError::new(42, "some error", Some("root cause"));
        assert_eq!(err.to_string(), "Code 42: some error: root cause");
    }

    #[test]
    fn display_omits_data() {
        let err = EngineApiRpcError::new(42, "some error", None);
        assert_eq!(err.to_string(), "Code 42: some error");
    }

    #[test]
    fn unknown_payload_error() {
        let unknown = EngineApiRpcError::new(UNKNOWN_PAYLOAD_CODE, "Unknown payload", None);
        assert_eq!(unknown.kind(), EngineRpcErrorKind::UnknownPayload);
        assert!(unknown.is_unknown_payload());

        let other = EngineApiRpcError::new(-32603, "Internal error", None);
        assert_eq!(other.kind(), EngineRpcErrorKind::Other);
        assert!(!other.is_unknown_payload());
    }

    #[test]
    fn try_from_report_ok() {
        let original = EngineApiRpcError::new(42, "some error", Some("root cause"));
        let report = Report::new(original.clone());

        let extracted = EngineApiRpcError::try_from(&report).expect("should be EngineApiRpcError");
        assert_eq!(extracted.to_string(), original.to_string());
    }

    #[test]
    fn try_from_report_err() {
        let report = eyre!("some other error");
        assert!(EngineApiRpcError::try_from(&report).is_err());
    }

    #[test]
    fn from_json_error() {
        let json_err = JsonError {
            code: 42,
            message: "some error".to_string(),
            data: Some(json!("root cause")),
        };

        let err = EngineApiRpcError::from(json_err);
        assert_eq!(err.to_string(), "Code 42: some error: \"root cause\"");
        assert_eq!(err.kind(), EngineRpcErrorKind::Other);
    }

    #[test]
    fn from_error_object() {
        use jsonrpsee_types::error::ErrorObjectOwned;

        let owned = ErrorObjectOwned::owned(42, "some error", Some("root cause"));
        let err = EngineApiRpcError::from(owned);

        assert_eq!(err.to_string(), "Code 42: some error: \"root cause\"");
        assert_eq!(err.kind(), EngineRpcErrorKind::Other);
    }
}
