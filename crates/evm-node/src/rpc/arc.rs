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

//! Top level file of `reth` node API extension RPCs

use crate::rpc::common::ARC_DEFAULT_BASE_URL;
use crate::rpc::get_certificate::{
    rpc_get_certificate, CertificateSource, HttpCertificateSource, RpcCommitCertificate,
};
use crate::rpc::get_version::{rpc_get_version, RpcVersionInfo};
use async_trait::async_trait;
use jsonrpsee::{core::RpcResult, proc_macros::rpc, RpcModule};

pub type DefaultArcApiImpl = ArcApiImpl<HttpCertificateSource>;

/// ARC namespace exposing RPC methods. Full reference: `crates/node/src/rpc/openapi/openapi.yaml`.
#[rpc(server, namespace = "arc")]
#[async_trait]
pub trait ArcApi {
    /// Returns version information for the execution layer
    #[method(name = "getVersion")]
    fn version(&self) -> RpcResult<RpcVersionInfo>;

    #[method(name = "getCertificate")]
    async fn get_certificate(&self, height: u64) -> RpcResult<RpcCommitCertificate>;
}

pub struct ArcApiImpl<S: CertificateSource + 'static> {
    certificate_source: S,
}

impl<S: CertificateSource> ArcApiImpl<S> {
    pub fn new(certificate_source: S) -> Self {
        Self { certificate_source }
    }
}

#[async_trait]
impl<S: CertificateSource + 'static> ArcApiServer for ArcApiImpl<S> {
    async fn get_certificate(&self, height: u64) -> RpcResult<RpcCommitCertificate> {
        rpc_get_certificate(&self.certificate_source, height).await
    }
    fn version(&self) -> RpcResult<RpcVersionInfo> {
        rpc_get_version()
    }
}

pub fn build_arc_rpc_module(
    base_url: Option<String>,
) -> eyre::Result<RpcModule<ArcApiImpl<HttpCertificateSource>>> {
    let base = base_url.unwrap_or_else(|| ARC_DEFAULT_BASE_URL.to_string());
    let certificate_source = HttpCertificateSource::new(base)?;
    Ok(ArcApiImpl::new(certificate_source).into_rpc())
}
