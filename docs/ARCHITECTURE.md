# Architecture

This document provides a comprehensive overview of the Arc Network node architecture and codebase organization.

## Table of Contents

- [Overview](#overview)
- [Architectural Layers](#architectural-layers)
- [Transaction Lifecycle](#transaction-lifecycle)
- [Block Production Flow](#block-production-flow)
- [Synchronization Modes](#synchronization-modes)
- [Execution Layer Deep Dive](#execution-layer-deep-dive)
- [Data Flow](#data-flow)

## Overview

```mermaid
graph TB
    Node[arc-node]
    EVM[EVM Layer]
    Pool[Transaction Pool]
    Executor[Block Executor]
    Engine[Eth Engine]
    Consensus[Malachite Consensus]
    Types[Shared Types]

    Node --> EVM
    Node --> Pool
    Node --> Executor
    Node --> Engine
    Engine --> Consensus
    Node --> Types
    Pool --> Types
    Executor --> Types
```

## Architectural Layers

Arc Network follows a clean separation between consensus and execution layers, communicating via the Ethereum Engine API. This modular design allows each layer to evolve independently while maintaining a stable interface.

```mermaid
graph TB
    subgraph "Consensus Layer (Malachite)"
        CL[Malachite App]
        Propose[Block Proposer]
        Vote[Vote Keeper]
    end

    subgraph "Engine API Boundary"
        EngineAPI[Engine API<br/>IPC or HTTP/RPC]
    end

    subgraph "Execution Layer (Reth-based)"
        EVM[EVM + Precompiles]
        Executor[Block Executor]
        State[State Trie]
        TxPool[Transaction Pool]
    end

    CL --> |forkchoiceUpdated| EngineAPI
    CL --> |newPayload| EngineAPI
    EngineAPI --> |getPayload| CL

    EngineAPI <--> Executor
    Executor --> EVM
    EVM --> State
    TxPool --> Executor

    style EngineAPI fill:#e1f5ff
    style CL fill:#ffe1e1
    style Executor fill:#e1ffe1
```

### Consensus-Execution Boundary

The two layers communicate through the [Engine API](https://github.com/ethereum/execution-apis/blob/main/src/engine/), a standard interface originally designed for proof-of-stake Ethereum. Key implementation:

- **Engine API Client**: [crates/eth-engine/src/engine.rs](../crates/eth-engine/src/engine.rs)
- **Malachite Integration**: [crates/malachite-app/src/app.rs](../crates/malachite-app/src/app.rs)

## Transaction Lifecycle

Understanding how a transaction flows through the system illustrates how consensus and execution layers interact:

```mermaid
sequenceDiagram
    participant User
    participant RPC
    participant TxPool
    participant Consensus
    participant Engine
    participant Executor
    participant State

    User->>RPC: eth_sendRawTransaction
    RPC->>TxPool: Add transaction
    Note over TxPool: Validation, gas checks

    Consensus->>Engine: Request payload<br/>(getPayload)
    Engine->>TxPool: Select transactions
    TxPool-->>Engine: Ordered tx list
    Engine-->>Consensus: Execution payload

    Note over Consensus: Propose block<br/>Collect votes<br/>Reach consensus

    Consensus->>Engine: Finalize block<br/>(newPayload + forkchoiceUpdated)
    Engine->>Executor: Execute block
    Executor->>State: Apply state changes
    State-->>Executor: New state root
    Executor-->>Engine: Execution result
    Engine-->>Consensus: Success

    Consensus-->>User: Transaction finalized<br/>(via WebSocket/logs)
```

### Key Stages

1. **Transaction Submission** ([crates/execution-txpool/src/pool.rs](../crates/execution-txpool/src/pool.rs))
   - Validates transaction format and signature
   - Adds to mempool if valid

2. **Block Proposal** ([crates/malachite-app/src/app.rs](../crates/malachite-app/src/app.rs))
   - Consensus layer requests payload via `engine_getPayload`
   - Execution layer builds block with selected transactions
   - Proposer signs and broadcasts proposal

3. **Consensus** (Malachite core)
   - Validators vote on proposed block
   - Once ⅔+ votes collected, block is decided
   - Certificate generated with validator signatures

4. **Execution** ([crates/evm/src/executor.rs](../crates/evm/src/executor.rs))
   - Consensus sends `engine_newPayload` + `engine_forkchoiceUpdated`
   - Block executor runs transactions through EVM
   - State trie updated with new balances, storage
   - Custom precompiles handle native features

5. **Finalization**
   - State root committed to database
   - Transaction receipts generated
   - Events emitted for subscribers

## Block Production Flow

```mermaid
graph LR
    subgraph Round[Consensus Round]
        Propose[1. Propose]
        Prevote[2. Prevote]
        Precommit[3. Precommit]
        Decide[4. Decide]
    end

    subgraph Execution[Execution Layer]
        Build[Build Payload]
        Validate[Validate Block]
        Execute[Execute & Commit]
    end

    Build --> Propose
    Propose --> Prevote
    Prevote --> Precommit
    Precommit --> Decide
    Decide --> Validate
    Validate --> Execute

    Execute --> |Next Round| Build
```

The consensus layer implements a variant of Tendermint with three voting phases per round. The execution layer is consulted at the beginning (payload building) and end (validation & execution) of each round.

## Synchronization Modes

Arc nodes support two synchronization strategies:

### P2P Sync (Default)
Traditional gossip-based synchronization where nodes:
- Exchange blocks via libp2p
- Participate in consensus as validators or full nodes
- Maintain peer connections for liveness

**Implementation**: [crates/malachite-app/src/node.rs](../crates/malachite-app/src/node.rs)

### RPC Sync Mode
Alternative for lightweight full nodes that:
- Fetch blocks via HTTP from trusted RPC endpoints
- Subscribe to block headers via WebSocket
- Don't participate in P2P networking or consensus

**Implementation**: [crates/malachite-app/src/rpc_sync/](../crates/malachite-app/src/rpc_sync/)

```mermaid
graph TB
    subgraph P2P["P2P Sync"]
        N1[Validator 1]
        N2[Validator 2]
        N3[Full Node]
        N1 <--> N2
        N2 <--> N3
        N1 <--> N3
    end

    subgraph RPC["RPC Sync"]
        V1[Validator]
        V2[Validator]
        L1[Light Node]
        L1 -->|HTTP/WS| V1
        L1 -->|HTTP/WS| V2
    end

    style L1 fill:#ffe1e1
    style N3 fill:#e1f5ff
```

## Execution Layer Deep Dive

The execution layer is split across several dedicated crates. Here's a breakdown:

### EVM Customization

The EVM layer ([crates/evm/src/evm.rs](../crates/evm/src/evm.rs)) allows for:

- Custom base fee calculation logic
- Integration of custom precompiles
- EVM configuration overrides

### Precompiles

Custom precompiles are defined in [crates/precompiles/src/](../crates/precompiles/src/):

- **Native Coin Authority** (`0x1800..0000`) - Mint, burn, transfer operations for native coin
- **Native Coin Control** (`0x1800..0001`) - Address blocklist
- **System Accounting** (`0x1800..0002`) - Gas fee ring buffer
- **Call From** (`0x1800..0003`) - Plumbing to support native batch and memo txns.
- **PQ Signature Verify** (`0x1800..0004`) - Post-quantum SLH-DSA-SHA2-128s verification

### Transaction Pool

The custom transaction pool ([crates/execution-txpool/src/](../crates/execution-txpool/src/)) provides:

- Configurable pool parameters
- Custom validation logic

### Payload Building

The payload builder ([crates/execution-payload/src/payload.rs](../crates/execution-payload/src/payload.rs)) handles:

- Block construction
- Transaction selection and ordering
- Gas limit management
- Adds to the InvalidTxList during execution panics

## Data Flow

```mermaid
graph LR
    TxSubmit[Transaction Submitted]
    TxPool[Transaction Pool]
    Validator[Pool Validator]
    Payload[Payload Builder]
    Executor[Block Executor]
    EVM[EVM Execution]
    State[State Update]

    TxSubmit --> Validator
    Validator --> TxPool
    TxPool --> Payload
    Payload --> Executor
    Executor --> EVM
    EVM --> State
```

## Further Reading

- [Contributing Guide](../CONTRIBUTING.md) - Development workflow and guidelines
- [ADRs](adr/README.md) - Architecture Decision Records
