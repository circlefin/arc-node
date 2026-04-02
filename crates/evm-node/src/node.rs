// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

//! Arc Node types config.
//! Fork from https://github.com/paradigmxyz/reth/blob/v1.7.0/crates/ethereum/node/src/node.rs
//! Reference to EthereumNode and add our customization
//! - inject the EVM customization in ArcExecutorBuilder
//! - inject our consensus ArcConsensus in ArcConsensusBuilder
//! - inject ArcEngineValidatorBuilder in ArcEngineValidatorBuilder

use alloy_network::Ethereum;
use alloy_rpc_types_engine::ExecutionData;
use arc_evm::{ArcEvmConfig, ArcEvmFactory};
use arc_execution_validation::ArcConsensus;
use reth_chainspec::{EthereumHardforks, Hardforks};
use reth_engine_local::LocalPayloadAttributesBuilder;
use reth_engine_primitives::EngineTypes;
use reth_ethereum::{node::EthEngineTypes, node::EthEvmConfig};
use reth_ethereum_engine_primitives::{
    EthBuiltPayload, EthPayloadAttributes, EthPayloadBuilderAttributes,
};
use reth_ethereum_primitives::EthPrimitives;
use reth_evm::{ConfigureEvm, EvmFactory, EvmFactoryFor, NextBlockEnvAttributes};
use reth_network::{primitives::BasicNetworkPrimitives, NetworkHandle, PeersInfo};
use reth_node_api::{
    AddOnsContext, FullNodeComponents, HeaderTy, NodeAddOns, PayloadAttributesBuilder,
    PrimitivesTy, TxTy,
};
use reth_node_builder::{
    components::{
        BasicPayloadServiceBuilder, ComponentsBuilder, ConsensusBuilder, ExecutorBuilder,
        NetworkBuilder,
    },
    node::{FullNodeTypes, NodeTypes},
    rpc::{
        BasicEngineApiBuilder, BasicEngineValidatorBuilder, EngineApiBuilder, EngineValidatorAddOn,
        EngineValidatorBuilder, EthApiBuilder, EthApiCtx, PayloadValidatorBuilder, RethRpcAddOns,
        RpcAddOns, RpcHandle,
    },
    BuilderContext, DebugNode, Node, NodeAdapter,
};
use reth_payload_primitives::PayloadTypes;
use reth_provider::{providers::ProviderFactoryBuilder, EthStorage};
use reth_rpc::{
    eth::core::{EthApiFor, EthRpcConverterFor},
    ValidationApi,
};
use reth_rpc_api::servers::BlockSubmissionValidationApiServer;
use reth_rpc_builder::{config::RethRpcServerConfig, middleware::RethRpcMiddleware};
use reth_rpc_eth_api::{
    helpers::{
        config::{EthConfigApiServer, EthConfigHandler},
        pending_block::BuildPendingEnv,
    },
    RpcConvert, RpcTypes, SignableTxRequest,
};
use reth_rpc_eth_types::{error::FromEvmError, EthApiError};
use reth_rpc_server_types::RethRpcModule;
use reth_tracing::tracing::info;
use reth_transaction_pool::{PoolPooledTx, PoolTransaction, TransactionPool};
use revm::context::TxEnv;
use std::{default::Default, marker::PhantomData, sync::Arc};

use arc_execution_config::addresses_denylist::AddressesDenylistConfig;
use arc_execution_config::chainspec::ArcChainSpec;
use arc_execution_payload::payload::ArcNetworkPayloadBuilderBuilder;
use arc_execution_txpool::{ArcPoolBuilder, InvalidTxList, InvalidTxListConfig};

// FIXME use the ethereum chain spec temporary, we need to define Arc chain spec
// original traits for ChainSpec in this file `Hardforks + EthereumHardforks + EthExecutorSpec`

use crate::rpc_middleware::ArcRpcLayer;
use crate::ArcEngineValidator;

/// Type configuration for a regular Arc node.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ArcNode {
    pub rpc_cfg: ArcRpcConfig,
    pub invalid_tx_list_cfg: InvalidTxListConfig,
    pub addresses_denylist_config: AddressesDenylistConfig,
    /// Custom payload builder loop time limit in milliseconds. When set, used instead of Reth's builder.deadline.
    pub payload_builder_deadline_ms: Option<u64>,
    /// When true, `on_missing_payload` waits for the in-flight build instead of
    /// racing an empty block.
    pub wait_for_payload: bool,
    /// When true (default), pending-tx RPCs are restricted (`eth_subscribe("newPendingTransactions")`, `eth_newPendingTransactionFilter`).
    /// When false, all requests are forwarded.
    pub filter_pending_txs: bool,
}

impl Default for ArcNode {
    fn default() -> Self {
        Self {
            rpc_cfg: ArcRpcConfig::default(),
            invalid_tx_list_cfg: InvalidTxListConfig::default(),
            addresses_denylist_config: AddressesDenylistConfig::default(),
            payload_builder_deadline_ms: None,
            wait_for_payload: true,
            filter_pending_txs: true,
        }
    }
}

impl ArcNode {
    /// Creates a new `ArcNode`.
    pub fn new(
        rpc_cfg: ArcRpcConfig,
        invalid_tx_list_cfg: InvalidTxListConfig,
        addresses_denylist_config: AddressesDenylistConfig,
        payload_builder_deadline_ms: Option<u64>,
        wait_for_payload: bool,
        filter_pending_txs: bool,
    ) -> Self {
        Self {
            rpc_cfg,
            invalid_tx_list_cfg,
            addresses_denylist_config,
            payload_builder_deadline_ms,
            wait_for_payload,
            filter_pending_txs,
        }
    }

    /// Returns a [`ComponentsBuilder`] configured for a regular Arc node.
    pub fn components<Node>(
        invalid_tx_list_cfg: &InvalidTxListConfig,
        addresses_denylist_config: &AddressesDenylistConfig,
        payload_builder_deadline_ms: Option<u64>,
        wait_for_payload: bool,
    ) -> ComponentsBuilder<
        Node,
        ArcPoolBuilder,
        BasicPayloadServiceBuilder<ArcNetworkPayloadBuilderBuilder>,
        ArcNetworkBuilder,
        ArcExecutorBuilder,
        ArcConsensusBuilder,
    >
    where
        Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ArcChainSpec, Primitives = EthPrimitives>>,
        <Node::Types as NodeTypes>::Payload: PayloadTypes<
            BuiltPayload = EthBuiltPayload,
            PayloadAttributes = EthPayloadAttributes,
            PayloadBuilderAttributes = EthPayloadBuilderAttributes,
        >,
    {
        let invalid_tx_list_opt = if invalid_tx_list_cfg.enabled {
            info!(
                capacity = invalid_tx_list_cfg.capacity,
                "Invalid tx list is enabled; initializing"
            );
            Some(InvalidTxList::new(invalid_tx_list_cfg.capacity))
        } else {
            info!("Invalid tx list is disabled");
            None
        };

        let pb_builder = ArcNetworkPayloadBuilderBuilder::new(
            invalid_tx_list_opt.clone(),
            payload_builder_deadline_ms,
            wait_for_payload,
        );
        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(ArcPoolBuilder::new(
                invalid_tx_list_opt,
                addresses_denylist_config.clone(),
            ))
            .executor(ArcExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::new(pb_builder))
            .network(ArcNetworkBuilder::default())
            .consensus(ArcConsensusBuilder::default())
    }

    /// Instantiates the [`ProviderFactoryBuilder`] for an Arc node.
    pub fn provider_factory_builder() -> ProviderFactoryBuilder<Self> {
        ProviderFactoryBuilder::default()
    }
}

impl NodeTypes for ArcNode {
    type Primitives = EthPrimitives;
    type ChainSpec = ArcChainSpec;
    type Storage = EthStorage;
    type Payload = EthEngineTypes;
}

/// Builds [`EthApi`](reth_rpc::EthApi) for Arc.
#[derive(Debug)]
pub struct ArcEthApiBuilder<NetworkT = Ethereum>(PhantomData<NetworkT>);

impl<NetworkT> Default for ArcEthApiBuilder<NetworkT> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<N, NetworkT> EthApiBuilder<N> for ArcEthApiBuilder<NetworkT>
where
    N: FullNodeComponents<
        Types: NodeTypes<ChainSpec = ArcChainSpec, Primitives = EthPrimitives>,
        Evm: ConfigureEvm<NextBlockEnvCtx: BuildPendingEnv<HeaderTy<N::Types>>>,
    >,
    NetworkT: RpcTypes<TransactionRequest: SignableTxRequest<TxTy<N::Types>>>,
    EthRpcConverterFor<N, NetworkT>: RpcConvert<
        Primitives = PrimitivesTy<N::Types>,
        Error = EthApiError,
        Network = NetworkT,
        Evm = N::Evm,
    >,
    EthApiError: FromEvmError<N::Evm>,
{
    type EthApi = EthApiFor<N, NetworkT>;

    async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
        Ok(ctx
            .eth_api_builder()
            .map_converter(|r| r.with_network())
            .build())
    }
}

/// Configuration for the ARC RPC namespace.
#[derive(Debug, Clone, Default)]
pub struct ArcRpcConfig {
    /// Whether the ARC namespace is enabled.
    pub enabled: bool,
    /// Optional upstream `malachite-app` RPC URL.
    pub upstream_url: Option<String>,
}

impl ArcRpcConfig {
    pub fn new(enabled: bool, upstream_url: Option<String>) -> Self {
        Self {
            enabled,
            upstream_url,
        }
    }
}

/// Add-ons for Arc
#[derive(Debug)]
pub struct ArcAddOns<
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
    PVB,
    EB = BasicEngineApiBuilder<PVB>,
    EVB = BasicEngineValidatorBuilder<PVB>,
    RpcMiddleware = ArcRpcLayer,
> {
    inner: RpcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
    arc_rpc: ArcRpcConfig,
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> ArcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Creates a new instance from the inner `RpcAddOns`.
    pub fn new(
        inner: RpcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
        arc_rpc: ArcRpcConfig,
    ) -> Self {
        Self { inner, arc_rpc }
    }
}

impl<N> Default for ArcAddOns<N, ArcEthApiBuilder, ArcEngineValidatorBuilder>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec = ArcChainSpec,
            Payload: EngineTypes<ExecutionData = ExecutionData>
                         + PayloadTypes<PayloadAttributes = EthPayloadAttributes>,
            Primitives = EthPrimitives,
        >,
    >,
    ArcEthApiBuilder: EthApiBuilder<N>,
{
    fn default() -> Self {
        let addons = RpcAddOns::new(
            ArcEthApiBuilder::default(),
            ArcEngineValidatorBuilder::default(),
            BasicEngineApiBuilder::default(),
            BasicEngineValidatorBuilder::default(),
            Default::default(),
        );
        Self::new(addons, ArcRpcConfig::default())
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> ArcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Replace the engine API builder.
    pub fn with_engine_api<T>(
        self,
        engine_api_builder: T,
    ) -> ArcAddOns<N, EthB, PVB, T, EVB, RpcMiddleware>
    where
        T: Send,
    {
        let Self { inner, arc_rpc } = self;
        ArcAddOns::new(inner.with_engine_api(engine_api_builder), arc_rpc)
    }

    /// Replace the payload validator builder.
    pub fn with_payload_validator<V, T>(
        self,
        payload_validator_builder: T,
    ) -> ArcAddOns<N, EthB, T, EB, EVB, RpcMiddleware> {
        let Self { inner, arc_rpc } = self;
        ArcAddOns::new(
            inner.with_payload_validator(payload_validator_builder),
            arc_rpc,
        )
    }

    /// Sets rpc middleware
    pub fn with_rpc_middleware<T>(self, rpc_middleware: T) -> ArcAddOns<N, EthB, PVB, EB, EVB, T>
    where
        T: Send,
    {
        let Self { inner, arc_rpc } = self;
        ArcAddOns::new(inner.with_rpc_middleware(rpc_middleware), arc_rpc)
    }

    /// Sets the tokio runtime for the RPC servers.
    ///
    /// Caution: This runtime must not be created from within asynchronous context.
    pub fn with_tokio_runtime(self, tokio_runtime: Option<tokio::runtime::Handle>) -> Self {
        let Self { inner, arc_rpc } = self;
        Self::new(inner.with_tokio_runtime(tokio_runtime), arc_rpc)
    }

    /// Replace entire ARC RPC config.
    pub fn with_arc_rpc_config(mut self, cfg: ArcRpcConfig) -> Self {
        self.arc_rpc = cfg;
        self
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> NodeAddOns<N>
    for ArcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec = ArcChainSpec,
            Primitives = EthPrimitives,
            Payload: EngineTypes<ExecutionData = ExecutionData>,
        >,
        Evm: ConfigureEvm<NextBlockEnvCtx = NextBlockEnvAttributes>,
    >,
    EthB: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    EthApiError: FromEvmError<N::Evm>,
    EvmFactoryFor<N::Evm>: EvmFactory<Tx = TxEnv>,
    RpcMiddleware: RethRpcMiddleware,
{
    type Handle = RpcHandle<N, EthB::EthApi>;

    async fn launch_add_ons(
        self,
        ctx: reth_node_api::AddOnsContext<'_, N>,
    ) -> eyre::Result<Self::Handle> {
        let validation_api = ValidationApi::<_, _, <N::Types as NodeTypes>::Payload>::new(
            ctx.node.provider().clone(),
            Arc::new(ctx.node.consensus().clone()),
            ctx.node.evm_config().clone(),
            ctx.config.rpc.flashbots_config(),
            Box::new(ctx.node.task_executor().clone()),
            Arc::new(ArcEngineValidator::new(ctx.config.chain.clone())),
        );

        let eth_config =
            EthConfigHandler::new(ctx.node.provider().clone(), ctx.node.evm_config().clone());

        self.inner
            .launch_add_ons_with(ctx, move |container| {
                container.modules.merge_if_module_configured(
                    RethRpcModule::Flashbots,
                    validation_api.into_rpc(),
                )?;

                container
                    .modules
                    .merge_if_module_configured(RethRpcModule::Eth, eth_config.into_rpc())?;

                if self.arc_rpc.enabled {
                    if let Ok(arc_module) =
                        crate::rpc::arc::build_arc_rpc_module(self.arc_rpc.upstream_url.clone())
                    {
                        container.modules.merge_configured(arc_module)?;
                    }
                }

                Ok(())
            })
            .await
    }
}

impl<N, EthB, PVB, EB, EVB> RethRpcAddOns<N> for ArcAddOns<N, EthB, PVB, EB, EVB>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec = ArcChainSpec,
            Primitives = EthPrimitives,
            Payload: EngineTypes<ExecutionData = ExecutionData>,
        >,
        Evm: ConfigureEvm<NextBlockEnvCtx = NextBlockEnvAttributes>,
    >,
    EthB: EthApiBuilder<N>,
    PVB: PayloadValidatorBuilder<N>,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    EthApiError: FromEvmError<N::Evm>,
    EvmFactoryFor<N::Evm>: EvmFactory<Tx = TxEnv>,
{
    type EthApi = EthB::EthApi;

    fn hooks_mut(&mut self) -> &mut reth_node_builder::rpc::RpcHooks<N, Self::EthApi> {
        self.inner.hooks_mut()
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> EngineValidatorAddOn<N>
    for ArcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec = ArcChainSpec,
            Primitives = EthPrimitives,
            Payload: EngineTypes<ExecutionData = ExecutionData>,
        >,
        Evm: ConfigureEvm<NextBlockEnvCtx = NextBlockEnvAttributes>,
    >,
    EthB: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    EthApiError: FromEvmError<N::Evm>,
    EvmFactoryFor<N::Evm>: EvmFactory<Tx = TxEnv>,
    RpcMiddleware: Send,
{
    type ValidatorBuilder = EVB;

    fn engine_validator_builder(&self) -> Self::ValidatorBuilder {
        self.inner.engine_validator_builder()
    }
}

impl<N> Node<N> for ArcNode
where
    N: FullNodeTypes<Types = Self>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        ArcPoolBuilder,
        BasicPayloadServiceBuilder<ArcNetworkPayloadBuilderBuilder>,
        ArcNetworkBuilder,
        ArcExecutorBuilder,
        ArcConsensusBuilder,
    >;

    type AddOns = ArcAddOns<
        NodeAdapter<N>,
        ArcEthApiBuilder,
        ArcEngineValidatorBuilder,
        BasicEngineApiBuilder<ArcEngineValidatorBuilder>,
        BasicEngineValidatorBuilder<ArcEngineValidatorBuilder>,
        ArcRpcLayer,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        Self::components(
            &self.invalid_tx_list_cfg,
            &self.addresses_denylist_config,
            self.payload_builder_deadline_ms,
            self.wait_for_payload,
        )
    }

    fn add_ons(&self) -> Self::AddOns {
        ArcAddOns::default()
            .with_arc_rpc_config(self.rpc_cfg.clone())
            .with_rpc_middleware(ArcRpcLayer::new(self.filter_pending_txs))
    }
}

impl<N: FullNodeComponents<Types = Self>> DebugNode<N> for ArcNode {
    type RpcBlock = alloy_rpc_types_eth::Block;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> reth_ethereum_primitives::Block {
        rpc_block.into_consensus().convert_transactions()
    }

    fn local_payload_attributes_builder(
        chain_spec: &Self::ChainSpec,
    ) -> impl PayloadAttributesBuilder<<Self::Payload as PayloadTypes>::PayloadAttributes> {
        LocalPayloadAttributesBuilder::new(Arc::new(chain_spec.clone()))
    }
}

/// A regular Arc evm and executor builder.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ArcExecutorBuilder;

impl<Types, Node> ExecutorBuilder<Node> for ArcExecutorBuilder
where
    Types: NodeTypes<ChainSpec = ArcChainSpec, Primitives = EthPrimitives>,
    Node: FullNodeTypes<Types = Types>,
{
    type EVM = ArcEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let evm_config = EthEvmConfig::new_with_evm_factory(
            ctx.chain_spec().clone(),
            ArcEvmFactory::new(ctx.chain_spec().clone()),
        );
        Ok(ArcEvmConfig::new(evm_config))
    }
}

/// A basic Arc payload service.
#[derive(Debug, Default, Clone, Copy)]
pub struct ArcNetworkBuilder {
    // TODO add closure to modify network
}

impl<Node, Pool> NetworkBuilder<Node, Pool> for ArcNetworkBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: Hardforks>>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
{
    type Network =
        NetworkHandle<BasicNetworkPrimitives<PrimitivesTy<Node::Types>, PoolPooledTx<Pool>>>;

    async fn build_network(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<Self::Network> {
        let network = ctx.network_builder().await?;
        let handle = ctx.start_network(network, pool);
        info!(target: "reth::cli", enode=%handle.local_node_record(), "P2P networking initialized");
        Ok(handle)
    }
}

/// A basic Arc consensus builder.
#[derive(Debug, Default, Clone, Copy)]
pub struct ArcConsensusBuilder {
    // TODO add closure to modify consensus
}

impl<Node> ConsensusBuilder<Node> for ArcConsensusBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ArcChainSpec, Primitives = EthPrimitives>>,
{
    type Consensus = Arc<ArcConsensus<<Node::Types as NodeTypes>::ChainSpec>>;

    async fn build_consensus(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Consensus> {
        Ok(Arc::new(ArcConsensus::new(ctx.chain_spec())))
    }
}

/// Builder for [`ArcEngineValidator`].
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct ArcEngineValidatorBuilder;

impl<Node, Types> PayloadValidatorBuilder<Node> for ArcEngineValidatorBuilder
where
    Types: NodeTypes<
        ChainSpec: Hardforks + EthereumHardforks + Clone + 'static,
        Payload: EngineTypes<ExecutionData = ExecutionData>
                     + PayloadTypes<PayloadAttributes = EthPayloadAttributes>,
        Primitives = EthPrimitives,
    >,
    Node: FullNodeComponents<Types = Types>,
{
    type Validator = ArcEngineValidator<Types::ChainSpec>;

    async fn build(self, ctx: &AddOnsContext<'_, Node>) -> eyre::Result<Self::Validator> {
        Ok(ArcEngineValidator::new(ctx.config.chain.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_rpc_config_construction() {
        let disabled = ArcRpcConfig::default();
        assert!(!disabled.enabled);
        assert!(disabled.upstream_url.is_none());

        let enabled = ArcRpcConfig::new(true, Some("http://example".into()));
        assert!(enabled.enabled);
        assert_eq!(enabled.upstream_url.as_deref(), Some("http://example"));
    }

    #[test]
    fn invalid_tx_list_config_default() {
        let cfg = InvalidTxListConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.capacity, 100_000);
    }

    #[test]
    fn invalid_tx_list_config_custom() {
        let cfg = InvalidTxListConfig::new(true, 50_000);
        assert!(cfg.enabled);
        assert_eq!(cfg.capacity, 50_000);
    }

    #[test]
    fn arc_node_construction_with_invalid_tx_list() {
        let rpc_cfg = ArcRpcConfig::default();
        let invalid_tx_list_cfg = InvalidTxListConfig::new(true, 25_000);
        let node = ArcNode::new(
            rpc_cfg,
            invalid_tx_list_cfg,
            AddressesDenylistConfig::default(),
            None,
            true,
            true,
        );

        assert!(!node.rpc_cfg.enabled);
        assert!(node.invalid_tx_list_cfg.enabled);
        assert_eq!(node.invalid_tx_list_cfg.capacity, 25_000);
        assert!(!node.addresses_denylist_config.is_enabled());
    }

    #[test]
    fn arc_node_construction_with_addresses_denylist_config() {
        use alloy_primitives::{address, b256};
        let addresses_cfg = AddressesDenylistConfig::try_new(
            true,
            Some(address!("0x3600000000000000000000000000000000000001")),
            Some(b256!(
                "0x0000000000000000000000000000000000000000000000000000000000000001"
            )),
            Vec::new(),
        )
        .unwrap();
        let node = ArcNode::new(
            ArcRpcConfig::default(),
            InvalidTxListConfig::default(),
            addresses_cfg.clone(),
            None,
            true,
            true,
        );
        assert!(node.addresses_denylist_config.is_enabled());
        if let AddressesDenylistConfig::Enabled {
            contract_address, ..
        } = &node.addresses_denylist_config
        {
            assert_eq!(
                *contract_address,
                address!("0x3600000000000000000000000000000000000001")
            );
        } else {
            panic!("expected Enabled variant");
        }
    }

    #[test]
    fn arc_node_default_has_pending_txs_filter_enabled() {
        let node = ArcNode::default();
        assert!(
            node.filter_pending_txs,
            "Default ArcNode should have pending txs filter enabled"
        );
    }

    #[test]
    fn arc_node_construction_with_pending_tx_filter_disabled() {
        let node = ArcNode::new(
            ArcRpcConfig::default(),
            InvalidTxListConfig::default(),
            AddressesDenylistConfig::default(),
            None,
            true,
            false,
        );
        assert!(!node.filter_pending_txs);
    }

    #[test]
    fn arc_node_default_wait_for_payload_enabled() {
        let node = ArcNode::default();
        assert!(
            node.wait_for_payload,
            "Default ArcNode should have wait_for_payload enabled"
        );
    }

    #[test]
    fn arc_node_construction_with_wait_for_payload_disabled() {
        let node = ArcNode::new(
            ArcRpcConfig::default(),
            InvalidTxListConfig::default(),
            AddressesDenylistConfig::default(),
            None,
            false,
            false,
        );
        assert!(!node.wait_for_payload);
    }
}
