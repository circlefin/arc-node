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

//! Type definitions for the test framework

use color_eyre::eyre::{bail, Result};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use crate::testnet::Testnet;

/// Result of an individual check within a test
#[derive(Debug)]
pub(crate) struct CheckResult {
    pub name: String,
    pub success: bool,
    pub message: String,
}

impl CheckResult {
    pub fn success(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            success: true,
            message: message.into(),
        }
    }

    pub fn failure(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            success: false,
            message: message.into(),
        }
    }
}

impl From<arc_checks::CheckResult> for CheckResult {
    fn from(c: arc_checks::CheckResult) -> Self {
        Self {
            name: c.name,
            success: c.passed,
            message: c.message,
        }
    }
}

/// Structured test result with detailed check information
#[derive(Debug, Default)]
pub(crate) struct TestOutcome {
    pub checks: Vec<CheckResult>,
    pub summary: Option<String>,
}

impl TestOutcome {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_check(&mut self, check: CheckResult) {
        self.checks.push(check);
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn is_success(&self) -> bool {
        self.checks.iter().all(|c| c.success)
    }

    pub fn failed_count(&self) -> usize {
        self.checks.iter().filter(|c| !c.success).count()
    }

    pub fn print(&self) {
        for check in &self.checks {
            if check.success {
                println!("\x1b[32m✓ {}: {}\x1b[0m", check.name, check.message);
            } else {
                println!("\x1b[31m✗ {}: {}\x1b[0m", check.name, check.message);
            }
        }

        if let Some(summary) = &self.summary {
            if self.is_success() {
                println!("\x1b[1m\x1b[32m✓ {}\x1b[0m", summary);
            } else {
                println!("\x1b[1m\x1b[31m✗ {}\x1b[0m", summary);
            }
        }
    }

    pub fn into_result(self) -> Result<()> {
        self.print();

        if self.is_success() {
            Ok(())
        } else {
            let failed_count = self.failed_count();
            let msg = self
                .summary
                .unwrap_or_else(|| format!("{} check(s) failed", failed_count));
            bail!("{}", msg)
        }
    }

    /// Generate a summary based on success/failure counts.
    ///
    /// This helper reduces duplication in test implementations by providing
    /// a standard pattern for success/failure messages.
    ///
    /// # Arguments
    /// * `success_msg` - Message to use when all checks pass
    /// * `failure_template` - Template string for failure message containing `{}` for failed count
    ///
    /// # Example
    /// ```ignore
    /// outcome.auto_summary("All tests passed", "{} test(s) failed")
    /// ```
    pub fn auto_summary(self, success_msg: &str, failure_template: &str) -> Self {
        let summary = if self.is_success() {
            success_msg.to_string()
        } else {
            failure_template.replace("{}", &self.failed_count().to_string())
        };
        self.with_summary(summary)
    }
}

/// Factory for creating RPC clients with consistent configuration.
///
/// This ensures all test RPC operations use the same timeout settings,
/// preventing inconsistent behavior across different test cases.
#[derive(Clone)]
pub(crate) struct RpcClientFactory {
    timeout: tokio::time::Duration,
}

impl RpcClientFactory {
    pub fn new(timeout: tokio::time::Duration) -> Self {
        Self { timeout }
    }

    /// Create an RPC client for the given URL with the factory's timeout
    pub fn create(&self, url: reqwest::Url) -> crate::rpc::RpcClient {
        crate::rpc::RpcClient::new(url, self.timeout)
    }
}

/// Optional parameters passed from the CLI to tests via `--set key=value`.
#[derive(Debug, Default)]
pub struct TestParams {
    inner: HashMap<String, String>,
}

impl TestParams {
    /// Look up a parameter by key.
    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(|s| s.as_str())
    }

    /// Look up a parameter, returning a default if not set.
    pub fn get_or(&self, key: &str, default: &str) -> String {
        self.inner
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }
}

impl From<Vec<(String, String)>> for TestParams {
    fn from(pairs: Vec<(String, String)>) -> Self {
        Self {
            inner: pairs.into_iter().collect(),
        }
    }
}

/// A test function type.
///
/// Tests are async functions that take a `&Testnet`, `&RpcClientFactory`, and `&TestParams`
/// reference and return a pinned boxed future that resolves to `Result<()>`.
///
/// Test description: Description of what the test validates
pub(crate) type TestResult<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// A test function that can be executed against a testnet.
///
/// This is the manual equivalent of `async fn(&Testnet, &RpcClientFactory, &TestParams) -> Result<()>`.
/// We can't use `async fn` directly because function pointers can't be async,
/// so tests return a pinned boxed future instead via `Box::pin(async move { ... })`.
///
/// The `'_` lifetime ties the future to the references, preventing
/// the future from outliving the data it borrows.
///
/// The factory parameter allows tests to create RPC clients with a consistent timeout.
/// The params parameter carries optional CLI arguments for scenario-specific tests.
pub(crate) type TestFn =
    for<'a> fn(&'a Testnet, &'a RpcClientFactory, &'a TestParams) -> TestResult<'a>;

/// Test registration submitted via the inventory system.
/// This is used by the #[quake_test] macro to automatically register tests.
pub(crate) struct TestRegistration {
    pub(crate) group: &'static str,
    pub(crate) name: &'static str,
    pub(crate) test_fn: TestFn,
    pub(crate) disabled: bool,
}

inventory::collect!(TestRegistration);

/// A test group containing related tests
pub(crate) struct TestGroup {
    pub tests: HashMap<String, TestFn>,
}

impl TestGroup {
    pub fn new() -> Self {
        Self {
            tests: HashMap::new(),
        }
    }

    pub fn add_test(&mut self, name: &str, test_fn: TestFn) {
        self.tests.insert(name.to_string(), test_fn);
    }
}

/// Registry of all test groups
pub(crate) struct TestRegistry {
    groups: HashMap<String, TestGroup>,
}

impl TestRegistry {
    pub fn new() -> Self {
        let mut groups: HashMap<String, TestGroup> = HashMap::new();

        // Collect all test registrations from the inventory
        for registration in inventory::iter::<TestRegistration> {
            // Skip disabled tests
            if registration.disabled {
                continue;
            }

            let group = groups
                .entry(registration.group.to_string())
                .or_insert_with(TestGroup::new);

            group.add_test(registration.name, registration.test_fn);
        }

        Self { groups }
    }

    pub fn get_group(&self, name: &str) -> Option<&TestGroup> {
        self.groups.get(name)
    }

    /// Returns an iterator over group names.
    ///
    /// Prefer this over `list_groups()` when you don't need ownership
    /// to avoid unnecessary allocation.
    #[allow(dead_code)]
    pub fn groups(&self) -> impl Iterator<Item = &str> {
        self.groups.keys().map(|s| s.as_str())
    }

    /// Returns a vector of group names.
    ///
    /// Use `groups()` iterator if you don't need to collect into a Vec.
    pub fn list_groups(&self) -> Vec<&String> {
        self.groups.keys().collect()
    }
}

impl Default for TestRegistry {
    fn default() -> Self {
        Self::new()
    }
}
