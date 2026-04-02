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

//! arc-snapshots — download and extract Arc node snapshots.

use std::path::PathBuf;

use arc_snapshots::download::{self, Chain};
use clap::{Parser, Subcommand};
use eyre::Result;
use tracing::info;

#[derive(Debug, Parser)]
#[command(
    name = "arc-snapshots",
    version = arc_version::SHORT_VERSION,
    long_version = arc_version::LONG_VERSION,
    about = "Arc node snapshot utilities",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Download the latest snapshot and extract EL and CL data to their respective directories.
    Download(DownloadArgs),
}

/// Download Arc node snapshots.
///
/// Downloads separate EL and CL archives and extracts them:
///   - execution archive (bare paths, e.g. db/) → --execution-path
///   - consensus archive (bare paths, e.g. store.db) → --consensus-path
#[derive(Debug, Parser)]
struct DownloadArgs {
    /// URL of the execution layer snapshot archive.
    ///
    /// If omitted, the latest snapshot for --chain is fetched automatically.
    #[arg(long)]
    execution_url: Option<String>,

    /// URL of the consensus layer snapshot archive.
    ///
    /// If omitted, the latest snapshot for --chain is fetched automatically.
    #[arg(long)]
    consensus_url: Option<String>,

    /// Network to download a snapshot for.
    #[arg(long, default_value = "arc-testnet")]
    chain: Chain,

    /// Directory to extract execution layer data into.
    ///
    /// Defaults to ~/.arc/execution.
    #[arg(long)]
    execution_path: Option<PathBuf>,

    /// Directory to extract consensus layer data into.
    ///
    /// Defaults to ~/.arc/consensus.
    #[arg(long)]
    consensus_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Basic tracing to stdout
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Download(args) => run_download(args).await,
    }
}

pub(crate) async fn run_download(args: DownloadArgs) -> Result<()> {
    let chain = args.chain;

    let (execution_url, consensus_url) = match (args.execution_url, args.consensus_url) {
        (Some(el), Some(cl)) => (el, cl),
        (Some(_), None) | (None, Some(_)) => {
            eyre::bail!("provide both --execution-url and --consensus-url, or neither")
        }
        (None, None) => {
            info!(chain = %chain, "Fetching latest snapshot URLs");
            download::fetch_latest_snapshot_urls(chain).await?
        }
    };

    let execution_dir = args
        .execution_path
        .or_else(|| chain.default_execution_path())
        .ok_or_else(|| {
            eyre::eyre!("Could not determine default execution path; use --execution-path")
        })?;

    let consensus_dir = args
        .consensus_path
        .or_else(Chain::default_consensus_path)
        .ok_or_else(|| {
            eyre::eyre!("Could not determine default consensus path; use --consensus-path")
        })?;

    let tmp_dir = execution_dir.join(".snapshot-tmp");

    info!(
        execution_url = %execution_url,
        consensus_url = %consensus_url,
        execution_dir = %execution_dir.display(),
        consensus_dir = %consensus_dir.display(),
        "Starting snapshot download"
    );

    download::stream_and_extract_both(
        execution_url,
        consensus_url,
        execution_dir,
        consensus_dir,
        tmp_dir,
    )
    .await?;

    info!("Snapshot downloaded and extracted successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn parse_download_with_explicit_paths() {
        let cli = parse(&[
            "arc-snapshots",
            "download",
            "--execution-url",
            "http://example.com/el.tar.lz4",
            "--consensus-url",
            "http://example.com/cl.tar.lz4",
            "--execution-path",
            "/tmp/el",
            "--consensus-path",
            "/tmp/cl",
        ])
        .unwrap();
        let Commands::Download(args) = cli.command;
        assert_eq!(
            args.execution_url.as_deref(),
            Some("http://example.com/el.tar.lz4")
        );
        assert_eq!(
            args.consensus_url.as_deref(),
            Some("http://example.com/cl.tar.lz4")
        );
        assert_eq!(args.execution_path, Some(PathBuf::from("/tmp/el")));
        assert_eq!(args.consensus_path, Some(PathBuf::from("/tmp/cl")));
    }

    #[test]
    fn parse_download_chain_default_is_testnet() {
        let cli = parse(&["arc-snapshots", "download"]).unwrap();
        let Commands::Download(args) = cli.command;
        assert!(matches!(args.chain, Chain::Testnet));
    }

    #[test]
    fn parse_download_explicit_chain_devnet() {
        let cli = parse(&[
            "arc-snapshots",
            "download",
            "--chain",
            "arc-devnet",
            "--execution-url",
            "http://x/el",
            "--consensus-url",
            "http://x/cl",
        ])
        .unwrap();
        let Commands::Download(args) = cli.command;
        assert!(matches!(args.chain, Chain::Devnet));
    }

    #[test]
    fn parse_download_bare_chain_name_is_error() {
        // "testnet" without the "arc-" prefix must be rejected
        assert!(parse(&[
            "arc-snapshots",
            "download",
            "--chain",
            "testnet",
            "--execution-url",
            "http://x/el",
            "--consensus-url",
            "http://x/cl",
        ])
        .is_err());
    }

    #[test]
    fn parse_download_invalid_chain_is_error() {
        assert!(parse(&[
            "arc-snapshots",
            "download",
            "--chain",
            "not-a-chain",
            "--execution-url",
            "http://x/el",
            "--consensus-url",
            "http://x/cl",
        ])
        .is_err());
    }

    #[test]
    fn parse_no_subcommand_is_error() {
        assert!(parse(&["arc-snapshots"]).is_err());
    }

    #[tokio::test]
    async fn run_download_errors_with_only_one_url() {
        let args = DownloadArgs {
            execution_url: None,
            consensus_url: Some("http://x/cl".into()),
            chain: Chain::Devnet,
            execution_path: Some("/tmp/el".into()),
            consensus_path: Some("/tmp/cl".into()),
        };
        let err = run_download(args).await.unwrap_err();
        assert!(err.to_string().contains("both"));

        let args = DownloadArgs {
            execution_url: Some("http://x/el".into()),
            consensus_url: None,
            chain: Chain::Devnet,
            execution_path: Some("/tmp/el".into()),
            consensus_path: Some("/tmp/cl".into()),
        };
        let err = run_download(args).await.unwrap_err();
        assert!(err.to_string().contains("both"));
    }
}
