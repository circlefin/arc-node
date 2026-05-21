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

//! Default configuration for Arc Network node.
//!
//! This module provides default values for various node components including
//! snapshot download URLs for quick node bootstrapping, and RPC connection limits.

use reth_cli_commands::download::DownloadDefaults;
use reth_node_core::args::DefaultRpcServerArgs;
use std::borrow::Cow;

// FIXME: Update this to the actual snapshot URL.
/// Default snapshot URL for Arc Network testnet (chain ID 5042002).
pub(crate) const DEFAULT_DOWNLOAD_URL: &str = "https://snapshots.arc.network/5042002";

/// Max simultaneous RPC connections (HTTP + WS pooled). Bounds WS subscription fan-out memory.
pub const RPC_MAX_CONNECTIONS: u32 = 250;

/// Max subscriptions per RPC connection. Real-world clients multiplex ~5 over one WS socket.
pub const RPC_MAX_SUBSCRIPTIONS_PER_CONNECTION: u32 = 32;

fn init_download_urls() {
    let download_defaults = DownloadDefaults {
        available_snapshots: vec![
            // FIXME: Update this to the actual snapshot URL.
            Cow::Borrowed("https://snapshots.arc.network/5042002 (testnet)"),
            Cow::Borrowed("https://snapshots.arc.network/5042001 (devnet)"),
        ],
        default_base_url: Cow::Borrowed(DEFAULT_DOWNLOAD_URL),
        default_chain_aware_base_url: None,
        long_help: None,
    };
    let _ = download_defaults.try_init();
}

fn init_rpc_defaults() {
    let _ = DefaultRpcServerArgs::default()
        .with_rpc_max_connections(RPC_MAX_CONNECTIONS.into())
        .with_rpc_max_subscriptions_per_connection(RPC_MAX_SUBSCRIPTIONS_PER_CONNECTION.into())
        .try_init();
}

/// Register Arc defaults with Reth. Must run before CLI parsing.
pub fn init_defaults() {
    init_download_urls();
    init_rpc_defaults();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Args, Parser};
    use reth_node_core::args::RpcServerArgs;

    #[derive(Parser)]
    struct CommandParser<T: Args> {
        #[command(flatten)]
        args: T,
    }

    // Reth's RpcServerArgs read defaults from a process-global OnceLock, so
    // every test in this binary must init first to be order-independent.
    fn ensure_initialized() {
        init_defaults();
    }

    #[test]
    fn rpc_defaults_match_arc_constants_when_no_override() {
        ensure_initialized();
        let args = CommandParser::<RpcServerArgs>::parse_from(["arc-node-execution"]).args;
        assert_eq!(args.rpc_max_connections.get(), RPC_MAX_CONNECTIONS);
        assert_eq!(
            args.rpc_max_subscriptions_per_connection.get(),
            RPC_MAX_SUBSCRIPTIONS_PER_CONNECTION,
        );
    }

    #[test]
    fn rpc_max_connections_cli_override_wins() {
        ensure_initialized();
        let args = CommandParser::<RpcServerArgs>::parse_from([
            "arc-node-execution",
            "--rpc.max-connections",
            "777",
        ])
        .args;
        assert_eq!(args.rpc_max_connections.get(), 777);
    }

    #[test]
    fn rpc_max_subscriptions_per_connection_cli_override_wins() {
        ensure_initialized();
        let args = CommandParser::<RpcServerArgs>::parse_from([
            "arc-node-execution",
            "--rpc.max-subscriptions-per-connection",
            "1024",
        ])
        .args;
        assert_eq!(args.rpc_max_subscriptions_per_connection.get(), 1024);
    }

    #[test]
    fn rpc_overrides_are_independent() {
        ensure_initialized();
        let args = CommandParser::<RpcServerArgs>::parse_from([
            "arc-node-execution",
            "--rpc.max-connections",
            "500",
        ])
        .args;
        assert_eq!(args.rpc_max_connections.get(), 500);
        assert_eq!(
            args.rpc_max_subscriptions_per_connection.get(),
            RPC_MAX_SUBSCRIPTIONS_PER_CONNECTION,
        );
    }
}
