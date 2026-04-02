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

//! Arc test environment for e2e tests.

use alloy_primitives::{BlockHash, TxHash};
use arc_evm_node::node::ArcNode;
use reth_e2e_test_utils::{wallet::Wallet, NodeHelperType};
use reth_node_builder::NodeTypesWithDBAdapter;
use reth_provider::providers::BlockchainProvider;
use std::collections::HashMap;

/// Type alias for the Arc node test context.
pub type ArcNodeTestContext = NodeHelperType<
    ArcNode,
    BlockchainProvider<NodeTypesWithDBAdapter<ArcNode, reth_e2e_test_utils::TmpDB>>,
>;

/// Information about a block.
#[derive(Debug, Clone)]
pub struct BlockInfo {
    /// Block hash.
    pub hash: BlockHash,
    /// Block number.
    pub number: u64,
    /// Block timestamp.
    pub timestamp: u64,
}

impl BlockInfo {
    /// Creates a new BlockInfo.
    pub fn new(hash: BlockHash, number: u64, timestamp: u64) -> Self {
        Self {
            hash,
            number,
            timestamp,
        }
    }
}

/// Arc test environment containing the node context and state.
pub struct ArcEnvironment {
    /// The single node test context.
    node: Option<ArcNodeTestContext>,
    /// Current block information.
    current_block: Option<BlockInfo>,
    /// Wallet for signing transactions in tests.
    wallet: Option<Wallet>,
    /// Named transaction hashes for test assertions.
    tx_hashes: HashMap<String, TxHash>,
}

impl Default for ArcEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

impl ArcEnvironment {
    /// Creates a new empty environment.
    pub fn new() -> Self {
        Self {
            node: None,
            current_block: None,
            wallet: None,
            tx_hashes: HashMap::new(),
        }
    }

    /// Sets the node context. Called by `ArcSetup::apply()`.
    pub(crate) fn set_node(&mut self, node: ArcNodeTestContext) {
        self.node = Some(node);
    }

    /// Sets the wallet. Called by `ArcSetup::apply()`.
    pub(crate) fn set_wallet(&mut self, wallet: Wallet) {
        self.wallet = Some(wallet);
    }

    /// Updates the current block info.
    pub fn set_current_block(&mut self, block: BlockInfo) {
        self.current_block = Some(block);
    }

    /// Returns a reference to the node
    pub fn node(&self) -> &ArcNodeTestContext {
        self.node.as_ref().expect("Node not initialized.")
    }

    /// Returns a mutable reference to the node
    pub fn node_mut(&mut self) -> &mut ArcNodeTestContext {
        self.node.as_mut().expect("Node not initialized")
    }

    /// Returns the current block info
    pub fn current_block(&self) -> &BlockInfo {
        self.current_block
            .as_ref()
            .expect("No current block available.")
    }

    /// Returns the current block number.
    pub fn block_number(&self) -> u64 {
        self.current_block().number
    }

    /// Returns a mutable reference to the wallet
    pub fn wallet_mut(&mut self) -> eyre::Result<&mut Wallet> {
        self.wallet
            .as_mut()
            .ok_or_else(|| eyre::eyre!("No wallet available in environment"))
    }

    /// Stores a transaction hash by name for later assertions.
    pub fn insert_tx_hash(&mut self, name: String, tx_hash: TxHash) -> eyre::Result<()> {
        if let Some(existing) = self.tx_hashes.get(&name) {
            return Err(eyre::eyre!(
                "Transaction name '{}' is already in use (existing tx_hash: {}). \
                 Each transaction must have a unique name.",
                name,
                existing
            ));
        }
        self.tx_hashes.insert(name, tx_hash);
        Ok(())
    }

    /// Gets a transaction hash by name.
    pub fn get_tx_hash(&self, name: &str) -> Option<&TxHash> {
        self.tx_hashes.get(name)
    }
}
