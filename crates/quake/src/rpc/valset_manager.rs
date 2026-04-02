// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
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

use alloy_consensus::{SignableTransaction, Signed, TxEip1559};
use alloy_primitives::{address, Address, Bytes, TxKind, U256};
use alloy_signer::Signer;
use alloy_signer_local::LocalSigner;
use alloy_sol_macro::sol;
use alloy_sol_types::SolCall;
use arc_consensus_types::{signing::PublicKey, Address as MalachiteAddress};
use color_eyre::eyre::{eyre, Context, Result};
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::node::NodeName;

sol! {
    function updateValidatorVotingPower(uint64 newVotingPower);

    #[derive(Debug, Deserialize)]
    enum ContractValidatorStatus { Unknown, Registered, Active }

    #[derive(Debug, Deserialize)]
    struct ContractValidator {
        // status does not indicate whether a validator is part of the validator set
        // and participating in consensus. It's related to the smart contract's
        // admin operations to handle validator registration and activation.
        // Therefore, the code in this crate does not use it.
        ContractValidatorStatus status;
        bytes publicKey;
        uint64 votingPower;
    }
    function getValidator(address controller) external view returns (ContractValidator memory);
}

impl ContractValidator {
    pub fn public_key(&self) -> Result<PublicKey> {
        let key_bytes: [u8; 32] = self.publicKey[..].try_into()?;
        Ok(PublicKey::from_bytes(key_bytes))
    }

    pub fn address(&self) -> Result<MalachiteAddress> {
        let public_key = self.public_key()?;
        Ok(MalachiteAddress::from_public_key(&public_key))
    }
}

impl fmt::Display for ContractValidator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let public_key = self.public_key().map_err(|_| fmt::Error)?;
        let address = self.address().map_err(|_| fmt::Error)?;
        writeln!(f, "\t- Address: {address}")?;
        writeln!(
            f,
            "\t- Public Key: 0x{}",
            hex::encode(public_key.as_bytes())
        )?;
        writeln!(f, "\t- Voting Power: {}", self.votingPower)?;
        Ok(())
    }
}

pub(crate) fn public_key_to_hex(public_key: &PublicKey) -> String {
    hex::encode(public_key.as_bytes())
}

// The address of the PermissionedValidatorManager contract
const PERMISSIONED_VALIDATOR_MANAGER_ADDRESS: Address =
    address!("0x3600000000000000000000000000000000000003");

// tx parameters
// the chain ID is set in the hardhat/genesis.config.ts script
const QUAKE_TESTNET_CHAIN_ID: u64 = 1337;
// 1 gwei
const MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;
// 1000 gwei. This is the value of maxBaseFee set in config.json.
const MAX_FEE_PER_GAS: u128 = 1_000_000_000_000;
// Estimated by manually running eth_estimateGas on the CLI with a variety
// of parameters for updateValidatorVotingPower(uint64).
// The alternative would be to do the RPC call eth_estimateGas here.
// This number should be enough for the call to succeed.
const GAS_LIMIT: u64 = 600_000;

/// The controllers's config file stores ControllerInfo objects keyed by
/// validator name. Here is an example of the format:
/// {
///   "validator-blue": {
///     "index": 1,
///     "address": "0x...",
///     "signingKey": "0x...",
///     "nonce": 0
///   },
///   "validator-green": {
///     "index": 2,
///     "address": "0x...",
///     "signingKey": "0x...",
///     "nonce": 0
///   }
///   "validator3": {
///     "index": 3,
///     "address": "0x...",
///     "signingKey": "0x...",
///     "nonce": 0
///   }
/// }
const CONTROLLERS_CONFIG_FILE: &str = "controllers-config.json";

/// The controllers controlling the validators in this testnet.
#[derive(Clone)]
pub(crate) struct Controllers(HashMap<NodeName, ControllerInfo>);

impl Controllers {
    /// Loads the controllers configuration from a JSON-formatted file storing
    /// the controllers' configuration JSON file. The file should exist at
    /// 'config_dir/controllers-config.json`.
    /// 'quake setup' created the file when generating the genesis for the
    /// testnet.
    pub(crate) fn load_from_file(config_dir: impl AsRef<Path>) -> Result<Controllers> {
        let config_path = config_dir.as_ref().join(CONTROLLERS_CONFIG_FILE);
        let data = std::fs::read_to_string(config_path).wrap_err_with(|| {
            format!(
                "Failed to read controllers config file: {}",
                CONTROLLERS_CONFIG_FILE
            )
        })?;

        let controller_configs = serde_json::from_str::<HashMap<String, ControllerInfo>>(&data)
            .wrap_err_with(|| {
                format!(
                    "failed to read JSON data from controllers config file at {}",
                    CONTROLLERS_CONFIG_FILE
                )
            })?;

        Ok(Controllers(controller_configs))
    }

    /// Returns the controller configuration for a given validator name.
    pub(crate) fn load_controller(&self, validator_name: &str) -> Result<ControllerInfo> {
        let controller = self
            .0
            .get(validator_name)
            .ok_or_else(|| eyre!("{validator_name} not found in controllers config"))?;

        Ok(controller.clone())
    }

    /// Stores the given controller configuration for a validator name.
    pub(crate) fn store_controller(&mut self, validator_name: &str, controller: ControllerInfo) {
        self.0.insert(validator_name.to_string(), controller);
    }

    /// Stores the controllers configuration to a JSON-formatted file.
    /// The file should exist at 'config_dir/controllers-config.json`.
    /// 'quake setup' created the file when generating the genesis for the
    /// testnet.
    pub(crate) fn store_to_file(&self, config_dir: impl AsRef<Path>) -> Result<()> {
        let config_path = config_dir.as_ref().join(CONTROLLERS_CONFIG_FILE);
        let serialized = serde_json::to_string_pretty(&self.0)
            .wrap_err("failed to serialize controllers configuration")?;

        std::fs::write(&config_path, serialized).wrap_err_with(|| {
            format!(
                "failed to write controllers config file at {}",
                config_path.display()
            )
        })?;

        Ok(())
    }
}

/// Information about a validator's controller needed to send write transactions
/// to the validator manager contract, which only accepts them from a
/// validator's controller.
/// see contracts/src/validator-manager/PermissionedValidatorManager.sol
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ControllerInfo {
    pub(super) index: u64,
    #[serde(rename = "signingKey")]
    signing_key: String,
    pub(super) address: String,
    pub(super) nonce: u64,
}

impl ControllerInfo {
    pub(crate) fn eth_address(&self) -> &str {
        &self.address
    }
}

/// Builds a transaction calling updateValidatorVotingPower on the
/// PermissionedValidatorManager contract.
/// Returns the raw signed transaction as a hex string.
pub(super) async fn build_validator_update_tx(
    controller: &ControllerInfo,
    new_voting_power: u64,
) -> Result<String> {
    let tx = TxEip1559 {
        chain_id: QUAKE_TESTNET_CHAIN_ID,
        nonce: controller.nonce,
        max_fee_per_gas: MAX_FEE_PER_GAS,
        max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
        gas_limit: GAS_LIMIT,
        to: TxKind::Call(PERMISSIONED_VALIDATOR_MANAGER_ADDRESS),
        value: U256::ZERO,
        access_list: Default::default(),
        input: validator_update_calldata(new_voting_power),
    };

    let signed_tx = sign_tx(controller, tx)
        .await
        .wrap_err("failed to sign transaction")?;

    let eip2718_encoded_tx = encode_tx(signed_tx);
    let raw_tx = format!("0x{}", hex::encode(eip2718_encoded_tx));

    Ok(raw_tx)
}

/// returns the calldata for updating a validator's voting power
fn validator_update_calldata(voting_power: u64) -> Bytes {
    let call = updateValidatorVotingPowerCall {
        newVotingPower: voting_power,
    };

    call.abi_encode().into()
}

// Signs the given transaction using the controller's signing key.
async fn sign_tx(controller: &ControllerInfo, tx: TxEip1559) -> Result<Signed<TxEip1559>> {
    let sign_key = controller.signing_key.clone();
    let sign_key_hex = sign_key.strip_prefix("0x").unwrap_or(sign_key.as_str());
    let mut signer = LocalSigner::<SigningKey>::from_str(sign_key_hex).wrap_err_with(|| {
        format!(
            "failed to parse signing key for controller: {:?}",
            controller
        )
    })?;

    signer.set_chain_id(Some(QUAKE_TESTNET_CHAIN_ID));

    let sig_hash = tx.signature_hash();
    let signature = signer.sign_hash(&sig_hash).await?;

    Ok(tx.into_signed(signature))
}

/// Encodes the given signed transaction into EIP-2718 format.
fn encode_tx(signed_tx: Signed<TxEip1559>) -> Bytes {
    let mut payload = Vec::with_capacity(signed_tx.eip2718_encoded_length());
    signed_tx.eip2718_encode(&mut payload);

    payload.into()
}

/// returns the JSON params for the call to getValidator().
pub(super) fn get_validator_call_params(controller_addr: &str) -> serde_json::Value {
    let call = getValidatorCall {
        controller: Address::from_str(controller_addr).unwrap(),
    };

    let call_abi = call.abi_encode();

    let params = json!([
        {
            "to": PERMISSIONED_VALIDATOR_MANAGER_ADDRESS,
            "data": format!("0x{}", hex::encode(call_abi)),
        },
        "latest"
    ]);

    params
}

/// Decodes the ABI response of the getValidator() call and returns the validator.
pub(super) fn get_validator_response_decode(response: &str) -> Result<ContractValidator> {
    let hex_str = response.strip_prefix("0x").unwrap_or(response);
    let hex_decoded =
        hex::decode(hex_str).wrap_err("failed to hex-decode getValidator() response")?;
    let validator = getValidatorCall::abi_decode_returns(&hex_decoded)
        .wrap_err("failed to decode ABI response for getValidator()")?;

    Ok(validator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Signature;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("quake_valset_manager_{prefix}_{timestamp}"));
            fs::create_dir(&path).expect("failed to create temporary directory for testing");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn load_from_file_reads_valid_config() {
        let temp_dir = TempDir::new("load_success");
        let config_path = temp_dir.path.join(CONTROLLERS_CONFIG_FILE);
        let config = r#"{
            "validator1": {
                "index": 1,
                "signingKey": "0x1234",
                "address": "0xAAAA",
                "nonce": 7
            }
        }"#;
        fs::write(&config_path, config)
            .expect("failed to write controllers config file for testing");

        let controllers = Controllers::load_from_file(&temp_dir.path)
            .expect("expected test controllers configuration to load successfully");

        let controller = controllers
            .load_controller("validator1")
            .expect("validator1 should be in the test controllers config file");

        assert_eq!(controller.index, 1);
        assert_eq!(controller.signing_key, "0x1234");
        assert_eq!(controller.address, "0xAAAA");
        assert_eq!(controller.nonce, 7);
    }

    #[test]
    fn load_from_file_fails_with_invalid_json() {
        let temp_dir = TempDir::new("load_invalid_json");
        let config_path = temp_dir.path.join(CONTROLLERS_CONFIG_FILE);
        fs::write(&config_path, "not-json")
            .expect("failed to write controllers config file for testing");

        let result = Controllers::load_from_file(&temp_dir.path);

        assert!(result.is_err(), "expected invalid JSON to error");
    }

    #[test]
    fn store_to_file_writes_controller_map() {
        let mut controllers = Controllers(HashMap::new());
        controllers.store_controller(
            "validator-green",
            ControllerInfo {
                index: 2,
                signing_key: "0xasf313".to_string(),
                address: "0xBBBB".to_string(),
                nonce: 3,
            },
        );
        let temp_dir = TempDir::new("store_success");

        controllers
            .store_to_file(&temp_dir.path)
            .expect("store_to_file should succeed");

        let controllers_from_file = Controllers::load_from_file(&temp_dir.path)
            .expect("load_from_file should read stored controllers config file");

        let controller = controllers_from_file
            .load_controller("validator-green")
            .expect("validator-green should be in the test controllers config file");
        assert_eq!(controller.index, 2);
        assert_eq!(controller.signing_key, "0xasf313");
        assert_eq!(controller.address, "0xBBBB");
        assert_eq!(controller.nonce, 3);
    }

    #[test]
    fn store_to_file_errors_when_directory_missing() {
        let controllers = Controllers(HashMap::new());
        let dir = PathBuf::from("/nonexistent/quake_dir");
        let result = controllers.store_to_file(&dir);

        assert!(result.is_err(), "should fail when directory is missing");
    }

    fn sample_tx() -> TxEip1559 {
        TxEip1559 {
            chain_id: QUAKE_TESTNET_CHAIN_ID,
            nonce: 42,
            max_fee_per_gas: MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
            gas_limit: GAS_LIMIT,
            to: TxKind::Call(PERMISSIONED_VALIDATOR_MANAGER_ADDRESS),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        }
    }

    #[tokio::test]
    async fn sign_tx_signs_with_controller_key() {
        const TEST_SIGNING_KEY: &str =
            "0x92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e";

        let controller = ControllerInfo {
            index: 1,
            signing_key: TEST_SIGNING_KEY.to_string(),
            address: "0xbbbb".to_string(),
            nonce: 0,
        };

        let expected_address =
            LocalSigner::<SigningKey>::from_str(TEST_SIGNING_KEY.trim_start_matches("0x"))
                .expect("valid signing key for testing")
                .address();

        let signed_tx = sign_tx(&controller, sample_tx())
            .await
            .expect("signing test tx should succeed");

        let sighash = signed_tx.signature_hash();
        let recovered = signed_tx
            .signature()
            .recover_address_from_prehash(&sighash)
            .expect("recovering address from signature should succeed");

        assert_eq!(recovered, expected_address);
    }

    #[tokio::test]
    async fn sign_tx_fails_for_invalid_key() {
        let controller = ControllerInfo {
            index: 1,
            signing_key: "invalid-key".to_string(),
            address: "0x0".to_string(),
            nonce: 0,
        };

        let result = sign_tx(&controller, sample_tx()).await;
        assert!(result.is_err(), "invalid signing key should error");
    }

    #[test]
    fn encode_tx_matches_manual_encoding() {
        let signed = Signed::new_unhashed(sample_tx(), Signature::test_signature());
        let mut expected = Vec::new();
        signed.clone().eip2718_encode(&mut expected);

        let encoded = encode_tx(signed);

        assert_eq!(encoded.as_ref(), expected.as_slice());
    }

    #[test]
    fn encode_tx_matches_decode_tx() {
        let signed = Signed::new_unhashed(sample_tx(), Signature::test_signature());
        let encoded = encode_tx(signed.clone());

        let mut encoded_slice = encoded.as_ref();
        let decoded =
            Signed::<TxEip1559>::eip2718_decode(&mut encoded_slice).expect("decode should succeed");

        assert_eq!(decoded.tx(), signed.tx());
        assert_eq!(decoded.signature(), signed.signature());
    }

    #[test]
    fn get_validator_response_decode_parses_struct() {
        let validator = ContractValidator {
            status: ContractValidatorStatus::Active,
            publicKey: Bytes::from(vec![0xAA; 32]),
            votingPower: 42,
        };
        let encoded = getValidatorCall::abi_encode_returns(&validator.clone());
        let response = format!("0x{}", hex::encode(encoded));

        let decoded = get_validator_response_decode(&response)
            .expect("decoding valid response should succeed");

        assert!(matches!(decoded.status, ContractValidatorStatus::Active));
        assert_eq!(decoded.publicKey, validator.publicKey);
        assert_eq!(decoded.votingPower, validator.votingPower);
    }

    #[test]
    fn get_validator_response_decode_errors_on_malformed_hex() {
        let result = get_validator_response_decode("0x1234");
        assert!(result.is_err(), "malformed response should error");
    }
}
