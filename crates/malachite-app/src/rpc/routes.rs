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

use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use eyre::Result;
use serde_json::json;
use tokio::net::{TcpListener, ToSocketAddrs};
use tracing::{error, info};

use super::middleware::extract_version;
use super::types::{EndpointInfo, RpcState, TxConsensusReq, TxNetworkReq};
use super::version::ApiVersion;
use crate::request::TxAppReq;

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
        "Get the commit certificate for a specific height",
        params = {
            "height (optional)" => "The height of the commit certificate to retrieve. No height returns the latest certificate."
        }
    ),
    route!(
        get,
        "/misbehavior-evidence",
        crate::rpc::handlers::get_misbehavior_evidence,
        "Get misbehavior evidence (double votes or proposals) for a specific height",
        params = {
            "height (optional)" => "The height of the misbehavior evidence to retrieve. No height returns the latest."
        }
    ),
    route!(
        get,
        "/proposal-monitor",
        crate::rpc::handlers::get_proposal_monitor,
        "Get round-0 proposal monitoring data (timing and success) for a specific height",
        params = {
            "height (optional)" => "The height to get monitoring data for. No height returns the latest."
        }
    ),
    route!(
        get,
        "/invalid-payloads",
        crate::rpc::handlers::get_invalid_payloads,
        "Get invalid payloads for a specific height",
        params = {
            "height (optional)" => "The height of the invalid payloads to retrieve. No height returns the latest."
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
        post,
        "/persistent-peers",
        crate::rpc::handlers::add_persistent_peer,
        "Add a persistent peer at runtime.",
        params = {
            "body" => "JSON object with \"addr\" (string): multiaddr of the peer, e.g. \"/ip4/127.0.0.1/tcp/26656/p2p/12D3KooW...\"."
        }
    ),
    route!(
        delete,
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
) {
    if let Err(e) = inner(listen_addr, tx_consensus_req, tx_app_req, tx_network_req).await {
        error!("RPC server failed: {e}");
    }
}

/// Build the RPC router with all routes and middleware
///
/// This is exposed publicly for testing purposes, allowing integration tests
/// to create a server with the actual production router.
pub fn build_router(
    tx_consensus_req: TxConsensusReq,
    tx_app_req: TxAppReq,
    tx_network_req: TxNetworkReq,
) -> Router {
    let rpc_state = RpcState {
        tx_consensus_req,
        tx_app_req,
        tx_network_req,
    };

    let routes = build_routes();

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

    // Apply version extraction middleware
    router
        .layer(axum::middleware::from_fn(extract_version))
        .with_state(rpc_state)
}

async fn inner(
    listen_addr: impl ToSocketAddrs,
    tx_consensus_req: TxConsensusReq,
    tx_app_req: TxAppReq,
    tx_network_req: TxNetworkReq,
) -> Result<()> {
    let app = build_router(tx_consensus_req, tx_app_req, tx_network_req);

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
    use crate::request::{AppRequest, CommitCertificateInfo, Status};
    use crate::rpc::types::{
        RpcAppStatus, RpcCommitCertificate, RpcConsensusStateDump, RpcNetworkStateDump,
    };

    enum MockValue {
        Present,
        Absent,
    }

    enum MockConfig {
        AppGetHealth,
        AppGetStatus,
        AppGetCertificate(MockValue),
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
                MockConfig::AppGetCertificate(ret) => {
                    let Some(AppRequest::GetCertificate(None, reply_port)) = msg else {
                        panic!("Unexpected msg");
                    };
                    let _ = reply_port.send(match ret {
                        MockValue::Present => Some(Self::a_commit_cert_info()),
                        MockValue::Absent => None,
                    });
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
                input_queue: dump_types::BoundedQueue::new(0),
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
            let v = Validator::new(sk.public_key(), 1234);
            let validator_set = ValidatorSet::new(vec![v]);

            Status {
                height,
                round,
                address,
                proposer,
                height_start_time,
                prev_payload_hash,
                db_latest_height,
                db_earliest_height,
                undecided_blocks_count,
                pending_proposal_parts,
                validator_set,
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
        let app = build_router(tx_consensus_req, tx_app_req, tx_network_req);
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

    async fn build_router_and_request_with_body(
        method: &str,
        tx_consensus_req: mpsc::Sender<ConsensusRequest<ArcContext>>,
        tx_app_req: mpsc::Sender<AppRequest>,
        tx_network_req: mpsc::Sender<NetworkRequest>,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let app = build_router(tx_consensus_req, tx_app_req, tx_network_req);
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
}
