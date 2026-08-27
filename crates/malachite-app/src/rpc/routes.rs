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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use eyre::Result;
use serde_json::json;
use tokio::net::{TcpListener, ToSocketAddrs};
use tower::limit::ConcurrencyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tracing::{error, info};

use super::middleware::extract_version;
use super::types::{EndpointInfo, RpcState, TxConsensusReq, TxNetworkReq};
use super::version::ApiVersion;
use crate::metrics::AppMetrics;
use crate::request::TxAppReq;

// DoS-mitigation limits for the CL RPC server.
const RPC_MAX_BODY_SIZE: usize = 2 * 1024;
const RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RPC_MAX_CONCURRENT_REQUESTS: usize = 100;

// List of RPC routes.
routes![
    route!(
        get,
        "/consensus-state",
        crate::rpc::handlers::get_consensus_state,
        "Get the current consensus state."
    ),
    route!(
        get,
        "/commit",
        crate::rpc::handlers::get_commit,
        "Get the commit certificate for a specific height or range of heights",
        params = {
            "height (optional)" => "The height of the commit certificate to retrieve. No height returns the latest certificate.",
            "count (optional)" => "Total heights to return starting at height, inclusive (forward range). Defaults to 1 (single object). When greater than 1, height is required and the response is a JSON array; capped at 1000."
        }
    ),
    route!(
        get,
        "/misbehavior-evidence",
        crate::rpc::handlers::get_misbehavior_evidence,
        "Get misbehavior evidence (double votes or proposals) for a specific height or range of heights",
        params = {
            "height (optional)" => "The height of the misbehavior evidence to retrieve. No height returns the latest.",
            "count (optional)" => "Total heights to return starting at height, inclusive (forward range). Defaults to 1 (single object). When greater than 1, height is required and the response is a JSON array; capped at 1000."
        }
    ),
    route!(
        get,
        "/proposal-monitor",
        crate::rpc::handlers::get_proposal_monitor,
        "Get round-0 proposal monitoring data (timing and success) for a specific height or range of heights",
        params = {
            "height (optional)" => "The height to get monitoring data for. No height returns the latest.",
            "count (optional)" => "Total heights to return starting at height, inclusive (forward range). Defaults to 1 (single object). When greater than 1, height is required and the response is a JSON array; capped at 1000."
        }
    ),
    route!(
        get,
        "/invalid-payloads",
        crate::rpc::handlers::get_invalid_payloads,
        "Get invalid payloads for a specific height or range of heights",
        params = {
            "height (optional)" => "The height of the invalid payloads to retrieve. No height returns the latest.",
            "count (optional)" => "Total heights to return starting at height, inclusive (forward range). Defaults to 1 (single object). When greater than 1, height is required and the response is a JSON array; capped at 1000."
        }
    ),
    route!(
        get,
        "/status",
        crate::rpc::handlers::get_status,
        "Get the application status"
    ),
    route!(
        get,
        "/health",
        crate::rpc::handlers::get_health,
        "Returns empty value. Used to check the node's health"
    ),
    route!(
        get,
        "/ready",
        crate::rpc::handlers::get_ready,
        "Readiness probe. Returns 200 when in sync, 503 when catching up"
    ),
    route!(
        get,
        "/version",
        crate::rpc::handlers::get_version,
        "Get the consensus layer version information"
    ),
    route!(
        get,
        "/network-state",
        crate::rpc::handlers::get_network_state,
        "Get the current network state (peers, topics, scores)"
    ),
    route!(
        admin post,
        "/persistent-peers",
        crate::rpc::handlers::add_persistent_peer,
        "Add a persistent peer at runtime.",
        params = {
            "body" => "JSON object with \"addr\" (string): multiaddr of the peer, e.g. \"/ip4/127.0.0.1/tcp/26656/p2p/12D3KooW...\"."
        }
    ),
    route!(
        admin delete,
        "/persistent-peers",
        crate::rpc::handlers::remove_persistent_peer,
        "Remove a persistent peer at runtime.",
        params = {
            "body" => "JSON object with \"addr\" (string): multiaddr of the peer to remove, e.g. \"/ip4/127.0.0.1/tcp/26656/p2p/12D3KooW...\"."
        }
    ),
];

#[tracing::instrument(name = "rpc", skip_all)]
pub async fn serve(
    listen_addr: impl ToSocketAddrs,
    tx_consensus_req: TxConsensusReq,
    tx_app_req: TxAppReq,
    tx_network_req: TxNetworkReq,
    admin_enabled: bool,
) {
    serve_with_metrics(
        listen_addr,
        tx_consensus_req,
        tx_app_req,
        tx_network_req,
        admin_enabled,
        AppMetrics::default(),
    )
    .await
}

pub(crate) async fn serve_with_metrics(
    listen_addr: impl ToSocketAddrs,
    tx_consensus_req: TxConsensusReq,
    tx_app_req: TxAppReq,
    tx_network_req: TxNetworkReq,
    admin_enabled: bool,
    metrics: AppMetrics,
) {
    if let Err(e) = inner(
        listen_addr,
        tx_consensus_req,
        tx_app_req,
        tx_network_req,
        admin_enabled,
        metrics,
    )
    .await
    {
        error!("RPC server failed: {e}");
    }
}

/// Build the RPC router with all routes and middleware
///
/// Admin routes are mounted only when `admin_enabled` is true;
/// otherwise they are neither served nor advertised in the index.
///
/// This is exposed publicly for testing purposes, allowing integration tests
/// to create a server with the actual production router.
pub fn build_router(
    tx_consensus_req: TxConsensusReq,
    tx_app_req: TxAppReq,
    tx_network_req: TxNetworkReq,
    admin_enabled: bool,
) -> Router {
    build_router_with_metrics(
        tx_consensus_req,
        tx_app_req,
        tx_network_req,
        admin_enabled,
        AppMetrics::default(),
    )
}

pub(crate) fn build_router_with_metrics(
    tx_consensus_req: TxConsensusReq,
    tx_app_req: TxAppReq,
    tx_network_req: TxNetworkReq,
    admin_enabled: bool,
    metrics: AppMetrics,
) -> Router {
    let rpc_state = RpcState {
        tx_consensus_req,
        tx_app_req,
        tx_network_req,
        metrics,
    };

    let routes = build_routes()
        .into_iter()
        .filter(|route| admin_enabled || !route.admin)
        .collect::<Vec<_>>();

    let mut router = Router::new();
    for route in &routes {
        router = router.route(route.path, (route.handler)());
    }

    let docs = routes
        .into_iter()
        .map(|r| (format!("{} {}", r.method, r.path), r.doc))
        .collect::<BTreeMap<_, _>>();

    router = {
        let docs = Arc::new(docs);
        router.route("/", get(move || get_index(Arc::clone(&docs))))
    };

    router
        .layer(axum::middleware::from_fn(extract_version))
        .layer(DefaultBodyLimit::max(RPC_MAX_BODY_SIZE))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            RPC_REQUEST_TIMEOUT,
        ))
        .layer(ConcurrencyLimitLayer::new(RPC_MAX_CONCURRENT_REQUESTS))
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(rpc_state)
}

async fn inner(
    listen_addr: impl ToSocketAddrs,
    tx_consensus_req: TxConsensusReq,
    tx_app_req: TxAppReq,
    tx_network_req: TxNetworkReq,
    admin_enabled: bool,
    metrics: AppMetrics,
) -> Result<()> {
    let app = build_router_with_metrics(
        tx_consensus_req,
        tx_app_req,
        tx_network_req,
        admin_enabled,
        metrics,
    );

    let listener = TcpListener::bind(listen_addr).await?;
    let address = listener.local_addr()?;

    info!(%address, "RPC server listening");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn get_index(endpoints: Arc<BTreeMap<String, EndpointInfo>>) -> impl IntoResponse {
    Json(json!({
        "endpoints": endpoints,
        "rpc_versioning": {
            "method": "header-based",
            "header": "Accept",
            "format": "application/vnd.arc.v{N}+json",
            "supported_versions": [ApiVersion::V1.to_string()],
            "default_version": "v1",
            "example": "curl -H \"Accept: application/vnd.arc.v1+json\" http://localhost:26658/status"
        }
    }))
}

#[cfg(test)]
mod tests {
    use arc_consensus_types::CommitCertificateType;
    use axum::body::Body;
    use axum::http::{Request, Response, StatusCode};
    use core::panic;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::time::{Duration, SystemTime};
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    use arc_consensus_types::{
        signing::PrivateKey, Address, ArcContext, BlockHash, Height, Round, Validator,
        ValidatorSet, ValueId,
    };
    use malachitebft_app_channel::app::{
        engine::{
            consensus::state_dump::{
                types as dump_types, ProposalKeeperState, StateDump, VoteKeeperState,
            },
            network::NetworkStateDump,
        },
        net::{Multiaddr, PeerId},
    };
    use malachitebft_app_channel::{ConsensusRequest, NetworkRequest};
    use malachitebft_core_state_machine::state::State as MState;
    use malachitebft_core_types::CommitCertificate;
    use malachitebft_network::{LocalNodeInfo, PersistentPeerError, ValidatorInfo};

    use super::*;
    use crate::request::{AppRequest, CommitCertificateInfo, HeightRangeRequest, Status};
    use crate::rpc::types::{
        RpcAppStatus, RpcCommitCertificate, RpcConsensusStateDump, RpcNetworkStateDump,
    };
    use crate::store::{RangeFailureReason, RangeQueryResult};
    use crate::utils::sync_state::SyncState;
    use arc_consensus_db::invalid_payloads::StoredInvalidPayloads;
    use arc_consensus_types::evidence::StoredMisbehaviorEvidence;
    use arc_consensus_types::proposal_monitor::ProposalMonitor;

    enum MockValue {
        Present,
        Absent,
    }

    enum MockConfig {
        AppGetHealth,
        AppGetStatus,
        AppGetSyncState(SyncState),
        AppGetCertificate(MockValue),
        AppGetCertificateRange(
            HeightRangeRequest,
            Option<RangeQueryResult<CommitCertificateInfo>>,
        ),
        AppGetMisbehaviorEvidenceRange(
            HeightRangeRequest,
            Option<RangeQueryResult<StoredMisbehaviorEvidence>>,
        ),
        AppGetProposalMonitorDataRange(
            HeightRangeRequest,
            Option<RangeQueryResult<ProposalMonitor>>,
        ),
        AppGetInvalidPayloadsRange(
            HeightRangeRequest,
            Option<RangeQueryResult<StoredInvalidPayloads>>,
        ),
        ConsensusDumpState(MockValue),
        NetworkDumpState(MockValue),
        AddPersistentPeer(Result<(), PersistentPeerError>),
        RemovePersistentPeer(Result<(), PersistentPeerError>),
    }

    struct MockBackend {
        rx_consensus: mpsc::Receiver<ConsensusRequest<ArcContext>>,
        rx_app: mpsc::Receiver<AppRequest>,
        rx_network: mpsc::Receiver<NetworkRequest>,
        config: MockConfig,
    }

    impl MockBackend {
        fn spawn_new(
            config: MockConfig,
        ) -> (
            mpsc::Sender<ConsensusRequest<ArcContext>>,
            mpsc::Sender<AppRequest>,
            mpsc::Sender<NetworkRequest>,
        ) {
            let (tx_consensus, rx_consensus) = mpsc::channel::<ConsensusRequest<ArcContext>>(1);
            let (tx_app, rx_app) = mpsc::channel::<AppRequest>(1);
            let (tx_network, rx_network) = mpsc::channel::<NetworkRequest>(1);

            let backend = Self {
                rx_consensus,
                rx_app,
                rx_network,
                config,
            };
            tokio::spawn(backend.run());

            (tx_consensus, tx_app, tx_network)
        }

        async fn run(self) {
            let MockBackend {
                mut rx_consensus,
                mut rx_app,
                mut rx_network,
                config,
            } = self;

            tokio::select! {
                msg = rx_consensus.recv() => {
                    Self::handle_consensus_msg(msg, config);
                },
                msg = rx_app.recv() => {
                    Self::handle_app_msg(msg, config);
                },
                msg = rx_network.recv() => {
                    Self::handle_network_msg(msg, config);
                },
                _ = tokio::time::sleep(Duration::from_secs(2)) => {
                    panic!("Mock backend did not receive any request within 2s");
                },
            }
        }

        fn handle_consensus_msg(msg: Option<ConsensusRequest<ArcContext>>, config: MockConfig) {
            match config {
                MockConfig::ConsensusDumpState(ret) => {
                    let Some(ConsensusRequest::DumpState(reply_port)) = msg else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply_port.send(match ret {
                        MockValue::Present => Some(Self::a_consensus_dump()),
                        MockValue::Absent => None,
                    });
                }
                _ => panic!("Unexpected config"),
            }
        }

        fn handle_app_msg(msg: Option<AppRequest>, config: MockConfig) {
            match config {
                MockConfig::AppGetHealth => {
                    let Some(AppRequest::GetHealth(reply)) = msg else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply.send(());
                }
                MockConfig::AppGetStatus => {
                    let Some(AppRequest::GetStatus(reply_port)) = msg else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply_port.send(Self::a_status());
                }
                MockConfig::AppGetSyncState(state) => {
                    let Some(AppRequest::GetSyncState(reply)) = msg else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply.send(state);
                }
                MockConfig::AppGetCertificate(ret) => {
                    let Some(AppRequest::GetCertificate {
                        height: None,
                        reply: reply_port,
                        ..
                    }) = msg
                    else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply_port.send(match ret {
                        MockValue::Present => Some(Self::a_commit_cert_info()),
                        MockValue::Absent => None,
                    });
                }
                MockConfig::AppGetCertificateRange(expected, ret) => {
                    let Some(AppRequest::GetCertificateRange(range, reply_port)) = msg else {
                        panic!("Unexpected msg");
                    };
                    assert_eq!(range, expected);
                    let _ = reply_port.send(ret);
                }
                MockConfig::AppGetMisbehaviorEvidenceRange(expected, ret) => {
                    let Some(AppRequest::GetMisbehaviorEvidenceRange(range, reply_port)) = msg
                    else {
                        panic!("Unexpected msg");
                    };
                    assert_eq!(range, expected);
                    let _ = reply_port.send(ret);
                }
                MockConfig::AppGetProposalMonitorDataRange(expected, ret) => {
                    let Some(AppRequest::GetProposalMonitorDataRange(range, reply_port)) = msg
                    else {
                        panic!("Unexpected msg");
                    };
                    assert_eq!(range, expected);
                    let _ = reply_port.send(ret);
                }
                MockConfig::AppGetInvalidPayloadsRange(expected, ret) => {
                    let Some(AppRequest::GetInvalidPayloadsRange(range, reply_port)) = msg else {
                        panic!("Unexpected msg");
                    };
                    assert_eq!(range, expected);
                    let _ = reply_port.send(ret);
                }
                _ => panic!("Unexpected config"),
            }
        }

        fn handle_network_msg(msg: Option<NetworkRequest>, config: MockConfig) {
            match config {
                MockConfig::NetworkDumpState(ret) => {
                    let Some(NetworkRequest::DumpState(reply_port)) = msg else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply_port.send(match ret {
                        MockValue::Present => Some(Self::a_network_dump()),
                        MockValue::Absent => None,
                    });
                }
                MockConfig::AddPersistentPeer(result) => {
                    let Some(NetworkRequest::UpdatePersistentPeers(_, reply_port)) = msg else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply_port.send(result);
                }
                MockConfig::RemovePersistentPeer(result) => {
                    let Some(NetworkRequest::UpdatePersistentPeers(_, reply_port)) = msg else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply_port.send(result);
                }
                _ => panic!("Unexpected config"),
            }
        }

        fn a_consensus_dump() -> StateDump<ArcContext> {
            let consensus = MState::<ArcContext>::default();
            let address = Address::new([0xEE; 20]);
            let proposer = Some(Address::new([0xBC; 20]));

            let sk = PrivateKey::from([0x77; 32]);
            let v = Validator::new(sk.public_key(), 541);
            let validator_set = ValidatorSet::new(vec![v]);

            let vote_keeper = VoteKeeperState {
                votes: BTreeMap::new(),
                evidence: dump_types::VoteEvidenceMap::new(),
            };
            let proposal_keeper = ProposalKeeperState {
                proposals: BTreeMap::new(),
                evidence: dump_types::ProposalEvidenceMap::new(),
            };

            let params = dump_types::ConsensusParams {
                address,
                threshold_params: dump_types::ThresholdParams::default(),
                value_payload: dump_types::ValuePayload::ProposalAndParts,
                enabled: true,
            };

            StateDump {
                consensus,
                address,
                proposer,
                params,
                validator_set,
                vote_keeper,
                proposal_keeper,
                full_proposal_keeper: Default::default(),
                last_signed_prevote: None,
                last_signed_precommit: None,
                round_certificate: None,
                input_queue: dump_types::BoundedQueue::new(0, 0),
            }
        }

        fn a_network_dump() -> malachitebft_app_channel::app::engine::network::NetworkStateDump {
            let mut peer_id_bytes = vec![0x00, 0x20]; // identity multihash code + length
            peer_id_bytes.extend_from_slice(&[5u8; 32]); // 32 bytes of data
            let peer_id = PeerId::from_bytes(&peer_id_bytes).unwrap();

            let listen_addr: Multiaddr = "/ip4/127.0.0.1/tcp/34567".parse().unwrap();
            let local = LocalNodeInfo {
                moniker: "a-node".to_string(),
                peer_id,
                listen_addr: listen_addr.clone(),
                consensus_address: Some("ADDR1".to_string()),
                is_validator: true,
                persistent_peers_only: false,
                subscribed_topics: HashSet::from(["/consensus".to_string()]),
                proof_bytes: None,
            };

            NetworkStateDump {
                local_node: local,
                peers: HashMap::new(),
                validator_set: vec![ValidatorInfo {
                    address: "ADDR1".to_string(),
                    public_key: vec![0x01; 32],
                    voting_power: 313,
                }],
                persistent_peer_ids: vec![peer_id],
                persistent_peer_addrs: vec![listen_addr.clone()],
            }
        }

        fn a_status() -> Status {
            let height = Height::new(42);
            let round = Round::new(10);
            let address = Address::new([0x11; 20]);
            let proposer = Some(Address::new([0x22; 20]));
            let height_start_time = SystemTime::UNIX_EPOCH;
            let prev_payload_hash = Some(BlockHash::new([0xCC; 32]));
            let db_latest_height = Height::new(100);
            let db_earliest_height = Height::new(2);
            let undecided_blocks_count = 3;
            let pending_proposal_parts = vec![];

            let sk = PrivateKey::from([0x33; 32]);
            let public_key = sk.public_key();
            let v = Validator::new(public_key, 1234);
            let validator_set = ValidatorSet::new(vec![v]);

            Status {
                height,
                round,
                address,
                public_key,
                proposer,
                height_start_time,
                prev_payload_hash,
                db_latest_height,
                db_earliest_height,
                undecided_blocks_count,
                pending_proposal_parts,
                validator_set,
                sync_state: SyncState::InSync,
            }
        }

        fn a_commit_cert() -> CommitCertificate<ArcContext> {
            let height = Height::new(7);
            let round = Round::new(3);
            let value_id = ValueId::new(BlockHash::new([0xAA; 32]));
            let votes = vec![];
            CommitCertificate::new(height, round, value_id, votes)
        }

        fn a_commit_cert_info() -> CommitCertificateInfo {
            CommitCertificateInfo {
                certificate: Self::a_commit_cert(),
                certificate_type: CommitCertificateType::Minimal,
                proposer: Address::new([0x55; 20]),
            }
        }

        fn a_commit_cert_info_at(height: u64) -> CommitCertificateInfo {
            CommitCertificateInfo {
                certificate: CommitCertificate::new(
                    Height::new(height),
                    Round::new(0),
                    ValueId::new(BlockHash::new([0xAA; 32])),
                    vec![],
                ),
                certificate_type: CommitCertificateType::Minimal,
                proposer: Address::new([0x55; 20]),
            }
        }
    }

    async fn response_to_json(resp: Response<Body>) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn build_no_backend_router_and_request(uri: &str) -> (StatusCode, serde_json::Value) {
        let (tx_dummy_cons_req, _dummy_rx_c) = mpsc::channel::<ConsensusRequest<ArcContext>>(1);
        let (tx_dummy_app_req, _dummy_rx_a) = mpsc::channel::<AppRequest>(1);
        let (tx_dummy_nw_req, _dummy_rx_n) = mpsc::channel::<NetworkRequest>(1);
        build_router_and_request(tx_dummy_cons_req, tx_dummy_app_req, tx_dummy_nw_req, uri).await
    }

    async fn build_router_and_request(
        tx_consensus_req: mpsc::Sender<ConsensusRequest<ArcContext>>,
        tx_app_req: mpsc::Sender<AppRequest>,
        tx_network_req: mpsc::Sender<NetworkRequest>,
        uri: &str,
    ) -> (StatusCode, serde_json::Value) {
        // Read routes are available regardless of the admin toggle.
        let app = build_router(tx_consensus_req, tx_app_req, tx_network_req, false);
        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let val = response_to_json(resp).await;
        (status, val)
    }

    /// Like `build_router_and_request` but sets an optional `Accept-Encoding`
    /// and returns the response headers and raw (possibly compressed) body, so
    /// compression behavior can be asserted.
    async fn build_router_and_raw_request(
        tx_consensus_req: mpsc::Sender<ConsensusRequest<ArcContext>>,
        tx_app_req: mpsc::Sender<AppRequest>,
        tx_network_req: mpsc::Sender<NetworkRequest>,
        uri: &str,
        accept_encoding: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let app = build_router(tx_consensus_req, tx_app_req, tx_network_req, true);
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(encoding) = accept_encoding {
            builder = builder.header("accept-encoding", encoding);
        }
        let req = builder.body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, bytes)
    }

    #[tokio::test]
    async fn test_response_gzip_compressed_when_requested() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificate(MockValue::Present));
        let (status, headers, body) = build_router_and_raw_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/commit",
            Some("gzip"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("content-encoding").unwrap(), "gzip");

        let mut decoder = flate2::read::GzDecoder::new(&body[..]);
        let mut json = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut json).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected =
            serde_json::to_value(RpcCommitCertificate::from(MockBackend::a_commit_cert_info()))
                .unwrap();
        assert_eq!(val, expected);
    }

    #[tokio::test]
    async fn test_response_zstd_compressed_when_requested() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificate(MockValue::Present));
        let (status, headers, body) = build_router_and_raw_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/commit",
            Some("zstd"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("content-encoding").unwrap(), "zstd");

        let mut decoder = zstd::stream::read::Decoder::new(&body[..]).unwrap();
        let mut json = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut json).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected =
            serde_json::to_value(RpcCommitCertificate::from(MockBackend::a_commit_cert_info()))
                .unwrap();
        assert_eq!(val, expected);
    }

    #[tokio::test]
    async fn test_response_brotli_compressed_when_requested() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificate(MockValue::Present));
        let (status, headers, body) =
            build_router_and_raw_request(tx_cons_req, tx_app_req, tx_nw_req, "/commit", Some("br"))
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("content-encoding").unwrap(), "br");

        let mut decoder = brotli::Decompressor::new(&body[..], 4096);
        let mut json = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut json).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected =
            serde_json::to_value(RpcCommitCertificate::from(MockBackend::a_commit_cert_info()))
                .unwrap();
        assert_eq!(val, expected);
    }

    #[tokio::test]
    async fn test_response_deflate_compressed_when_requested() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificate(MockValue::Present));
        let (status, headers, body) = build_router_and_raw_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/commit",
            Some("deflate"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("content-encoding").unwrap(), "deflate");

        // tower-http emits zlib-wrapped deflate (RFC 1950) for
        // Content-Encoding: deflate, so decode with ZlibDecoder.
        let mut decoder = flate2::read::ZlibDecoder::new(&body[..]);
        let mut json = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut json).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected =
            serde_json::to_value(RpcCommitCertificate::from(MockBackend::a_commit_cert_info()))
                .unwrap();
        assert_eq!(val, expected);
    }

    #[tokio::test]
    async fn test_response_uncompressed_without_accept_encoding() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificate(MockValue::Present));
        let (status, headers, body) =
            build_router_and_raw_request(tx_cons_req, tx_app_req, tx_nw_req, "/commit", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get("content-encoding").is_none());
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let expected =
            serde_json::to_value(RpcCommitCertificate::from(MockBackend::a_commit_cert_info()))
                .unwrap();
        assert_eq!(val, expected);
    }

    async fn build_router_and_request_with_body(
        method: &str,
        tx_consensus_req: mpsc::Sender<ConsensusRequest<ArcContext>>,
        tx_app_req: mpsc::Sender<AppRequest>,
        tx_network_req: mpsc::Sender<NetworkRequest>,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        // The mutating persistent-peer routes exist only when admin is enabled.
        let app = build_router(tx_consensus_req, tx_app_req, tx_network_req, true);
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let val = response_to_json(resp).await;
        (status, val)
    }

    #[test]
    fn test_build_routes_contains_expected_paths() {
        let mut paths: Vec<_> = build_routes().iter().map(|r| r.path).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "/commit",
                "/consensus-state",
                "/health",
                "/invalid-payloads",
                "/misbehavior-evidence",
                "/network-state",
                "/persistent-peers",
                "/persistent-peers",
                "/proposal-monitor",
                "/ready",
                "/status",
                "/version",
            ]
        );
    }

    #[test]
    fn test_commit_route_has_params_docs() {
        let routes = build_routes();
        let commit = routes
            .iter()
            .find(|r| r.method == "GET" && r.path == "/commit")
            .unwrap();
        let params = commit.doc.params.as_ref().unwrap();
        assert_eq!(
            *params.get("height (optional)").unwrap(),
            "The height of the commit certificate to retrieve. No height returns the latest certificate."
        );
    }

    #[tokio::test]
    async fn test_get_index_json() {
        let mut docs = BTreeMap::new();
        docs.insert(
            "GET /dummy".to_string(),
            EndpointInfo {
                desc: "Dummy endpoint",
                params: None,
            },
        );

        let resp = get_index(Arc::new(docs)).await.into_response();
        let val = response_to_json(resp).await;

        assert!(val.get("endpoints").is_some());
        assert!(val["endpoints"].get("GET /dummy").is_some());
        assert!(val.get("rpc_versioning").is_some());
        assert!(val["rpc_versioning"].get("default_version").is_some());
        let supported = val["rpc_versioning"]["supported_versions"]
            .as_array()
            .unwrap();
        assert!(supported.contains(&json!("v1")));
    }

    #[tokio::test]
    async fn test_index_documents_both_persistent_peers_methods() {
        let routes = build_routes();
        let docs = routes
            .into_iter()
            .map(|r| (format!("{} {}", r.method, r.path), r.doc))
            .collect::<BTreeMap<_, _>>();
        let resp = get_index(Arc::new(docs)).await.into_response();
        let val = response_to_json(resp).await;
        let endpoints = &val["endpoints"];
        assert!(
            endpoints.get("POST /persistent-peers").is_some(),
            "index must document POST /persistent-peers"
        );
        assert!(
            endpoints.get("DELETE /persistent-peers").is_some(),
            "index must document DELETE /persistent-peers"
        );
        assert_eq!(
            endpoints["POST /persistent-peers"]["desc"],
            "Add a persistent peer at runtime."
        );
        assert_eq!(
            endpoints["DELETE /persistent-peers"]["desc"],
            "Remove a persistent peer at runtime."
        );
    }

    #[tokio::test]
    async fn test_version_success() {
        // '/version' endpoint does not use the backend
        let (status, val) = build_no_backend_router_and_request("/version").await;
        assert_eq!(status, StatusCode::OK);
        assert!(val.get("git_version").is_some());
        assert!(val.get("git_commit").is_some());
        assert!(val.get("git_short_hash").is_some());
        assert!(val.get("cargo_version").is_some());
    }

    #[tokio::test]
    async fn test_commit_latest_404() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificate(MockValue::Absent));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/commit").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(val, json!({"error": "Certificate not found"}));
    }

    #[tokio::test]
    async fn test_consensus_state_503() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::ConsensusDumpState(MockValue::Absent));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/consensus-state").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(val, json!({"error": "Consensus state not available"}));
    }

    #[tokio::test]
    async fn test_network_state_503() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::NetworkDumpState(MockValue::Absent));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/network-state").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(val, json!({"error": "Network state not available"}));
    }

    #[tokio::test]
    async fn test_health_success() {
        let (tx_cons_req, tx_app_req, tx_nw_req) = MockBackend::spawn_new(MockConfig::AppGetHealth);
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(val, json!({"status": "ok"}));
    }

    #[tokio::test]
    async fn test_ready_in_sync() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetSyncState(SyncState::InSync));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/ready").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(val, json!({"sync_state": "InSync"}));
    }

    #[tokio::test]
    async fn test_ready_catching_up() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetSyncState(SyncState::CatchingUp));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/ready").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(val, json!({"sync_state": "CatchingUp"}));
    }

    #[tokio::test]
    async fn test_status_success() {
        let (tx_cons_req, tx_app_req, tx_nw_req) = MockBackend::spawn_new(MockConfig::AppGetStatus);
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/status").await;
        assert_eq!(status, StatusCode::OK);
        let expected = serde_json::to_value(RpcAppStatus::from(MockBackend::a_status())).unwrap();
        assert_eq!(val, expected);
    }

    #[tokio::test]
    async fn test_commit_latest_success() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificate(MockValue::Present));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/commit").await;
        assert_eq!(status, StatusCode::OK);
        let expected =
            serde_json::to_value(RpcCommitCertificate::from(MockBackend::a_commit_cert_info()))
                .unwrap();
        assert_eq!(val, expected);
    }

    #[tokio::test]
    async fn test_commit_count_one_is_single_object() {
        // count=1 must take the legacy single-height path: a single object,
        // never a 1-element array. The mock accepts only GetCertificate.
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificate(MockValue::Present));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/commit?count=1").await;
        assert_eq!(status, StatusCode::OK);
        let expected =
            serde_json::to_value(RpcCommitCertificate::from(MockBackend::a_commit_cert_info()))
                .unwrap();
        assert_eq!(val, expected);
        assert!(!val.is_array(), "count=1 must return a single object");
    }

    #[tokio::test]
    async fn test_commit_range_returns_ordered_array() {
        let range = HeightRangeRequest {
            from: Height::new(7),
            count: 2,
        };
        let reply = RangeQueryResult::Complete(vec![
            MockBackend::a_commit_cert_info_at(7),
            MockBackend::a_commit_cert_info_at(8),
        ]);
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificateRange(range, Some(reply)));
        let (status, val) = build_router_and_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/commit?height=7&count=2",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let expected = serde_json::to_value(vec![
            RpcCommitCertificate::from(MockBackend::a_commit_cert_info_at(7)),
            RpcCommitCertificate::from(MockBackend::a_commit_cert_info_at(8)),
        ])
        .unwrap();
        assert_eq!(val, expected);
    }

    #[tokio::test]
    async fn test_misbehavior_evidence_range_returns_array() {
        let range = HeightRangeRequest {
            from: Height::new(1),
            count: 3,
        };
        let reply = RangeQueryResult::Complete(vec![
            StoredMisbehaviorEvidence::empty(Height::new(1)),
            StoredMisbehaviorEvidence::empty(Height::new(2)),
            StoredMisbehaviorEvidence::empty(Height::new(3)),
        ]);
        let (tx_cons_req, tx_app_req, tx_nw_req) = MockBackend::spawn_new(
            MockConfig::AppGetMisbehaviorEvidenceRange(range, Some(reply)),
        );
        let (status, val) = build_router_and_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/misbehavior-evidence?height=1&count=3",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let heights: Vec<u64> = val
            .as_array()
            .expect("array body")
            .iter()
            .map(|e| e["height"].as_u64().unwrap())
            .collect();
        assert_eq!(heights, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_proposal_monitor_range_returns_array() {
        let proposer = Address::new([0x22; 20]);
        let range = HeightRangeRequest {
            from: Height::new(4),
            count: 2,
        };
        let reply = RangeQueryResult::Complete(vec![
            ProposalMonitor::new(Height::new(4), proposer, SystemTime::UNIX_EPOCH),
            ProposalMonitor::new(Height::new(5), proposer, SystemTime::UNIX_EPOCH),
        ]);
        let (tx_cons_req, tx_app_req, tx_nw_req) = MockBackend::spawn_new(
            MockConfig::AppGetProposalMonitorDataRange(range, Some(reply)),
        );
        let (status, val) = build_router_and_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/proposal-monitor?height=4&count=2",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let heights: Vec<u64> = val
            .as_array()
            .expect("array body")
            .iter()
            .map(|e| e["height"].as_u64().unwrap())
            .collect();
        assert_eq!(heights, vec![4, 5]);
    }

    #[tokio::test]
    async fn test_invalid_payloads_range_returns_array() {
        let range = HeightRangeRequest {
            from: Height::new(10),
            count: 2,
        };
        let reply = RangeQueryResult::Complete(vec![
            StoredInvalidPayloads::empty(Height::new(10)),
            StoredInvalidPayloads::empty(Height::new(11)),
        ]);
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetInvalidPayloadsRange(range, Some(reply)));
        let (status, val) = build_router_and_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/invalid-payloads?height=10&count=2",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let heights: Vec<u64> = val
            .as_array()
            .expect("array body")
            .iter()
            .map(|e| e["height"].as_u64().unwrap())
            .collect();
        assert_eq!(heights, vec![10, 11]);
    }

    #[tokio::test]
    async fn test_count_zero_rejected() {
        let (status, val) = build_no_backend_router_and_request("/commit?height=5&count=0").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(val, json!({"error": "count must be at least 1"}));
    }

    #[tokio::test]
    async fn test_count_greater_than_one_without_height_rejected() {
        let (status, val) = build_no_backend_router_and_request("/commit?count=2").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            val,
            json!({"error": "height is required when count is greater than 1"})
        );
    }

    #[tokio::test]
    async fn test_count_over_limit_returns_structured_400() {
        let (status, val) =
            build_no_backend_router_and_request("/commit?height=10&count=1001").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            val,
            json!({
                "error": "partial range unavailable",
                "requested": {"from": 10, "to": 1010},
                "reason": "over_limit"
            })
        );
        assert!(
            val.get("failed_heights").is_none(),
            "over_limit must omit failed_heights"
        );
    }

    #[tokio::test]
    async fn test_count_overflow_rejected() {
        let (status, val) =
            build_no_backend_router_and_request("/commit?height=18446744073709551615&count=2")
                .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            val,
            json!({"error": "requested range exceeds the maximum u64 height"})
        );
    }

    #[tokio::test]
    async fn test_count_malformed_is_query_rejection() {
        // axum rejects an unparseable count with a plain-text 400 (same as
        // height=abc today), so assert only the status, not a JSON body.
        let (tx_c, _rc) = mpsc::channel::<ConsensusRequest<ArcContext>>(1);
        let (tx_a, _ra) = mpsc::channel::<AppRequest>(1);
        let (tx_n, _rn) = mpsc::channel::<NetworkRequest>(1);
        let app = build_router(tx_c, tx_a, tx_n, true);
        let req = Request::builder()
            .method("GET")
            .uri("/commit?count=abc")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_range_above_head_structured_error_body() {
        let range = HeightRangeRequest {
            from: Height::new(8),
            count: 5,
        };
        let reply = RangeQueryResult::Unavailable {
            reason: RangeFailureReason::AboveCurrentHead,
            failed_heights: vec![Height::new(11), Height::new(12)],
        };
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificateRange(range, Some(reply)));
        let (status, val) = build_router_and_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/commit?height=8&count=5",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            val,
            json!({
                "error": "partial range unavailable",
                "requested": {"from": 8, "to": 12},
                "failed_heights": [11, 12],
                "reason": "above_current_head"
            })
        );
    }

    #[tokio::test]
    async fn test_range_empty_store_returns_legacy_404() {
        let range = HeightRangeRequest {
            from: Height::new(5),
            count: 3,
        };
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AppGetCertificateRange(range, None));
        let (status, val) = build_router_and_request(
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/commit?height=5&count=3",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(val, json!({"error": "Certificate not found"}));
    }

    #[tokio::test]
    async fn test_consensus_state_success() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::ConsensusDumpState(MockValue::Present));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/consensus-state").await;
        assert_eq!(status, StatusCode::OK);
        let expected =
            serde_json::to_value(RpcConsensusStateDump::from(&MockBackend::a_consensus_dump()))
                .unwrap();
        assert_eq!(val, expected);
    }

    #[tokio::test]
    async fn test_network_state_success() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::NetworkDumpState(MockValue::Present));
        let (status, val) =
            build_router_and_request(tx_cons_req, tx_app_req, tx_nw_req, "/network-state").await;
        assert_eq!(status, StatusCode::OK);
        let expected =
            serde_json::to_value(RpcNetworkStateDump::from(MockBackend::a_network_dump())).unwrap();
        assert_eq!(val, expected);
    }

    fn valid_add_persistent_peer_addr() -> serde_json::Value {
        json!({ "addr": "/ip4/127.0.0.1/tcp/26656/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN" })
    }

    #[tokio::test]
    async fn test_add_persistent_peer_success() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AddPersistentPeer(Ok(())));
        let (status, val) = build_router_and_request_with_body(
            "POST",
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/persistent-peers",
            valid_add_persistent_peer_addr(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(val, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn test_add_persistent_peer_already_exists() {
        let (tx_cons_req, tx_app_req, tx_nw_req) = MockBackend::spawn_new(
            MockConfig::AddPersistentPeer(Err(PersistentPeerError::AlreadyExists)),
        );
        let (status, val) = build_router_and_request_with_body(
            "POST",
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/persistent-peers",
            valid_add_persistent_peer_addr(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(val, json!({"error": "Persistent peer already exists"}));
    }

    // For future ref: https://github.com/circlefin/malachite/pull/1485
    // #[tokio::test]
    // async fn test_add_persistent_peer_missing_p2p() {
    //     let (tx_cons_req, tx_app_req, tx_nw_req) =
    //         MockBackend::spawn_new(MockConfig::NetworkDumpState(MockValue::Absent));
    //     let (status, val) = build_router_and_request_with_body("POST",
    //         tx_cons_req,
    //         tx_app_req,
    //         tx_nw_req,
    //         "/persistent-peers",
    //         json!({ "addr": "/ip4/127.0.0.1/tcp/26656" }),
    //     )
    //     .await;
    //     assert_eq!(status, StatusCode::BAD_REQUEST);
    //     assert!(val
    //         .get("error")
    //         .and_then(|e| e.as_str())
    //         .unwrap_or("")
    //         .contains("/p2p/"));
    // }

    #[tokio::test]
    async fn test_add_persistent_peer_network_stopped() {
        let (tx_cons_req, tx_app_req, tx_nw_req) = MockBackend::spawn_new(
            MockConfig::AddPersistentPeer(Err(PersistentPeerError::NetworkStopped)),
        );
        let (status, val) = build_router_and_request_with_body(
            "POST",
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/persistent-peers",
            valid_add_persistent_peer_addr(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(val, json!({"error": "Network not started"}));
    }

    #[tokio::test]
    async fn test_add_persistent_peer_internal_error() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::AddPersistentPeer(Err(
                PersistentPeerError::InternalError("detail".to_string()),
            )));
        let (status, val) = build_router_and_request_with_body(
            "POST",
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/persistent-peers",
            valid_add_persistent_peer_addr(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(val, json!({"error": "Internal error"}));
    }

    #[tokio::test]
    async fn test_add_persistent_peer_invalid_multiaddr() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::NetworkDumpState(MockValue::Absent));
        let (status, val) = build_router_and_request_with_body(
            "POST",
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/persistent-peers",
            json!({ "addr": "not-a-valid-multiaddr" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(val, json!({"error": "Invalid multiaddr"}));
    }

    #[tokio::test]
    async fn test_remove_persistent_peer_success() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::RemovePersistentPeer(Ok(())));
        let (status, val) = build_router_and_request_with_body(
            "DELETE",
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/persistent-peers",
            valid_add_persistent_peer_addr(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(val, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn test_remove_persistent_peer_not_found() {
        let (tx_cons_req, tx_app_req, tx_nw_req) = MockBackend::spawn_new(
            MockConfig::RemovePersistentPeer(Err(PersistentPeerError::NotFound)),
        );
        let (status, val) = build_router_and_request_with_body(
            "DELETE",
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/persistent-peers",
            valid_add_persistent_peer_addr(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(val, json!({"error": "Persistent peer not found"}));
    }

    #[tokio::test]
    async fn test_remove_persistent_peer_invalid_multiaddr() {
        let (tx_cons_req, tx_app_req, tx_nw_req) =
            MockBackend::spawn_new(MockConfig::NetworkDumpState(MockValue::Absent));
        let (status, val) = build_router_and_request_with_body(
            "DELETE",
            tx_cons_req,
            tx_app_req,
            tx_nw_req,
            "/persistent-peers",
            json!({ "addr": "not-a-valid-multiaddr" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(val, json!({"error": "Invalid multiaddr"}));
    }

    #[tokio::test]
    async fn test_oversized_body_is_rejected() {
        let (tx_dummy_cons_req, _dummy_rx_c) = mpsc::channel::<ConsensusRequest<ArcContext>>(1);
        let (tx_dummy_app_req, _dummy_rx_a) = mpsc::channel::<AppRequest>(1);
        let (tx_dummy_nw_req, _dummy_rx_n) = mpsc::channel::<NetworkRequest>(1);
        // Admin enabled so the mutating /persistent-peers route is mounted.
        let app = build_router(tx_dummy_cons_req, tx_dummy_app_req, tx_dummy_nw_req, true);

        // RPC_MAX_BODY_SIZE = 2 KiB; send 8 KiB of padding inside a JSON string.
        let oversize = "a".repeat(8 * 1024);
        let body = serde_json::to_vec(&json!({ "addr": oversize })).unwrap();
        assert!(body.len() > RPC_MAX_BODY_SIZE);

        let req = Request::builder()
            .method("POST")
            .uri("/persistent-peers")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Send a request to a router built with no backend and the given admin
    /// setting. Uses an invalid multiaddr body so the mutating handlers reject
    /// at parse time (BAD_REQUEST) instead of blocking on an absent backend —
    /// letting us tell "route mounted" (BAD_REQUEST) from "route absent" (404).
    async fn status_no_backend(method: &str, uri: &str, admin_enabled: bool) -> StatusCode {
        let (tx_c, _rc) = mpsc::channel::<ConsensusRequest<ArcContext>>(1);
        let (tx_a, _ra) = mpsc::channel::<AppRequest>(1);
        let (tx_n, _rn) = mpsc::channel::<NetworkRequest>(1);
        let app = build_router(tx_c, tx_a, tx_n, admin_enabled);
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "addr": "not-a-valid-multiaddr" })).unwrap(),
            ))
            .unwrap();
        app.oneshot(req).await.unwrap().status()
    }

    async fn index_json(admin_enabled: bool) -> serde_json::Value {
        let (tx_c, _rc) = mpsc::channel::<ConsensusRequest<ArcContext>>(1);
        let (tx_a, _ra) = mpsc::channel::<AppRequest>(1);
        let (tx_n, _rn) = mpsc::channel::<NetworkRequest>(1);
        let app = build_router(tx_c, tx_a, tx_n, admin_enabled);
        let req = Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        response_to_json(resp).await
    }

    #[tokio::test]
    async fn test_persistent_peer_mutation_routes_absent_without_admin() {
        assert_eq!(
            status_no_backend("POST", "/persistent-peers", false).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_no_backend("DELETE", "/persistent-peers", false).await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn test_persistent_peer_mutation_routes_present_with_admin() {
        // Route mounted: the request reaches the handler, which rejects the
        // invalid multiaddr (BAD_REQUEST) rather than returning 404.
        assert_eq!(
            status_no_backend("POST", "/persistent-peers", true).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_no_backend("DELETE", "/persistent-peers", true).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn test_read_route_available_without_admin() {
        // A read route stays reachable in the default (admin-off) configuration.
        assert_eq!(
            status_no_backend("GET", "/version", false).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn test_index_excludes_admin_routes_without_admin() {
        let val = index_json(false).await;
        let endpoints = &val["endpoints"];
        assert!(
            endpoints.get("GET /status").is_some(),
            "read routes must still be advertised"
        );
        assert!(
            endpoints.get("POST /persistent-peers").is_none(),
            "admin routes must not be advertised when admin is off"
        );
        assert!(endpoints.get("DELETE /persistent-peers").is_none());
    }

    #[tokio::test]
    async fn test_index_includes_admin_routes_with_admin() {
        let val = index_json(true).await;
        let endpoints = &val["endpoints"];
        assert!(endpoints.get("POST /persistent-peers").is_some());
        assert!(endpoints.get("DELETE /persistent-peers").is_some());
    }
}
