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

//! Mock node handle for test files, used to test the **framework itself**, not
//! real Arc nodes.
//!
//! The `NodeRunner` implementations themselves live next to the tests that use
//! them (see `tests/`).

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::events::ArcEvent;
use crate::NodeHandle;

/// Handle to a mock node backed by a broadcast channel and an optional background task.
pub struct MockNodeHandle {
    tx: broadcast::Sender<ArcEvent>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl MockNodeHandle {
    /// Create a handle from a broadcast sender and an optional background task.
    pub fn new(tx: broadcast::Sender<ArcEvent>, task: Option<tokio::task::JoinHandle<()>>) -> Self {
        Self { tx, task }
    }

    fn abort_task(&self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Fulfils the [`NodeHandle`] drop contract by aborting the background task.
impl Drop for MockNodeHandle {
    fn drop(&mut self) {
        self.abort_task();
    }
}

#[async_trait]
impl NodeHandle for MockNodeHandle {
    fn subscribe(&self) -> broadcast::Receiver<ArcEvent> {
        self.tx.subscribe()
    }

    async fn kill_cl(&self) -> eyre::Result<()> {
        self.abort_task();
        Ok(())
    }

    async fn kill_el(&self) -> eyre::Result<()> {
        self.abort_task();
        Ok(())
    }
}
