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

//! Registration tests for the `arc` JSON-RPC namespace.

use arc_evm_node::node::ArcRpcConfig;
use arc_evm_node::rpc::common::codes::UPSTREAM_UNREACHABLE;
use arc_execution_e2e::{ArcSetup, ArcTestNode};
use eyre::Result;
use jsonrpsee::core::client::{ClientT, Error as RpcClientError};
use jsonrpsee::core::params::ArrayParams;
use jsonrpsee::types::ErrorCode;

fn method_not_found(err: &RpcClientError) -> bool {
    matches!(
        err,
        RpcClientError::Call(obj) if obj.code() == ErrorCode::MethodNotFound.code()
    )
}

async fn rpc_call(
    node: &ArcTestNode,
    method: &str,
    params: ArrayParams,
) -> Result<serde_json::Value, RpcClientError> {
    let client = node.node.rpc_client().expect("RPC client not available");
    client.request(method, params).await
}

/// The node default leaves both methods unregistered.
#[tokio::test]
async fn arc_methods_absent_when_namespace_disabled() -> Result<()> {
    reth_tracing::init_test_tracing();

    let node = ArcTestNode::start(ArcSetup::new()).await?;

    let version_err = rpc_call(&node, "arc_getVersion", ArrayParams::new())
        .await
        .expect_err("arc_getVersion should be unregistered by default");
    assert!(
        method_not_found(&version_err),
        "expected method-not-found for arc_getVersion, got {version_err:?}"
    );

    let mut cert_params = ArrayParams::new();
    // Arbitrary valid height.
    cert_params.insert(7u64)?;
    let cert_err = rpc_call(&node, "arc_getCertificate", cert_params)
        .await
        .expect_err("arc_getCertificate should be unregistered by default");
    assert!(
        method_not_found(&cert_err),
        "expected method-not-found for arc_getCertificate, got {cert_err:?}"
    );
    Ok(())
}

/// A handler error from the closed upstream still proves method registration.
#[tokio::test]
async fn arc_methods_reachable_when_namespace_enabled() -> Result<()> {
    reth_tracing::init_test_tracing();

    let node = ArcTestNode::start(
        ArcSetup::new()
            .with_arc_rpc_config(ArcRpcConfig::new(true, Some("http://127.0.0.1:1".into()))),
    )
    .await?;

    let version = rpc_call(&node, "arc_getVersion", ArrayParams::new()).await?;
    assert!(
        version
            .get("cargo_version")
            .and_then(|v| v.as_str())
            .is_some(),
        "arc_getVersion missing cargo_version: {version}"
    );

    let mut cert_params = ArrayParams::new();
    // Arbitrary valid height.
    cert_params.insert(7u64)?;
    match rpc_call(&node, "arc_getCertificate", cert_params).await {
        Ok(value) => {
            panic!("expected a handler error without a CL upstream, got {value}");
        }
        Err(err) => {
            let RpcClientError::Call(obj) = &err else {
                panic!("expected a JSON-RPC call error, got {err:?}");
            };
            assert_eq!(
                obj.code(),
                UPSTREAM_UNREACHABLE,
                "closed upstream should return UPSTREAM_UNREACHABLE, got {err:?}"
            );
        }
    }
    Ok(())
}
