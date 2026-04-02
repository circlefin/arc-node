# Profiling

Both binaries (`arc-node-execution` and `arc-node-consensus`) support
heap and CPU profiling via [pprof](https://github.com/google/pprof),
gated behind the `pprof` Cargo feature. When the feature is disabled
(the default), all profiling code compiles to no-ops with zero overhead.

__Table of contents__
- [Prerequisites](#prerequisites)
- [Building](#building)
  - [Local (cargo)](#local-cargo)
  - [Docker](#docker)
- [Running](#running)
- [Collecting profiles](#collecting-profiles)
  - [Endpoints](#endpoints)
  - [CPU profiling](#cpu-profiling)
  - [Heap profiling](#heap-profiling)
  - [Raw download (no Go required)](#raw-download-no-go-required)
- [Jemalloc metrics (EL only)](#jemalloc-metrics-el-only)

## Prerequisites

Analyzing profiles requires either Go or the standalone `pprof`
tool. Install one of them:

```bash
# Option 1: standalone pprof via Homebrew (no Go required)
brew install pprof

# Option 2: use go tool pprof (bundled with any Go installation)
# Install Go from https://go.dev/dl/
```

Flamegraph rendering requires Graphviz:

```bash
brew install graphviz
```

## Building

Use the `profiling` Cargo profile, which inherits `release` but keeps
debug symbols (`debug = true`, `strip = false`) for readable
flamegraphs. Pass `--features pprof` to compile in the pprof HTTP
server and jemalloc heap profiling.

### Local (cargo)

```bash
# Both binaries
cargo build --profile profiling --features pprof

# Just EL
cargo build --profile profiling --features pprof -p arc-node-execution

# Just CL
cargo build --profile profiling --features pprof -p arc-node-consensus
```

The first build with the `profiling` profile is slow (thin LTO + debug
symbols). Subsequent incremental builds are faster.

### Docker

The deployment compose files use layer-specific feature environment
variables so each layer can be configured independently:

| Variable | Layer | Default |
| -------- | ----- | ------- |
| `EL_FEATURES` | Execution | `default js-tracer` |
| `CL_FEATURES` | Consensus | _(empty)_ |

```bash
# EL
EL_FEATURES="default js-tracer pprof" \
  docker compose -f deployments/arc_execution.yaml build \
  --build-arg BUILD_PROFILE=profiling

# CL
CL_FEATURES=pprof \
  docker compose -f deployments/arc_consensus.yaml build \
  --build-arg BUILD_PROFILE=profiling
```

For Quake testnets (local and remote), see the
[Profiling section](../crates/quake/README.md#profiling) of the
Quake README. Remote testnets use an nginx reverse proxy on
the Control Center to route pprof requests to individual nodes.

## Running

When built with `--features pprof`, the pprof HTTP server starts
automatically on the default port. No extra flags are needed.

| Binary                     | Default pprof port |
| -------------------------- | ------------------ |
| `arc-node-consensus` (CL)  | 6060               |
| `arc-node-execution` (EL)  | 6061               |

Override with `--pprof.addr=0.0.0.0:<port>`.

## Collecting profiles

### Endpoints

| Path | Type | Description |
| ---- | ---- | ----------- |
| `/debug/pprof/profile` | CPU | Samples CPU stacks at 99 Hz |
| `/debug/pprof/allocs` | Heap | Dumps jemalloc heap profile |
| `/debug/pprof/heap` | Heap | Alias for `/debug/pprof/allocs` |

### CPU profiling

The `/debug/pprof/profile` endpoint accepts query parameters:

- `seconds` — sampling duration (default: **30**).
- `sampling` — sampling frequency in Hz (default: **99**).

```bash
# 30-second CPU profile (default)
pprof -http :8080 http://localhost:6061/debug/pprof/profile
go tool pprof -http :8080 http://localhost:6061/debug/pprof/profile

# 60-second CPU profile
pprof -http :8080 'http://localhost:6061/debug/pprof/profile?seconds=60'
go tool pprof -http :8080 'http://localhost:6061/debug/pprof/profile?seconds=60'
```

### Heap profiling

```bash
pprof -http :8080 http://localhost:6061/debug/pprof/allocs
go tool pprof -http :8080 http://localhost:6061/debug/pprof/allocs
```

### Raw download (no Go required)

```bash
curl -o cpu.pb.gz  'http://localhost:6061/debug/pprof/profile?seconds=30'
curl -o heap.pb.gz  http://localhost:6061/debug/pprof/allocs
```

## Jemalloc metrics (EL only)

When built with `pprof`, the EL enables `reth-node-metrics/jemalloc`,
which publishes six gauges on the existing metrics endpoint (default
`0.0.0.0:9001`):

| Metric               | Description                                      |
| -------------------- | ------------------------------------------------ |
| `jemalloc.active`    | Bytes in active pages                            |
| `jemalloc.allocated` | Bytes allocated by the application               |
| `jemalloc.mapped`    | Bytes in active extents mapped by the allocator  |
| `jemalloc.metadata`  | Bytes dedicated to metadata                      |
| `jemalloc.resident`  | Bytes in physically resident pages               |
| `jemalloc.retained`  | Bytes in virtual memory mappings retained for reuse |

```bash
curl -s http://localhost:9001/metrics | grep jemalloc
```
