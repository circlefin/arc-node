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

use axum::extract::Extension;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use malachitebft_app_channel::ConsensusRequest;
use malachitebft_app_channel::NetworkRequest;

use arc_consensus_types::Height;

use crate::metrics::AppMetrics;
use crate::request::{AppRequest, AppRequestError, HeightRangeRequest, TxAppReq};
use crate::rpc::types::persistent_peer_error_to_response;
use crate::rpc::types::request_error_to_response;
use crate::rpc::types::RpcVersion;
use crate::rpc::types::{range_error, RpcRangeReason};
use crate::rpc::version::ApiVersion;
use crate::store::RangeQueryResult;
use crate::utils::sync_state::SyncState;

use super::types::{
    AddOrRemovePersistentPeerBody, HeightRangeParams, RpcAppStatus, RpcCommitCertificate,
    RpcConsensusStateDump, RpcInvalidPayloads, RpcMisbehaviorEvidence, RpcNetworkStateDump,
    RpcProposalMonitorData,
};
use super::types::{TxConsensusReq, TxNetworkReq};

/// Maximum number of heights one range query may request. Not configurable.
/// Requests above it are rejected.
const MAX_RANGE_COUNT: u64 = 1000;

/// Validated outcome of the `height`/`count` query parameters.
#[derive(Debug, PartialEq)]
enum ResolvedQuery {
    /// `count` omitted or `1`: the legacy single-height path (`None` = latest).
    Single(Option<Height>),
    /// `count > 1`: a validated forward range and its inclusive upper bound.
    Range { range: HeightRangeRequest, to: u64 },
}

/// A range request rejected before reaching the consensus task.
#[derive(Debug, PartialEq)]
enum BadRange {
    /// `count` was 0.
    Zero,
    /// `count > 1` without an explicit `height` to anchor the range.
    NoAnchor,
    /// `height + count - 1` overflows `u64`.
    Overflow,
    /// `count` exceeds `MAX_RANGE_COUNT`; carries the requested bounds for the
    /// structured error body.
    OverLimit { from: u64, to: u64 },
}

/// Resolve `height`/`count` into a [`ResolvedQuery`] or a [`BadRange`] rejection.
fn resolve_query(height: Option<Height>, count: Option<u64>) -> Result<ResolvedQuery, BadRange> {
    let count = match count {
        None | Some(1) => return Ok(ResolvedQuery::Single(height)),
        Some(0) => return Err(BadRange::Zero),
        Some(count) => count,
    };

    // A multi-height range needs an explicit anchor.
    let Some(from) = height else {
        return Err(BadRange::NoAnchor);
    };

    // Overflow is checked before the cap so `to` is always exact for the
    // over_limit body. count >= 2 here, so checked_sub(1) is always Some.
    let Some(to) = count
        .checked_sub(1)
        .and_then(|offset| from.as_u64().checked_add(offset))
    else {
        return Err(BadRange::Overflow);
    };

    if count > MAX_RANGE_COUNT {
        return Err(BadRange::OverLimit {
            from: from.as_u64(),
            to,
        });
    }

    Ok(ResolvedQuery::Range {
        range: HeightRangeRequest { from, count },
        to,
    })
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

/// Render a rejected range. Argument errors get a plain `{"error": ...}` body;
/// only `over_limit` uses the structured body (it is a range-availability fact).
fn render_bad_range(bad: BadRange) -> Response {
    match bad {
        BadRange::Zero => bad_request("count must be at least 1"),
        BadRange::NoAnchor => bad_request("height is required when count is greater than 1"),
        BadRange::Overflow => bad_request("requested range exceeds the maximum u64 height"),
        BadRange::OverLimit { from, to } => {
            range_error(from, to, RpcRangeReason::OverLimit, Vec::new()).into_response()
        }
    }
}

pub(crate) async fn get_consensus_state(
    tx_consensus_req: State<TxConsensusReq>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_consensus_state called");

    match ConsensusRequest::dump_state(&tx_consensus_req).await {
        Ok(Some(state)) => Json(RpcConsensusStateDump::from(&state)).into_response(),
        Ok(None) => {
            let body = Json(json!({ "error": "Consensus state not available" }));
            (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
        }
        Err(e) => request_error_to_response(e).into_response(),
    }
}

pub(crate) async fn get_network_state(
    tx_network_req: State<TxNetworkReq>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_network_state called");

    match NetworkRequest::dump_state(&tx_network_req).await {
        Ok(Some(state)) => Json(RpcNetworkStateDump::from(state)).into_response(),
        Ok(None) => {
            let body = Json(json!({ "error": "Network state not available" }));
            (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
        }
        Err(e) => request_error_to_response(e).into_response(),
    }
}

pub(crate) async fn get_commit(
    metrics: State<AppMetrics>,
    tx_app_req: State<TxAppReq>,
    query: Query<HeightRangeParams>,
    Extension(version): Extension<ApiVersion>,
) -> Response {
    let _guard = metrics.start_rpc_request_timer("/commit");
    tracing::debug!(?version, "get_commit called");

    match resolve_query(query.height, query.count) {
        Err(bad) => render_bad_range(bad),
        Ok(ResolvedQuery::Single(height)) => {
            let result = AppRequest::get_certificate(height, &tx_app_req).await;
            if matches!(result, Err(AppRequestError::Full)) {
                metrics.inc_app_request_full_count("GetCertificate");
            }
            match result {
                Err(e) => request_error_to_response(e).into_response(),
                Ok(Some(cert)) => Json(RpcCommitCertificate::from(cert)).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Certificate not found"})),
                )
                    .into_response(),
            }
        }
        Ok(ResolvedQuery::Range { range, to }) => {
            let from = range.from.as_u64();
            let result = AppRequest::get_certificate_range(range, &tx_app_req).await;
            if matches!(result, Err(AppRequestError::Full)) {
                metrics.inc_app_request_full_count("GetCertificate");
            }
            match result {
                Err(e) => request_error_to_response(e).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Certificate not found"})),
                )
                    .into_response(),
                Ok(Some(RangeQueryResult::Complete(items))) => Json(
                    items
                        .into_iter()
                        .map(RpcCommitCertificate::from)
                        .collect::<Vec<_>>(),
                )
                .into_response(),
                Ok(Some(RangeQueryResult::Unavailable {
                    reason,
                    failed_heights,
                })) => range_error(from, to, reason.into(), failed_heights).into_response(),
            }
        }
    }
}

pub(crate) async fn get_misbehavior_evidence(
    tx_app_req: State<TxAppReq>,
    query: Query<HeightRangeParams>,
    Extension(version): Extension<ApiVersion>,
) -> Response {
    tracing::debug!(?version, "get_misbehavior_evidence called");

    match resolve_query(query.height, query.count) {
        Err(bad) => render_bad_range(bad),
        Ok(ResolvedQuery::Single(height)) => {
            match AppRequest::get_misbehavior_evidence(height, &tx_app_req).await {
                Err(e) => request_error_to_response(e).into_response(),
                Ok(Some(evidence)) => Json(RpcMisbehaviorEvidence::from(evidence)).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Misbehavior evidence not found"})),
                )
                    .into_response(),
            }
        }
        Ok(ResolvedQuery::Range { range, to }) => {
            let from = range.from.as_u64();
            match AppRequest::get_misbehavior_evidence_range(range, &tx_app_req).await {
                Err(e) => request_error_to_response(e).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Misbehavior evidence not found"})),
                )
                    .into_response(),
                Ok(Some(RangeQueryResult::Complete(items))) => Json(
                    items
                        .into_iter()
                        .map(RpcMisbehaviorEvidence::from)
                        .collect::<Vec<_>>(),
                )
                .into_response(),
                Ok(Some(RangeQueryResult::Unavailable {
                    reason,
                    failed_heights,
                })) => range_error(from, to, reason.into(), failed_heights).into_response(),
            }
        }
    }
}

pub(crate) async fn get_proposal_monitor(
    tx_app_req: State<TxAppReq>,
    query: Query<HeightRangeParams>,
    Extension(version): Extension<ApiVersion>,
) -> Response {
    tracing::debug!(?version, "get_proposal_monitor called");

    match resolve_query(query.height, query.count) {
        Err(bad) => render_bad_range(bad),
        Ok(ResolvedQuery::Single(height)) => {
            match AppRequest::get_proposal_monitor_data(height, &tx_app_req).await {
                Err(e) => request_error_to_response(e).into_response(),
                Ok(Some(data)) => Json(RpcProposalMonitorData::from(data)).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Proposal monitor data not found"})),
                )
                    .into_response(),
            }
        }
        Ok(ResolvedQuery::Range { range, to }) => {
            let from = range.from.as_u64();
            match AppRequest::get_proposal_monitor_data_range(range, &tx_app_req).await {
                Err(e) => request_error_to_response(e).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Proposal monitor data not found"})),
                )
                    .into_response(),
                Ok(Some(RangeQueryResult::Complete(items))) => Json(
                    items
                        .into_iter()
                        .map(RpcProposalMonitorData::from)
                        .collect::<Vec<_>>(),
                )
                .into_response(),
                Ok(Some(RangeQueryResult::Unavailable {
                    reason,
                    failed_heights,
                })) => range_error(from, to, reason.into(), failed_heights).into_response(),
            }
        }
    }
}

pub(crate) async fn get_invalid_payloads(
    tx_app_req: State<TxAppReq>,
    query: Query<HeightRangeParams>,
    Extension(version): Extension<ApiVersion>,
) -> Response {
    tracing::debug!(?version, "get_invalid_payloads called");

    match resolve_query(query.height, query.count) {
        Err(bad) => render_bad_range(bad),
        Ok(ResolvedQuery::Single(height)) => {
            match AppRequest::get_invalid_payloads(height, &tx_app_req).await {
                Err(e) => request_error_to_response(e).into_response(),
                Ok(Some(payloads)) => Json(RpcInvalidPayloads::from(payloads)).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Invalid payloads not found"})),
                )
                    .into_response(),
            }
        }
        Ok(ResolvedQuery::Range { range, to }) => {
            let from = range.from.as_u64();
            match AppRequest::get_invalid_payloads_range(range, &tx_app_req).await {
                Err(e) => request_error_to_response(e).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Invalid payloads not found"})),
                )
                    .into_response(),
                Ok(Some(RangeQueryResult::Complete(items))) => Json(
                    items
                        .into_iter()
                        .map(RpcInvalidPayloads::from)
                        .collect::<Vec<_>>(),
                )
                .into_response(),
                Ok(Some(RangeQueryResult::Unavailable {
                    reason,
                    failed_heights,
                })) => range_error(from, to, reason.into(), failed_heights).into_response(),
            }
        }
    }
}

pub(crate) async fn get_status(
    tx_app_req: State<TxAppReq>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_status called");

    AppRequest::get_status(&tx_app_req)
        .await
        .map(|cert| Json(RpcAppStatus::from(cert)))
        .map_err(request_error_to_response)
}

pub(crate) async fn get_health(
    tx_app_req: State<TxAppReq>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_health called");

    AppRequest::get_health(&tx_app_req)
        .await
        .map(|()| Json(json!({ "status": "ok" })))
        .map_err(request_error_to_response)
}

pub(crate) async fn get_ready(
    tx_app_req: State<TxAppReq>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_ready called");

    match AppRequest::get_sync_state(&tx_app_req).await {
        Ok(sync_state) => {
            let status_code = match sync_state {
                SyncState::InSync => StatusCode::OK,
                SyncState::CatchingUp => StatusCode::SERVICE_UNAVAILABLE,
            };
            (status_code, Json(json!({ "sync_state": sync_state }))).into_response()
        }
        Err(e) => request_error_to_response(e).into_response(),
    }
}

pub(crate) async fn get_version(Extension(version): Extension<ApiVersion>) -> impl IntoResponse {
    tracing::debug!(?version, "get_version called");

    Json(RpcVersion {
        git_version: arc_version::GIT_VERSION,
        git_commit: arc_version::GIT_COMMIT_HASH,
        git_short_hash: arc_version::GIT_SHORT_HASH,
        cargo_version: arc_version::SHORT_VERSION,
    })
}

pub(crate) async fn add_persistent_peer(
    tx_network_req: State<TxNetworkReq>,
    Extension(version): Extension<ApiVersion>,
    Json(body): Json<AddOrRemovePersistentPeerBody>,
) -> impl IntoResponse {
    let addr: malachitebft_app_channel::app::net::Multiaddr = match body.addr.parse() {
        Ok(a) => a,
        Err(_) => {
            let body = Json(json!({"error": "Invalid multiaddr"}));
            return (StatusCode::BAD_REQUEST, body).into_response();
        }
    };

    tracing::debug!(?version, ?addr, "add_persistent_peer called");

    // For future ref: https://github.com/circlefin/malachite/pull/1485
    // let has_p2p = addr.iter().any(|p| matches!(p, multiaddr::Protocol::P2p(_)));
    // if !has_p2p {
    //     let body = Json(json!({
    //         "error": "Multiaddr must include /p2p/<peer_id>, e.g. /ip4/127.0.0.1/tcp/26656/p2p/12D3KooW..."
    //     }));
    //     return (StatusCode::BAD_REQUEST, body).into_response();
    // }

    match NetworkRequest::add_persistent_peer(&tx_network_req, addr).await {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({ "status": "ok" }))).into_response(),
        Ok(Err(e)) => persistent_peer_error_to_response(e).into_response(),
        Err(e) => request_error_to_response(e).into_response(),
    }
}

pub(crate) async fn remove_persistent_peer(
    tx_network_req: State<TxNetworkReq>,
    Extension(version): Extension<ApiVersion>,
    Json(body): Json<AddOrRemovePersistentPeerBody>,
) -> impl IntoResponse {
    let addr: malachitebft_app_channel::app::net::Multiaddr = match body.addr.parse() {
        Ok(a) => a,
        Err(_) => {
            let body = Json(json!({"error": "Invalid multiaddr"}));
            return (StatusCode::BAD_REQUEST, body).into_response();
        }
    };

    tracing::debug!(?version, ?addr, "remove_persistent_peer called");

    match NetworkRequest::remove_persistent_peer(&tx_network_req, addr).await {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({ "status": "ok" }))).into_response(),
        Ok(Err(e)) => persistent_peer_error_to_response(e).into_response(),
        Err(e) => request_error_to_response(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u64) -> Height {
        Height::new(n)
    }

    #[test]
    fn resolve_query_valid() {
        // count omitted queries a single height
        assert_eq!(resolve_query(None, None), Ok(ResolvedQuery::Single(None)));
        assert_eq!(
            resolve_query(Some(h(5)), None),
            Ok(ResolvedQuery::Single(Some(h(5))))
        );

        // count = 1 queries a single height
        assert_eq!(
            resolve_query(None, Some(1)),
            Ok(ResolvedQuery::Single(None))
        );
        assert_eq!(
            resolve_query(Some(h(5)), Some(1)),
            Ok(ResolvedQuery::Single(Some(h(5))))
        );

        // count > 1 queries a range
        assert_eq!(
            resolve_query(Some(h(10)), Some(3)),
            Ok(ResolvedQuery::Range {
                range: HeightRangeRequest {
                    from: h(10),
                    count: 3,
                },
                to: 12,
            })
        );

        // count = MAX_RANGE_COUNT queries a range at the cap
        assert_eq!(
            resolve_query(Some(h(1)), Some(MAX_RANGE_COUNT)),
            Ok(ResolvedQuery::Range {
                range: HeightRangeRequest {
                    from: h(1),
                    count: MAX_RANGE_COUNT,
                },
                to: MAX_RANGE_COUNT,
            })
        );
    }

    #[test]
    fn resolve_query_bad_range() {
        // count 0
        assert_eq!(resolve_query(Some(h(5)), Some(0)), Err(BadRange::Zero));
        assert_eq!(resolve_query(None, Some(0)), Err(BadRange::Zero));

        // count > 1 without height
        assert_eq!(resolve_query(None, Some(2)), Err(BadRange::NoAnchor));

        // count > 1 with height that overflows
        assert_eq!(
            resolve_query(Some(h(u64::MAX)), Some(2)),
            Err(BadRange::Overflow)
        );

        // count > 1 above cap
        assert_eq!(
            resolve_query(Some(h(10)), Some(MAX_RANGE_COUNT + 1)),
            Err(BadRange::OverLimit {
                from: 10,
                to: 10 + MAX_RANGE_COUNT,
            })
        );
    }
}
