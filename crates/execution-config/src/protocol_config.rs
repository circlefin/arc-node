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

//! ProtocolConfig
//!
//! This module contains functions and types for reading data from the ProtocolConfig contract.

use alloy_primitives::address;
use alloy_primitives::Bytes;
use alloy_sol_types::sol;
use alloy_sol_types::SolCall;
use reth_evm::Evm;
use revm::DatabaseCommit;
use revm_primitives::Address;

/// Error types for ProtocolConfig contract interactions
#[derive(Debug, thiserror::Error)]
pub enum ProtocolConfigError<H> {
    #[error("System call execution failed: {0:?}")]
    SystemCallFailed(revm::context::result::ExecutionResult<H>),
    #[error("ProtocolConfig contract returned empty output")]
    EmptyOutput,
    #[error("Failed to decode contract response: {0}")]
    DecodingError(#[from] alloy_sol_types::Error),
    #[error("EVM execution error: {0}")]
    EvmError(String),
}

// Constants

// ProtocolConfig contract address
pub const PROTOCOL_CONFIG_ADDRESS: Address = address!("0x3600000000000000000000000000000000000001");

sol! {
    /// ProtocolConfig interface for reward beneficiary and gas parameters
    interface IProtocolConfig {
        /// FeeParams struct matching the contract definition
        struct FeeParams {
            uint64 alpha;
            uint64 kRate;
            uint64 inverseElasticityMultiplier;
            uint256 minBaseFee;
            uint256 maxBaseFee;
            uint256 blockGasLimit;
        }

        /// Returns the current reward beneficiary address
        function rewardBeneficiary() external view returns (address beneficiary);

        /// Returns the current fee parameters
        function feeParams() external view returns (FeeParams params);
    }
}

/// Returns the gas limit from `fee_params` if it is representable as `u64` and
/// within the configured bounds, otherwise returns the default.
///
/// Used by both the proposer (payload building) and the receiver (pre-execution
/// validation) to derive the expected block gas limit from ProtocolConfig.
pub fn expected_gas_limit(
    fee_params: Option<&IProtocolConfig::FeeParams>,
    config: &crate::chainspec::BlockGasLimitConfig,
) -> u64 {
    fee_params
        .and_then(|fp| fp.blockGasLimit.try_into().ok())
        .filter(|&gl: &u64| gl >= config.min() && gl <= config.max())
        .unwrap_or(config.default())
}

/// Clamps the base fee based on configurations
pub fn determine_bounded_base_fee(fee_params: &IProtocolConfig::FeeParams, base_fee: u64) -> u64 {
    let configured_max = fee_params.maxBaseFee.try_into().unwrap_or(u64::MAX);
    let configured_min = fee_params.minBaseFee.try_into().unwrap_or(u64::MAX);

    // Nonsensical range
    if configured_max == 0 || configured_max < configured_min {
        base_fee
    } else {
        base_fee.clamp(configured_min, configured_max)
    }
}

/// Query the ProtocolConfig system contract for the configured fee parameters
///
/// Returns the fee parameters if successfully queried,
/// or `Err(ProtocolConfigError)` if there was an error during execution,
/// contract deployment issues, or empty output.
pub fn retrieve_fee_params<E>(
    evm: &mut E,
) -> Result<IProtocolConfig::FeeParams, ProtocolConfigError<E::HaltReason>>
where
    E: Evm,
    E::DB: DatabaseCommit,
{
    let call_data = IProtocolConfig::feeParamsCall {}.abi_encode();

    let result_and_state = evm
        .transact_system_call(
            Address::ZERO,           // caller (use zero address to avoid RejectCallerWithCode)
            PROTOCOL_CONFIG_ADDRESS, // contract address
            Bytes::from(call_data),
        )
        .map_err(|e| ProtocolConfigError::EvmError(format!("{e:?}")))?;

    if !result_and_state.result.is_success() {
        return Err(ProtocolConfigError::SystemCallFailed(
            result_and_state.result,
        ));
    }

    let output = result_and_state
        .result
        .output()
        .ok_or(ProtocolConfigError::EmptyOutput)?;

    let fee_params = IProtocolConfig::feeParamsCall::abi_decode_returns(output)
        .map_err(ProtocolConfigError::DecodingError)?;

    Ok(fee_params)
}

/// Returns the beneficiary address if successfully queried,
/// or `Err(ProtocolConfigError)` if there was an error during execution,
/// contract deployment issues, or empty output.
pub fn retrieve_reward_beneficiary<E>(
    evm: &mut E,
) -> Result<Address, ProtocolConfigError<E::HaltReason>>
where
    E: Evm,
    E::DB: DatabaseCommit,
{
    // Create call data for rewardBeneficiary() function
    let call_data = IProtocolConfig::rewardBeneficiaryCall {}.abi_encode();

    // Make the system call to ProtocolConfig contract
    let result_and_state = evm
        .transact_system_call(
            Address::ZERO,
            PROTOCOL_CONFIG_ADDRESS, // contract address
            Bytes::from(call_data),
        )
        .map_err(|e| ProtocolConfigError::EvmError(format!("{e:?}")))?;

    // Check if the call was successful
    if !result_and_state.result.is_success() {
        return Err(ProtocolConfigError::SystemCallFailed(
            result_and_state.result,
        ));
    }

    // Decode the returned address from the call result
    let output = result_and_state.result.output().unwrap_or_default();
    if output.is_empty() {
        return Err(ProtocolConfigError::EmptyOutput);
    }

    let beneficiary = IProtocolConfig::rewardBeneficiaryCall::abi_decode_returns(output)?;
    Ok(beneficiary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chainspec::BlockGasLimitConfig;
    use reth_ethereum::evm::revm::primitives::U256;

    fn fee_params_with(block_gas_limit: U256) -> IProtocolConfig::FeeParams {
        IProtocolConfig::FeeParams {
            alpha: 0,
            kRate: 0,
            inverseElasticityMultiplier: 0,
            minBaseFee: U256::from(0),
            maxBaseFee: U256::from(0),
            blockGasLimit: block_gas_limit,
        }
    }

    fn fee_params_with_min_max(min: U256, max: U256) -> IProtocolConfig::FeeParams {
        IProtocolConfig::FeeParams {
            alpha: 0,
            kRate: 0,
            inverseElasticityMultiplier: 0,
            minBaseFee: min,
            maxBaseFee: max,
            blockGasLimit: U256::from(30_000_000u64),
        }
    }

    #[test]
    fn determine_bounded_base_fee_table() {
        struct Case {
            name: &'static str,
            min: U256,
            max: U256,
            base: u64,
            expect: u64,
        }

        let cases = vec![
            // No max configured (max==0) -> passthrough
            Case {
                name: "no_max_configured_passthrough",
                min: U256::from(0),
                max: U256::from(0),
                base: 1_000_000,
                expect: 1_000_000,
            },
            // Invalid range (max < min) -> passthrough
            Case {
                name: "invalid_range_passthrough",
                min: U256::from(2_000_000u64),
                max: U256::from(1_000_000u64),
                base: 1_500_000,
                expect: 1_500_000,
            },
            // Clamp up to min
            Case {
                name: "below_min_clamped_up",
                min: U256::from(1_000_000u64),
                max: U256::from(10_000_000u64),
                base: 999_999,
                expect: 1_000_000,
            },
            // Clamp down to max
            Case {
                name: "above_max_clamped_down",
                min: U256::from(1_000_000u64),
                max: U256::from(10_000_000u64),
                base: 10_000_001,
                expect: 10_000_000,
            },
            // Within range unchanged
            Case {
                name: "within_range_unchanged",
                min: U256::from(1_000_000u64),
                max: U256::from(10_000_000u64),
                base: 4_000_000,
                expect: 4_000_000,
            },
            // Below min, narrow range
            Case {
                name: "below_min_narrow_Range",
                min: U256::from(1_000_000u64),
                max: U256::from(1_000_000u64),
                base: 900_000,
                expect: 1_000_000u64,
            },
            // Above max, narrow range
            Case {
                name: "above_max_narrow_Range",
                min: U256::from(1_000_000u64),
                max: U256::from(1_000_000u64),
                base: 2_000_000,
                expect: 1_000_000u64,
            },
        ];

        for c in cases {
            let fee = fee_params_with_min_max(c.min, c.max);
            let got = determine_bounded_base_fee(&fee, c.base);
            assert_eq!(got, c.expect, "case: {}", c.name);
        }
    }

    #[test]
    fn expected_gas_limit_returns_value_when_in_bounds() {
        let fee = fee_params_with(U256::from(30_000_000u64));
        let config = BlockGasLimitConfig::new(1_000_000, 1_000_000_000, 5_000_000);
        assert_eq!(expected_gas_limit(Some(&fee), &config), 30_000_000);
    }

    #[test]
    fn expected_gas_limit_returns_default_when_below_min() {
        let fee = fee_params_with(U256::from(500_000u64));
        let config = BlockGasLimitConfig::new(1_000_000, 1_000_000_000, 30_000_000);
        assert_eq!(expected_gas_limit(Some(&fee), &config), 30_000_000);
    }

    #[test]
    fn expected_gas_limit_returns_default_when_above_max() {
        let fee = fee_params_with(U256::from(2_000_000_000u64));
        let config = BlockGasLimitConfig::new(1_000_000, 1_000_000_000, 30_000_000);
        assert_eq!(expected_gas_limit(Some(&fee), &config), 30_000_000);
    }

    #[test]
    fn expected_gas_limit_returns_default_when_overflows_u64() {
        let fee = fee_params_with(U256::from(u64::MAX) + U256::from(1));
        let config = BlockGasLimitConfig::new(1_000_000, 1_000_000_000, 30_000_000);
        assert_eq!(expected_gas_limit(Some(&fee), &config), 30_000_000);
    }

    #[test]
    fn expected_gas_limit_returns_default_when_none() {
        let config = BlockGasLimitConfig::new(1_000_000, 1_000_000_000, 30_000_000);
        assert_eq!(expected_gas_limit(None, &config), 30_000_000);
    }

    #[test]
    fn expected_gas_limit_accepts_boundary_values() {
        let config = BlockGasLimitConfig::new(1_000_000, 1_000_000_000, 5_000_000);

        let fee_at_min = fee_params_with(U256::from(1_000_000u64));
        assert_eq!(expected_gas_limit(Some(&fee_at_min), &config), 1_000_000);

        let fee_at_max = fee_params_with(U256::from(1_000_000_000u64));
        assert_eq!(
            expected_gas_limit(Some(&fee_at_max), &config),
            1_000_000_000
        );
    }
}
