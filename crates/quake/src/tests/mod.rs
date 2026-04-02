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

//! Test framework for running validation tests against a live testnet.
//!
//! This module provides infrastructure for organizing and executing tests against
//! a running quake testnet. Tests are organized into groups, where each group
//! contains related tests.
//!
//! # Architecture
//!
//! - **TestGroup**: A collection of related tests (e.g., "probe", "net")
//! - **TestFn**: An async function that takes a `&Testnet` reference and validates some aspect
//! - **TestRegistry**: Central registry that manages all test groups
//!
//! # Adding New Tests
//!
//! Tests are automatically registered using the `#[quake_test]` macro:
//! 1. Create a new file in `tests/` (e.g., `foobar.rs`) or add to existing files
//! 2. Add `mod foobar;` to this file if creating a new module
//! 3. Define test functions with the `#[quake_test]` attribute
//!
//! # Example
//!
//! ```rust,ignore
//! use tracing::debug;
//! use super::{quake_test, in_parallel, CheckResult, RpcClientFactory, TestOutcome, TestResult};
//! use crate::testnet::Testnet;
//!
//! // tests/foobar.rs
//! #[quake_test(group = "foobar", name = "something")]
//! fn something_test<'a>(
//!     testnet: &'a Testnet,
//!     factory: &'a RpcClientFactory,
//!     _params: &'a TestParams,
//! ) -> TestResult<'a> {
//!     Box::pin(async move {
//!         debug!("Running test...");
//!
//!         // Use in_parallel helper for concurrent RPC operations
//!         let node_urls = testnet.nodes_metadata.all_execution_urls();
//!         let results = in_parallel(&node_urls, factory, |client| async move {
//!             client.get_block_number().await
//!         }).await;
//!
//!         // Use structured test results
//!         let mut outcome = TestOutcome::new();
//!         for (name, url, result) in results {
//!             match result {
//!                 Ok(block) => outcome.add_check(CheckResult::success(name, format!("{} at #{}", url, block))),
//!                 Err(e) => outcome.add_check(CheckResult::failure(name, format!("{} - {}", url, e))),
//!             }
//!         }
//!
//!         outcome.with_summary("Test complete").into_result()
//!     })
//! }
//! ```
//!
//! # Usage
//!
//! Tests are executed via the `quake test` command with glob pattern support:
//!
//! ## Basic Usage
//! - `quake test` - Run all tests except excluded groups (`validation`, `health`)
//! - `quake test probe` - Run all tests in the probe group
//! - `quake test probe:connectivity` - Run a single test
//! - `quake test probe:connectivity,sync` - Run multiple specific tests
//! - `quake test validation:basic` - Run excluded groups explicitly
//! - `quake test health:stability` - Run excluded groups explicitly
//! - `quake test --dry-run` - List all available test groups and tests
//! - `quake test probe --dry-run` - List tests in a specific group
//!
//! ## Glob Pattern Matching
//! Patterns support `*` (any characters) and `?` (single character). Quote to prevent shell expansion.
//! - `quake test 'n*'` - Run tests in groups starting with 'n'
//! - `quake test '*:sync'` - Run all tests named 'sync' in any group
//! - `quake test 'probe:conn*'` - Run probe tests starting with 'conn'
//! - `quake test 'n*:*peer*'` - Run tests containing 'peer' in groups starting with 'n'
//! - `quake test '*:*'` - Run all tests including excluded groups
//!
//! ## Options
//! - `quake test --rpc-timeout 10s` - Run with custom RPC timeout
//! - `quake test --dry-run` - List tests without running them

use color_eyre::eyre::{bail, Result};

use crate::testnet::Testnet;

// Re-export the macro for use in test modules
pub use quake_macros::quake_test;

// Type definitions
mod types;
pub(crate) use types::{
    CheckResult, RpcClientFactory, TestGroup, TestOutcome, TestParams, TestRegistration,
    TestRegistry, TestResult,
};

// Utility functions
mod util;
pub(crate) use util::{in_parallel, match_test_specs};

// Reusable RPC query helpers
pub(crate) mod historical_queries;

// Snapshot creation and restoration helpers
pub(crate) mod snapshot;

// Test modules - must come after type definitions so they can use them
mod arc_node;
mod health;
mod mempool;
mod mesh;
mod mev;
mod net;
mod perf;
mod probe;
mod sanity;
mod tx;

/// List matched tests in a formatted way
fn list_matched_tests(matched_tests: &std::collections::HashMap<String, Vec<String>>) {
    if matched_tests.len() == 1 && matched_tests.values().next().unwrap().len() == 1 {
        // Single test - just list it directly
        let (group_name, test_names) = matched_tests.iter().next().unwrap();
        println!("{}:{}", group_name, test_names[0]);
    } else {
        // Multiple groups or tests - show hierarchical format
        let mut group_names: Vec<&String> = matched_tests.keys().collect();
        group_names.sort();

        for group_name in group_names {
            let mut test_names = matched_tests.get(group_name).unwrap().clone();
            test_names.sort();

            println!("\n{}:", group_name);
            for test_name in test_names {
                println!("  - {}", test_name);
            }
        }
    }
}

/// Run tests from a specific group and return (passed, failed) counts
async fn run_test_group(
    group_name: &str,
    tests_to_run: &[String],
    group: &TestGroup,
    testnet: &Testnet,
    factory: &RpcClientFactory,
    params: &TestParams,
) -> (usize, usize) {
    let mut passed = 0;
    let mut failed = 0;

    for test_name in tests_to_run {
        match group.tests.get(test_name) {
            Some(test_fn) => {
                println!("\nRunning test {}:{}", group_name, test_name);
                match test_fn(testnet, factory, params).await {
                    Ok(_) => {
                        println!("✓ Pass {}:{}", group_name, test_name);
                        passed += 1;
                    }
                    Err(e) => {
                        println!("✗ Fail {}:{} - {}", group_name, test_name, e);
                        failed += 1;
                    }
                }
            }
            None => {
                println!("✗ Not found {}:{}", group_name, test_name);
                failed += 1;
            }
        }
    }

    (passed, failed)
}

/// Execute or list tests from a specific group or specific tests.
/// Supports glob patterns for matching groups and tests.
///
/// If dry_run is true, just lists the tests that would be run.
///
/// Examples:
/// - "" - Run all tests
/// - "probe" - Run all tests in probe group
/// - "n*" - Run all tests in groups starting with 'n'
/// - "probe:sync" - Run specific test
/// - "probe:*peer*" - Run all tests containing 'peer' in probe group
/// - "n*:*peer*" - Run all tests containing 'peer' in groups starting with 'n'
pub(crate) async fn run_tests(
    testnet: &Testnet,
    spec: &str,
    dry_run: bool,
    rpc_timeout: tokio::time::Duration,
    params: &TestParams,
) -> Result<()> {
    let registry = TestRegistry::new();

    // Parse the test specification
    let (group_pattern, test_patterns) = if spec.is_empty() {
        // Empty spec means all tests
        ("*".to_string(), None)
    } else if spec.contains(':') {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() != 2 {
            bail!(
                "Test spec must be in format 'group' or 'group:test1,test2', got: '{}'",
                spec
            );
        }
        let group = parts[0].to_string();
        let tests: Vec<String> = parts[1]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        (group, Some(tests))
    } else {
        // Just a group pattern - run all tests in matching groups
        (spec.to_string(), None)
    };

    // Match test specifications using glob patterns
    let mut matched_tests = match_test_specs(&registry, &group_pattern, test_patterns)?;

    // Exclude flaky / strict groups from the default (empty spec) run.
    // - `validation`: generates load that leaves pending txs, elevated metrics
    // - `health`: assertions (e.g. sync_fell_behind == 0) are too strict for CI
    // Run explicitly: `quake test validation:basic`, `quake test health:stability`
    // NOTE: update module-level doc comments (Basic Usage / Glob Pattern) if changing exclusions.
    if spec.is_empty() {
        matched_tests.remove("validation");
        matched_tests.remove("health");
    }

    // If dry-run, just list the tests
    if dry_run {
        list_matched_tests(&matched_tests);
        return Ok(());
    }

    // Run all matched tests in sorted group order for deterministic execution.
    let factory = RpcClientFactory::new(rpc_timeout);
    let mut total_passed = 0;
    let mut total_failed = 0;

    let mut sorted_tests: Vec<_> = matched_tests.into_iter().collect();
    sorted_tests.sort_by(|a, b| a.0.cmp(&b.0));

    for (group_name, tests_to_run) in sorted_tests {
        let group = registry
            .get_group(&group_name)
            .expect("group validated by match_test_specs");
        let (passed, failed) =
            run_test_group(&group_name, &tests_to_run, group, testnet, &factory, params).await;
        total_passed += passed;
        total_failed += failed;
    }

    println!(
        "\nTest results: {} passed, {} failed",
        total_passed, total_failed
    );

    if total_failed > 0 {
        bail!("{} test(s) failed", total_failed)
    } else {
        Ok(())
    }
}
