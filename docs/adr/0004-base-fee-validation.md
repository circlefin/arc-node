# ADR-0004: Base Fee Parameter Validation

| Field         | Value          |
|---------------|----------------|
| Status        | Draft          |
| Author(s)     | @asoghoian     |
| Created       | 2026-03-03     |
| Updated       | 2026-03-03     |
| Supersedes    | -              |
| Superseded by | -              |

## Context

On Arc, the base fee calculation combines: 1) an EIP-1559-style algorithm, 2)  several at-runtime-governance-tunable values, and 3) an exponentially-smoothed view of historical gas use. 

The value of 1) is that it is familiar and well-understood. The value of 2) is to enable rapid adjusting of fee parameters and strictly enforcing block gas limits through governance. The value of 3) is to dampen fee increases against sudden shocks in usage with Arc's fast blocktimes.

The combination of these makes it challenging for integrators to precisely understand the next block base fee as the calculation is more involved. To address this, Arc includes the next block base fee the the header, computed at the end of the parent's block execution, since it is fully known at that point. 

Currently, the implementation has several gaps in validation. This ADR, similar to ADR-003, seeks to document solutions to these gaps.

### Gap 1: No validation of the base fee computation

There is currently no validation of the base fee calculation, just a check that the parent's header contains the next block's base fee. This is clearly insufficient -- a proposer could miscalculate a base fee, store it in the block header, and force the next proposer to use it. 

### Gap 2: The current fallback behavior is complex for integrators

If `retrieve_fee_params` fails, for whatever reason, the chain falls back to a simplified EIP-1559 algorithm and additionally skips including the next block base fee in the block header.

This adds complexity as: 1) an integrator may not know how to "derive" the base fee, since they cannot grab it from the header, and 2) there are now 2 algorithms to compute the next block base fee (stateful EMA vs. stateless EIP-1559).

## Decision

Always include the next block base fee, use a consistent algorithm, and apply strict validation. 

### Chainspec Updates

Two new types are attached to the chainspec via `base_fee_config(block_height)`:

```rust
/// Bounds on the three calculation parameters from ProtocolConfig.
struct BaseFeeCalcParams {
    alpha: u64,                  // EMA smoothing factor [0, 100]
    k_rate: u64,                 // Max change rate in basis points (200 = 2%)
    elasticity_multiplier: u64,  // Target gas ratio in basis points (5000 = 50%)
}

/// Complete base fee configuration for a network.
struct BaseFeeConfig {
    /// Calculation parameter bounds (min/default/max pattern per ADR-003).
    params_min: BaseFeeCalcParams,
    params_default: BaseFeeCalcParams,
    params_max: BaseFeeCalcParams,
    /// Absolute floor/ceiling on the computed base fee output.
    absolute_min_base_fee: u64,
    absolute_max_base_fee: u64,
}
```

The three `BaseFeeCalcParams` instances control the inputs to the calculation: if a ProtocolConfig value is outside `[params_min, params_max]` for that field, `params_default` is used.

The two absolute bounds control the *output*: after computation and after the ProtocolConfig's own `minBaseFee`/`maxBaseFee` clamp, the result is clamped to `[absolute_min_base_fee, absolute_max_base_fee]`.

#### `BaseFeeCalcParams` values

**alpha** — EMA smoothing factor. 0 = no smoothing update, 100 = raw gas used.

| Network | min | default | max |
|---------|-----|---------|-----|
| localdev | 0 | 20 | 100 |
| devnet | 0 | 20 | 100 |
| testnet | 0 | 20 | 100 |

**k_rate** — max base fee change rate per block (basis points). 200 = 2%.

| Network | min | default | max |
|---------|-----|---------|-----|
| localdev | 0 | 200 | 10000 |
| devnet | 0 | 200 | 10000 |
| testnet | 1 | 200 | 500 |

**elasticity_multiplier** — target gas utilization (basis points). 5000 = 50%.

| Network | min | default | max |
|---------|-----|---------|-----|
| localdev | 1 | 5000 | 10000 |
| devnet | 1 | 5000 | 10000 |
| testnet | 1 | 5000 | 9000 |

#### Absolute base fee bounds

Live ProtocolConfig values (devnet and testnet, queried 2026-03-03): `minBaseFee = 20,000,000,000` (20 gwei), `maxBaseFee = 20,000,000,000,000` (20,000 gwei).

| Network | `absolute_min_base_fee` | `absolute_max_base_fee` |
|---------|-------------------------|-------------------------|
| localdev | 1 | u64::MAX |
| devnet | 1 | u64::MAX |
| testnet | 1 | 20,000,000,000,000 (20,000 gwei) |

### Executor flow

```python
fee_params = protocol_config.fee_params()
config = chainspec.base_fee_config(block_height)

def validated(value, field):
    """Return value if within [params_min.field, params_max.field], else params_default.field."""
    if config.params_min.field <= value <= config.params_max.field:
        return value
    return config.params_default.field

if fee_params is None:
    calc = config.params_default
else:
    calc = BaseFeeCalcParams(
        alpha             = validated(fee_params.alpha, alpha),
        k_rate            = validated(fee_params.kRate, k_rate),
        elasticity_multiplier = validated(fee_params.elasticityMultiplier, elasticity_multiplier),
    )

# 1. Compute smoothed gas
smoothed_gas = ema(parent_smoothed_gas, block_gas_used, calc.alpha)

# 2. Compute next base fee
next_base_fee = arc_calc_next_block_base_fee(smoothed_gas, gas_limit, base_fee, calc.k_rate, calc.elasticity_multiplier)

# 3. Apply ProtocolConfig's own minBaseFee/maxBaseFee clamp (if available)
if fee_params is not None:
    next_base_fee = clamp(next_base_fee, fee_params.minBaseFee, fee_params.maxBaseFee)

# 4. Apply chainspec absolute bounds
next_base_fee = clamp(next_base_fee, config.absolute_min_base_fee, config.absolute_max_base_fee)

# 5. Always persist
system_accounting.store(block_number, gas_used, smoothed_gas, next_base_fee)
```

The assembler always writes `next_base_fee` from SystemAccounting to `extra_data`. No conditional skip.

### Block validation

**Stateless** (header-only, pre-execution):

```python
config = chainspec.base_fee_config(block_height)

# 1. Absolute bounds check
assert config.absolute_min_base_fee <= header.base_fee <= config.absolute_max_base_fee

# 2. Rate check (proportional, using max allowed k_rate)
if parent.number > 0:
    max_delta = parent.base_fee * config.params_max.k_rate / 10000
    assert abs(header.base_fee - parent.base_fee) <= max_delta
```

**Stateful** (after block execution):

```python
# extra_data must equal the nextBaseFee persisted during execution
expected = system_accounting.retrieve(block_number).nextBaseFee
assert decode(header.extra_data) == expected
```

This replaces the current `parent.extra_data → child.base_fee` check with a post-execution invariant: the proposer's `extra_data` must match the deterministic output of execution.

## Consequences

### Positive

- Clearly documented fallback values tied to chainspec
- Full validation of base fee
- Consistent calculation (no EIP-1559 fallback)
- Stateless checks enable fast rejection of invalid blocks before execution

### Negative

- Arguably complex local chainspec configuration
- Next block base fee should arguably live in a dedicated block header field vs. monopolizing the extra data field
