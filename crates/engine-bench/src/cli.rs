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

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "arc-engine-bench",
    version = arc_version::SHORT_VERSION,
    long_version = arc_version::LONG_VERSION,
    about = "Benchmark Arc Engine API block import via newPayload + forkchoiceUpdated"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Prepare a local payload fixture directory from historical source blocks.
    PreparePayload(PreparePayloadArgs),
    /// Replay historical blocks into a target Arc node with newPayload + forkchoiceUpdated.
    NewPayloadFcu(NewPayloadFcuArgs),
}

#[derive(Debug, Args, Clone)]
pub struct CommonArgs {
    /// Engine API IPC socket path. Mutually exclusive with --engine-rpc-url / --jwt-secret.
    #[arg(long, value_name = "ENGINE_IPC", group = "engine_transport")]
    pub engine_ipc: Option<PathBuf>,
    /// Authenticated Engine API HTTP endpoint. Requires --jwt-secret.
    #[arg(long, value_name = "ENGINE_RPC_URL", group = "engine_transport")]
    pub engine_rpc_url: Option<String>,
    /// JWT secret used to authenticate Engine API requests (required with --engine-rpc-url).
    #[arg(long = "jwt-secret", value_name = "PATH", requires = "engine_rpc_url")]
    pub jwt_secret: Option<PathBuf>,
    /// Timeout for Ethereum JSON-RPC requests used by this command, in milliseconds (must be >= 1).
    #[arg(long, value_name = "MILLISECONDS", default_value_t = 10_000, value_parser = clap::value_parser!(u64).range(1..))]
    pub eth_rpc_timeout_ms: u64,
    /// Output directory for CSV artifacts. Defaults to target/engine-bench/<mode>-<timestamp>.
    #[arg(long, short, value_name = "OUTPUT_DIR")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub struct PreparePayloadArgs {
    /// Chain name or path to a genesis JSON file. Used to store the genesis config in the fixture.
    #[arg(long, default_value = "arc-localdev")]
    pub chain: String,
    /// Read historical payloads from this source RPC endpoint.
    #[arg(long, value_name = "SOURCE_RPC_URL")]
    pub source_rpc_url: String,
    /// First source block number to include in the fixture (must be >= 1).
    #[arg(long, value_name = "FROM_BLOCK", value_parser = clap::value_parser!(u64).range(1..))]
    pub from: u64,
    /// Last source block number to include in the fixture, inclusive.
    #[arg(long, value_name = "TO_BLOCK")]
    pub to: u64,
    /// Batch size for source RPC block fetching.
    #[arg(long, value_name = "BATCH_SIZE", default_value_t = 20)]
    pub batch_size: usize,
    /// Timeout for source Ethereum JSON-RPC requests, in milliseconds (must be >= 1).
    #[arg(long, value_name = "MILLISECONDS", default_value_t = 10_000, value_parser = clap::value_parser!(u64).range(1..))]
    pub eth_rpc_timeout_ms: u64,
    /// Output directory for the prepared payload fixture.
    #[arg(long, value_name = "OUTPUT_DIR")]
    pub output_dir: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub struct NewPayloadFcuArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Regular RPC endpoint of the target Arc execution node. Used to verify the target head before replay starts.
    #[arg(long, value_name = "TARGET_ETH_RPC_URL")]
    pub target_eth_rpc_url: String,
    /// Payload fixture directory containing genesis.json, metadata.json, and payloads.jsonl.
    #[arg(long, value_name = "PAYLOAD_DIR")]
    pub payload: PathBuf,
}
