# ADR-0003: Dynamic Block Gas Limit Configuration Validation

| Field         | Value          |
|---------------|----------------|
| Status        | Draft          |
| Author(s)     | @asoghoian     |
| Created       | 2026-02-20     |
| Updated       | 2026-02-24     |
| Supersedes    | -              |
| Superseded by | -              |

## Context

Arc's block parameters — gas limit, base fee parameters, and reward beneficiary — are configured via the `ProtocolConfig` governance smart contract (`0x3600000000000000000000000000000000000001`). The proposer reads these values when building blocks, and other full nodes accept the resulting headers with minimal verification.

Three governance-controlled values are dervied from `ProtocolConfig` values and end up in block headers:

| Value | Header Field | Current Validation |
|-------|--------------|--------------------|
| Gas limit | `gas_limit` | Clamped to hardcoded [1M, 1B] by proposer; no consensus validation |
| Base fee | `base_fee_per_gas` (child), `extra_data` (parent) | Parent extra_data → child base_fee check, but silently skipped if extra_data is malformed |
| Reward beneficiary | `beneficiary` | None — proposer can set any address |

This design has several gaps, mainly around validation, intentionally ommitted until this time. This ADR focuses on the `gasLimit` in particular.

### Gap 1: No consensus-level gas limit validation

`ArcConsensus` does not validate `gas_limit` against any bounds. A proposer can set any gas limit and all nodes accept it, provided `gas_used <= gas_limit`.

### Gap 2: No stateful checks against ProtocolConfig conformance

The gas limit is not verified to match what is suggested by the `ProtocolConfig`.

### Gap 3: Bounds are hardcoded constants

Gas limit bounds (`MINIMUM_BLOCK_GAS_LIMIT = 1M`, `MAXIMUM_BLOCK_GAS_LIMIT = 1B` in `protocol_config.rs:48-51`) are compile-time constants. They cannot vary per network and are not easily evolved through future releases. Sensible client-level bounds are necessary to guard against nonsensically-configured values in the `ProtocolConfig`. 

## Decision

### Block gas limits

New chainspec fields: 

| Field | Type | Purpose |
|-------|------|---------|
| `default_block_gas_limit` | `u64` | Gas limit used when ProtocolConfig is unavailable or returns an out-of-bounds value |
| `min_block_gas_limit` | `u64` | Absolute minimum gas limit |
| `max_block_gas_limit` | `u64` | Absolute maximum gas limit |

#### Proposer Flow

Pseudocode: use the value from `ProtocolConfig`, within bounds, if possible, else default chain-spec values:

```python
# static values
abs_min = chainspec.min_block_gas_limit()
abs_max = chainspec.max_block_gas_limit()
default = chainspec.default_block_gas_limit()

# flow
fee_params = protocol_config.fee_params()
if fee_params is None:
     return default

gas_limit = fee_params.gas_limit
if gas_limit is None:
     return default

elif gas_limit < abs_min or gas_limit > abs_max:
     return default

return gas_limit
```

#### Receiver Flow

Stateless consensus checks ensure proposed block gas limit is within static bounds:

```python
# static values
abs_min = chainspec.min_block_gas_limit()
abs_max = chainspec.max_block_gas_limit()

def validate_header(header):
     assert header.gas_limit > abs_min and header.gas_limit < abs_max
```

Stateful checks ensure conformance with the `ProtocolConfig` value, else default:

```python
# static values
default = chainspec.default_block_gas_limit()
abs_min = chainspec.min_block_gas_limit()
abs_max = chainspec.max_block_gas_limit()

def pre_execution(block):
     fee_params = protocol_config.fee_params()
     if fee_params is None:
          assert block.gas_limit == default
     
     if fee_params.gas_limit < abs_min or fee_params.gas_limit > abs_max:
          assert block.gas_limit == default
     
     assert block.gas_limit == fee_params.gas_limit
```

#### Chainspec values

| Network   | `default_block_gas_limit` | `min_block_gas_limit` | `max_block_gas_limit` |
|-----------|---------------------------|-----------------------|-----------------------|
| localdev  | 30,000,000 | 1,000,000 | 1,000,000,000 |
| devnet    | 30,000,000 | 1,000,000 | 1,000,000,000 |
| testnet   | 30,000,000 | 10,000,000 | 200,000,000   |

