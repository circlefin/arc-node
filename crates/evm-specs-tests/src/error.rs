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

use thiserror::Error;

/// Errors that can occur during arc-evm-specs-tests execution.
#[derive(Debug, Error)]
pub enum EvmSpecsTestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {path}: {source}")]
    JsonParse {
        path: String,
        source: serde_json::Error,
    },

    #[error("Test error in '{name}': {kind}")]
    TestFailure { name: String, kind: TestErrorKind },
    #[error("No JSON fixture files found at: {path}")]
    NoJsonFiles { path: String },
    #[error("Missing chain_id for test '{test_name}': neither config.chainid nor env.current_chain_id is available")]
    MissingChainId { test_name: String },
    #[error("Malformed config.chainid for test '{test_name}': '{raw_value}' (expected decimal like '1' or hex like '0x1')")]
    MalformedChainId {
        test_name: String,
        raw_value: String,
    },
    #[error("Runner queue mutex poisoned")]
    RunnerQueuePoisoned,
    #[error("Failed to spawn runner worker thread: {0}")]
    WorkerSpawn(std::io::Error),
    #[error("Runner worker thread panicked")]
    WorkerPanic,
}

/// Specific kinds of test failures.
#[derive(Debug, Error)]
pub enum TestErrorKind {
    #[error("EVM execution error: error_class={error_class}; error_kind={error_kind}; {detail}")]
    EvmError {
        error_class: &'static str,
        error_kind: &'static str,
        detail: String,
    },
}

impl TestErrorKind {
    pub fn evm(
        error_class: &'static str,
        error_kind: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self::EvmError {
            error_class,
            error_kind,
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evm_error_kind_formats_with_class_kind_and_detail() {
        let err = TestErrorKind::evm("EXECUTION_MISMATCH", "WRONG_EXCEPTION", "details");

        assert_eq!(
            err.to_string(),
            "EVM execution error: error_class=EXECUTION_MISMATCH; error_kind=WRONG_EXCEPTION; details"
        );
    }

    #[test]
    fn top_level_error_wraps_test_failure_context() {
        let err = EvmSpecsTestError::TestFailure {
            name: "fixture/Prague/d0_g0_v0".to_string(),
            kind: TestErrorKind::evm("EXECUTION_MISMATCH", "STATE_ROOT_MISMATCH", "nope"),
        };

        let rendered = err.to_string();
        assert!(rendered.contains("Test error in 'fixture/Prague/d0_g0_v0'"));
        assert!(rendered.contains("error_class=EXECUTION_MISMATCH"));
        assert!(rendered.contains("error_kind=STATE_ROOT_MISMATCH"));
    }

    #[test]
    fn missing_chain_id_error_mentions_expected_sources() {
        let err = EvmSpecsTestError::MissingChainId {
            test_name: "fixture".to_string(),
        };

        assert_eq!(
            err.to_string(),
            "Missing chain_id for test 'fixture': neither config.chainid nor env.current_chain_id is available"
        );
    }

    #[test]
    fn error_variants_render_expected_messages() {
        let io_err = EvmSpecsTestError::Io(std::io::Error::other("disk"));
        assert!(io_err.to_string().contains("IO error: disk"));

        let json_err = EvmSpecsTestError::JsonParse {
            path: "fixture.json".to_string(),
            source: serde_json::from_str::<serde_json::Value>("{").expect_err("invalid json"),
        };
        assert!(json_err
            .to_string()
            .contains("JSON parse error: fixture.json"));

        let malformed = EvmSpecsTestError::MalformedChainId {
            test_name: "fixture".to_string(),
            raw_value: "xyz".to_string(),
        };
        assert!(malformed.to_string().contains("Malformed config.chainid"));

        assert_eq!(
            EvmSpecsTestError::RunnerQueuePoisoned.to_string(),
            "Runner queue mutex poisoned"
        );
        assert_eq!(
            EvmSpecsTestError::WorkerPanic.to_string(),
            "Runner worker thread panicked"
        );

        let spawn = EvmSpecsTestError::WorkerSpawn(std::io::Error::other("thread"));
        assert!(spawn
            .to_string()
            .contains("Failed to spawn runner worker thread: thread"));
    }
}
