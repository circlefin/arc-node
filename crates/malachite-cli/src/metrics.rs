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
use tower_http::compression::CompressionLayer;
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
    let app = metrics_router();
    let listener = TcpListener::bind(listen_addr).await?;
    let local_addr = listener.local_addr()?;

    info!(address = %local_addr, "Serving metrics");
    axum::serve(listener, app).await?;

    Ok(())
}

fn metrics_router() -> Router {
    Router::new()
        .route("/metrics", get(get_metrics))
        .layer(CompressionLayer::new())
}

async fn get_metrics() -> impl IntoResponse {
    let mut buf = String::new();
    export(&mut buf);

    ([("Content-Type", CONTENT_TYPE)], buf)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;
    use tower_http::compression::CompressionLayer;

    // The global prometheus registry is empty in tests, so using metrics_router()
    // directly would produce an empty body and skip compression. Build an
    // equivalent router with a non-empty synthetic body to exercise the layer.
    fn router_with_compression_and_non_empty_body() -> Router {
        Router::new()
            .route(
                "/metrics",
                get(|| async {
                    (
                        [("content-type", super::CONTENT_TYPE)],
                        "# HELP test A test.\n# TYPE test counter\ntest_total 1\n# EOF\n",
                    )
                }),
            )
            .layer(CompressionLayer::new())
    }

    #[tokio::test]
    async fn metrics_endpoint_compresses_when_accept_encoding_gzip() {
        let response = router_with_compression_and_non_empty_body()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(|v| v.as_bytes()),
            Some(b"gzip".as_slice()),
        );
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_plaintext_without_accept_encoding() {
        let response = router_with_compression_and_non_empty_body()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
    }
}
