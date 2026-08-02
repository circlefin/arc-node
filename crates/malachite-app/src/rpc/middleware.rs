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

//! Middleware for API version extraction and negotiation, and for the admin
//! credential the privileged routes require.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::debug;

use arc_consensus_types::AdminToken;

use super::version::ApiVersion;

/// Axum middleware that rejects a request unless it presents the configured admin
/// token as `Authorization: Bearer <token>`.
///
/// This is applied with `Router::route_layer`, so it only runs for the privileged
/// routes. Those routes are not registered at all when no token is configured,
/// which keeps peer mutation off a node that has not opted in.
pub async fn require_admin_token(
    State(expected): State<AdminToken>,
    req: Request,
    next: Next,
) -> Response {
    let Some(presented) = bearer_token(&req) else {
        debug!(
            path = %req.uri().path(),
            "Privileged RPC request without a bearer token, returning 401"
        );
        return unauthorized("Missing bearer token");
    };

    if !expected.matches(presented) {
        debug!(
            path = %req.uri().path(),
            "Privileged RPC request with a bearer token that does not match, returning 401"
        );
        return unauthorized("Invalid admin token");
    }

    next.run(req).await
}

/// Extract the credential from an `Authorization: Bearer <token>` header.
fn bearer_token(req: &Request) -> Option<&str> {
    let value = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;

    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }

    let token = token.trim();
    if token.is_empty() {
        return None;
    }

    Some(token)
}

fn unauthorized(message: &'static str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        axum::Json(json!({ "error": message })),
    )
        .into_response()
}

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

    #[test]
    fn test_bearer_token_parsing() {
        let with_auth = |value: &str| {
            Request::builder()
                .header(header::AUTHORIZATION, value)
                .body(axum::body::Body::empty())
                .unwrap()
        };

        assert_eq!(bearer_token(&with_auth("Bearer s3cret")), Some("s3cret"));
        assert_eq!(bearer_token(&with_auth("bearer s3cret")), Some("s3cret"));
        assert_eq!(bearer_token(&with_auth("Bearer  s3cret ")), Some("s3cret"));
        assert_eq!(bearer_token(&with_auth("Basic s3cret")), None);
        assert_eq!(bearer_token(&with_auth("Bearer")), None);
        assert_eq!(bearer_token(&with_auth("Bearer ")), None);
        assert_eq!(
            bearer_token(&Request::builder().body(axum::body::Body::empty()).unwrap()),
            None
        );
    }
}
