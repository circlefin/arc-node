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

use std::io;

use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use tokio::net::{TcpListener, ToSocketAddrs};
use tracing::{error, info};

use malachitebft_app::metrics::export;

const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

#[tracing::instrument(name = "metrics", skip_all)]
pub async fn serve(listen_addr: impl ToSocketAddrs) {
    if let Err(e) = inner(listen_addr).await {
        error!("Metrics server failed: {e}");
    }
}

async fn inner(listen_addr: impl ToSocketAddrs) -> io::Result<()> {
    let app = Router::new().route("/metrics", get(get_metrics));
    let listener = TcpListener::bind(listen_addr).await?;
    let local_addr = listener.local_addr()?;

    info!(address = %local_addr, "Serving metrics");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn get_metrics() -> impl IntoResponse {
    let mut buf = String::new();
    export(&mut buf);

    ([("Content-Type", CONTENT_TYPE)], buf)
}
