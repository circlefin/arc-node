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

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::time::Duration;

use jsonrpsee::rpc_params;
use serde_json::{json, Value};
use tempfile::TempDir;
use url::Url;

use alloy_rpc_types_engine::JwtSecret;
use reth_node_builder::{NodeBuilder, NodeConfig};
use reth_tasks::TaskExecutor;

use arc_consensus_types::Address;
use arc_eth_engine::ipc::engine_ipc::EngineIPC;
use arc_eth_engine::retry::NoRetry;
use arc_eth_engine::rpc::EngineApiRpcError;
use arc_eth_engine::{engine::Engine, rpc::engine_rpc::EngineRpc};
use arc_evm_node::node::{ArcNode, ArcRpcConfig};
use arc_execution_config::addresses_denylist::AddressesDenylistConfig;
use arc_execution_config::chainspec::ArcChainSpec;
use arc_execution_config::chainspec::LOCAL_DEV;
use arc_execution_txpool::InvalidTxListConfig;

/// Common test suite for engine implementations
async fn test_engine_common(engine: &Engine, initial_block_number: &str) {
    // Test getting chain ID
    let chain_id = engine
        .eth
        .get_chain_id()
        .await
        .expect("Failed to get chain ID");
    assert_eq!(chain_id, "0x539"); // 1337 in hex

    // Test getting genesis block
    let genesis_block = engine
        .eth
        .get_genesis_block()
        .await
        .expect("Failed to get genesis block");
    assert!(
        !genesis_block.block_hash.is_zero(),
        "Genesis hash should not be zero"
    );

    // Test getting a block
    let block = engine
        .eth
        .get_block_by_number(initial_block_number)
        .await
        .expect("Failed to get block");
    assert!(block.is_some(), "Block should exist");

    // Test get active validator set
    let validator_set = engine
        .eth
        .get_active_validator_set(0)
        .await
        .expect("Failed to get active validator set");
    assert!(
        !validator_set.validators.is_empty(),
        "Validator set should not be empty"
    );

    // Test get consensus params
    let consensus_params = engine
        .eth
        .get_consensus_params(0)
        .await
        .expect("Failed to get consensus params");
    assert!(
        consensus_params.timeouts().propose > Duration::ZERO,
        "Consensus params should have valid timeout_propose"
    );

    // Test exchange capabilities
    let capabilities = engine.check_capabilities().await;
    assert!(
        capabilities.is_ok(),
        "Failed to check capabilities: {:?}",
        capabilities
    );

    // Test generate new block
    let block = engine
        .generate_block(
            &block.unwrap(),
            Engine::timestamp_now() + 1,
            &Address::default(),
        )
        .await
        .expect("Failed to generate new block");
    assert!(block.payload_inner.payload_inner.block_hash.len() == 32);

    // Test notify new block
    let status = engine
        .notify_new_block(&block, Vec::new())
        .await
        .expect("Failed to notify new block");
    assert!(
        status.status.is_valid(),
        "Block validation failed: {:?}",
        status
    );

    // Test set latest forkchoice state
    let head_block_hash = engine
        .set_latest_forkchoice_state(block.payload_inner.payload_inner.block_hash)
        .await
        .expect("Failed to set latest forkchoice state");
    assert!(head_block_hash.len() == 32);
}

#[tokio::test]
async fn test_engine() {
    let executor = TaskExecutor::test();

    let chain_spec = LOCAL_DEV.clone();

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let ipc_path = temp_dir.path().join("reth.ipc");
    let auth_ipc_path = temp_dir.path().join("auth.ipc");
    let jwt_path = temp_dir.path().join("jwtsecret");
    JwtSecret::try_create_random(jwt_path.as_path()).expect("Failed to create JWT secret");

    // Create node config with ArcChainSpec type
    let mut node_config: NodeConfig<ArcChainSpec> = NodeConfig::new(chain_spec);
    node_config.rpc.ipcpath = ipc_path.to_string_lossy().to_string();
    node_config.rpc.auth_ipc = true;
    node_config.rpc.auth_ipc_path = auth_ipc_path.to_string_lossy().to_string();
    node_config.rpc.http = true;
    node_config.rpc.ws = false;
    node_config.rpc.auth_jwtsecret = Some(jwt_path.clone());
    let node_config = node_config.set_dev(true);

    // Build and start the reth node with Arc types
    let arc_node = ArcNode::new(
        ArcRpcConfig::default(),
        InvalidTxListConfig::default(),
        AddressesDenylistConfig::default(),
        None,
        true,
        true,
    );
    let node_handle = NodeBuilder::new(node_config)
        .testing_node(executor)
        .node(arc_node)
        .launch()
        .await
        .expect("Failed to launch reth node");

    // Spawn the node in a background task
    let node_task = tokio::spawn(async move {
        println!("Node task started, waiting for exit...");
        node_handle
            .node_exit_future
            .await
            .expect("Node exited unexpectedly");
    });

    // Test IPC engine
    {
        let engine = Engine::new_ipc(auth_ipc_path.to_str().unwrap(), ipc_path.to_str().unwrap())
            .await
            .expect("Failed to connect to IPC socket");

        // Configure Osaka activation from the localdev chainspec (chain_id = 1337)
        engine.set_osaka_from_chain_id(1337);

        // Test transaction pool status (IPC-specific test)
        let txpool_status = engine
            .eth
            .txpool_status()
            .await
            .expect("Failed to get txpool status");
        assert_eq!(txpool_status.pending, 0);
        assert_eq!(txpool_status.queued, 0);

        // Test transaction pool inspect (IPC-specific test)
        let txpool_inspect = engine
            .eth
            .txpool_inspect()
            .await
            .expect("Failed to get txpool inspect");
        assert!(txpool_inspect.pending.is_empty());
        assert!(txpool_inspect.queued.is_empty());

        // Run common engine tests
        test_engine_common(&engine, "0x0").await;

        // Test IPC error handling
        let ipc_client =
            EngineIPC::new_with_timeout(auth_ipc_path.to_str().unwrap(), Duration::from_secs(5))
                .await
                .expect("Failed to build IPC client for error test");
        let params = rpc_params!(1u64);
        let err = ipc_client
            .rpc_request::<Value>(
                "engine_newPayloadV4",
                params,
                Duration::from_secs(5),
                NoRetry,
            )
            .await
            .expect_err("malformed IPC payload should error");
        assert!(
            err.downcast_ref::<EngineApiRpcError>().is_some(),
            "IPC error should downcast to EngineApiRpcError"
        );
    }

    // Test RPC engine
    {
        let engine = Engine::new_rpc(
            Url::parse("http://localhost:8551").unwrap(),
            Url::parse("http://localhost:8545").unwrap(),
            None,
            jwt_path.to_str().unwrap(),
        )
        .await
        .expect("Failed to create RPC engine client");

        // Configure Osaka activation from the localdev chainspec (chain_id = 1337)
        engine.set_osaka_from_chain_id(1337);

        // Run common engine tests
        test_engine_common(&engine, "latest").await;

        // Test RPC error handling
        let rpc_client = EngineRpc::new(
            Url::parse("http://localhost:8551").unwrap(),
            Path::new(jwt_path.to_str().unwrap()),
        )
        .expect("Failed to build RPC client for error test");

        let params = json!(1u64);
        let err = rpc_client
            .rpc_request::<Value>(
                "engine_newPayloadV4",
                params,
                Duration::from_secs(5),
                NoRetry,
            )
            .await
            .expect_err("malformed RPC payload should error");
        assert!(
            err.downcast_ref::<EngineApiRpcError>().is_some(),
            "RPC error should downcast to EngineApiRpcError"
        );
    }

    // Shutdown the node at the end of the test
    node_task.abort();
}
