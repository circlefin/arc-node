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

//! Download command for the consensus layer.
//!
//! Downloads a CL snapshot archive and extracts bare paths (e.g. `store.db`) directly
//! into the home directory.

use std::path::Path;

use arc_snapshots::download::{fetch_latest_consensus_url, stream_restore_consensus, Chain};
use clap::Args;
use eyre::Result;
use tracing::info;

#[derive(Args, Clone, Debug)]
pub struct DownloadCmd {
    /// URL of the CL snapshot to download.
    ///
    /// If omitted, the latest snapshot for --chain is fetched automatically.
    #[arg(long, short)]
    pub url: Option<String>,

    /// Network to download a snapshot for.
    #[arg(long, default_value = "arc-testnet")]
    pub chain: Chain,

    /// Force re-download even if snapshot data already exists.
    #[arg(long = "force")]
    pub force_redownload: bool,
}

impl DownloadCmd {
    pub async fn run(&self, home_dir: &Path) -> Result<()> {
        let url = match &self.url {
            Some(u) => u.clone(),
            None => {
                info!(chain = %self.chain, "Fetching latest CL snapshot URL");
                fetch_latest_consensus_url(self.chain).await?
            }
        };

        info!(
            url = %url,
            home_dir = %home_dir.display(),
            "Starting CL snapshot download"
        );

        // One implementation, shared with arc-snapshots: it decides whether the
        // restore is needed, stages the archive, and records what was restored.
        //
        // Staging inside the home is safe here and nowhere else: the consensus
        // restore never removes its target, and the node reads `store.db`,
        // `config/` and `wal/` — never `.snapshot-tmp`. Keeping it inside also
        // keeps the archive on the volume the operator gave the home, which a
        // parent directory is not guaranteed to be.
        stream_restore_consensus(
            url,
            home_dir.to_path_buf(),
            home_dir.join(".snapshot-tmp"),
            self.force_redownload,
        )
        .await?;

        info!("CL snapshot restore complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// Wraps the command so `--chain` can be parsed the way the real CLI parses
    /// it, rather than through a second hand-written parser.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        cmd: DownloadCmd,
    }

    fn parse_chain_arg(value: &str) -> Result<Chain, clap::Error> {
        TestCli::try_parse_from(["arc-node-consensus", "--chain", value]).map(|cli| cli.cmd.chain)
    }

    #[test]
    fn chain_accepts_every_supported_network() {
        // arc-mainnet in particular: the CL used to reject it while the
        // execution side and the docs both advertised it.
        assert!(matches!(
            parse_chain_arg("arc-testnet").unwrap(),
            Chain::Testnet
        ));
        assert!(matches!(
            parse_chain_arg("arc-devnet").unwrap(),
            Chain::Devnet
        ));
        assert!(matches!(
            parse_chain_arg("arc-mainnet").unwrap(),
            Chain::Mainnet
        ));
    }

    #[test]
    fn chain_rejects_unknown_and_unprefixed_values() {
        assert!(parse_chain_arg("unknown").is_err());
        // The bare network name is what the snapshot API uses, not the CLI.
        assert!(parse_chain_arg("testnet").is_err());
    }

    #[test]
    fn chain_defaults_to_testnet() {
        let cli = TestCli::try_parse_from(["arc-node-consensus"]).unwrap();
        assert!(matches!(cli.cmd.chain, Chain::Testnet));
    }

    #[tokio::test]
    async fn run_extracts_cl_snapshot_into_home_dir() -> eyre::Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Build a minimal CL archive with bare paths
        let buf = Vec::new();
        let encoder = lz4::EncoderBuilder::new().build(buf)?;
        let mut builder = tar::Builder::new(encoder);
        let content = b"consensus-store";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "store.db", content.as_ref())?;
        let (data, result) = builder.into_inner()?.finish();
        result?;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(data.clone())
                    .append_header("Content-Length", data.len().to_string().as_str()),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let home = dir.path().join("consensus");
        let cmd = DownloadCmd {
            url: Some(format!("{}/cl.tar.lz4", server.uri())),
            chain: Chain::Devnet,
            force_redownload: false,
        };

        let url = format!("{}/cl.tar.lz4", server.uri());
        cmd.run(&home).await?;

        assert!(home.join("store.db").exists());
        // Version marker should be written
        assert_eq!(std::fs::read_to_string(home.join(".snapshot-url"))?, url);
        // Staging is cleaned up, so the ~14 GB archive does not sit in the home
        // beside the store it was unpacked into.
        assert!(!home.join(".snapshot-tmp").exists());
        Ok(())
    }

    #[tokio::test]
    async fn run_skips_when_url_matches() -> eyre::Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let buf = Vec::new();
        let encoder = lz4::EncoderBuilder::new().build(buf)?;
        let mut builder = tar::Builder::new(encoder);
        let content = b"consensus-store";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "store.db", content.as_ref())?;
        let (data, result) = builder.into_inner()?.finish();
        result?;

        let server = MockServer::start().await;
        let mock = Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(data.clone())
                    .append_header("Content-Length", data.len().to_string().as_str()),
            )
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let home = dir.path().join("consensus");
        std::fs::create_dir_all(&home)?;
        let url = format!("{}/cl.tar.lz4", server.uri());

        // Pre-populate data and matching marker
        std::fs::write(home.join("store.db"), b"existing")?;
        std::fs::write(home.join(".snapshot-url"), &url)?;

        let cmd = DownloadCmd {
            url: Some(url),
            chain: Chain::Devnet,
            force_redownload: false,
        };
        cmd.run(&home).await?;

        // Data should be untouched
        assert_eq!(std::fs::read(home.join("store.db"))?, b"existing");

        drop(mock);
        Ok(())
    }
}
