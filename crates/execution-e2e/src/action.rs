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

//! Action trait and utilities for Arc e2e tests.

use crate::ArcEnvironment;
use futures_util::future::BoxFuture;

/// An action that can be executed on the Arc test environment.
///
/// Actions are the building blocks of test scenarios.
pub trait Action: Send + 'static {
    /// Executes the action on the given environment.
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>>;
}

/// Type-erased wrapper for actions, allowing storage in heterogeneous collections.
pub struct ActionBox(Box<dyn Action>);

impl ActionBox {
    /// Creates a new boxed action.
    pub fn new<A: Action>(action: A) -> Self {
        Self(Box::new(action))
    }

    /// Executes the wrapped action.
    pub async fn execute(&mut self, env: &mut ArcEnvironment) -> eyre::Result<()> {
        self.0.execute(env).await
    }
}
