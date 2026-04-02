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

use std::time::Duration;

use tracing::debug;

use arc_consensus_types::signing::PublicKey;
use arc_consensus_types::{Address, ConsensusParams, Validator, ValidatorSet};
use malachitebft_core_types::{LinearTimeouts, VotingPower};

use alloy_sol_macro::sol;
use alloy_sol_types::SolCall;

// ABI types for decoding the return value of the `getActiveValidatorSet` and `consensusParams` functions.
sol! {
    #[derive(PartialEq)]
    enum ContractValidatorStatus { Unknown, Registered, Active }
    struct ContractValidator {
        ContractValidatorStatus status;
        bytes publicKey;
        uint64 votingPower;
    }

    function getActiveValidatorSet() external view returns (ContractValidator[] memory activeValidators);

    struct ContractConsensusParams {
        uint16 timeoutProposeMs;
        uint16 timeoutProposeDeltaMs;
        uint16 timeoutPrevoteMs;
        uint16 timeoutPrevoteDeltaMs;
        uint16 timeoutPrecommitMs;
        uint16 timeoutPrecommitDeltaMs;
        uint16 timeoutRebroadcastMs;
        uint16 targetBlockTimeMs;
    }

    function consensusParams() external view override returns (ContractConsensusParams memory);
}

/// Decode validator set from ABI-encoded result
pub fn abi_decode_validator_set(result: Vec<u8>) -> eyre::Result<ValidatorSet> {
    // Decode the function's return payload exactly as the ABI defines it.
    let active_validators: Vec<ContractValidator> =
        getActiveValidatorSetCall::abi_decode_returns(&result)?;

    // Map contract validators to use Malachite's domain type
    let validators = active_validators
        .into_iter()
        .filter(|cv| cv.status == ContractValidatorStatus::Active && cv.votingPower > 0)
        .map(|cv| {
            // Convert contract publicKey bytes to Malachite's domain type
            if cv.publicKey.len() != 32 {
                eyre::bail!(
                    "Public key must be exactly 32 bytes, got {}",
                    cv.publicKey.len()
                );
            }
            let mut pk = [0u8; 32];
            pk.copy_from_slice(&cv.publicKey);

            let public_key = PublicKey::from_bytes(pk);
            let voting_power: VotingPower = cv.votingPower;
            Ok(Validator::new(public_key, voting_power))
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    debug!("ABI decoded validators:");
    for validator in &validators {
        debug!(
            "  - address: {}, public key: {}, voting power: {}",
            Address::from_public_key(&validator.public_key),
            hex::encode(validator.public_key.as_bytes()),
            validator.voting_power
        );
    }

    Ok(ValidatorSet::new(validators))
}

pub fn abi_decode_consensus_params(result: Vec<u8>) -> eyre::Result<ConsensusParams> {
    // Decode the function's return payload exactly as the ABI defines it.
    let contract_params: ContractConsensusParams =
        consensusParamsCall::abi_decode_returns(&result)?;

    let target_block_time = (contract_params.targetBlockTimeMs != 0)
        .then(|| Duration::from_millis(contract_params.targetBlockTimeMs as u64));

    // Map contract consensus params to use the domain type
    let consensus_params = ConsensusParams::new(
        target_block_time,
        LinearTimeouts {
            propose: Duration::from_millis(contract_params.timeoutProposeMs as u64),
            propose_delta: Duration::from_millis(contract_params.timeoutProposeDeltaMs as u64),
            prevote: Duration::from_millis(contract_params.timeoutPrevoteMs as u64),
            prevote_delta: Duration::from_millis(contract_params.timeoutPrevoteDeltaMs as u64),
            precommit: Duration::from_millis(contract_params.timeoutPrecommitMs as u64),
            precommit_delta: Duration::from_millis(contract_params.timeoutPrecommitDeltaMs as u64),
            rebroadcast: Duration::from_millis(contract_params.timeoutRebroadcastMs as u64),
        },
    );

    debug!("ABI decoded consensus params: {consensus_params:?}");
    Ok(consensus_params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_consensus_types::Address;
    use malachitebft_core_types::Validator as _;

    #[test]
    fn test_decode_valset_abi() {
        let mut pub_keys = [
            "c992c8696818bda11d628f38584022a6332c144b7f929b4d972bd39a23244aec",
            "35121369a803f64463e1688af1ba5d963a40b7d71eeffadd8496f1d5b8d61d53",
            "eda755457e2e7b8cb56956372611f5b5d37698eef26aa7fdb01616a6e7824f22",
        ];
        let raw_valset = [
            "0000000000000000000000000000000000000000000000000000000000000020",
            "0000000000000000000000000000000000000000000000000000000000000003",
            "0000000000000000000000000000000000000000000000000000000000000060",
            "0000000000000000000000000000000000000000000000000000000000000100",
            "00000000000000000000000000000000000000000000000000000000000001a0",
            "0000000000000000000000000000000000000000000000000000000000000002",
            "0000000000000000000000000000000000000000000000000000000000000060",
            "000000000000000000000000000000000000000000000000000000000000000f",
            "0000000000000000000000000000000000000000000000000000000000000020",
            pub_keys[0],
            "0000000000000000000000000000000000000000000000000000000000000002",
            "0000000000000000000000000000000000000000000000000000000000000060",
            "000000000000000000000000000000000000000000000000000000000000000f",
            "0000000000000000000000000000000000000000000000000000000000000020",
            pub_keys[1],
            "0000000000000000000000000000000000000000000000000000000000000002",
            "0000000000000000000000000000000000000000000000000000000000000060",
            "000000000000000000000000000000000000000000000000000000000000000f",
            "0000000000000000000000000000000000000000000000000000000000000020",
            pub_keys[2],
        ];
        let result = hex::decode(raw_valset.concat()).unwrap();

        let valset = abi_decode_validator_set(result).unwrap();
        // sort pub_keys by address
        pub_keys.sort_unstable_by(|pk, pk2| {
            let pk_bytes: [u8; 32] = hex::decode(pk).unwrap().try_into().unwrap();
            let pk2_bytes: [u8; 32] = hex::decode(pk2).unwrap().try_into().unwrap();
            let a1 = Address::from_public_key(&PublicKey::from_bytes(pk_bytes));
            let a2 = Address::from_public_key(&PublicKey::from_bytes(pk2_bytes));
            a1.cmp(&a2)
        });

        assert_eq!(valset.validators.len(), 3);
        for (i, (v, expected_pk)) in valset.validators.iter().zip(&pub_keys).enumerate() {
            let actual_pk = hex::encode(v.public_key().as_bytes());
            assert_eq!(
                actual_pk, *expected_pk,
                "Validator {i} has unexpected public key",
            );
            assert_eq!(
                v.voting_power(),
                15,
                "Validator {i} has unexpected voting power",
            );
        }
    }

    #[test]
    fn test_corrupted_public_key_validation_fails() {
        let pub_key = "c992c8696818bda11d628f38584022a6332c144b7f929b4d972bd39a23244aec";

        let raw_valset = [
            "0000000000000000000000000000000000000000000000000000000000000020",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000020",
            "0000000000000000000000000000000000000000000000000000000000000002",
            "0000000000000000000000000000000000000000000000000000000000000060",
            "0000000000000000000000000000000000000000000000000000000000000014",
            "0000000000000000000000000000000000000000000000000000000000000020",
            pub_key,
        ];
        // First, try with valid pubkey
        let mut bytes = hex::decode(raw_valset.concat()).unwrap();
        let valset = abi_decode_validator_set(bytes.clone()).unwrap();
        assert_eq!(valset.validators.len(), 1);

        let flip_byte_index = bytes.len() - 19; // Flip a bit to corrupt the public key
        bytes[flip_byte_index] ^= 0b0000_1000;

        // Then, try with corrupted pubkey
        let result = std::panic::catch_unwind(|| {
            let _ = abi_decode_validator_set(bytes.clone());
        });
        assert!(result.is_err(), "Expected panic on malformed public key");

        let payload = result.unwrap_err();
        let panic_msg = payload
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("<unknown panic>");
        assert!(
            panic_msg.contains("MalformedPublicKey"),
            "Expected panic with 'MalformedPublicKey', got: {panic_msg}",
        );
    }

    #[test]
    fn test_decode_consensus_params_abi() {
        // Test data representing ABI-encoded consensus parameters
        // Each uint16 is 32 bytes in ABI encoding (padded to 32 bytes)
        let raw_consensus_params = [
            "00000000000000000000000000000000000000000000000000000000000003e8", // timeoutProposeMs = 1000
            "000000000000000000000000000000000000000000000000000000000000012c", // timeoutProposeDeltaMs = 300
            "00000000000000000000000000000000000000000000000000000000000007d0", // timeoutPrevoteMs = 2000
            "000000000000000000000000000000000000000000000000000000000000012c", // timeoutPrevoteDeltaMs = 300
            "0000000000000000000000000000000000000000000000000000000000000bb8", // timeoutPrecommitMs = 3000
            "000000000000000000000000000000000000000000000000000000000000012c", // timeoutPrecommitDeltaMs = 300
            "0000000000000000000000000000000000000000000000000000000000000fa0", // timeoutRebroadcastMs = 4000
            "0000000000000000000000000000000000000000000000000000000000001388", // targetBlockTimeMs = 5000
        ];
        let result = hex::decode(raw_consensus_params.concat()).unwrap();

        let consensus_params = abi_decode_consensus_params(result).unwrap();

        assert_eq!(
            consensus_params.timeouts().propose,
            Duration::from_millis(1000)
        );
        assert_eq!(
            consensus_params.timeouts().propose_delta,
            Duration::from_millis(300)
        );
        assert_eq!(
            consensus_params.timeouts().prevote,
            Duration::from_millis(2000)
        );
        assert_eq!(
            consensus_params.timeouts().prevote_delta,
            Duration::from_millis(300)
        );
        assert_eq!(
            consensus_params.timeouts().precommit,
            Duration::from_millis(3000)
        );
        assert_eq!(
            consensus_params.timeouts().precommit_delta,
            Duration::from_millis(300)
        );
        assert_eq!(
            consensus_params.timeouts().rebroadcast,
            Duration::from_millis(4000)
        );
        assert_eq!(
            consensus_params.target_block_time(),
            Some(Duration::from_millis(500))
        );
    }

    #[test]
    fn test_decode_consensus_params_with_zero_values_returns_default() {
        // Test with all zero values
        let raw_consensus_params = [
            "0000000000000000000000000000000000000000000000000000000000000000", // timeoutProposeMs = 0
            "0000000000000000000000000000000000000000000000000000000000000000", // timeoutProposeDeltaMs = 0
            "0000000000000000000000000000000000000000000000000000000000000000", // timeoutPrevoteMs = 0
            "0000000000000000000000000000000000000000000000000000000000000000", // timeoutPrevoteDeltaMs = 0
            "0000000000000000000000000000000000000000000000000000000000000000", // timeoutPrecommitMs = 0
            "0000000000000000000000000000000000000000000000000000000000000000", // timeoutPrecommitDeltaMs = 0
            "0000000000000000000000000000000000000000000000000000000000000000", // timeoutRebroadcastMs = 0
            "0000000000000000000000000000000000000000000000000000000000000000", // targetBlockTimeMs = 0
        ];
        let result = hex::decode(raw_consensus_params.concat()).unwrap();

        let consensus_params = abi_decode_consensus_params(result).unwrap();
        let default = ConsensusParams::default();

        assert_eq!(
            consensus_params.timeouts().propose,
            default.timeouts().propose
        );
        assert_eq!(
            consensus_params.timeouts().propose_delta,
            default.timeouts().propose_delta
        );
        assert_eq!(
            consensus_params.timeouts().prevote,
            default.timeouts().prevote
        );
        assert_eq!(
            consensus_params.timeouts().prevote_delta,
            default.timeouts().prevote_delta
        );
        assert_eq!(
            consensus_params.timeouts().precommit,
            default.timeouts().precommit
        );
        assert_eq!(
            consensus_params.timeouts().precommit_delta,
            default.timeouts().precommit_delta
        );
        assert_eq!(
            consensus_params.timeouts().rebroadcast,
            default.timeouts().rebroadcast
        );
        // target_block_time should be None when 0 is provided
        assert_eq!(consensus_params.target_block_time(), None);
    }

    #[test]
    fn test_decode_consensus_params_with_max_values_returns_default() {
        // Test with maximum uint16 values (65535)
        let raw_consensus_params = [
            "000000000000000000000000000000000000000000000000000000000000ffff", // timeoutProposeMs = 65535
            "000000000000000000000000000000000000000000000000000000000000ffff", // timeoutProposeDeltaMs = 65535
            "000000000000000000000000000000000000000000000000000000000000ffff", // timeoutPrevoteMs = 65535
            "000000000000000000000000000000000000000000000000000000000000ffff", // timeoutPrevoteDeltaMs = 65535
            "000000000000000000000000000000000000000000000000000000000000ffff", // timeoutPrecommitMs = 65535
            "000000000000000000000000000000000000000000000000000000000000ffff", // timeoutPrecommitDeltaMs = 65535
            "000000000000000000000000000000000000000000000000000000000000ffff", // timeoutRebroadcastMs = 65535
            "000000000000000000000000000000000000000000000000000000000000ffff", // targetBlockTimeMs = 65535
        ];
        let result = hex::decode(raw_consensus_params.concat()).unwrap();

        let default = ConsensusParams::default();
        let consensus_params = abi_decode_consensus_params(result).unwrap();

        assert_eq!(
            consensus_params.timeouts().propose,
            default.timeouts().propose
        );
        assert_eq!(
            consensus_params.timeouts().propose_delta,
            default.timeouts().propose_delta
        );
        assert_eq!(
            consensus_params.timeouts().prevote,
            default.timeouts().prevote
        );
        assert_eq!(
            consensus_params.timeouts().prevote_delta,
            default.timeouts().prevote_delta
        );
        assert_eq!(
            consensus_params.timeouts().precommit,
            default.timeouts().precommit
        );
        assert_eq!(
            consensus_params.timeouts().precommit_delta,
            default.timeouts().precommit_delta
        );
        assert_eq!(
            consensus_params.timeouts().rebroadcast,
            default.timeouts().rebroadcast
        );
        assert_eq!(
            consensus_params.target_block_time(),
            default.target_block_time()
        );
    }

    #[test]
    fn test_decode_consensus_params_invalid_data() {
        // Test with invalid data (too short)
        let invalid_data =
            hex::decode("0000000000000000000000000000000000000000000000000000000000000000")
                .unwrap();

        let result = abi_decode_consensus_params(invalid_data);
        assert!(result.is_err(), "Expected error for invalid data");
    }
}
