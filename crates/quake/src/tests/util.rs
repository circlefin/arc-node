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

//! Utility functions for the test framework

use color_eyre::eyre::{bail, Result};
use futures_util::future::join_all;
use regex::Regex;
use std::collections::HashMap;
use std::future::Future;

use super::{RpcClientFactory, TestRegistry};

/// Execute an RPC operation on all nodes in parallel
///
/// This helper abstracts the common pattern of spawning parallel tasks to check
/// all nodes and collecting their results. It takes a closure that performs the
/// actual RPC operation for each node.
///
/// # Returns
/// A vector of tuples containing (node_name, url, result) for each node.
pub(crate) async fn in_parallel<F, Fut, T>(
    node_urls: &[(String, reqwest::Url)],
    factory: &RpcClientFactory,
    check_fn: F,
) -> Vec<(String, reqwest::Url, Result<T>)>
where
    F: Fn(crate::rpc::RpcClient) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let futures = node_urls.iter().map(|(name, url)| {
        let name = name.clone();
        let url = url.clone();
        let client = factory.create(url.clone());
        let future = check_fn(client);
        async move { (name, url, future.await) }
    });

    join_all(futures).await
}

/// Converts a glob pattern to a regular expression pattern.
///
/// Supports:
/// - `*` matches zero or more characters
/// - `?` matches exactly one character
/// - All regex special characters are escaped
///
/// # Examples
/// ```ignore
/// assert_eq!(glob_to_regex("foo*"), "^foo.*$");
/// assert_eq!(glob_to_regex("ba?.rs"), "^ba.\\.rs$");
/// ```
pub(crate) fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' | '+' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' => {
                regex.push('\\');
                regex.push(ch);
            }
            _ => regex.push(ch),
        }
    }
    regex.push('$');
    regex
}

/// Match test specifications against available tests using glob patterns.
/// Returns a map of group_name -> Vec<test_name>
pub(crate) fn match_test_specs(
    registry: &TestRegistry,
    group_pattern: &str,
    test_patterns: Option<Vec<String>>,
) -> Result<HashMap<String, Vec<String>>> {
    let group_regex = Regex::new(&glob_to_regex(group_pattern))
        .map_err(|e| color_eyre::eyre::eyre!("Invalid group pattern '{}': {}", group_pattern, e))?;

    let mut matched_tests: HashMap<String, Vec<String>> = HashMap::new();

    // Find all matching groups
    for group_name in registry.list_groups() {
        if !group_regex.is_match(group_name) {
            continue;
        }

        let group = registry
            .get_group(group_name)
            .expect("group exists in registry");

        // If no test patterns specified, include all tests in this group
        let tests = if let Some(ref patterns) = test_patterns {
            let mut matched = Vec::new();

            // Compile all test patterns to regex
            let test_regexes: Result<Vec<Regex>> = patterns
                .iter()
                .map(|p| {
                    Regex::new(&glob_to_regex(p))
                        .map_err(|e| color_eyre::eyre::eyre!("Invalid test pattern '{}': {}", p, e))
                })
                .collect();
            let test_regexes = test_regexes?;

            // Match each test against all patterns
            for test_name in group.tests.keys() {
                if test_regexes.iter().any(|re| re.is_match(test_name)) {
                    matched.push(test_name.clone());
                }
            }

            // Skip groups with no matches when using wildcards
            if matched.is_empty() {
                continue;
            }

            matched
        } else {
            group.tests.keys().cloned().collect()
        };

        matched_tests.insert(group_name.to_string(), tests);
    }

    if matched_tests.is_empty() {
        bail!(
            "No tests matched pattern '{}:{}'. Available groups: {:?}",
            group_pattern,
            test_patterns
                .as_ref()
                .map(|v| v.join(","))
                .unwrap_or_else(|| "*".to_string()),
            registry.list_groups()
        );
    }

    Ok(matched_tests)
}
