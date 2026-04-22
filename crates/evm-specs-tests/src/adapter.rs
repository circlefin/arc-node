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

use std::collections::HashMap;
use std::sync::Arc;

use arc_evm::ArcEvmFactory;
use arc_execution_config::chainspec::{ArcChainSpec, LOCAL_DEV};
use reth_evm::EvmEnv;
use revm::context::CfgEnv;
use revm_statetest_types::{SpecName, TestUnit};

use crate::error::EvmSpecsTestError;

/// Extract a test_name → chain_id map from raw JSON before revm-statetest-types
/// deserialization.
///
/// EEST StateFixture JSON has `"config": {"chainid": "0x01"}` at the test-unit
/// level. `revm-statetest-types` v14 does NOT parse this field (it silently
/// drops unknown fields). We parse the raw JSON as a generic map to extract
/// `config.chainid` per test name, then let revm-statetest-types handle the
/// execution-relevant fields.
///
/// TODO(arc-evm-specs-tests): remove this raw JSON extraction once `revm-statetest-types`
/// natively deserializes `config.chainid` for EEST fixtures.
/// Verified workaround requirement against `revm-statetest-types = 14.x`.
pub fn extract_chain_ids(
    raw_json: &serde_json::Value,
) -> Result<HashMap<String, u64>, EvmSpecsTestError> {
    let mut map = HashMap::new();
    if let Some(obj) = raw_json.as_object() {
        for (test_name, test_value) in obj {
            if let Some(raw_chain_id) = test_value
                .get("config")
                .and_then(|c| c.get("chainid"))
                .and_then(|v| v.as_str())
            {
                let chain_id = parse_chain_id(raw_chain_id).ok_or_else(|| {
                    EvmSpecsTestError::MalformedChainId {
                        test_name: test_name.clone(),
                        raw_value: raw_chain_id.to_string(),
                    }
                })?;
                map.insert(test_name.clone(), chain_id);
            }
        }
    }
    Ok(map)
}

fn parse_chain_id(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16).ok();
    }

    trimmed.parse::<u64>().ok()
}

/// Resolve chain ID for a test with precedence:
///   1. config.chainid (from raw JSON map — EEST source of truth)
///   2. env.current_chain_id (revm-statetest-types, older reference format)
///   3. Error (do NOT silently default)
pub fn resolve_chain_id(
    test_name: &str,
    chain_id_map: &HashMap<String, u64>,
    unit: &TestUnit,
) -> Result<u64, EvmSpecsTestError> {
    if let Some(&id) = chain_id_map.get(test_name) {
        return Ok(id);
    }
    if let Some(id) = unit.env.current_chain_id {
        return id
            .try_into()
            .map_err(|_| EvmSpecsTestError::MissingChainId {
                test_name: test_name.to_string(),
            });
    }
    Err(EvmSpecsTestError::MissingChainId {
        test_name: test_name.to_string(),
    })
}

/// Build the default ARC chain spec used for statetest execution.
///
/// This runner always executes against ARC localdev with all ARC hardforks
/// active. Ethereum fixture fork names still choose the `cfg.spec`, but the
/// underlying ARC executor is the full local ARC chain configuration.
///
/// This is intentional ARC-mode execution, not pure Ethereum fork isolation.
/// The fixture chooses the EVM spec id; the chain-level execution context
/// remains ARC `LOCAL_DEV`.
pub fn build_default_arc_chain_spec() -> Arc<ArcChainSpec> {
    LOCAL_DEV.clone()
}

/// Build the ArcEvmFactory from a chain spec.
///
/// Note: ArcEvmFactory::new takes a single arg (chain_spec).
/// The struct is #[non_exhaustive] so the API may expand in the future.
pub fn build_evm_factory(chain_spec: Arc<ArcChainSpec>) -> ArcEvmFactory {
    ArcEvmFactory::new(chain_spec)
}

/// Build CfgEnv + BlockEnv from a TestUnit for a given SpecName + chain_id.
///
/// Chain ID is resolved externally via `resolve_chain_id()` before calling
/// this function, so it takes chain_id as an explicit parameter.
pub fn build_evm_env(
    unit: &TestUnit,
    spec_name: &SpecName,
    chain_id: u64,
) -> Result<EvmEnv, EvmSpecsTestError> {
    let spec_id = spec_name.to_spec_id();
    let mut cfg = CfgEnv::default();

    cfg.chain_id = chain_id;
    cfg.spec = spec_id;

    let block = unit.block_env(&mut cfg);

    Ok(EvmEnv {
        cfg_env: cfg,
        block_env: block,
    })
}

/// Phase 1: Only standard Ethereum forks are supported.
/// `SpecName::Unknown` (custom ARC names) is unsupported.
pub fn is_supported_spec(spec_name: &SpecName) -> bool {
    !matches!(spec_name, SpecName::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, U256};
    use revm_primitives::hardfork::SpecId;
    use revm_statetest_types::{Env, TestUnit, TransactionParts};
    use std::collections::BTreeMap;

    fn unit_with_chain_id(chain_id: Option<U256>) -> TestUnit {
        TestUnit {
            info: None,
            env: Env {
                current_chain_id: chain_id,
                current_coinbase: Address::ZERO,
                current_difficulty: U256::ZERO,
                current_gas_limit: U256::from(30_000_000),
                current_number: U256::from(1),
                current_timestamp: U256::from(1),
                current_base_fee: Some(U256::ZERO),
                previous_hash: None,
                current_random: None,
                current_beacon_root: None,
                current_withdrawals_root: None,
                current_excess_blob_gas: None,
            },
            pre: alloy_primitives::map::HashMap::default(),
            post: BTreeMap::default(),
            transaction: TransactionParts {
                tx_type: None,
                data: vec![],
                gas_limit: vec![],
                gas_price: None,
                nonce: U256::ZERO,
                secret_key: Default::default(),
                sender: None,
                to: None,
                value: vec![],
                max_fee_per_gas: None,
                max_priority_fee_per_gas: None,
                initcodes: None,
                access_lists: vec![],
                authorization_list: None,
                blob_versioned_hashes: vec![],
                max_fee_per_blob_gas: None,
            },
            out: None,
        }
    }

    #[test]
    fn extracts_hex_and_decimal_chain_ids() {
        let json = serde_json::json!({
            "hex_case": { "config": { "chainid": "0x10" } },
            "dec_case": { "config": { "chainid": "10" } }
        });
        let ids = extract_chain_ids(&json).expect("chain ids should parse");
        assert_eq!(ids.get("hex_case"), Some(&16));
        assert_eq!(ids.get("dec_case"), Some(&10));
    }

    #[test]
    fn rejects_malformed_chain_id_with_context() {
        let json = serde_json::json!({
            "bad_case": { "config": { "chainid": "xyz" } }
        });
        let err = extract_chain_ids(&json).expect_err("invalid chain id should fail");
        assert!(matches!(
            err,
            EvmSpecsTestError::MalformedChainId { test_name, raw_value }
                if test_name == "bad_case" && raw_value == "xyz"
        ));
    }

    #[test]
    fn extract_chain_ids_ignores_missing_or_non_string_chain_ids() {
        let json = serde_json::json!({
            "missing": { "config": {} },
            "non_string": { "config": { "chainid": 1 } },
            "good": { "config": { "chainid": "0X2a" } }
        });

        let ids = extract_chain_ids(&json).expect("chain ids should parse");

        assert_eq!(ids.len(), 1);
        assert_eq!(ids.get("good"), Some(&42));
    }

    #[test]
    fn supported_spec_rejects_unknown_only() {
        assert!(is_supported_spec(&SpecName::Prague));
        assert!(!is_supported_spec(&SpecName::Unknown));
    }

    #[test]
    fn resolve_chain_id_prefers_explicit_map_over_env() {
        let unit = unit_with_chain_id(Some(U256::from(7)));
        let ids = HashMap::from([(String::from("fixture"), 42_u64)]);

        let resolved = resolve_chain_id("fixture", &ids, &unit).expect("chain id should resolve");

        assert_eq!(resolved, 42);
    }

    #[test]
    fn resolve_chain_id_falls_back_to_env_chain_id() {
        let unit = unit_with_chain_id(Some(U256::from(7)));

        let resolved = resolve_chain_id("fixture", &HashMap::default(), &unit)
            .expect("chain id should resolve");

        assert_eq!(resolved, 7);
    }

    #[test]
    fn resolve_chain_id_errors_when_no_sources_exist() {
        let unit = unit_with_chain_id(None);

        let err = resolve_chain_id("fixture", &HashMap::default(), &unit)
            .expect_err("missing chain id should fail");

        assert!(matches!(
            err,
            EvmSpecsTestError::MissingChainId { test_name } if test_name == "fixture"
        ));
    }

    #[test]
    fn build_default_arc_chain_spec_returns_local_dev() {
        let chain_spec = build_default_arc_chain_spec();

        assert_eq!(chain_spec.inner.chain.id(), LOCAL_DEV.inner.chain.id());
    }

    #[test]
    fn build_evm_env_sets_requested_chain_id_and_spec() {
        let unit = unit_with_chain_id(Some(U256::from(1)));

        let env = build_evm_env(&unit, &SpecName::Prague, 5042).expect("env should build");

        assert_eq!(env.cfg_env.chain_id, 5042);
        assert_eq!(env.cfg_env.spec, SpecId::PRAGUE);
        assert_eq!(env.block_env.number.to::<u64>(), 1);
    }

    #[test]
    fn build_evm_factory_uses_supplied_chain_spec() {
        let chain_spec = build_default_arc_chain_spec();
        let _factory = build_evm_factory(chain_spec);

        // Construction succeeds with the supplied chain spec.
    }
}
