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

//! Setup configuration for Arc e2e tests.

use crate::environment::{ArcEnvironment, BlockInfo};
use arc_evm_node::node::ArcNode;
use arc_execution_config::addresses_denylist::AddressesDenylistConfig;
use arc_execution_config::chainspec::{ArcChainSpec, LOCAL_DEV};
use arc_execution_txpool::InvalidTxListConfig;
use reth_chainspec::EthChainSpec;
use reth_e2e_test_utils::NodeHelperType;
use reth_ethereum_engine_primitives::EthPayloadBuilderAttributes;
use reth_node_builder::{EngineNodeLauncher, Node, NodeBuilder, NodeConfig, NodeHandle};
use reth_node_core::args::{DiscoveryArgs, NetworkArgs, RpcServerArgs};
use reth_provider::providers::BlockchainProvider;
use reth_provider::HeaderProvider;
use reth_rpc_server_types::RpcModuleSelection;
use reth_tasks::Runtime;
use std::sync::Arc;

/// Setup configuration for Arc e2e tests.
///
/// Creates a single-node environment using the Arc chain spec and node configuration.
pub struct ArcSetup {
    chain_spec: Arc<ArcChainSpec>,
    addresses_denylist_config: Option<AddressesDenylistConfig>,
    invalid_tx_list_config: Option<InvalidTxListConfig>,
}

impl Default for ArcSetup {
    fn default() -> Self {
        Self::new()
    }
}

impl ArcSetup {
    /// Creates a new setup with default configuration.
    ///
    /// Uses the LOCAL_DEV chain spec which has all Arc hardforks active at block 0.
    pub fn new() -> Self {
        Self {
            chain_spec: LOCAL_DEV.clone(),
            addresses_denylist_config: None,
            invalid_tx_list_config: None,
        }
    }

    /// Sets a custom chain spec.
    pub fn with_chain_spec(mut self, chain_spec: Arc<ArcChainSpec>) -> Self {
        self.chain_spec = chain_spec;
        self
    }

    /// Sets a custom addresses denylist config for Engine API / newPayload validation tests.
    pub fn with_addresses_denylist_config(
        mut self,
        addresses_denylist_config: AddressesDenylistConfig,
    ) -> Self {
        self.addresses_denylist_config = Some(addresses_denylist_config);
        self
    }

    /// Sets a custom invalid tx list config for testing invalid transaction caching.
    pub fn with_invalid_tx_list_config(
        mut self,
        invalid_tx_list_config: InvalidTxListConfig,
    ) -> Self {
        self.invalid_tx_list_config = Some(invalid_tx_list_config);
        self
    }

    /// Applies the setup to create the test environment.
    ///
    /// This creates a single Arc node and initializes the environment with
    /// the genesis block information.
    pub async fn apply(self, env: &mut ArcEnvironment) -> eyre::Result<()> {
        let mut arc_node = ArcNode::default();
        if let Some(cfg) = self.addresses_denylist_config {
            arc_node.addresses_denylist_config = cfg;
        }
        if let Some(cfg) = self.invalid_tx_list_config {
            arc_node.invalid_tx_list_cfg = cfg;
        }

        let (node, wallet, genesis_block) = Self::launch_node(self.chain_spec, arc_node).await?;

        env.set_node(node);
        env.set_wallet(wallet);
        env.set_current_block(genesis_block);
        Ok(())
    }

    /// Launches a single Arc test node with the given chain spec and node configuration.
    async fn launch_node(
        chain_spec: Arc<ArcChainSpec>,
        arc_node: ArcNode,
    ) -> eyre::Result<(
        NodeHelperType<ArcNode>,
        reth_e2e_test_utils::wallet::Wallet,
        BlockInfo,
    )> {
        let attributes_generator = |_timestamp: u64| -> EthPayloadBuilderAttributes {
            EthPayloadBuilderAttributes::default()
        };

        let runtime = Runtime::with_existing_handle(tokio::runtime::Handle::current())?;
        let network_config = NetworkArgs {
            discovery: DiscoveryArgs {
                disable_discovery: true,
                ..DiscoveryArgs::default()
            },
            ..NetworkArgs::default()
        };
        let tree_config =
            reth_node_api::TreeConfig::default().with_cross_block_cache_size(1024 * 1024);
        let node_config = NodeConfig::new(chain_spec.clone())
            .with_network(network_config)
            .with_unused_ports()
            .with_rpc(
                RpcServerArgs::default()
                    .with_unused_ports()
                    .with_http()
                    .with_http_api(RpcModuleSelection::All),
            );

        let NodeHandle {
            node,
            node_exit_future,
        } = NodeBuilder::new(node_config)
            .testing_node(runtime)
            .with_types_and_provider::<ArcNode, BlockchainProvider<_>>()
            .with_components(arc_node.components_builder())
            .with_add_ons(arc_node.add_ons())
            .launch_with_fn(|builder| {
                let launcher = EngineNodeLauncher::new(
                    builder.task_executor().clone(),
                    builder.config().datadir(),
                    tree_config,
                );
                builder.launch_with(launcher)
            })
            .await?;

        tokio::spawn(async move {
            node_exit_future.await.expect("node exited unexpectedly");
        });

        let node: NodeHelperType<ArcNode> =
            reth_e2e_test_utils::node::NodeTestContext::new(node, attributes_generator).await?;
        let genesis_number = chain_spec.genesis_header().number;
        let genesis = node.block_hash(genesis_number);
        node.update_forkchoice(genesis, genesis).await?;

        let genesis_header = node
            .inner
            .provider()
            .header_by_number(genesis_number)?
            .ok_or_else(|| eyre::eyre!("Genesis header not found"))?;
        let genesis_block = BlockInfo::new(genesis, 0, genesis_header.timestamp);

        let wallet =
            reth_e2e_test_utils::wallet::Wallet::default().with_chain_id(chain_spec.chain().id());

        Ok((node, wallet, genesis_block))
    }
}
