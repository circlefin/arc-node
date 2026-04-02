# ADR-0002: Block Dissemination Protocol

| Field         | Value              |
|---------------|--------------------|
| Status        | Draft              |
| Author(s)     | @romac             |
| Created       | 2026-01-13         |
| Updated       | 2026-01-13         |
| Supersedes    | -                  |
| Superseded by | -                  |

## Context

Arc requires an efficient mechanism to disseminate block proposals from proposers to validators during consensus. 
Block proposals can be large (up to 16 MiB) due to the execution payload, making atomic transmission impractical using gossip protocols, which usually define a maximum broadcast message size.

Key constraints:
- **Large payloads**: Execution payloads (SSZ-encoded `ExecutionPayloadV3`) can be several megabytes
- **Unordered network**: Messages may arrive out-of-order or be duplicated over Gossipsub
- **Resource constraints**: Nodes must protect against memory exhaustion from malicious peers
- **Cryptographic integrity**: Proposals must be authenticated and tamper-proof
- **Consensus integration**: Must support re-proposals (restreaming) when a validator has a valid value from a previous round

The protocol was developed having as an additional constraint, which gives its "streaming" nature, which was the ability to propagate an arbitrary amount of bytes, not known a priori.
This, in particular, is the reason why we compute hash and signatures at the end of the "streaming".
This is not a requirement in the case of Arc.

## Decision

Implement a streaming protocol that splits proposals into fixed-size chunks transmitted as a stream of messages over [libp2p Gossipsub][gossipsub].

### Message Types

A proposal is split into three types of parts:

```
ProposalPart ::= ProposalInit | ProposalData | ProposalFin
```

**ProposalInit** - Proposal metadata:
- `height: u64` - Block height
- `round: u32` - Consensus round
- `pol_round: Option<u32>` - Proof-of-lock round (for re-proposals)
- `proposer: Address` - 20-byte proposer address

**ProposalData** - Payload chunk:
- `bytes: Bytes` - Up to 128 KiB of SSZ-encoded `ExecutionPayloadV3` (configurable)

**ProposalFin** - Signature:
- `signature: Signature` - 64-byte Ed25519 signature over the proposal hash

Each `ProposalPart` is wrapped in a `StreamMessage` for transmission:
```
StreamMessage {
    stream_id: StreamId,      // height || round || nonce
    sequence: u64,            // 0, 1, 2, ...
    content: Init | Data | Fin
}
```

### Stream Structure

```
┌─────────────┬─────────────┬─────────────┬─────────────┬─────────────┐
│   Init      │   Data[0]   │   Data[1]   │    ...      │    Fin      │
│   seq=0     │   seq=1     │   seq=2     │             │   seq=N     │
└─────────────┴─────────────┴─────────────┴─────────────┴─────────────┘
```

### Protocol Constants

| Parameter | Value | Description |
|-----------|-------|-------------|
| `CHUNK_SIZE`              | 128 KiB    | Maximum size of each `Data` chunk |
| `MAX_MESSAGES_PER_STREAM` | 128        | Maximum messages per stream (including `ProposalInit` and `ProposalFin`) |
| `MAX_BLOCK_SIZE`          | 15.75 MiB  | Maximum block size (126 chunks × 128 KiB) |
| `MAX_STREAMS_PER_PEER`    | 64         | Maximum concurrent streams per peer |
| `MAX_TOTAL_STREAMS`       | 100        | Maximum total concurrent streams |
| `MAX_STREAM_AGE`          | 60 seconds | Stream eviction timeout |
> [!NOTE]
> The values for the `CHUNK_SIZE` and `MAX_MESSAGES_PER_STREAM` parameters were chosen heuristically to satisfy the following:
> - That most blocks seen on testnet so far would fit in a single proposal part
> - That the max block size would be high enough to never be a limiting factor in practice, even under high load
> - That each proposal part would be small enough to not put too much strain on the p2p layer, given that these parts are gossiped with high redundancy

### Hash Computation and Signing

**Hash algorithm**: Keccak256 (SHA3-256) producing a 32-byte hash

**Hash input format** (exact byte order):
```
hash_input = height (8 bytes, big-endian u64)
           || round (8 bytes, big-endian i64, Option<u32> encoded as None => -1, Some(v) => v)
           || data_chunk[0] || data_chunk[1] || ... || data_chunk[N-1]
```

> [!NOTE] 
> The hash does NOT include the content of `Init` or `Fin` parts—only height, round, and raw data bytes.

**Signature scheme**: Ed25519 (64-byte signature, 32-byte public key)

### Sending Protocol

1. Generate unique `StreamId` using `height || round || nonce`. 
    - The `nonce` is an 8-byte incrementing counter used to prevent stream ID collisions for re-transmissions within the same round.
2. Create `ProposalInit` with `height`, `round`, `pol_round`, `proposer`
3. SSZ-encode the `ExecutionPayloadV3`
4. Initialize Keccak256 hasher with `height.to_be_bytes()` and `round.to_be_bytes()`
5. Split encoded payload into 128 KiB chunks, adding each of them to the hasher
6. Finalize `hash` and sign the `hash` with the proposer's Ed25519 private key
7. Create `ProposalFin` with the `Signature` produced at the previous step
8. Wrap each part in `StreamMessage` with incrementing sequence numbers
9. Publish via `NetworkMsg::PublishProposalPart` over libp2p Gossipsub

**Restreaming**: When restreaming a previous proposal, reuse the existing signature (do not re-sign).

### Receiving Protocol

For each incoming `StreamMessage`:

1. Evict stale streams (age > 60 seconds)
2. Reject if stream was previously evicted
3. The set of previously evicted stream ids is then cleared to avoid unbounded growth
4. Enforce per-peer limit (≥ 64 streams → reject)
5. Enforce global limit (≥ 100 streams → evict oldest)
6. Deduplicate by sequence number
7. Evict if stream exceeds 128 messages
8. Buffer in min-heap ordered by sequence
9. Track: `height` from `Init`, `fin_received` flag, `expected_messages` count

**Stream completion**: A stream is considered complete when the `Fin` message (with sequence number `N`) has been received and the buffer contains all `N+1` messages from sequence `0` to `N`. The total number of expected messages is only known after the `Fin` message arrives.

**Assembly**: Drain buffer in sequence order → validate one `Init` and one `Fin` → concatenate data chunks → SSZ-decode to `ExecutionPayloadV3` → reconstruct `ConsensusBlock`.

### Validation

1. **Proposer verification**: Check proposer matches expected proposer for (height, round)
2. **Signature verification**: Recompute `hash` from parts, as in the Sending Protocol
 steps 3 and 4, and verify `hash` against `ProposalFin.signature` using proposer's public key
3. **Payload validation**: Validate execution payload with execution engine API

### Wire Format (Protobuf)

```protobuf
message ProposalPart {
  oneof part {
    ProposalInit init = 1;
    ProposalData data = 2;
    ProposalFin fin = 3;
  }
}

message ProposalInit {
  uint64 height = 1;
  uint32 round = 2;
  Address proposer = 4;
  optional uint32 pol_round = 5;
}

message ProposalData {
  bytes bytes = 1;
}

message ProposalFin {
  Signature signature = 1;
}

message StreamMessage {
  bytes stream_id = 1;
  uint64 sequence = 2;
  oneof content {
    // Serialized ProposalPart (ie. Init, Data or Fin)
    bytes data = 3;
    // Marker for end of stream
    bool fin = 4;
  }
}
```

> [!NOTE]
> In `StreamMessage`:
> - `data` is the encoded `ProposalPart` (ie. `ProposalInit`, `ProposalData` or `ProposalFin`).
> - `fin` is a marker for end-of-stream. It is redundant with the `ProposalFin` message but inherited from the original spec from Starkware.

## Consequences

### Positive

- **Efficient large block handling**: 128 KiB chunks allow streaming large payloads without memory spikes
- **Out-of-order tolerance**: Min-heap buffering handles network reordering gracefully
- **Resource protection**: Multi-layered limits (per-peer, global, per-stream, age-based) prevent exhaustion attacks
- **Cryptographic integrity**: Keccak256 + Ed25519 ensures authenticity and tamper detection
- **Consensus compatibility**: Signature reuse enables efficient re-proposals without re-signing

### Negative

- **Complexity**: Streaming adds complexity compared to atomic message transmission
- **Memory overhead**: Buffering incomplete streams consumes memory (up to 1.6 GiB globally)
- **Susceptible to message loss**: There is no mechanism to request the retransmission of parts
- **Late validation**: Signature validation requires all data parts, preventing full validation until the final message arrives. Early rejection is only possible by applying consensus rules to the `ProposalInit` message (e.g., invalid proposer for the given height/round).

### Neutral

- **Gossipsub dependency**: Relies on libp2p Gossipsub's mesh redundancy for reliability
- **Fixed chunk size**: 128 KiB is a reasonable tradeoff but not dynamically tunable

## Alternatives Considered

### 1. Atomic Block Messages
Send entire blocks as single messages. Rejected because:
- Gossipsub has practical message size limits
- Large messages cause memory spikes and network congestion
- No partial progress—entire block must be retransmitted on failure

### 2. Erasure Coding
Use Reed-Solomon or similar coding for redundancy. Rejected because:
- Adds significant complexity
- Gossipsub mesh already provides redundancy
- Current approach with timeouts is simpler and sufficient

### 3. Request-Response Protocol
Pull-based model where validators request missing parts. Rejected because:
- Adds round-trip latency
- Requires tracking which peers have which parts
- Push-based gossip is more suitable for time-sensitive consensus

---

## Appendix: Implementation Files

| Component | File | Key Lines |
|-----------|------|-----------|
| ProposalPart types | `crates/types/src/proposal_part.rs` | 57-118 |
| ProposalParts assembly | `crates/types/src/proposal_parts.rs` | 16-115 |
| Hash computation (create) | `crates/malachite-app/src/proposal_parts.rs` | 144-155 |
| Hash computation (verify) | `crates/types/src/proposal_parts.rs` | 100-115 |
| Stream preparation | `crates/malachite-app/src/proposal_parts.rs` | 98-173 |
| Stream buffering | `crates/malachite-app/src/streaming.rs` | 198-383 |
| Receive handler | `crates/malachite-app/src/handlers/received_proposal_part.rs` | 101-275 |
| Signature validation | `crates/malachite-app/src/proposal_parts.rs` | 179-242 |
| Block assembly | `crates/malachite-app/src/proposal_parts.rs` | 244-270 |
| Restream handler | `crates/malachite-app/src/handlers/restream_proposal.rs` | 43-148 |
| Protobuf definitions | `crates/types/proto/arc/consensus/v1/consensus.proto` | 50-71 |

## Appendix: Security Considerations

**Resource Exhaustion Protection**:
- Per-peer: 64 streams × 16 MiB = 1 GiB max per peer
- Global: 100 streams × 16 MiB = 1.6 GiB max total
- Age-based eviction prevents stale stream accumulation

**Attack Mitigations**:
| Attack                 | Mitigation                            |
|------------------------|---------------------------------------|
| Stream flooding        | Per-peer and global stream limits     |
| Large message attack   | 128 KiB chunk size, 128 message limit |
| Stale stream attack    | 60-second timeout eviction            |
| Invalid signature spam | Rejection at assembly time            |

[gossipsub]: https://github.com/libp2p/specs/blob/master/pubsub/gossipsub/README.md
