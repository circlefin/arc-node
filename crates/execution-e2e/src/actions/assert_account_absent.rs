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

//! Assertion that an address is absent from state (EIP-161 empty-account clearing).

use crate::{action::Action, ArcEnvironment};
use alloy_primitives::Address;
use futures_util::future::BoxFuture;
use reth_provider::{AccountReader, StateProviderFactory};
use tracing::info;

/// Asserts that `address` is absent from the latest state.
///
/// Passes when `basic_account` returns `None` (the account is not in the state
/// trie); fails when it returns `Some(_)`. This pins EIP-161 clearing: a
/// touched-but-empty account must be removed, not persisted as an empty entry.
/// Reads of `balance`/`nonce`/`code` cannot distinguish a cleared account from a
/// persisted empty one — all read zero either way — so this checks trie
/// membership directly through the state provider.
#[derive(Debug)]
pub struct AssertAccountAbsent {
    address: Address,
}

impl AssertAccountAbsent {
    /// Creates a new account-absence assertion.
    pub fn new(address: Address) -> Self {
        Self { address }
    }
}

impl Action for AssertAccountAbsent {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let state = env.node().inner.provider().latest()?;
            let account = state.basic_account(&self.address)?;

            info!(
                address = %self.address,
                present = account.is_some(),
                "Asserting account absent from state"
            );

            if let Some(account) = account {
                return Err(eyre::eyre!(
                    "Account {} is present in state but expected absent (EIP-161 clearing): {account:?}",
                    self.address,
                ));
            }
            Ok(())
        })
    }
}
