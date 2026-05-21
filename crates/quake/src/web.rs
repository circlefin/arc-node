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

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_admin::PeerInfo;
use alloy_transport_ws::WsConnect;
use axum::extract::Path as AxumPath;
use axum::response::Html;
use axum::{routing::get, routing::post, Json, Router};
use color_eyre::eyre::Result;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, trace, warn};
use url::Url;

use crate::latency::REGION_ASSIGNMENTS_FILENAME;
use crate::manifest::{Manifest, NodeType};
use crate::node::{ContainerKind, NodeName};
use crate::nodes::NodesMetadata;
use crate::perturb::Perturbation;
use crate::testnet::Testnet;

const EL_RECONNECT_DELAY: Duration = Duration::from_secs(2);
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const DOCKER_EVENTS_RETRY_DELAY: Duration = Duration::from_secs(2);
const INDEX_HTML_REFRESH_PLACEHOLDER: &str = "/*REFRESH_MS*/500";
const LAYER_CL: &str = "cl";
const LAYER_EL: &str = "el";
const SOURCE_LIVE: &str = "live";
const SOURCE_MANIFEST: &str = "manifest";
const NODE_STATUS_OK: &str = "ok";
const NODE_STATUS_UNREACHABLE: &str = "unreachable";
const NODE_STATUS_DISCONNECTED: &str = "disconnected";
const CONTAINER_STATUS_RUNNING: &str = "running";
const CONTAINER_STATUS_DISCONNECTED: &str = "disconnected";
const DEFAULT_POOL_MAX: u64 = 10_000;

/// Consecutive `admin_peers` failures tolerated before we drop the WS
/// connection and reconnect. Small enough to recover quickly from a stuck
/// connection, large enough to tolerate transient RPC blips.
const EL_MAX_CONSECUTIVE_PEER_FAILURES: u32 = 3;

/// Container status with the timestamp of its last update.
///
/// The events subscriber and the inspect poller both write into the shared
/// status cache. The timestamp lets the (slow) inspect poller skip entries
/// that the (fast) events subscriber has updated while a `docker inspect`
/// call was in flight — without it, a stale inspect snapshot would overwrite
/// a fresh `die`/`pause` event.
#[derive(Clone, Debug)]
struct CachedStatus {
    status: String,
    updated_at: std::time::Instant,
}

impl CachedStatus {
    fn new(status: String) -> Self {
        Self {
            status,
            updated_at: std::time::Instant::now(),
        }
    }
}

/// Cached EL data pushed by per-node WebSocket subscribers.
#[derive(Clone, Default)]
struct ElLiveData {
    heights: HashMap<NodeName, u64>,
    peers: HashMap<NodeName, Vec<PeerInfo>>,
    /// Per-node EL connection errors (cleared on successful reconnect).
    errors: HashMap<NodeName, String>,
    /// Per-node mempool status: (pending, queued) transaction counts.
    mempool: HashMap<NodeName, (u64, u64)>,
}

/// Shared state for the web server.
#[derive(Clone)]
struct AppState {
    testnet: Arc<RwLock<Testnet>>,
    el_live_data: Arc<RwLock<ElLiveData>>,
    container_statuses: Arc<RwLock<HashMap<String, CachedStatus>>>,
    /// Gates `txpool_status` polling in the per-node EL tasks. Flipped by the
    /// `/api/mempool/{on|off}` route so the frontend can opt into (or out of)
    /// mempool stats without restarting the server.
    mempool_active: Arc<AtomicBool>,
    /// Region assignments loaded once at startup. The file doesn't change
    /// over the testnet's lifetime, so we avoid re-reading it on every poll.
    region_assignments: Arc<HashMap<String, String>>,
    refresh_ms: u64,
}

#[derive(Clone)]
struct TopologySnapshot {
    testnet_name: String,
    manifest: Manifest,
    nodes_metadata: NodesMetadata,
    el_data: ElLiveData,
    container_statuses: HashMap<String, String>,
    region_assignments: Arc<HashMap<String, String>>,
}

pub(crate) async fn run_server(
    testnet: Testnet,
    host: String,
    port: u16,
    refresh_ms: u64,
    el_refresh_ms: u64,
    container_refresh_ms: u64,
) -> Result<()> {
    let el_live_data = Arc::new(RwLock::new(ElLiveData::default()));
    let container_statuses = Arc::new(RwLock::new(HashMap::new()));

    // Spawn one unified WS task per EL node (heights + peers + mempool)
    let mempool_active = Arc::new(AtomicBool::new(false));
    spawn_el_node_tasks(
        &testnet.nodes_metadata,
        Arc::clone(&el_live_data),
        Arc::clone(&mempool_active),
        el_refresh_ms,
    );

    // Spawn background poller for Docker container statuses
    let container_names: Vec<String> = testnet.nodes_metadata.all_container_names();
    spawn_container_status_poller(
        Arc::clone(&container_statuses),
        container_names,
        container_refresh_ms,
    );

    let region_assignments = Arc::new(load_region_assignments(&testnet.dir));

    let state = AppState {
        testnet: Arc::new(RwLock::new(testnet)),
        el_live_data,
        container_statuses,
        mempool_active,
        region_assignments,
        refresh_ms,
    };

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/topology", get(topology_handler))
        .route("/api/node/{name}/{action}", post(node_action_handler))
        .route("/api/mempool/{action}", post(mempool_toggle_handler))
        .with_state(state);

    if host != "127.0.0.1" && host != "localhost" {
        warn!(
            host = %host,
            "Binding quake web beyond localhost exposes unauthenticated control endpoints"
        );
    }

    let bind_addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Failed to bind to {bind_addr}: {e}"))?;

    info!(
        host = %host,
        port,
        "Quake web server listening — open http://{host}:{port}"
    );

    axum::serve(listener, app)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Web server error: {e}"))?;

    Ok(())
}

// ── EL WebSocket per-node tasks ──────────────────────────────────────────

/// Spawn one unified WS task per EL node.
///
/// Each task maintains a single WebSocket connection and handles:
/// - Block header subscriptions (push, real-time)
/// - Periodic `admin_peers` polling (every `el_refresh_ms`)
/// - Periodic `txpool_status` polling (every `el_refresh_ms`, only when `mempool_active`)
fn spawn_el_node_tasks(
    nodes: &NodesMetadata,
    el_data: Arc<RwLock<ElLiveData>>,
    mempool_active: Arc<AtomicBool>,
    el_refresh_ms: u64,
) {
    for (name, ws_url) in nodes.all_execution_ws_urls() {
        let el_data = Arc::clone(&el_data);
        let mempool_active = Arc::clone(&mempool_active);
        tokio::spawn(el_node_task(
            name,
            ws_url,
            el_data,
            mempool_active,
            el_refresh_ms,
        ));
    }
}

/// Long-lived per-node task: single WS connection for all EL data.
///
/// Runs a block subscription and a periodic poll loop concurrently via
/// `tokio::select!`. If either branch fails (WS disconnect), the task
/// clears cached data and reconnects after a delay.
async fn el_node_task(
    name: NodeName,
    ws_url: Url,
    el_data: Arc<RwLock<ElLiveData>>,
    mempool_active: Arc<AtomicBool>,
    el_refresh_ms: u64,
) {
    loop {
        debug!(node = %name, %ws_url, "Connecting EL WebSocket");

        let ws = WsConnect::new(ws_url.to_string());
        let provider = match ProviderBuilder::new().connect_ws(ws).await {
            Ok(p) => p,
            Err(e) => {
                warn!(node = %name, %ws_url, "WS connect failed: {e}");
                set_el_error(&name, format!("{name}: EL unreachable"), &el_data).await;
                tokio::time::sleep(EL_RECONNECT_DELAY).await;
                continue;
            }
        };

        let mut sub = match provider.subscribe_blocks().await {
            Ok(s) => s,
            Err(e) => {
                warn!(node = %name, "Block subscription failed: {e}");
                set_el_error(&name, format!("{name}: EL unreachable"), &el_data).await;
                tokio::time::sleep(EL_RECONNECT_DELAY).await;
                continue;
            }
        };

        debug!(node = %name, "EL WS connected (blocks + peers + mempool)");
        el_data.write().await.errors.remove(&name);

        // Run block subscription and periodic polling concurrently.
        // If either branch completes (WS disconnect or poll error), we reconnect.
        tokio::select! {
            _ = async {
                while let Ok(header) = sub.recv().await {
                    let height = header.number;
                    trace!(node = %name, height, "New block via WS");
                    el_data.write().await.heights.insert(name.clone(), height);
                }
            } => {
                warn!(node = %name, "EL WS block stream ended, reconnecting");
            }
            _ = async {
                let interval = Duration::from_millis(el_refresh_ms);
                let mut consecutive_failures = 0u32;
                loop {
                    tokio::time::sleep(interval).await;

                    // Fetch peers via raw WS RPC
                    match provider.raw_request::<_, Vec<PeerInfo>>("admin_peers".into(), ()).await {
                        Ok(peers) => {
                            consecutive_failures = 0;
                            el_data.write().await.peers.insert(name.clone(), peers);
                        }
                        Err(e) => {
                            consecutive_failures += 1;
                            debug!(node = %name, failures = consecutive_failures, "admin_peers WS error: {e}");
                            if consecutive_failures >= EL_MAX_CONSECUTIVE_PEER_FAILURES { break; }
                            continue;
                        }
                    }

                    // Fetch mempool status (only when toggle is on)
                    if mempool_active.load(Ordering::Relaxed) {
                        match provider.raw_request::<_, alloy_rpc_types_txpool::TxpoolStatus>(
                            "txpool_status".into(), ()
                        ).await {
                            Ok(status) => {
                                el_data.write().await.mempool.insert(
                                    name.clone(),
                                    (status.pending, status.queued),
                                );
                            }
                            Err(e) => {
                                debug!(node = %name, "txpool_status WS error: {e}");
                            }
                        }
                    }
                }
            } => {
                warn!(node = %name, "EL WS poll loop failed, reconnecting");
            }
        }

        set_el_error(&name, format!("{name}: EL disconnected"), &el_data).await;
        tokio::time::sleep(EL_RECONNECT_DELAY).await;
    }
}

/// Record an EL error and clear transient data (peers, mempool).
///
/// Peers and mempool counts are removed because keeping them after a
/// disconnect would show outdated edges in the topology graph and wrong
/// pool counts in the UI. Heights are preserved so the UI can show the
/// last known value as stale rather than blank.
async fn set_el_error(name: &str, msg: String, el_data: &Arc<RwLock<ElLiveData>>) {
    let mut data = el_data.write().await;
    data.peers.remove(name);
    data.mempool.remove(name);
    data.errors.insert(name.to_string(), msg);
}

// GET / — serve the HTML SPA with refresh interval injected.
// In debug builds, read from disk so changes are visible on browser refresh without recompiling.
// In release builds, use the compile-time embedded copy.
async fn index_handler(state: axum::extract::State<AppState>) -> Html<String> {
    let raw = load_index_html();
    let html = raw.replace(
        INDEX_HTML_REFRESH_PLACEHOLDER,
        &state.refresh_ms.to_string(),
    );
    Html(html)
}

fn load_index_html() -> String {
    #[cfg(debug_assertions)]
    {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("files/web_index.html");
        match std::fs::read_to_string(&path) {
            Ok(contents) => return contents,
            Err(e) => {
                tracing::warn!(?path, %e, "failed to read web_index.html from disk, using embedded copy");
            }
        }
    }
    include_str!("../files/web_index.html").to_string()
}

// GET /api/topology — return topology JSON
async fn topology_handler(state: axum::extract::State<AppState>) -> Json<TopologyResponse> {
    let snapshot = {
        let testnet = state.testnet.read().await;
        let el_data = state.el_live_data.read().await;
        let container_statuses = state.container_statuses.read().await;
        let flattened: HashMap<String, String> = container_statuses
            .iter()
            .map(|(k, v)| (k.clone(), v.status.clone()))
            .collect();
        snapshot_topology_state(
            &testnet,
            &el_data,
            &flattened,
            Arc::clone(&state.region_assignments),
        )
    };
    let response = build_topology(&snapshot).await;
    Json(response)
}

// POST /api/node/{name}/{action}?target=cl|el — per-node/container action
//
// `name` is the node name (e.g. "validator1").
// `action` is one of: start, stop, restart, kill, pause, unpause, disconnect, reconnect.
// `target` query param: "cl" (CL container only), "el" (EL container only),
// or omitted (both containers, i.e. the whole node).

#[derive(Deserialize)]
struct NodeActionQuery {
    target: Option<String>,
}

fn resolve_action_targets(
    target: Option<&str>,
    node_name: &str,
) -> std::result::Result<Vec<String>, &'static str> {
    match target {
        Some(LAYER_CL) => Ok(vec![container_name(node_name, ContainerKind::Consensus)]),
        Some(LAYER_EL) => Ok(vec![container_name(node_name, ContainerKind::Execution)]),
        Some(_) => Err("invalid target (expected 'cl' or 'el')"),
        None => Ok(vec![node_name.to_string()]),
    }
}

async fn node_action_handler(
    state: axum::extract::State<AppState>,
    AxumPath((name, action)): AxumPath<(String, String)>,
    axum::extract::Query(query): axum::extract::Query<NodeActionQuery>,
) -> Json<serde_json::Value> {
    let valid_actions = [
        "start",
        "stop",
        "restart",
        "kill",
        "pause",
        "unpause",
        "disconnect",
        "reconnect",
    ];
    if !valid_actions.contains(&action.as_str()) {
        return Json(serde_json::json!({"ok": false, "error": "unknown action"}));
    }

    // Resolve the target container name(s)
    let targets = match resolve_action_targets(query.target.as_deref(), &name) {
        Ok(targets) => targets,
        Err(err) => return Json(serde_json::json!({"ok": false, "error": err})),
    };

    let testnet = Arc::clone(&state.testnet);
    let result = match action.as_str() {
        "start" => {
            let testnet = testnet.read().await;
            testnet.start(targets, false).await
        }
        "stop" => {
            let testnet = testnet.read().await;
            testnet.stop(targets).await
        }
        "restart" => {
            let mut testnet = testnet.write().await;
            testnet.with_seed(None);
            testnet
                .perturb(
                    Perturbation::Restart { targets },
                    Duration::from_secs(0),
                    Duration::from_secs(0),
                )
                .await
        }
        "pause" | "kill" | "unpause" => {
            let testnet = testnet.read().await;
            match testnet.nodes_metadata.expand_to_containers_list(&targets) {
                Ok(containers) => {
                    let infra = Arc::clone(&testnet.infra);
                    let action = action.clone();
                    tokio::task::spawn_blocking(move || match action.as_str() {
                        "pause" => infra.pause(&containers),
                        "kill" => infra.kill(&containers),
                        _ => infra.unpause(&containers),
                    })
                    .await
                    .unwrap_or_else(|e| Err(color_eyre::eyre::eyre!("{e}")))
                }
                Err(e) => Err(e),
            }
        }
        "disconnect" | "reconnect" => {
            let testnet = testnet.read().await;
            match testnet.nodes_metadata.expand_to_containers_list(&targets) {
                Ok(container_names) => {
                    let containers = testnet.nodes_metadata.to_containers(&container_names);
                    let containers_subnets: Vec<_> = containers
                        .iter()
                        .map(|c| (*c, c.subnet_ip_map().keys().collect::<Vec<_>>()))
                        .collect();
                    let containers_subnets: Vec<_> = containers_subnets
                        .iter()
                        .map(|(c, subs)| (*c, subs.as_slice()))
                        .collect();
                    let infra = Arc::clone(&testnet.infra);
                    if action == "disconnect" {
                        infra.disconnect(&containers_subnets)
                    } else {
                        infra.connect(&containers_subnets)
                    }
                }
                Err(e) => Err(e),
            }
        }
        _ => unreachable!(),
    };

    match result {
        Ok(()) => Json(serde_json::json!({"ok": true})),
        Err(e) => {
            warn!(action = %action, node = %name, "Node action failed: {e}");
            Json(serde_json::json!({"ok": false, "error": e.to_string()}))
        }
    }
}

/// Build the topology response from all available data sources.
///
/// EL heights and peers are read from the shared `ElLiveData` (populated by
/// background WS subscribers). CL data and proposer are still fetched via HTTP.
async fn build_topology(snapshot: &TopologySnapshot) -> TopologyResponse {
    let manifest = &snapshot.manifest;
    let nodes_metadata = &snapshot.nodes_metadata;
    let el_data = &snapshot.el_data;
    let container_statuses = &snapshot.container_statuses;
    let region_assignments = snapshot.region_assignments.as_ref();

    let mut nodes = build_node_list(manifest, region_assignments);
    let mut networks = IndexMap::new();
    let mut errors = Vec::new();
    let mut has_live_data = false;

    // --- Manifest-based graphs (always available) ---

    // CL Persistent Peers edges are no longer a separate tab.
    // The frontend overlays them on the Manifest tab via a toggle,
    // using the per-node `explicit_cl_peers` field.

    let manifest_edges = build_manifest_topology_edges(manifest);
    networks.insert(
        "Manifest".to_string(),
        NetworkGraph {
            layer: LAYER_CL.to_string(),
            source: SOURCE_MANIFEST.to_string(),
            edges: manifest_edges,
        },
    );

    // Note: EL Trusted Peers (manifest) is omitted — the live "EL Peers" tab
    // with a "Trusted only" frontend toggle replaces it.

    // --- Live data (best-effort, requires running testnet) ---

    let mut latest_height = None;
    let mut current_proposer = None;

    if !nodes_metadata.nodes.is_empty() {
        // EL heights + peers come from the WS-populated shared state (no HTTP calls)
        let el_has_data = !el_data.heights.is_empty();

        // Populate per-node heights from cached EL data
        latest_height = el_data.heights.values().copied().max();
        for node in &mut nodes {
            node.height = el_data.heights.get(&node.name).copied();
        }

        // Populate container statuses from background-polled cache
        populate_container_statuses(&mut nodes, container_statuses);

        // Build EL peer edges from cached data
        let ip_to_name = build_ip_to_node_name_map(nodes_metadata);
        let (el_network, el_unreachable) = build_el_live_edges(el_data, &ip_to_name);
        if let Some((name, graph)) = el_network {
            has_live_data = true;
            networks.insert(name, graph);
        }
        populate_el_peer_details(&mut nodes, el_data, &ip_to_name);

        // CL data + proposer are still fetched via HTTP per-request.
        // Skip nodes whose CL container is not running to avoid timeout delays.
        let (cl_result, (proposer, rounds)) = tokio::join!(
            fetch_cl_live_topology(nodes_metadata, container_statuses),
            fetch_current_proposer(nodes_metadata, container_statuses),
        );

        current_proposer = proposer;
        for node in &mut nodes {
            node.round = rounds.get(&node.name).copied();
        }

        // Process CL live data
        let (cl_networks, cl_errors, cl_unreachable, mut cl_peers_map) = cl_result;
        if !cl_networks.is_empty() {
            has_live_data = true;
        }
        for (name, graph) in cl_networks {
            networks.insert(name, graph);
        }
        // Assign per-node CL peer lists for the detail panel
        for node in &mut nodes {
            if let Some(peers) = cl_peers_map.remove(&node.name) {
                node.cl_peers = peers;
            }
        }

        if el_has_data {
            has_live_data = true;
        }

        // Only report errors when we have live data (not manifest-only mode)
        if has_live_data {
            merge_per_node_errors(&cl_errors, el_data, container_statuses, &mut errors);
        }

        if has_live_data {
            detect_unreachable_nodes(
                &mut nodes,
                el_data,
                cl_unreachable,
                el_unreachable,
                current_proposer.as_deref(),
                container_statuses,
                &mut errors,
            );
        }

        // Populate per-node mempool data from cached poller results
        populate_mempool_data(&mut nodes, el_data);
    }

    let source = if has_live_data {
        SOURCE_LIVE
    } else {
        SOURCE_MANIFEST
    };

    TopologyResponse {
        testnet_name: snapshot.testnet_name.clone(),
        source: source.to_string(),
        latest_height,
        current_proposer,
        nodes,
        networks,
        errors,
        node_regions: build_node_regions(manifest, region_assignments),
        mempool_max: resolve_mempool_max(manifest),
    }
}

fn snapshot_topology_state(
    testnet: &Testnet,
    el_data: &ElLiveData,
    container_statuses: &HashMap<String, String>,
    region_assignments: Arc<HashMap<String, String>>,
) -> TopologySnapshot {
    TopologySnapshot {
        testnet_name: resolve_testnet_name(testnet),
        manifest: testnet.manifest.clone(),
        nodes_metadata: testnet.nodes_metadata.clone(),
        el_data: el_data.clone(),
        container_statuses: container_statuses.clone(),
        region_assignments,
    }
}

/// Derive testnet name: prefer manifest `name` field, fall back to directory stem.
fn resolve_testnet_name(testnet: &Testnet) -> String {
    testnet
        .manifest
        .name
        .clone()
        .or_else(|| {
            testnet
                .dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
        })
        .unwrap_or_default()
}

/// Populate per-node EL peer lists from cached `admin_peers` data.
fn populate_el_peer_details(
    nodes: &mut [GraphNode],
    el_data: &ElLiveData,
    ip_to_name: &HashMap<String, String>,
) {
    for node in nodes {
        if let Some(peers) = el_data.peers.get(&node.name) {
            node.el_peers = peers
                .iter()
                .filter_map(|p| {
                    let peer_name = resolve_enode_peer_name(&p.enode, ip_to_name)?;
                    Some(NodePeer {
                        name: peer_name,
                        trusted: p.network.trusted,
                        inbound: p.network.inbound,
                        static_node: p.network.static_node,
                        ..Default::default()
                    })
                })
                .collect();
            node.el_peers.sort();
        }
    }
}

fn populate_container_statuses(
    nodes: &mut [GraphNode],
    container_statuses: &HashMap<String, String>,
) {
    for node in nodes {
        node.cl_status =
            node_container_status(container_statuses, &node.name, ContainerKind::Consensus)
                .map(str::to_owned);
        node.el_status =
            node_container_status(container_statuses, &node.name, ContainerKind::Execution)
                .map(str::to_owned);
    }
}

fn populate_mempool_data(nodes: &mut [GraphNode], el_data: &ElLiveData) {
    for node in nodes {
        if let Some(&counts) = el_data.mempool.get(&node.name) {
            node.mempool = Some(counts);
        }
    }
}

/// Mark nodes as unreachable or disconnected based on which layers failed.
///
/// If the failing layer's container is "disconnected", the node status is
/// "disconnected" (orange) rather than "unreachable" (red).
fn detect_unreachable_nodes(
    nodes: &mut [GraphNode],
    el_data: &ElLiveData,
    cl_unreachable: Vec<String>,
    el_unreachable: Vec<String>,
    current_proposer: Option<&str>,
    container_statuses: &HashMap<String, String>,
    errors: &mut Vec<String>,
) {
    let cl_unreachable: BTreeSet<String> = cl_unreachable.into_iter().collect();
    let mut el_unreachable_set: BTreeSet<String> = el_unreachable.into_iter().collect();
    for name in el_data.errors.keys() {
        el_unreachable_set.insert(name.clone());
    }
    if let Some(proposer) = current_proposer {
        el_unreachable_set.remove(proposer);
    }

    let errored_nodes: BTreeSet<String> = errors
        .iter()
        .filter_map(|e| e.split(':').next().map(str::to_string))
        .collect();
    for node in nodes.iter_mut() {
        let cl_down = cl_unreachable.contains(&node.name);
        let el_down = el_unreachable_set.contains(&node.name);
        if !cl_down && !el_down {
            continue;
        }

        // Check if the failing layer's container is disconnected (not truly unreachable)
        let cl_disconnected = cl_down
            && node_container_has_status(
                container_statuses,
                &node.name,
                ContainerKind::Consensus,
                CONTAINER_STATUS_DISCONNECTED,
            );
        let el_disconnected = el_down
            && node_container_has_status(
                container_statuses,
                &node.name,
                ContainerKind::Execution,
                CONTAINER_STATUS_DISCONNECTED,
            );

        // If all failing layers are disconnected (not crashed), mark as disconnected
        let all_disconnected = (!cl_down || cl_disconnected) && (!el_down || el_disconnected);
        node.status = if all_disconnected {
            NODE_STATUS_DISCONNECTED
        } else {
            NODE_STATUS_UNREACHABLE
        }
        .to_string();

        if !errored_nodes.contains(&node.name) {
            let layer = if all_disconnected {
                match (cl_down, el_down) {
                    (true, true) => "CL+EL disconnected",
                    (true, false) => "CL disconnected",
                    (false, true) => "EL disconnected",
                    _ => unreachable!(),
                }
            } else {
                match (cl_down, el_down) {
                    (true, true) => "CL+EL not responding",
                    (true, false) => "CL not responding",
                    (false, true) => "EL not responding",
                    _ => unreachable!(),
                }
            };
            errors.push(format!("{}: {layer}", node.name));
        }
    }
}

/// Merge CL and EL errors into unified per-node messages.
///
/// If both CL and EL report errors for the same node, they are replaced with
/// a single "CL+EL not responding" message instead of two separate entries.
///
/// Skips nodes whose failing container is disconnected (those are handled by
/// `detect_unreachable_nodes` with a more specific "disconnected" message).
fn merge_per_node_errors(
    cl_errors: &[String],
    el_data: &ElLiveData,
    container_statuses: &HashMap<String, String>,
    errors: &mut Vec<String>,
) {
    let is_disconnected = |name: &str, kind: ContainerKind| -> bool {
        node_container_has_status(
            container_statuses,
            name,
            kind,
            CONTAINER_STATUS_DISCONNECTED,
        )
    };

    let mut cl_nodes: BTreeSet<String> = BTreeSet::new();
    for e in cl_errors {
        if let Some(name) = e.split(':').next() {
            if !is_disconnected(name, ContainerKind::Consensus) {
                cl_nodes.insert(name.to_string());
            }
        }
    }
    let mut el_nodes: BTreeSet<String> = BTreeSet::new();
    for name in el_data.errors.keys() {
        if !is_disconnected(name, ContainerKind::Execution) {
            el_nodes.insert(name.clone());
        }
    }

    let both: BTreeSet<String> = cl_nodes.intersection(&el_nodes).cloned().collect();

    for name in &both {
        errors.push(format!("{name}: CL+EL not responding"));
    }
    for e in cl_errors {
        let node = e.split(':').next().unwrap_or("");
        if cl_nodes.contains(node) && !both.contains(node) {
            errors.push(e.clone());
        }
    }
    for (name, msg) in &el_data.errors {
        if el_nodes.contains(name.as_str()) && !both.contains(name.as_str()) {
            errors.push(msg.clone());
        }
    }
}

// ── Manifest-based topology ─────────────────────────────────────────────

/// Load region assignments from the testnet's `region_assignments.json` file.
///
/// Returns an empty map if the file doesn't exist or can't be parsed
/// (e.g. latency emulation was never enabled for this testnet).
fn load_region_assignments(testnet_dir: &Path) -> HashMap<String, String> {
    let path = testnet_dir.join(REGION_ASSIGNMENTS_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(map) => map,
            Err(e) => {
                warn!(?path, "Failed to parse {REGION_ASSIGNMENTS_FILENAME}: {e}");
                HashMap::new()
            }
        },
        Err(_) => HashMap::new(),
    }
}

/// Build the list of graph nodes from the manifest.
fn build_node_list(
    manifest: &Manifest,
    region_assignments: &HashMap<String, String>,
) -> Vec<GraphNode> {
    manifest
        .nodes
        .iter()
        .map(|(name, node)| {
            let node_type = match node.node_type {
                NodeType::Validator => "validator",
                NodeType::NonValidator => "non_validator",
            };
            let subnets = manifest.subnets.subnets_of(name);
            let region = region_assignments.get(name).or(node.region.as_ref());
            let config = build_node_config(node, region);

            let manifest_cl_peers =
                manifest_peers_or_subnet_default(&node.cl_persistent_peers, manifest, name);
            let manifest_el_peers =
                manifest_peers_or_subnet_default(&node.el_trusted_peers, manifest, name);

            GraphNode {
                name: name.clone(),
                node_type: node_type.to_string(),
                consensus_enabled: node.cl_config.consensus_enabled(),
                status: NODE_STATUS_OK.to_string(),
                subnets,
                height: None,
                round: None,
                cl_status: None,
                el_status: None,
                config,
                manifest_cl_peers,
                manifest_el_peers,
                explicit_cl_peers: node.cl_persistent_peers.clone().unwrap_or_default(),
                explicit_el_peers: node.el_trusted_peers.clone().unwrap_or_default(),
                cl_peers: Vec::new(),
                el_peers: Vec::new(),
                mempool: None,
            }
        })
        .collect()
}

/// Build a JSON map of non-default manifest configuration for a node.
///
/// This is an explicit allowlist, not a `#[derive(Serialize)]` of `Node`.
/// Rationale:
/// - Only fields differing from their defaults (None, false, empty) are
///   emitted, so the detail panel highlights what's actually interesting
///   about each node instead of a wall of defaults.
/// - The schema is decoupled from the on-disk manifest representation,
///   so internal layout changes don't leak into the frontend contract.
/// - Sensitive or irrelevant fields (raw configs, full cl_config blobs)
///   stay out of the response by default.
///
/// Trade-off: surfacing a new manifest field in the detail panel requires
/// adding a branch here. Prefer keeping the allowlist narrow and only
/// adding fields that are genuinely useful for operators.
///
/// The `region` parameter is the resolved region from `region_assignments.json`
/// (falling back to the manifest's `node.region`).
fn build_node_config(node: &crate::manifest::Node, region: Option<&String>) -> serde_json::Value {
    let mut map = serde_json::Map::new();

    if let Some(region) = region {
        map.insert("region".to_string(), serde_json::json!(region));
    }
    if let Some(start_at) = node.start_at {
        map.insert("start_at".to_string(), serde_json::json!(start_at));
    }
    if let Some(ref rs) = node.remote_signer {
        map.insert("remote_signer".to_string(), serde_json::json!(rs.get()));
    }
    if node.follow {
        map.insert("follow".to_string(), serde_json::json!(true));
    }
    if !node.follow_endpoints.is_empty() {
        map.insert(
            "follow_endpoints".to_string(),
            serde_json::json!(node.follow_endpoints),
        );
    }
    if let Some(ref peers) = node.cl_persistent_peers {
        map.insert("cl_persistent_peers".to_string(), serde_json::json!(peers));
    }
    if node.cl_persistent_peers_only {
        map.insert(
            "cl_persistent_peers_only".to_string(),
            serde_json::json!(true),
        );
    }
    if let Some(ref peers) = node.el_trusted_peers {
        map.insert("el_trusted_peers".to_string(), serde_json::json!(peers));
    }
    if let Some(vp) = node.cl_voting_power {
        map.insert("cl_voting_power".to_string(), serde_json::json!(vp));
    }

    serde_json::Value::Object(map)
}

/// Build edges from the manifest's subnet-based peer topology.
///
/// For each node, derives peers from explicit `cl_persistent_peers` or shared
/// subnets (the same logic as `manifest_peers_or_subnet_default`). This shows
/// the expected topology even when no testnet is running.
fn build_manifest_topology_edges(manifest: &Manifest) -> Vec<GraphEdge> {
    let mut seen = BTreeSet::new();
    let mut edges = Vec::new();

    for (name, node) in &manifest.nodes {
        let peers = manifest_peers_or_subnet_default(&node.cl_persistent_peers, manifest, name);
        for peer in peers {
            let (a, b) = normalize_edge(name, &peer);
            if seen.insert((a.clone(), b.clone())) {
                edges.push(GraphEdge {
                    from: a,
                    to: b,
                    metadata: serde_json::Value::Null,
                });
            }
        }
    }
    edges
}

// ── Live CL topology ────────────────────────────────────────────────────

/// Response from the CL `/network-state` endpoint.
#[derive(Deserialize)]
struct ClNetworkState {
    local_node: ClLocalNode,
    peers: Vec<ClPeerInfo>,
}

#[derive(Deserialize)]
struct ClLocalNode {
    moniker: String,
}

#[derive(Deserialize)]
struct ClPeerInfo {
    moniker: String,
    connection_direction: Option<String>,
    score: f64,
    topics: Vec<String>,
}

/// Partial response from the CL `/status` endpoint (only the fields we need).
#[derive(Deserialize)]
struct ClAppStatus {
    address: String,
    proposer: String,
    round: Option<i64>,
}

/// Check whether an address is the zero address (all zeros after removing `0x` prefix).
fn is_zero_address(addr: &str) -> bool {
    let stripped = addr.strip_prefix("0x").unwrap_or(addr);
    stripped.chars().all(|c| c == '0')
}

/// Fetch the current block proposer and per-node round by querying `/status` on each CL node.
///
/// Each node's status reports its own `address`, the current `proposer` address, and its `round`.
/// Returns `(proposer_name, node_name → round)`.
async fn fetch_current_proposer(
    nodes: &NodesMetadata,
    container_statuses: &HashMap<String, String>,
) -> (Option<String>, HashMap<String, i64>) {
    let client = build_http_client();

    let mut futures = Vec::new();
    for (name, meta) in &nodes.nodes {
        if !node_container_has_status(
            container_statuses,
            name,
            ContainerKind::Consensus,
            CONTAINER_STATUS_RUNNING,
        ) {
            continue;
        }
        let url = format!(
            "http://{}:{}/status",
            meta.public_ip, meta.consensus.rpc_port
        );
        let client = client.clone();
        let name = name.clone();
        futures.push(async move {
            let result = async {
                let resp = client.get(&url).send().await?;
                resp.json::<ClAppStatus>().await
            }
            .await;
            (name, result)
        });
    }

    let results = futures::future::join_all(futures).await;

    let mut address_to_name: HashMap<String, String> = HashMap::new();
    let mut proposer_address: Option<String> = None;
    let mut rounds: HashMap<String, i64> = HashMap::new();

    for (node_name, result) in results {
        match result {
            Ok(status) => {
                address_to_name.insert(status.address, node_name.clone());
                if let Some(r) = status.round {
                    rounds.insert(node_name, r);
                }
                if proposer_address.is_none() && !is_zero_address(&status.proposer) {
                    proposer_address = Some(status.proposer);
                }
            }
            Err(e) => debug!(node = %node_name, "CL /status error: {e}"),
        }
    }

    let proposer = proposer_address.and_then(|addr| address_to_name.get(&addr).cloned());
    (proposer, rounds)
}

/// Fetch live CL topology from each node's `/network-state` endpoint.
///
/// Returns: (topic_networks, errors, unreachable_node_names, per_node_cl_peers)
async fn fetch_cl_live_topology(
    nodes: &NodesMetadata,
    container_statuses: &HashMap<String, String>,
) -> (
    IndexMap<String, NetworkGraph>,
    Vec<String>,
    Vec<String>,
    HashMap<String, Vec<NodePeer>>,
) {
    let client = build_http_client();

    let mut futures = Vec::new();
    let mut skipped_unreachable: Vec<String> = Vec::new();
    for (name, meta) in &nodes.nodes {
        if !node_container_has_status(
            container_statuses,
            name,
            ContainerKind::Consensus,
            CONTAINER_STATUS_RUNNING,
        ) {
            skipped_unreachable.push(name.clone());
            continue;
        }
        let url = format!(
            "http://{}:{}/network-state",
            meta.public_ip, meta.consensus.rpc_port
        );
        let client = client.clone();
        let name = name.clone();
        futures.push(async move {
            let result = async {
                let resp = client.get(&url).send().await?;
                resp.json::<ClNetworkState>().await
            }
            .await;
            (name, url, result)
        });
    }

    let results = futures::future::join_all(futures).await;

    // Collect per-topic edges across all nodes. Key is (from, to) for dedup;
    // value holds the edge metadata (score, direction) from the first report.
    let mut topic_edges: IndexMap<String, IndexMap<(String, String), serde_json::Value>> =
        IndexMap::new();
    let mut errors = Vec::new();
    let mut unreachable = Vec::new();
    let mut cl_peers_map: HashMap<String, Vec<NodePeer>> = HashMap::new();

    for (node_name, url, result) in results {
        match result {
            Ok(state) => {
                let local_moniker = &state.local_node.moniker;
                trace!(node = %node_name, moniker = %local_moniker, peers = state.peers.len(), "CL network-state fetched");

                for peer in &state.peers {
                    let metadata = serde_json::json!({
                        "score": peer.score,
                        "direction": peer.connection_direction,
                    });
                    for topic in &peer.topics {
                        let (a, b) = normalize_edge(local_moniker, &peer.moniker);
                        topic_edges
                            .entry(topic.clone())
                            .or_default()
                            .entry((a, b))
                            .or_insert(metadata.clone());
                    }
                }

                let mut node_peers: Vec<NodePeer> = state
                    .peers
                    .iter()
                    .map(|p| NodePeer {
                        name: p.moniker.clone(),
                        direction: p.connection_direction.clone(),
                        score: Some(p.score),
                        topics: p.topics.clone(),
                        ..Default::default()
                    })
                    .collect();
                node_peers.sort();
                cl_peers_map.insert(node_name.clone(), node_peers);
            }
            Err(e) => {
                debug!(url, "{node_name}: CL fetch error: {e}");
                errors.push(format!("{node_name}: CL unreachable"));
                unreachable.push(node_name);
            }
        }
    }

    // Build NetworkGraph per topic
    let networks: IndexMap<String, NetworkGraph> = topic_edges
        .into_iter()
        .map(|(topic, edge_map)| {
            let edges = edge_map
                .into_iter()
                .map(|((from, to), metadata)| GraphEdge { from, to, metadata })
                .collect();
            (
                topic,
                NetworkGraph {
                    layer: LAYER_CL.to_string(),
                    source: SOURCE_LIVE.to_string(),
                    edges,
                },
            )
        })
        .collect();

    unreachable.extend(skipped_unreachable);
    (networks, errors, unreachable, cl_peers_map)
}

// ── Live EL topology (from cached WS data) ─────────────────────────────

/// Build EL peer edges from in-memory cached peer data.
///
/// Reads from the shared `ElLiveData` populated by WS subscribers instead
/// of making HTTP `admin_peers` calls.
///
/// EL unreachable detection is handled separately via `el_data.errors` in
/// `detect_unreachable_nodes`, so this function always returns an empty
/// unreachable list.
fn build_el_live_edges(
    el_data: &ElLiveData,
    ip_to_name: &HashMap<String, String>,
) -> (Option<(String, NetworkGraph)>, Vec<String>) {
    // Map from normalized (from, to) pair to edge index, so we can merge
    // metadata when both sides report the same connection.
    let mut edge_index: HashMap<(String, String), usize> = HashMap::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let unreachable = Vec::new();

    for (node_name, peers) in &el_data.peers {
        for peer in peers {
            let Some(peer_name) = resolve_enode_peer_name(&peer.enode, ip_to_name) else {
                continue;
            };

            let (a, b) = normalize_edge(node_name, &peer_name);
            let key = (a.clone(), b.clone());
            if let Some(&idx) = edge_index.get(&key) {
                // Merge: mark trusted/static if either side reports it.
                let md = &mut edges[idx].metadata;
                if peer.network.trusted {
                    md["trusted"] = serde_json::json!(true);
                }
                if peer.network.static_node {
                    md["static_node"] = serde_json::json!(true);
                }
            } else {
                let metadata = serde_json::json!({
                    "inbound": peer.network.inbound,
                    "trusted": peer.network.trusted,
                    "static_node": peer.network.static_node,
                });
                edge_index.insert(key, edges.len());
                edges.push(GraphEdge {
                    from: a,
                    to: b,
                    metadata,
                });
            }
        }
    }

    if edges.is_empty() {
        return (None, unreachable);
    }

    let network = NetworkGraph {
        layer: LAYER_EL.to_string(),
        source: SOURCE_LIVE.to_string(),
        edges,
    };

    (Some(("EL Peers".to_string(), network)), unreachable)
}

// ── Mempool toggle ──────────────────────────────────────────────────────

/// POST /api/mempool/{action} — toggle mempool polling on or off.
async fn mempool_toggle_handler(
    state: axum::extract::State<AppState>,
    AxumPath(action): AxumPath<String>,
) -> Json<serde_json::Value> {
    match action.as_str() {
        "on" => {
            state.mempool_active.store(true, Ordering::Relaxed);
            Json(serde_json::json!({"ok": true, "mempool": "on"}))
        }
        "off" => {
            state.mempool_active.store(false, Ordering::Relaxed);
            state.el_live_data.write().await.mempool.clear();
            Json(serde_json::json!({"ok": true, "mempool": "off"}))
        }
        _ => Json(serde_json::json!({"ok": false, "error": "expected 'on' or 'off'"})),
    }
}

// ── Docker container status ──────────────────────────────────────────────

/// Spawn two background tasks for container status tracking:
/// 1. A `docker events` subscriber for real-time state changes (die, start, pause, unpause)
/// 2. A periodic `docker inspect` poller for network disconnect detection (no event for that)
fn spawn_container_status_poller(
    cache: Arc<RwLock<HashMap<String, CachedStatus>>>,
    container_names: Vec<String>,
    interval_ms: u64,
) {
    // Real-time Docker events subscriber
    let cache_events = Arc::clone(&cache);
    tokio::spawn(async move {
        loop {
            debug!("Starting docker events subscriber");
            match spawn_docker_events_subscriber(Arc::clone(&cache_events)).await {
                Ok(()) => debug!("Docker events stream ended, restarting"),
                Err(e) => debug!("Docker events subscriber error: {e}"),
            }
            tokio::time::sleep(DOCKER_EVENTS_RETRY_DELAY).await;
        }
    });

    // Periodic inspect for network disconnect detection. We capture the
    // snapshot start time before the blocking call so that any entry the
    // events subscriber touches while `docker inspect` is running wins the
    // merge and isn't clobbered by our older view.
    tokio::spawn(async move {
        loop {
            let names = container_names.clone();
            let snapshot_start = std::time::Instant::now();
            let statuses =
                match tokio::task::spawn_blocking(move || fetch_container_statuses(&names)).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("Container status poller panicked: {e}");
                        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                        continue;
                    }
                };
            if !statuses.is_empty() {
                let mut cache = cache.write().await;
                for (name, status) in statuses {
                    match cache.get(&name) {
                        Some(existing) if existing.updated_at > snapshot_start => continue,
                        _ => {
                            cache.insert(name, CachedStatus::new(status));
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    });
}

/// Subscribe to `docker events` and update container statuses in real-time.
async fn spawn_docker_events_subscriber(
    cache: Arc<RwLock<HashMap<String, CachedStatus>>>,
) -> color_eyre::eyre::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let mut child = Command::new("docker")
        .args([
            "events",
            "--filter",
            "type=container",
            "--filter",
            &format!(
                "label=com.docker.compose.project={}",
                crate::infra::COMPOSE_PROJECT_NAME
            ),
            "--format",
            "{{.Actor.Attributes.name}} {{.Action}}",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| color_eyre::eyre::eyre!("Failed to capture docker events stdout"))?;
    let mut lines = BufReader::new(stdout).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() < 2 {
            continue;
        }
        let container_name = parts[0];
        let action = parts[1];

        let new_status = match action {
            "die" | "kill" | "stop" => Some("exited"),
            "start" => Some(CONTAINER_STATUS_RUNNING),
            "pause" => Some("paused"),
            "unpause" => Some(CONTAINER_STATUS_RUNNING),
            _ => None,
        };

        if let Some(status) = new_status {
            trace!(container = container_name, status, "Docker event");
            cache.write().await.insert(
                container_name.to_string(),
                CachedStatus::new(status.to_string()),
            );
        }
    }

    Ok(())
}

/// Fetch Docker container statuses via `docker inspect`.
///
/// Derives a single status per container from `State.Status`, `State.Paused`,
/// `State.Restarting`, and the number of attached networks.
/// A running container with only 1 network (host-access) is "disconnected".
fn fetch_container_statuses(service_names: &[String]) -> HashMap<String, String> {
    if service_names.is_empty() {
        return HashMap::new();
    }

    let format = "{{index .Config.Labels \"com.docker.compose.service\"}} \
                  {{.State.Paused}} {{.State.Restarting}} {{.State.Status}} \
                  {{len .NetworkSettings.Networks}}";
    let mut args = vec![
        "inspect".to_string(),
        "--format".to_string(),
        format.to_string(),
    ];
    for svc in service_names {
        args.push(svc.clone());
    }

    let output = match std::process::Command::new("docker")
        .args(&args)
        .stderr(std::process::Stdio::null())
        .output()
    {
        // docker inspect exits non-zero when some containers don't exist yet;
        // stdout still contains output for the ones that do exist.
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(e) => {
            warn!("Failed to run docker inspect: {e}");
            return HashMap::new();
        }
    };

    let mut map = HashMap::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.trim().splitn(5, ' ').collect();
        if parts.len() < 5 {
            continue;
        }
        let service = parts[0];
        let paused = parts[1] == "true";
        let restarting = parts[2] == "true";
        let status = parts[3];
        let net_count: usize = parts[4].parse().unwrap_or(0);
        let derived = if paused {
            "paused"
        } else if restarting {
            "restarting"
        } else if status == CONTAINER_STATUS_RUNNING && net_count <= 1 {
            CONTAINER_STATUS_DISCONNECTED
        } else {
            status
        };
        map.insert(service.to_string(), derived.to_string());
    }
    map
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Return explicit manifest peer names, or every other node sharing a subnet
fn manifest_peers_or_subnet_default(
    explicit: &Option<Vec<String>>,
    manifest: &Manifest,
    node_name: &NodeName,
) -> Vec<String> {
    match explicit {
        Some(peers) => peers.clone(),
        None => manifest
            .nodes
            .keys()
            .filter(|peer| *peer != node_name)
            .filter(|peer| !manifest.subnets.shared_subnets(node_name, peer).is_empty())
            .cloned()
            .collect(),
    }
}

/// Resolve a peer's node name from its enode URL (format: `enode://<id>@<host>:<port>`).
fn resolve_enode_peer_name(enode: &str, ip_to_name: &HashMap<String, String>) -> Option<String> {
    enode
        .split('@')
        .nth(1)
        .and_then(|host_port| host_port.split(':').next())
        .and_then(|ip| ip_to_name.get(ip))
        .cloned()
}

/// Map each EL private IP address back to its node name.
fn build_ip_to_node_name_map(nodes: &NodesMetadata) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (node_name, meta) in &nodes.nodes {
        for ip in meta.execution.private_ip_addresses() {
            map.insert(ip, node_name.clone());
        }
    }
    map
}

fn node_container_status<'a>(
    container_statuses: &'a HashMap<String, String>,
    node_name: &str,
    kind: ContainerKind,
) -> Option<&'a str> {
    let container = container_name(node_name, kind);
    container_statuses.get(&container).map(String::as_str)
}

fn node_container_has_status(
    container_statuses: &HashMap<String, String>,
    node_name: &str,
    kind: ContainerKind,
    expected: &str,
) -> bool {
    node_container_status(container_statuses, node_name, kind) == Some(expected)
}

fn resolve_mempool_max(manifest: &Manifest) -> Option<(u64, u64)> {
    let mut pending_max = 0u64;
    let mut queued_max = 0u64;
    for node in manifest.nodes.values() {
        pending_max = pending_max.max(
            node.el_config
                .txpool
                .pending_max_count
                .unwrap_or(DEFAULT_POOL_MAX),
        );
        queued_max = queued_max.max(
            node.el_config
                .txpool
                .queued_max_count
                .unwrap_or(DEFAULT_POOL_MAX),
        );
    }
    if pending_max == 0 {
        pending_max = DEFAULT_POOL_MAX;
    }
    if queued_max == 0 {
        queued_max = DEFAULT_POOL_MAX;
    }
    Some((pending_max, queued_max))
}

fn build_node_regions(
    manifest: &Manifest,
    region_assignments: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut node_regions = HashMap::new();
    for (name, node) in &manifest.nodes {
        if let Some(region) = region_assignments.get(name).or(node.region.as_ref()) {
            node_regions.insert(name.clone(), region.clone());
        }
    }
    node_regions
}

fn container_name(node: &str, kind: ContainerKind) -> String {
    format!("{}_{}", node, kind.suffix())
}

fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("failed to build HTTP client")
}

/// Normalize an edge by sorting the two node names alphabetically.
fn normalize_edge(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

// ── JSON types ──────────────────────────────────────────────────────────

/// Top-level response for `GET /api/topology`.
#[derive(Serialize, Default)]
pub(crate) struct TopologyResponse {
    /// Testnet name from the manifest (or manifest filename stem).
    pub testnet_name: String,
    /// Whether this data comes from "manifest" or "live" sources.
    pub source: String,
    /// Latest block height across all reachable nodes (None if no live data).
    pub latest_height: Option<u64>,
    /// Node name of the current block proposer (None if unknown).
    pub current_proposer: Option<String>,
    /// All nodes in the testnet.
    pub nodes: Vec<GraphNode>,
    /// Named overlay networks, keyed by display name.
    pub networks: IndexMap<String, NetworkGraph>,
    /// Errors encountered while fetching live data.
    pub errors: Vec<String>,
    /// Per-node region assignments (node name to AWS region string).
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub node_regions: HashMap<String, String>,
    /// Txpool max sizes: (pending_max, queued_max) from manifest config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mempool_max: Option<(u64, u64)>,
}

/// A node in the topology graph.
#[derive(Serialize, Clone)]
pub(crate) struct GraphNode {
    pub name: String,
    pub node_type: String,
    /// Whether the consensus engine runs for this node. Sync-only followers
    /// (`--no-consensus`) report `false`; frontend uses this to size nodes.
    pub consensus_enabled: bool,
    pub status: String,
    pub subnets: Vec<String>,
    /// Block height for this node (None if unreachable or no live data).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    /// Consensus round for this node (from CL /status).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<i64>,
    /// Docker container status for the CL container (e.g. "running", "exited", "paused").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cl_status: Option<String>,
    /// Docker container status for the EL container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub el_status: Option<String>,
    /// Non-default manifest configuration for this node.
    pub config: serde_json::Value,
    /// Expected CL peers from the manifest (persistent peers, or full mesh).
    pub manifest_cl_peers: Vec<String>,
    /// Expected EL trusted peers from the manifest.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub manifest_el_peers: Vec<String>,
    /// CL explicit/persistent peers from the manifest (direct delivery, bypass mesh).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub explicit_cl_peers: Vec<String>,
    /// EL explicit/trusted peers from the manifest.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub explicit_el_peers: Vec<String>,
    /// CL mesh peers for this node (from /network-state).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cl_peers: Vec<NodePeer>,
    /// EL peers for this node (from admin_peers).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub el_peers: Vec<NodePeer>,
    /// Mempool status: (pending, queued) transaction counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mempool: Option<(u64, u64)>,
}

/// A peer connection for a single node (used in the detail panel).
#[derive(Serialize, Clone, Default)]
pub(crate) struct NodePeer {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub trusted: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inbound: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub static_node: bool,
    /// Gossipsub topics shared with this peer (CL only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
}

impl PartialEq for NodePeer {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for NodePeer {}

impl PartialOrd for NodePeer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NodePeer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

/// A single overlay network (e.g. "CL Persistent Peers", "/consensus").
#[derive(Serialize)]
pub(crate) struct NetworkGraph {
    /// "cl" or "el"
    pub layer: String,
    /// "manifest" or "live"
    pub source: String,
    /// Edges in this overlay.
    pub edges: Vec<GraphEdge>,
}

/// An edge between two nodes.
#[derive(Serialize)]
pub(crate) struct GraphEdge {
    pub from: String,
    pub to: String,
    /// Optional metadata carried on the edge.
    #[serde(flatten)]
    pub metadata: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::{manifest_peers_or_subnet_default, resolve_action_targets};
    use crate::manifest::{Manifest, Node};
    use indexmap::IndexMap;

    fn test_manifest() -> Manifest {
        let nodes = IndexMap::from([
            ("source".to_string(), Node::default()),
            ("shared".to_string(), Node::default()),
            ("unshared".to_string(), Node::default()),
        ]);
        let node_subnets = IndexMap::from([
            ("source".to_string(), vec!["subnet-a".to_string()]),
            ("shared".to_string(), vec!["subnet-a".to_string()]),
            ("unshared".to_string(), vec!["subnet-b".to_string()]),
        ]);

        Manifest::new(Some("testnet".to_string()), &nodes, &node_subnets)
    }

    #[test]
    fn resolve_action_targets_defaults_to_whole_node() {
        let targets =
            resolve_action_targets(None, "validator1").expect("target resolution should succeed");
        assert_eq!(targets, vec!["validator1"]);
    }

    #[test]
    fn resolve_action_targets_supports_layer_specific_targets() {
        let cl_targets = resolve_action_targets(Some("cl"), "validator1")
            .expect("cl target resolution should succeed");
        assert_eq!(cl_targets, vec!["validator1_cl"]);

        let el_targets = resolve_action_targets(Some("el"), "validator1")
            .expect("el target resolution should succeed");
        assert_eq!(el_targets, vec!["validator1_el"]);
    }

    #[test]
    fn resolve_action_targets_rejects_unknown_targets() {
        let err = resolve_action_targets(Some("bad"), "validator1")
            .expect_err("unknown target should fail");
        assert_eq!(err, "invalid target (expected 'cl' or 'el')");
    }

    #[test]
    fn manifest_peers_or_subnet_default_resolves_peer_names() {
        let manifest = test_manifest();

        let cases = [
            (
                "explicit peers are returned unchanged",
                Some(vec!["unshared".to_string()]),
                vec!["unshared".to_string()],
            ),
            (
                "implicit peers are filtered by shared subnet",
                None,
                vec!["shared".to_string()],
            ),
        ];

        for (case, explicit, expected) in cases {
            let peers =
                manifest_peers_or_subnet_default(&explicit, &manifest, &"source".to_string());
            assert_eq!(peers, expected, "{case}");
        }
    }
}
