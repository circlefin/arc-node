// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

use alloy_primitives::Bytes;

const ALPHA_MAX: u128 = 100u128;

/// Computes the parent gas used value using Exponential Moving Average (EMA) smoothing.
/// This is fed into an EIP-1559 style base fee calculation.
pub fn determine_ema_parent_gas_used(
    smoothed_parent_gas_used: u64,
    raw_block_gas_used: u64,
    alpha: u64,
) -> Option<u64> {
    // Extraordinarily unlikely to overflow, but just in case
    let a = alpha as u128;
    if a > ALPHA_MAX {
        return None;
    }

    let raw = raw_block_gas_used as u128;
    let smoothed = smoothed_parent_gas_used as u128;

    // (1-α) * G[t-1] + α * G[t]
    // α is expressed as an integer value [0, 100]
    let left = (ALPHA_MAX - a).checked_mul(smoothed)?;
    let right = a.checked_mul(raw)?;
    let together = left.checked_add(right)?;

    // Floor (truncation)
    u64::try_from(together / ALPHA_MAX).ok()
}

/// Encode the base fee to bytes.
pub fn encode_base_fee_to_bytes(base_fee: u64) -> Bytes {
    let bytes: [u8; 8] = base_fee.to_be_bytes();
    bytes.into()
}

/// Decode the base fee from bytes.
pub fn decode_base_fee_from_bytes(extra_data: &Bytes) -> Option<u64> {
    if extra_data.len() != 8 {
        return None;
    }
    let bytes: [u8; 8] = extra_data.as_ref().try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

const ARC_BASE_FEE_FIXED_POINT_SCALE: u128 = 10_000;

// This is copied from alloy_eips::eip1559::calc_next_block_base_fee, and use fixed point for
// max_change_denominator and elasticity_multiplier to calculate the gas_target.
pub fn arc_calc_next_block_base_fee(
    gas_used: u64,
    gas_limit: u64,
    base_fee: u64,
    k_rate: u64,                        // 2500 => 25%
    inverse_elasticity_multiplier: u64, // 7500 => 75%
) -> u64 {
    // Calculate the target gas by dividing the gas limit by the elasticity multiplier.
    let gas_target = (gas_limit as u128 * inverse_elasticity_multiplier as u128
        / ARC_BASE_FEE_FIXED_POINT_SCALE) as u64;

    if gas_target == 0 || k_rate == 0 {
        return base_fee;
    }

    match gas_used.cmp(&gas_target) {
        // If the gas used in the current block is equal to the gas target, the base fee remains the
        // same (no increase).
        core::cmp::Ordering::Equal => base_fee,
        // If the gas used in the current block is greater than the gas target, calculate a new
        // increased base fee.
        core::cmp::Ordering::Greater => {
            // Calculate the increase in base fee based on the formula defined by EIP-1559.
            base_fee.saturating_add(core::cmp::max(
                // Ensure a minimum increase of 1.
                1,
                base_fee as u128 * (gas_used - gas_target) as u128
                    / (gas_target as u128 * ARC_BASE_FEE_FIXED_POINT_SCALE / k_rate as u128),
            ) as u64)
        }
        // If the gas used in the current block is less than the gas target, calculate a new
        // decreased base fee.
        core::cmp::Ordering::Less => {
            // Calculate the decrease in base fee based on the formula defined by EIP-1559.
            base_fee.saturating_sub(
                (base_fee as u128 * (gas_target - gas_used) as u128
                    / (gas_target as u128 * ARC_BASE_FEE_FIXED_POINT_SCALE / k_rate as u128))
                    as u64,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_eips::eip1559::DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR;
    use alloy_eips::eip1559::DEFAULT_ELASTICITY_MULTIPLIER;
    use alloy_primitives::{hex, Bytes};
    use reth_chainspec::BaseFeeParams;

    use crate::gas_fee::ARC_BASE_FEE_FIXED_POINT_SCALE;

    const ARC_BASE_FEE_K_RATE: u64 = 200;
    const ARC_BASE_FEE_INVERSE_ELASTICITY_MULTIPLIER: u64 = 7500;

    use super::{
        arc_calc_next_block_base_fee, decode_base_fee_from_bytes, determine_ema_parent_gas_used,
        encode_base_fee_to_bytes,
    };

    struct Case {
        name: &'static str,
        smoothed_parent_gas_used: u64,
        raw_block_gas_used: u64,
        alpha: u64,
        expected: Option<u64>,
    }

    #[test]
    fn determine_ema_parent_gas_used_table() {
        let max = u64::MAX;
        let _ = max; // keep for readability in cases below
        let cases: &[Case] = &[
            Case {
                name: "alpha > 100 -> None",
                smoothed_parent_gas_used: 100,
                raw_block_gas_used: 200,
                alpha: 101,
                expected: None,
            },
            Case {
                name: "alpha == 0 returns parent gas used (no change)",
                smoothed_parent_gas_used: 123_456,
                raw_block_gas_used: 987_654,
                alpha: 0,
                expected: Some(123_456),
            },
            Case {
                name: "alpha == 100 returns raw block gas used (no smoothing)",
                smoothed_parent_gas_used: 111,
                raw_block_gas_used: 222,
                alpha: 100,
                expected: Some(222),
            },
            Case {
                name: "50% average simple",
                smoothed_parent_gas_used: 100,
                raw_block_gas_used: 200,
                alpha: 50,
                expected: Some(150),
            },
            Case {
                name: "floor with single division (alpha=33, 1 & 2) => 1",
                smoothed_parent_gas_used: 1,
                raw_block_gas_used: 2,
                alpha: 33,
                expected: Some(1),
            },
            Case {
                name: "50% of (1,3) floors to 2",
                smoothed_parent_gas_used: 1,
                raw_block_gas_used: 3,
                alpha: 50,
                expected: Some(2),
            },
            // Wider accumulator behavior: these succeed (Some) rather than overflow to None.
            Case {
                name: "alpha=100 with very large raw succeeds (u128 accumulator)",
                smoothed_parent_gas_used: 0,
                raw_block_gas_used: (max / 100) + 1,
                alpha: 100,
                expected: Some((max / 100) + 1),
            },
            Case {
                name: "alpha=0 with very large smoothed succeeds (u128 accumulator)",
                smoothed_parent_gas_used: (max / 100) + 1,
                raw_block_gas_used: 0,
                alpha: 0,
                expected: Some((max / 100) + 1),
            },
        ];

        for tc in cases {
            let got = determine_ema_parent_gas_used(
                tc.smoothed_parent_gas_used,
                tc.raw_block_gas_used,
                tc.alpha,
            );
            match tc.expected {
                Some(expected) => assert_eq!(got, Some(expected), "{}", tc.name),
                None => assert!(got.is_none(), "{}", tc.name),
            }
        }
    }

    #[test]
    fn encode_decode_base_fee_roundtrip() {
        for base_fee in [0, 1, 77999931, u64::MAX] {
            let bytes = encode_base_fee_to_bytes(base_fee);
            let recovered = decode_base_fee_from_bytes(&bytes);

            assert_eq!(recovered, Some(base_fee), "{}", base_fee);
        }
    }

    #[test]
    fn decode_base_fee() {
        for (bytes, expected) in [
            (Bytes::from_static(&hex!("")), None),
            (Bytes::from_static(&hex!("0000000000000000")), Some(0_u64)),
            (Bytes::from_static(&hex!("000000000000000000")), None),
            (
                Bytes::from_static(&hex!("00000000000000000000000000000000")),
                None,
            ),
            (Bytes::from_static(b"ff"), None),
            (
                Bytes::from_static(&hex!("ffffffffffffffff")),
                Some(u64::MAX),
            ),
            (Bytes::from_static(&hex!("ffffffffffffffffff")), None),
            (
                Bytes::from_static(&hex!("ffffffffffffffffffffffffffffffffffffffffff")),
                None,
            ),
            (
                Bytes::from_static(&hex!("42681199dd51b2e2")),
                Some(4785093956621939426_u64),
            ),
            (
                Bytes::from_static(&hex!(
                    "42681199dd51b27f81ea319045dbb740f92740ce7f069cbcd3f25d97261969a4"
                )),
                None,
            ),
        ] {
            let recovered = decode_base_fee_from_bytes(&bytes);
            assert_eq!(
                recovered, expected,
                "recovered: {recovered:?}, expected: {expected:?}, bytes: {bytes:?}",
            );
        }
    }

    #[test]
    fn test_calc_next_block_base_fee() {
        // gas_used, gas_limit, base_fee, expected_base_fee
        let sample_blocks_info: [(u64, u64, u64, u64); _] = [
            (1000, 10000, 1000000, 982667),
            (1000, u64::MAX, 1000000, 980001),
            (3, 3, u64::MAX, u64::MAX),
            (3, 3, 451123121293128, 455634352506059),
            (100000, 300_000, u64::MAX, 18241780250668334375),
            (0, 30_000_000, 0, 0),
            (3, 3, 0, 1),
            (3, 3, 1, 2),
            (3, 3, 7, 8),
            (0, 30_000_000, 160_000_000_000, 156800000000),
            (1, 30_000_000, 160_000_000_000, 156800000143),
            (2, 30_000_000, 160_000_000_000, 156800000285),
            (3, 30_000_000, 160_000_000_000, 156800000427),
            (14_999_998, 30_000_000, 160_000_000_000, 158933333049),
            (14_999_999, 30_000_000, 160_000_000_000, 158933333192),
            (15_000_000, 30_000_000, 160_000_000_000, 158933333334),
            (15_000_001, 30_000_000, 160_000_000_000, 158933333476),
            (15_000_002, 30_000_000, 160_000_000_000, 158933333618),
            (15_000_003, 30_000_000, 160_000_000_000, 158933333760),
            (30_000_000, 30_000_000, 160_000_000_000, 161066666666),
            (29_999_999, 30_000_000, 160_000_000_000, 161066666524),
            (29_999_998, 30_000_000, 160_000_000_000, 161066666382),
            (1, 3, 160_000_000_000, 158400000000),
            (2, 3, 160_000_000_000, 160000000000),
            (3, 3, 160_000_000_000, 161600000000),
            (0, 30_000_000, 1, 1),
        ];

        for (gas_used, gas_limit, base_fee, _) in sample_blocks_info {
            if gas_used > gas_limit / 2 && base_fee == u64::MAX {
                continue; // eip1559 can not handle this
            }
            let eip1559_next_base_fee = BaseFeeParams::new(
                DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR as u128,
                DEFAULT_ELASTICITY_MULTIPLIER as u128,
            )
            .next_block_base_fee(gas_used, gas_limit, base_fee);

            // use kRate 12.5% and inverse elasticity multiplier 50% to compute the same value as eip1559
            let next_base_fee = arc_calc_next_block_base_fee(
                gas_used,
                gas_limit,
                base_fee,
                ARC_BASE_FEE_FIXED_POINT_SCALE as u64 / DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR,
                ARC_BASE_FEE_FIXED_POINT_SCALE as u64 / DEFAULT_ELASTICITY_MULTIPLIER,
            );

            assert_eq!(
                eip1559_next_base_fee, next_base_fee,
                "cmp with eip1559 gas used={gas_used}, gas limit={gas_limit}, base fee={base_fee}"
            );
        }

        for (gas_used, gas_limit, base_fee, expect_base_fee) in sample_blocks_info {
            let next_base_fee = arc_calc_next_block_base_fee(
                gas_used,
                gas_limit,
                base_fee,
                ARC_BASE_FEE_K_RATE,
                ARC_BASE_FEE_INVERSE_ELASTICITY_MULTIPLIER,
            );
            assert_eq!(
                expect_base_fee, next_base_fee,
                "gas used={gas_used}, gas limit={gas_limit}, base fee={base_fee}"
            );
        }
    }

    #[test]
    fn base_fee_should_greater_than_zero() {
        // For eip1559, the base fee could be zero if max_change_denominator is 1.
        for (
            denominator,
            elasticity_multiplier,
            gas_used,
            gas_limit,
            base_fee,
            expect_next_base_fee,
        ) in [
            (8, 2, 0, 30_000_000, 1, 1),
            (2, 2, 0, 30_000_000, 1, 1),
            (1, 2, 0, 30_000_000, 1, 0),
            (8, 2, 0, 30_000_000, 0, 0),
        ] {
            assert_eq!(
                BaseFeeParams::new(denominator, elasticity_multiplier as u128)
                    .next_block_base_fee(gas_used, gas_limit, base_fee),
                expect_next_base_fee,
                "gas_used={gas_used}, gas_limit={gas_limit}, base_fee={base_fee}, denominator={denominator}, elasticity_multiplier={elasticity_multiplier}",
            )
        }

        // We add a lower bound to make sure it will not be zero.
        for (k_rate, iem, gas_used, gas_limit, base_fee, expect_next_base_fee) in [
            (200, 7500, 0, 30_000_000, 1, 1), // the base fee should not be zero in normal case.
            (200, 5000, 0, 30_000_000, 1, 1), // the base fee should not be zero in normal case.
            (9000, 0, 1, 30_000_000, 10000, 10000), // if gas_target is zero, the base fee should be the same
            (10000, 10000, 0, 30_000_000, 1, 0),    // 100% decrease, but the result should be 1
            (10000, 10000, 0, 30_000_000, 10000, 0), // 100% decrease, but the result should be 1, not 0
            (0, 1, 0, 30_000_000, 8888, 8888),       // zero kRate, the base fee should be the same
            (200, 5000, 0, 30_000_000, 0, 0),        // keep zero value
            (0, 0, 0, 3, 1, 1),
            (0, 0, 1, 3, 1, 1),
        ] {
            let next_base_fee =
                arc_calc_next_block_base_fee(gas_used, gas_limit, base_fee, k_rate, iem);
            assert_eq!(
                next_base_fee, expect_next_base_fee,
                "gas_used={gas_used}, gas_limit={gas_limit}, base_fee={base_fee}, k_rate={k_rate}, inverse_elasticity_multiplier={iem}",
            );
        }
    }
}
