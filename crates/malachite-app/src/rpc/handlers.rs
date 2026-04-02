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
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use malachitebft_app_channel::ConsensusRequest;
use malachitebft_app_channel::NetworkRequest;

use crate::request::AppRequest;
use crate::request::TxAppReq;
use crate::rpc::types::persistent_peer_error_to_response;
use crate::rpc::types::request_error_to_response;
use crate::rpc::types::RpcVersion;
use crate::rpc::version::ApiVersion;

use super::types::{
    AddOrRemovePersistentPeerBody, GetCertificateParams, RpcAppStatus, RpcCommitCertificate,
    RpcConsensusStateDump, RpcInvalidPayloads, RpcMisbehaviorEvidence, RpcNetworkStateDump,
};
use super::types::{TxConsensusReq, TxNetworkReq};

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
    tx_app_req: State<TxAppReq>,
    query: Query<GetCertificateParams>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_commit called");

    AppRequest::get_certificate(query.height, &tx_app_req)
        .await
        .map_err(request_error_to_response)?
        .map(|cert| Json(RpcCommitCertificate::from(cert)))
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Certificate not found"})),
            )
        })
}

pub(crate) async fn get_misbehavior_evidence(
    tx_app_req: State<TxAppReq>,
    query: Query<GetCertificateParams>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_misbehavior_evidence called");

    AppRequest::get_misbehavior_evidence(query.height, &tx_app_req)
        .await
        .map_err(request_error_to_response)?
        .map(|evidence| Json(RpcMisbehaviorEvidence::from(evidence)))
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Misbehavior evidence not found"})),
            )
        })
}

pub(crate) async fn get_proposal_monitor(
    tx_app_req: State<TxAppReq>,
    query: Query<super::types::GetProposalMonitorParams>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_proposal_monitor called");

    match AppRequest::get_proposal_monitor_data(query.height, &tx_app_req).await {
        Ok(Some(data)) => {
            Ok(Json(super::types::RpcProposalMonitorData::from(data)).into_response())
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Proposal monitor data not found"})),
        )
            .into_response()),
        Err(e) => Err(request_error_to_response(e).into_response()),
    }
}

pub(crate) async fn get_invalid_payloads(
    tx_app_req: State<TxAppReq>,
    query: Query<GetCertificateParams>,
    Extension(version): Extension<ApiVersion>,
) -> impl IntoResponse {
    tracing::debug!(?version, "get_invalid_payloads called");

    AppRequest::get_invalid_payloads(query.height, &tx_app_req)
        .await
        .map_err(request_error_to_response)?
        .map(|payloads| Json(RpcInvalidPayloads::from(payloads)))
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Invalid payloads not found"})),
            )
        })
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
