# Monitoring an Arc Node

This guide describes a simple monitoring setup for operators running one Arc
Execution Layer (EL) and one Arc Consensus Layer (CL) on the same Linux host.

It uses:

- **Prometheus** for metrics collection;
- **Grafana** for dashboards;
- **SSH port forwarding** for safe access to local-only monitoring endpoints.

This guide assumes you already have a working Arc node by following
[installation](./installation.md) and
[running an Arc node](./running-an-arc-node.md).

## Prerequisites

Before setting up monitoring, confirm that:

- your Arc node is already running;
- the execution layer is exposing metrics on `127.0.0.1:9001`;
- the consensus layer is exposing metrics on `127.0.0.1:29000`;
- Docker Engine and Docker Compose are installed on the host.

The examples in this guide assume the following metrics flags are present:

### Execution layer

```sh
--metrics 127.0.0.1:9001
```

### Consensus layer

```sh
--metrics 127.0.0.1:29000
```

## Verify metrics endpoints

Before deploying Prometheus and Grafana, verify that both metrics endpoints are
reachable on the host.

The execution layer exposes metrics at `/`:

```sh
curl -s http://127.0.0.1:9001 | head
```

The consensus layer exposes metrics at `/metrics`:

```sh
curl -s http://127.0.0.1:29000/metrics | head
```

If either command fails, first fix the Arc node startup or metrics flags before
continuing.

## Create a monitoring directory

This guide stores monitoring files under `$ARC_HOME/monitoring`.

```sh
ARC_HOME="${ARC_HOME:-$HOME/.arc}"
ARC_MONITORING="${ARC_MONITORING:-$ARC_HOME/monitoring}"

mkdir -p "$ARC_MONITORING"/grafana-provisioning/datasources
mkdir -p "$ARC_MONITORING"/grafana-provisioning/dashboards
mkdir -p "$ARC_MONITORING"/prometheus-data
mkdir -p "$ARC_MONITORING"/dashboards
```

## Reuse the existing Arc dashboards

The repository already contains Grafana dashboard JSON files under
`deployments/monitoring/config-grafana/provisioning/dashboards-data`.

From the `arc-node` repository root, copy them into the monitoring directory:

```sh
cp -r deployments/monitoring/config-grafana/provisioning/dashboards-data/* \
  "$ARC_MONITORING"/dashboards/
```

## Create the Prometheus configuration

Write the following file to `$ARC_MONITORING/prometheus.yml`:

```yaml
global:
  scrape_interval: 1s

scrape_configs:
  - job_name: "arc_execution"
    metrics_path: "/"
    scrape_interval: 1s
    static_configs:
      - targets: ["127.0.0.1:9001"]
        labels:
          client_name: "reth"
          client_type: "execution"

  - job_name: "arc_consensus"
    metrics_path: "/metrics"
    scrape_interval: 1s
    static_configs:
      - targets: ["127.0.0.1:29000"]
        labels:
          client_name: "malachite"
          client_type: "consensus"
```

## Create the Grafana datasource configuration

Write the following file to
`$ARC_MONITORING/grafana-provisioning/datasources/prometheus.yml`:

```yaml
apiVersion: 1

datasources:
  - name: prometheus
    uid: prometheus
    type: prometheus
    url: http://127.0.0.1:9090
    isDefault: true
    editable: true
```

## Create the Grafana dashboards provisioning file

Write the following file to
`$ARC_MONITORING/grafana-provisioning/dashboards/default.yml`:

```yaml
apiVersion: 1

providers:
  - name: "arc"
    orgId: 1
    folder: ""
    type: file
    disableDeletion: false
    editable: true
    updateIntervalSeconds: 10
    options:
      path: /var/lib/grafana/dashboards
```

## Create the Docker Compose file

Write the following file to `$ARC_MONITORING/compose.yaml`:

```yaml
services:
  prometheus:
    image: prom/prometheus
    user: "0"
    network_mode: host
    command:
      - --config.file=/etc/prometheus/prometheus.yml
      - --storage.tsdb.path=/prometheus
      - --web.listen-address=127.0.0.1:9090
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - ./prometheus-data:/prometheus
    restart: unless-stopped

  grafana:
    image: grafana/grafana-oss
    network_mode: host
    environment:
      GF_SERVER_HTTP_ADDR: 127.0.0.1
      GF_SERVER_HTTP_PORT: 3000
      GF_SECURITY_ADMIN_USER: admin
      GF_SECURITY_ADMIN_PASSWORD: admin
    volumes:
      - ./grafana-provisioning:/etc/grafana/provisioning:ro
      - ./dashboards:/var/lib/grafana/dashboards:ro
    restart: unless-stopped
```

> **Why `network_mode: host`?** The Arc metrics endpoints in this guide are bound
> to `127.0.0.1`, so host networking is the simplest way for Prometheus to scrape
> them without changing the Arc node configuration or exposing metrics publicly.

## Start the monitoring stack

```sh
cd "$ARC_MONITORING"
docker compose up -d
```

Check that both containers are running:

```sh
docker compose ps
```

## Verify Prometheus and Grafana

Check that Grafana is healthy:

```sh
curl -s http://127.0.0.1:3000/api/health
```

Check that Prometheus is ready:

```sh
curl -s http://127.0.0.1:9090/-/ready
```

You can also inspect Prometheus targets in the browser later at:

- `http://localhost:9090/targets`

## Access the dashboards

This setup binds Grafana and Prometheus to `127.0.0.1` only.
That is intentional: the services are not exposed to the public internet.

If you are on the same machine, open:

- `http://127.0.0.1:3000` for Grafana;
- `http://127.0.0.1:9090` for Prometheus.

If you are connecting to a remote server, use SSH port forwarding from your
local machine:

```sh
ssh -N \
  -L 3000:127.0.0.1:3000 \
  -L 9090:127.0.0.1:9090 \
  user@YOUR_SERVER_IP
```

Then open on your local machine:

- `http://localhost:3000`
- `http://localhost:9090`

Grafana default credentials from the Compose file above are:

- username: `admin`
- password: `admin`

Change them before exposing Grafana through any reverse proxy or shared access setup.

## What to expect

Once Prometheus starts scraping both endpoints and Grafana loads the provisioned
dashboards, you should see metrics for:

- execution layer activity;
- consensus layer activity;
- block height;
- validator-related consensus telemetry;
- connected peers and chain progress.

If some panels show no data immediately after startup, wait a minute and refresh.
A newly started Prometheus instance needs a short time to collect enough samples.

## Troubleshooting

### Browser shows `ERR_CONNECTION_REFUSED`

If you are using a remote server, make sure you are opening:

- `http://localhost:3000`
- `http://localhost:9090`

on your **local machine**, not `http://YOUR_SERVER_IP:3000`.

Also confirm that the SSH tunnel is still running.

### Prometheus is restarting with a permissions error

If Prometheus logs include an error about `queries.active` or write permission
under `/prometheus`, fix the ownership of the local data directory:

```sh
sudo chown -R 65534:65534 "$ARC_MONITORING"/prometheus-data
docker compose restart prometheus
```

### Grafana is healthy but dashboards show no data

First, confirm Prometheus targets are up:

```sh
curl -s http://127.0.0.1:9090/api/v1/targets
```

Then re-check the Arc metrics endpoints:

```sh
curl -s http://127.0.0.1:9001 | head
curl -s http://127.0.0.1:29000/metrics | head
```

If Prometheus can reach the targets but some dashboard panels are still empty,
wait for more samples to accumulate and refresh the dashboard.

### Check Arc node health directly

You can still verify the Arc node independently of Grafana:

```sh
cast block-number --rpc-url http://127.0.0.1:8545
sudo journalctl -u arc-execution -f
sudo journalctl -u arc-consensus -f
```

## Next steps

This guide keeps monitoring intentionally minimal and local-only.
For more advanced deployments, operators may want to add:

- persistent Grafana storage;
- HTTPS and authentication through a reverse proxy;
- Alertmanager-based alerting;
- system metrics exporters such as `node_exporter`.
