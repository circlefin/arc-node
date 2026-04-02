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

#![allow(clippy::unwrap_used)]

//! Arc E2E Test Framework
//!
//! An e2e testing framework for Arc execution, inspired by reth's testsuite architecture.
//! Uses the Action pattern for composable test scenarios.

mod action;
pub mod actions;
pub mod chainspec;
mod environment;
mod setup;

pub use action::{Action, ActionBox};
pub use environment::{ArcEnvironment, BlockInfo};
pub use setup::ArcSetup;

/// Builder for creating and running Arc test scenarios.
///
/// Follows the builder pattern to compose tests from setup and actions.
pub struct ArcTestBuilder {
    setup: Option<ArcSetup>,
    actions: Vec<ActionBox>,
}

impl Default for ArcTestBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ArcTestBuilder {
    /// Creates a new test builder.
    pub fn new() -> Self {
        Self {
            setup: None,
            actions: Vec::new(),
        }
    }

    /// Sets the test setup configuration.
    pub fn with_setup(mut self, setup: ArcSetup) -> Self {
        self.setup = Some(setup);
        self
    }

    /// Adds an action to be executed.
    pub fn with_action<A: Action>(mut self, action: A) -> Self {
        self.actions.push(ActionBox::new(action));
        self
    }

    /// Runs the test scenario.
    ///
    /// 1. Applies the setup to create the node
    /// 2. Executes all actions in sequence
    pub async fn run(self) -> eyre::Result<()> {
        let mut env = ArcEnvironment::new();

        // Apply setup
        if let Some(setup) = self.setup {
            setup.apply(&mut env).await?;
        } else {
            return Err(eyre::eyre!("No setup configured"));
        }

        // Execute all actions
        for mut action in self.actions {
            action.execute(&mut env).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_without_setup_returns_error() {
        let result = ArcTestBuilder::new().run().await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("No setup configured"));
    }
}
