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

//! Middleware for API version extraction and negotiation

use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::debug;

use super::version::ApiVersion;

/// Axum middleware that extracts the API version from the Accept header
/// and stores it in the request extensions.
///
/// If the Accept header is missing or contains `application/json`,
/// defaults to the current API version (V1).
///
/// If the Accept header specifies an unsupported version,
/// returns a 406 Not Acceptable response.
pub async fn extract_version(mut req: Request, next: Next) -> Response {
    // Extract Accept header
    let accept_header = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|h| match h.to_str() {
            Ok(s) => Some(s),
            Err(err) => {
                debug!(?err, "Accept header is not a valid string");
                None
            }
        })
        .unwrap_or("");

    // Parse version from header
    match ApiVersion::from_accept_header(accept_header) {
        Some(version) => {
            debug!(?version, accept_header, "API version extracted");

            // Store version in request extensions
            req.extensions_mut().insert(version);

            // Continue to handler
            let mut response = next.run(req).await;

            // Add Content-Type header to response
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                version
                    .media_type()
                    .parse()
                    .expect("valid content-type header"), // okay since we know it's valid (see ApiVersion::media_type())
            );

            response
        }
        None => {
            // Unsupported version
            debug!(
                accept_header,
                "Unsupported API version requested, returning 406"
            );

            let body = json!({
                "error": "Unsupported API version",
                "supported_versions": [ApiVersion::V1.to_string()],
                "message": format!(
                    "The requested API version is not supported. Please use Accept: {} for {}.",
                    ApiVersion::V1.media_type(),
                    ApiVersion::V1.to_string()
                )
            });

            (StatusCode::NOT_ACCEPTABLE, axum::Json(body)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Integration tests for version negotiation are in the parent module's tests
    // These unit tests verify the middleware logic in isolation

    #[test]
    fn test_version_parsing() {
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1+json"),
            Some(ApiVersion::V1)
        );
        assert_eq!(
            ApiVersion::from_accept_header("application/json"),
            Some(ApiVersion::V1)
        );
        assert_eq!(ApiVersion::from_accept_header(""), Some(ApiVersion::V1));
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v99+json"),
            None
        );
    }
}
