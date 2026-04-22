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
//! Result-model types for the ARC state-test runner.
//!
//! This module owns the JSON-facing data structures emitted by the runner:
//! `TestResult` for aggregate stdout output, `JsonOutcome` for per-variant
//! execution metadata, and `TestSummary` / `RunStatus` for run-level reporting.
//!
//! Artifact mapping:
//! - `TestResult`: aggregate JSON written to stdout and consumed by
//!   `arc-execution-specs`
//! - `JsonOutcome`: embedded into `TestResult` and also emitted as per-variant
//!   lines in `per_test_outcomes.jsonl`
//! - `TestSummary`: CLI stderr summary text for the overall run
//! - `RunStatus`: process/report status used to derive command exit behavior
//!
//! The pytest HTML report is generated on the Python side and does not
//! serialize these Rust types directly.

use alloy_primitives::{Bytes, B256};
use serde::Serialize;
/// A single test result, serialized to JSON stdout.
#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    /// Consumer-facing fixture identity. This may be fixture-level for
    /// compatibility with existing consume-direct integrations.
    pub name: String,
    /// Stable unique identifier for a concrete test variant when available.
    ///
    /// The `variantId` string currently keeps the historical `d{}_g{}_v{}`
    /// suffix inherited from the upstream `revm` statetest runner shape
    /// (`bins/revme/src/cmd/statetest/runner.rs`). Structured JSON fields use
    /// the clearer `data_index` / `gas_index` / `value_index` names, while the
    /// per-test stderr artifact still carries legacy `d` / `g` / `v` aliases
    /// for compatibility with existing revm-style parsers.
    #[serde(rename = "variantId", skip_serializing_if = "Option::is_none")]
    pub variant_id: Option<String>,
    pub pass: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(rename = "stateRoot", skip_serializing_if = "Option::is_none")]
    pub state_root: Option<B256>,
    #[serde(rename = "logsRoot", skip_serializing_if = "Option::is_none")]
    pub logs_root: Option<B256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Bytes>,
    #[serde(rename = "gasUsed", skip_serializing_if = "Option::is_none")]
    pub gas_used: Option<u64>,
    #[serde(rename = "errorMsg", skip_serializing_if = "Option::is_none")]
    pub error_msg: Option<String>,
    #[serde(rename = "evmResult", skip_serializing_if = "Option::is_none")]
    pub evm_result: Option<String>,
    #[serde(rename = "postLogsHash", skip_serializing_if = "Option::is_none")]
    pub post_logs_hash: Option<B256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Index into fixture `transaction.data`.
    pub data_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Index into fixture `transaction.gasLimit`.
    pub gas_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Index into fixture `transaction.value`.
    pub value_index: Option<usize>,
}

impl TestResult {
    pub fn passed(name: String) -> Self {
        Self {
            name,
            variant_id: None,
            pass: true,
            error: String::new(),
            state_root: None,
            logs_root: None,
            output: None,
            gas_used: None,
            error_msg: None,
            evm_result: None,
            post_logs_hash: None,
            fork: None,
            test: None,
            data_index: None,
            gas_index: None,
            value_index: None,
        }
    }

    pub fn failed(name: String, error: String) -> Self {
        Self {
            name,
            variant_id: None,
            pass: false,
            error,
            state_root: None,
            logs_root: None,
            output: None,
            gas_used: None,
            error_msg: None,
            evm_result: None,
            post_logs_hash: None,
            fork: None,
            test: None,
            data_index: None,
            gas_index: None,
            value_index: None,
        }
    }

    pub fn with_json_outcome(mut self, outcome: JsonOutcome) -> Self {
        self.state_root = Some(outcome.state_root);
        self.logs_root = Some(outcome.logs_root);
        self.output = Some(outcome.output);
        self.gas_used = Some(outcome.gas_used);
        self.error_msg = Some(outcome.error_msg);
        self.evm_result = Some(outcome.evm_result);
        self.post_logs_hash = Some(outcome.post_logs_hash);
        self.fork = Some(outcome.fork);
        self.test = Some(outcome.test);
        self.data_index = Some(outcome.data_index);
        self.gas_index = Some(outcome.gas_index);
        self.value_index = Some(outcome.value_index);
        self
    }

    pub fn with_variant_id(mut self, variant_id: impl Into<String>) -> Self {
        self.variant_id = Some(variant_id.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct JsonOutcome {
    pub state_root: B256,
    pub logs_root: B256,
    pub output: Bytes,
    pub gas_used: u64,
    pub error_msg: String,
    pub evm_result: String,
    pub post_logs_hash: B256,
    pub fork: String,
    pub test: String,
    /// Index into fixture `transaction.data`.
    pub data_index: usize,
    /// Index into fixture `transaction.gasLimit`.
    pub gas_index: usize,
    /// Index into fixture `transaction.value`.
    pub value_index: usize,
}

/// Summary printed to stderr after all tests.
#[derive(Debug, Default)]
pub struct TestSummary {
    pub files_processed: usize,
    pub tests_total: usize,
    pub tests_passed: usize,
    pub tests_failed: usize,
    pub tests_skipped_by_spec: usize,
}

impl std::fmt::Display for TestSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "--- arc-evm-specs-tests summary ---")?;
        writeln!(f, "files processed:     {}", self.files_processed)?;
        writeln!(f, "tests total:         {}", self.tests_total)?;
        writeln!(f, "tests passed:        {}", self.tests_passed)?;
        writeln!(f, "tests failed:        {}", self.tests_failed)?;
        writeln!(f, "tests skipped (spec): {}", self.tests_skipped_by_spec)
    }
}

impl TestSummary {
    pub fn add_files_processed(&mut self, delta: usize) {
        self.files_processed = self
            .files_processed
            .checked_add(delta)
            .expect("files_processed overflow");
    }

    pub fn add_tests_total(&mut self, delta: usize) {
        self.tests_total = self
            .tests_total
            .checked_add(delta)
            .expect("tests_total overflow");
    }

    pub fn add_tests_passed(&mut self, delta: usize) {
        self.tests_passed = self
            .tests_passed
            .checked_add(delta)
            .expect("tests_passed overflow");
    }

    pub fn add_tests_failed(&mut self, delta: usize) {
        self.tests_failed = self
            .tests_failed
            .checked_add(delta)
            .expect("tests_failed overflow");
    }

    pub fn add_tests_skipped_by_spec(&mut self, delta: usize) {
        self.tests_skipped_by_spec = self
            .tests_skipped_by_spec
            .checked_add(delta)
            .expect("tests_skipped_by_spec overflow");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Success = 0,
    TestsFailed = 1,
    FatalFileErrors = 2,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{bytes, B256};

    #[test]
    fn passed_result_has_empty_error() {
        let result = TestResult::passed("fixture/Prague/d0_g0_v0".to_string());

        assert!(result.pass);
        assert_eq!(result.name, "fixture/Prague/d0_g0_v0");
        assert!(result.error.is_empty());
    }

    #[test]
    fn failed_result_preserves_error() {
        let result = TestResult::failed("fixture".to_string(), "boom".to_string());

        assert!(!result.pass);
        assert_eq!(result.name, "fixture");
        assert_eq!(result.error, "boom");
    }

    #[test]
    fn json_outcome_fields_serialize_only_when_present() {
        let result = TestResult::passed("fixture".to_string())
            .with_variant_id("fixture/Berlin/d0_g0_v0")
            .with_json_outcome(JsonOutcome {
                state_root: B256::ZERO,
                logs_root: B256::ZERO,
                output: bytes!("01"),
                gas_used: 21000,
                error_msg: String::new(),
                evm_result: "Success: Stop".to_string(),
                post_logs_hash: B256::ZERO,
                fork: "BERLIN".to_string(),
                test: "fixture/Berlin/d0_g0_v0".to_string(),
                data_index: 0,
                gas_index: 0,
                value_index: 0,
            });

        let json = serde_json::to_value(result).unwrap();
        assert_eq!(json.get("name").unwrap(), "fixture");
        assert_eq!(json.get("variantId").unwrap(), "fixture/Berlin/d0_g0_v0");
        assert_eq!(json.get("gasUsed").unwrap(), 21000);
        assert_eq!(json.get("fork").unwrap(), "BERLIN");
        assert!(json.get("stateRoot").is_some());
        assert_eq!(json.get("data_index").unwrap(), 0);
        assert_eq!(json.get("gas_index").unwrap(), 0);
        assert_eq!(json.get("value_index").unwrap(), 0);
    }

    #[test]
    fn summary_display_includes_all_counts() {
        let summary = TestSummary {
            files_processed: 2,
            tests_total: 7,
            tests_passed: 5,
            tests_failed: 1,
            tests_skipped_by_spec: 1,
        };

        let rendered = summary.to_string();

        assert!(rendered.contains("--- arc-evm-specs-tests summary ---"));
        assert!(rendered.contains("files processed:     2"));
        assert!(rendered.contains("tests total:         7"));
        assert!(rendered.contains("tests passed:        5"));
        assert!(rendered.contains("tests failed:        1"));
        assert!(rendered.contains("tests skipped (spec): 1"));
    }

    #[test]
    fn summary_adders_accumulate_counts() {
        let mut summary = TestSummary::default();

        summary.add_files_processed(2);
        summary.add_tests_total(7);
        summary.add_tests_passed(5);
        summary.add_tests_failed(1);
        summary.add_tests_skipped_by_spec(1);

        assert_eq!(summary.files_processed, 2);
        assert_eq!(summary.tests_total, 7);
        assert_eq!(summary.tests_passed, 5);
        assert_eq!(summary.tests_failed, 1);
        assert_eq!(summary.tests_skipped_by_spec, 1);
    }

    #[test]
    fn run_status_exit_codes_are_stable() {
        assert_eq!(RunStatus::Success as i32, 0);
        assert_eq!(RunStatus::TestsFailed as i32, 1);
        assert_eq!(RunStatus::FatalFileErrors as i32, 2);
    }
}
