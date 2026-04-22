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

use crate::cli::CommonArgs;
use arc_eth_engine::{
    engine::EngineAPI, ipc::engine_ipc::EngineIPC, rpc::engine_rpc::EngineRpc,
    rpc::ethereum_rpc::EthereumRPC,
};
use chrono::Utc;
use eyre::{bail, Context};
use reqwest::Url;
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    time::Duration,
};

const ETH_BATCH_TIMEOUT_FLOOR: Duration = Duration::from_secs(30);

/// Which Engine API transport was selected on the CLI.
pub(crate) enum EngineTransport {
    Ipc(PathBuf),
    Rpc { url: String, jwt_secret: PathBuf },
}

impl fmt::Display for EngineTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ipc(path) => write!(f, "ipc:{}", path.display()),
            Self::Rpc { url, .. } => write!(f, "rpc:{url}"),
        }
    }
}

pub(crate) struct BenchContext {
    transport: EngineTransport,
    output_dir: PathBuf,
    eth_rpc_timeout: Duration,
}

impl fmt::Debug for BenchContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BenchContext")
            .field("transport", &self.transport.to_string())
            .field("output_dir", &self.output_dir)
            .field("eth_rpc_timeout", &self.eth_rpc_timeout)
            .finish()
    }
}

impl BenchContext {
    pub(crate) fn new(common: &CommonArgs, mode: &str) -> eyre::Result<Self> {
        let transport = match (&common.engine_ipc, &common.engine_rpc_url) {
            (Some(ipc), None) => EngineTransport::Ipc(ipc.clone()),
            (None, Some(url)) => {
                let jwt = common
                    .jwt_secret
                    .as_ref()
                    .ok_or_else(|| eyre::eyre!("--jwt-secret is required with --engine-rpc-url"))?;
                if !jwt.exists() {
                    bail!("JWT secret file does not exist: {}", jwt.display());
                }
                EngineTransport::Rpc {
                    url: url.clone(),
                    jwt_secret: jwt.clone(),
                }
            }
            (None, None) => bail!(
                "specify either --engine-ipc <PATH> or --engine-rpc-url <URL> --jwt-secret <PATH>"
            ),
            _ => unreachable!("clap group prevents both"),
        };

        Ok(Self {
            transport,
            output_dir: resolve_output_dir(common, mode)?,
            eth_rpc_timeout: Duration::from_millis(common.eth_rpc_timeout_ms),
        })
    }

    pub(crate) fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    pub(crate) fn transport(&self) -> &EngineTransport {
        &self.transport
    }

    pub(crate) async fn engine(&self) -> eyre::Result<Box<dyn EngineAPI>> {
        match &self.transport {
            EngineTransport::Ipc(path) => {
                let path_str = path.to_str().ok_or_else(|| {
                    eyre::eyre!(
                        "engine IPC socket path is not valid UTF-8: {}",
                        path.display()
                    )
                })?;
                let ipc = EngineIPC::new(path_str)
                    .await
                    .wrap_err("failed to create engine IPC client")?;
                Ok(Box::new(ipc) as Box<dyn EngineAPI>)
            }
            EngineTransport::Rpc { url, jwt_secret } => {
                let rpc = EngineRpc::new(
                    Url::parse(url).wrap_err("invalid engine RPC URL")?,
                    jwt_secret.as_path(),
                )
                .wrap_err("failed to create engine RPC client")?;
                Ok(Box::new(rpc) as Box<dyn EngineAPI>)
            }
        }
    }

    pub(crate) fn ethereum_rpc(&self, rpc_url: &str, role: &str) -> eyre::Result<EthereumRPC> {
        ethereum_rpc_client(rpc_url, role, self.eth_rpc_timeout)
    }
}

pub(crate) fn ethereum_rpc_client(
    rpc_url: &str,
    role: &str,
    eth_rpc_timeout: Duration,
) -> eyre::Result<EthereumRPC> {
    EthereumRPC::new_with_timeouts(
        Url::parse(rpc_url).wrap_err_with(|| format!("invalid {role} url"))?,
        eth_rpc_timeout,
        eth_rpc_timeout.max(ETH_BATCH_TIMEOUT_FLOOR),
    )
    .wrap_err_with(|| format!("failed to create {role} client"))
}

fn resolve_output_dir(common: &CommonArgs, mode: &str) -> eyre::Result<PathBuf> {
    let output = match &common.output {
        Some(path) => path.clone(),
        None => {
            let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
            PathBuf::from("target")
                .join("engine-bench")
                .join(format!("{mode}-{timestamp}"))
        }
    };
    fs::create_dir_all(&output)
        .wrap_err_with(|| format!("failed to create benchmark output dir {}", output.display()))?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn common_args_ipc() -> CommonArgs {
        CommonArgs {
            engine_ipc: Some(PathBuf::from("/tmp/reth.ipc")),
            engine_rpc_url: None,
            jwt_secret: None,
            eth_rpc_timeout_ms: 10_000,
            output: None,
        }
    }

    fn common_args_rpc(jwt_path: PathBuf) -> CommonArgs {
        CommonArgs {
            engine_ipc: None,
            engine_rpc_url: Some("http://127.0.0.1:8551".to_string()),
            jwt_secret: Some(jwt_path),
            eth_rpc_timeout_ms: 10_000,
            output: None,
        }
    }

    #[test]
    fn resolve_output_dir_creates_explicit_output_directory() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("bench-output");
        let mut args = common_args_ipc();
        args.output = Some(output_dir.clone());

        let resolved = resolve_output_dir(&args, "new-payload-fcu").unwrap();

        assert_eq!(resolved, output_dir);
        assert!(resolved.is_dir());
    }

    #[test]
    fn new_context_selects_ipc_transport() {
        let args = common_args_ipc();
        let ctx = BenchContext::new(&args, "test").unwrap();
        assert!(matches!(ctx.transport(), EngineTransport::Ipc(_)));
    }

    #[test]
    fn new_context_selects_rpc_transport() {
        let temp_dir = TempDir::new().unwrap();
        let jwt_path = temp_dir.path().join("jwt.hex");
        fs::write(&jwt_path, "secret").unwrap();

        let args = common_args_rpc(jwt_path);
        let ctx = BenchContext::new(&args, "test").unwrap();
        assert!(matches!(ctx.transport(), EngineTransport::Rpc { .. }));
    }

    #[test]
    fn new_context_errors_when_neither_transport_specified() {
        let args = CommonArgs {
            engine_ipc: None,
            engine_rpc_url: None,
            jwt_secret: None,
            eth_rpc_timeout_ms: 10_000,
            output: None,
        };

        let err = BenchContext::new(&args, "test").unwrap_err();
        assert!(err.to_string().contains("--engine-ipc"));
    }

    #[test]
    fn new_context_errors_when_rpc_without_jwt() {
        let args = CommonArgs {
            engine_ipc: None,
            engine_rpc_url: Some("http://127.0.0.1:8551".to_string()),
            jwt_secret: None,
            eth_rpc_timeout_ms: 10_000,
            output: None,
        };

        let err = BenchContext::new(&args, "test").unwrap_err();
        assert!(err.to_string().contains("--jwt-secret"));
    }

    #[test]
    fn new_context_errors_when_jwt_file_missing() {
        let args = common_args_rpc(PathBuf::from("/tmp/nonexistent-jwt.hex"));
        let err = BenchContext::new(&args, "test").unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }
}
