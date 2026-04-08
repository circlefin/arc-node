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

use arc_snapshots::download::{
    consensus_snapshot_exists, fetch_latest_snapshot_urls, should_download, stream_and_extract,
    write_snapshot_version, Chain,
};
use clap::Args;
use eyre::Result;
use tracing::info;

#[derive(Args, Clone, Debug, Default)]
pub struct DownloadCmd {
    /// URL of the CL snapshot to download.
    ///
    /// If omitted, the latest snapshot for --chain is fetched automatically.
    #[arg(long, short)]
    pub url: Option<String>,

    /// Network to download a snapshot for.
    ///
    /// [possible values: arc-testnet, arc-devnet]
    #[arg(long, default_value = "arc-testnet")]
    pub chain: String,

    /// Force re-download even if snapshot data already exists.
    #[arg(long = "force")]
    pub force_redownload: bool,
}

impl DownloadCmd {
    pub async fn run(&self, home_dir: &Path) -> Result<()> {
        let chain = parse_chain(&self.chain)?;

        let url = match &self.url {
            Some(u) => u.clone(),
            None => {
                info!(chain = %self.chain, "Fetching latest CL snapshot URL");
                let (_el_url, cl_url) = fetch_latest_snapshot_urls(chain).await?;
                cl_url
            }
        };

        if !should_download(
            "Consensus layer",
            home_dir,
            &url,
            consensus_snapshot_exists(home_dir),
            self.force_redownload,
        ) {
            return Ok(());
        }

        let tmp_dir = home_dir.join(".snapshot-tmp");

        info!(
            url = %url,
            home_dir = %home_dir.display(),
            "Starting CL snapshot download"
        );

        stream_and_extract(url.clone(), home_dir.to_path_buf(), tmp_dir).await?;
        write_snapshot_version(home_dir, &url)?;

        info!("CL snapshot downloaded and extracted successfully");
        Ok(())
    }
}

fn parse_chain(name: &str) -> Result<Chain> {
    match name {
        "arc-testnet" => Ok(Chain::Testnet),
        "arc-devnet" => Ok(Chain::Devnet),
        other => Err(eyre::eyre!(
            "Unknown chain '{}'. Valid values: arc-testnet, arc-devnet",
            other
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chain_known_values() {
        assert!(matches!(
            parse_chain("arc-testnet").unwrap(),
            Chain::Testnet
        ));
        assert!(matches!(parse_chain("arc-devnet").unwrap(), Chain::Devnet));
    }

    #[test]
    fn parse_chain_unknown_is_error() {
        assert!(parse_chain("unknown").is_err());
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
        let cmd = DownloadCmd {
            url: Some(format!("{}/cl.tar.lz4", server.uri())),
            chain: "arc-devnet".into(),
            force_redownload: false,
        };

        let url = format!("{}/cl.tar.lz4", server.uri());
        cmd.run(dir.path()).await?;

        assert!(dir.path().join("store.db").exists());
        // Version marker should be written
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".snapshot-url"))?,
            url
        );
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
        let url = format!("{}/cl.tar.lz4", server.uri());

        // Pre-populate data and matching marker
        std::fs::write(dir.path().join("store.db"), b"existing")?;
        std::fs::write(dir.path().join(".snapshot-url"), &url)?;

        let cmd = DownloadCmd {
            url: Some(url),
            chain: "arc-devnet".into(),
            force_redownload: false,
        };
        cmd.run(dir.path()).await?;

        // Data should be untouched
        assert_eq!(std::fs::read(dir.path().join("store.db"))?, b"existing");

        drop(mock);
        Ok(())
    }

    #[tokio::test]
    async fn run_errors_on_unknown_chain() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = DownloadCmd {
            url: Some("http://example.com/cl.tar.lz4".into()),
            chain: "not-a-chain".into(),
            force_redownload: false,
        };
        assert!(cmd.run(dir.path()).await.is_err());
    }
}
