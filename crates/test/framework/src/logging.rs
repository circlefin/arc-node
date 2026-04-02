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

//! Tracing/logging setup for integration tests.

use std::sync::Once;

use tracing_subscriber::EnvFilter;

static INIT: Once = Once::new();

/// Initialize tracing for tests. Safe to call multiple times; only the first call takes effect.
///
/// Log levels can be tuned via environment variables:
/// ```sh
/// # Enable debug-level output for the test framework
/// RUST_LOG=arc_test_framework=debug cargo test ...
///
/// # Or enable debug for all arc crates
/// RUST_LOG=debug cargo test ... -- --nocapture
/// ```
pub fn init_logging() {
    INIT.call_once(|| {
        let filter = match EnvFilter::try_from_default_env() {
            Ok(filter) => filter,
            Err(e) => {
                eprintln!(
                    "arc-test-framework: invalid RUST_LOG value ({e}); falling back to default INFO filter"
                );
                EnvFilter::new("info")
            }
        };

        if let Err(e) = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .with_test_writer()
            .try_init()
        {
            eprintln!("arc-test-framework: tracing subscriber already set ({e}); skipping");
        }
    });
}
