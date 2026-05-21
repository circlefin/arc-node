# Quake Web Architecture

The `quake web` command serves a single-page application for real-time testnet monitoring and control. The backend is an Axum HTTP server; the frontend is a self-contained HTML file with inline CSS and JS using D3.js for graph visualization.

## Architecture diagram

```
Browser (user)
  |
  | HTTP poll (every refresh_ms)
  v
+---------------------------------------------------------------+
| Axum Web Server (web.rs)                                      |
|                                                               |
|  GET /             HTML SPA (web_index.html, refresh_ms)      |
|  GET /api/topology TopologyResponse JSON                      |
|  POST /api/node/*  Node actions (start/stop/kill/pause/...)   |
|  POST /api/mempool Mempool toggle (on/off)                    |
|                                                               |
|  AppState (shared via Arc<RwLock<>>)                          |
|  +------------------+  +------------------+  +--------------+ |
|  | ElLiveData       |  | ContainerStatuses|  | Testnet      | |
|  |  heights (map)   |  |  name -> status  |  |  manifest    | |
|  |  peers (map)     |  |  (from Docker)   |  |  nodes_meta  | |
|  |  mempool (map)   |  |                  |  |  infra       | |
|  |  errors (map)    |  |                  |  |              | |
|  +--------+---------+  +--------+---------+  +--------------+ |
|           |                      |                            |
+-----------+----------------------+----------------------------+
            |                      |
    Background tasks               |
            |                      |
   +--------+--------+         +---+------------+
   |                  |        |                |
   v                  v        v                v
+----------+  +----------+  +------------+  +---------------+
| EL Node  |  | EL Node  |  | Docker     |  | Docker        |
| Task #1  |  | Task #N  |  | Events     |  | Inspect       |
| (WS)     |  | (WS)     |  | Subscriber |  | Poller        |
+----+-----+  +----+-----+  +-----+------+  +------+--------+
     |              |              |                  |
     | WebSocket    | WebSocket    | docker events    | docker inspect
     |              |              | (real-time)      | (periodic)
     v              v              v                  v
+----------+  +----------+  +---------------------------------+
| EL Node  |  | EL Node  |  | Docker Daemon                   |
| (reth)   |  | (reth)   |  | (local or remote via SSH)       |
|          |  |          |  |                                 |
| - blocks |  | - blocks |  |  Container states:              |
| - peers  |  | - peers  |  |  running/exited/paused/         |
| - txpool |  | - txpool |  |  disconnected (net_count <= 1)  |
+----------+  +----------+  +---------------------------------+

         +-------------------------------------------+
         |                                           |
         | CL HTTP (per topology request)            |
         | (skips containers not running)            |
         v                                           v
  +-------------+                             +-------------+
  | CL Node #1  |  ...  (parallel)  ...       | CL Node #N  |
  | (malachite) |                             | (malachite) |
  |             |                             |             |
  | /network-state  (gossipsub mesh, peers)   |             |
  | /status         (proposer, round)         |             |
  +-------------+                             +-------------+
```

### Data flow per topology poll

```
build_topology()
  |
  +-- Manifest data (always)
  |     build_node_list()
  |     build_manifest_topology_edges()
  |     build_node_regions()
  |     resolve_mempool_max()
  |
  +-- EL cached data (from background WS tasks)
  |     el_live_data.heights -> node.height
  |     el_live_data.peers   -> EL Peers edges + node.el_peers
  |     el_live_data.mempool -> node.mempool
  |     el_live_data.errors  -> error messages
  |
  +-- Docker cached data (from events + inspect)
  |     container_statuses -> node.cl_status, node.el_status
  |     populate_container_statuses()
  |
  +-- CL fresh fetches (parallel HTTP, skips non-running)
  |     fetch_cl_live_topology()  -> CL topic edges + node.cl_peers
  |     fetch_current_proposer()  -> proposer name + node.round
  |
  +-- Error detection (only when live data available)
  |     merge_per_node_errors()      (CL + EL unified, skips disconnected)
  |     detect_unreachable_nodes()   (unreachable vs disconnected)
  |
  +-> TopologyResponse JSON
        { testnet_name, source, latest_height, current_proposer,
          nodes, networks, errors, node_regions, mempool_max }
```

### Frontend rendering pipeline

```
fetchTopology() --poll--> /api/topology
  |
  +-- Detect status transitions (shake with 5s cooldown)
  +-- Clear stale pending actions (button spinners, 10s timeout)
  |
  +-- updateStatus()        header: testnet name
  +-- updateTabs()          tab bar: topologies label + network tabs + Graph/Map
  +-- updateErrors()        error banner (max 20)
  +-- renderGraph()         if viewMode == 'graph'
  |   or renderWorldView()  if viewMode == 'world'
  |     |
  |     +-- resolveEdges()       returns edges + directedMode
  |     +-- buildNodeData()      D3 node datum (incl. disconnected flag)
  |     +-- [persistent peer overlay]  purple directional arrows (Manifest tab)
  |     +-- [EL trusted peer overlay]  orange directional arrows (Manifest tab)
  |     +-- renderNodeCircles()  circles with status colors
  |     +-- renderProposerStar() gold star overlay
  |     +-- renderMempoolBars()  queued/pending columns
  |     +-- renderNodeLabels()   text with outline
  |     +-- wireNodeInteraction() click/highlight/drag
  |     +-- drawSubnetHulls()    dashed hull shapes (graph only)
  |     +-- applyHighlight()     dim/brighten based on selection/subnet
  |
  +-- updateDetailPanel()   right: node inspector
  +-- updateHeightsList()   left: heights, rounds, mempool
  +-- buildLegend()         bottom-left: node types, subnets (clickable)
```

## Server

### Entry point

`run_server()` in `web.rs` binds the Axum server and spawns background tasks before serving requests.

### Shared state (`AppState`)

| Field | Type | Description |
|-------|------|-------------|
| `testnet` | `Arc<RwLock<Testnet>>` | Manifest, nodes metadata, infra provider |
| `el_live_data` | `Arc<RwLock<ElLiveData>>` | Block heights, peer lists, mempool counts, EL errors |
| `container_statuses` | `Arc<RwLock<HashMap>>` | Docker container status per service name |
| `mempool_active` | `Arc<AtomicBool>` | Controls whether txpool_status is polled |
| `refresh_ms` | `u64` | Frontend poll interval (injected into HTML) |

### Routes

| Route | Method | Description |
|-------|--------|-------------|
| `/` | GET | Serves the HTML SPA with `refresh_ms` injected |
| `/api/topology` | GET | Returns `TopologyResponse` JSON |
| `/api/node/{name}/{action}` | POST | Node/container action. Optional `?target=cl\|el`. Returns `{"ok": true}` or `{"ok": false, "error": "..."}` |
| `/api/mempool/{on\|off}` | POST | Toggle mempool polling |

### Node actions

start, stop, restart, kill, pause, unpause, disconnect, reconnect. Actions execute per-container (not batched). Partial failures are reported with error details.

## Background tasks

### Per-node EL WebSocket task (`el_node_task`)

One long-lived async task per EL node. Maintains a single WebSocket connection for all EL data:

- **Block subscription** (push): `eth_subscribe newHeads` updates `el_live_data.heights`
- **Peer polling** (every `el_refresh_ms`): `admin_peers` via `raw_request` updates `el_live_data.peers`
- **Mempool polling** (every `el_refresh_ms`, only when `mempool_active`): `txpool_status` via `raw_request` updates `el_live_data.mempool`

Both branches run concurrently via `tokio::select!`. On disconnect, the task records an error in `el_live_data.errors`, preserves the last known height, and reconnects after 2 seconds. Tolerates up to 3 consecutive `admin_peers` failures before reconnecting.

### Docker events subscriber

Subscribes to `docker events` for real-time container state detection (millisecond latency). Maps Docker actions to statuses:

| Docker action | Status |
|---------------|--------|
| die, kill, stop | exited |
| start | running |
| pause | paused |
| unpause | running |

Reconnects automatically if the event stream ends. stderr suppressed for containers not yet created.

### Docker inspect poller

Periodic `docker inspect` (every `container_refresh_ms`) for state that events don't cover:

- Derives status from `State.Status`, `State.Paused`, `State.Restarting`
- Detects **network disconnection**: a running container with only 1 network (host-access) is reported as `disconnected`
- Container names come from the manifest at startup
- stderr suppressed (containers with `start_at` delays don't exist yet)

## Topology response assembly

`build_topology()` reads all caches and assembles the response on each `/api/topology` call.

### Data sources

| Source | Data | Availability |
|--------|------|-------------|
| Manifest | Node list, subnets, regions, config, peer expectations, explicit CL/EL peers | Always |
| `region_assignments.json` | Node-to-region mapping | Always (if latency emulation enabled) |
| `el_live_data` | Block heights, EL peers, mempool, EL errors | Best-effort (requires running EL) |
| Docker inspect/events | Container statuses (running, exited, paused, disconnected) | Best-effort (requires Docker) |
| CL HTTP fetches | Gossipsub mesh topology, proposer, rounds | Best-effort (fresh per request, body parsed in parallel) |

### Assembly pipeline

1. Build node list from manifest (`build_node_list`) with explicit CL/EL peer lists
2. Build manifest-based network (Manifest Topology from subnet defaults)
3. If live data available:
   - Populate heights and container statuses (`populate_container_statuses`)
   - Build EL peer edges and detail (`build_el_live_edges`, `populate_el_peer_details`)
   - Fetch CL data in parallel (`fetch_cl_live_topology`, `fetch_current_proposer`), skipping containers that are not running
   - Merge errors (`merge_per_node_errors`, skips disconnected containers) and detect unreachable/disconnected nodes (`detect_unreachable_nodes`)
   - Populate mempool data (`populate_mempool_data`)
4. Build node regions and mempool max from manifest

### Unreachable vs disconnected

- **Unreachable** (red): CL or EL failed to respond, container is not in `disconnected` Docker state
- **Disconnected** (orange): failure is due to the container being network-isolated (only host-access network attached)
- Error messages use "disconnected" vs "not responding" accordingly

## Frontend

### Poll loop

`fetchTopology()` runs every `refresh_ms`. On each poll:

1. Fetch `/api/topology`
2. Detect status transitions (trigger shake with 5-second cooldown to prevent doubles)
3. Clear stale pending actions (button spinners, 10-second timeout)
4. Update all UI components

### Rendering pipeline

| Function | Description |
|----------|-------------|
| `updateStatus()` | Header testnet name and browser title |
| `updateTabs()` | Tab bar: topologies label + network tabs (dimmed when empty) + Graph/Map |
| `updateErrors()` | Error banner (max 20 messages) |
| `renderGraph()` / `renderWorldView()` | Main visualization (branched by `viewMode`) |
| `updateDetailPanel()` | Right-side node inspector |
| `updateHeightsList()` | Left sidebar heights, rounds, mempool columns |
| `buildLegend()` | Bottom-left legend (node types, clickable subnets) |

### Shared rendering helpers

Extracted to avoid duplication between Graph and Map views:

| Helper | Description |
|--------|-------------|
| `resolveEdges(net)` | Returns edges and directedMode |
| `buildNodeData(n, activeNodeNames, proposer)` | Builds D3 node datum from GraphNode (includes disconnected flag) |
| `renderNodeCircles(node)` | Appends circle with type-based fill and status-based stroke |
| `renderProposerStar(node, proposer)` | Appends gold star to proposer node |
| `renderMempoolBars(node)` | Appends queued/pending square columns |
| `renderNodeLabels(node)` | Appends text label with outline |
| `wireNodeInteraction(node, link)` | Attaches click handlers, highlight, subnet filter |

### View modes

**Graph** (default): D3 force simulation with link, charge, center, collide, and subnet attraction forces. Runs to equilibrium synchronously (no animation). Nodes are draggable and positions persist across polls. Initial zoom-to-fit is instant. Fit-to-page accounts for sidebar and detail panel.

**Map**: D3 NaturalEarth1 projection with land masses from world-atlas TopoJSON (CDN). Nodes placed at AWS region coordinates, spread in small circles when multiple nodes share a region. Edges drawn as straight lines. Region labels above clusters.

### Toggles

| Toggle | Visibility | Backend | Frontend effect |
|--------|-----------|---------|-----------------|
| Show proposer | Always | None | Gold star on proposer node, purple name in heights list |
| Show mempools | Always | POST `/api/mempool/on\|off` | Queued/pending bar columns on nodes, extra columns in heights list |
| Show latencies | Always | None | Inter-region latency labels on edges (from AWS matrix) |
| Show subnets | Always | None | Dashed hull shapes around subnet-grouped nodes (Graph only) |
| Show CL persistent peers | Manifest tab | None | Purple directional arrows from manifest cl_persistent_peers |
| Show EL trusted peers | Manifest tab | None | Orange directional arrows from manifest el_trusted_peers |

Peer overlay arrows (CL persistent, EL trusted) use inline SVG path elements instead of SVG markers for instant opacity response when selecting nodes.

### Node detail panel

Right-side panel, resizable via drag handle:

1. **Title row**: node name (bold/larger when selected) + action buttons for both layers
2. **Per-layer rows**: CL/EL status badge + per-container action buttons
3. **Peers section** (scrollable): merged CL + EL peer table with columns Dir, C L P E, Score, T I S
4. **Configuration**: non-default manifest fields
5. **Region**: AWS region with city/country label

### Heights panel

Left sidebar showing per-node block heights and consensus rounds. Heights are color-coded by lag tiers: at latest, 1 block behind, 2-5 blocks behind, 6+ blocks behind, unreachable (no data). Rounds are color-coded by severity: round 0, round 1, round > 1.

Resizable via drag handle. Node names are clickable (selects node on graph). A status dot next to each name indicates node type (validator, non-validator) or failure mode (disconnected, unreachable).

### Subnet filtering

Clicking a subnet name in the legend dims all nodes not in that subnet and fades unrelated edges (including overlay arrows). Subnet names underline on hover and stay underlined when selected. Click again, click a node, or click the background to clear.

### Button spinning

When an action button is clicked, it spins until Docker reports a status change. Tracked via `pendingActions` map with a 10-second timeout. Spinning buttons are disabled (dimmed and non-interactive). Shake animation triggers before the API call for instant feedback.

### Shake animation

- Triggers on button click for: restart, stop, kill, pause, disconnect
- Triggers from poll for status transitions to exited, paused, disconnected (terminal perturbations)
- 5-second cooldown after any shake prevents double-shakes
