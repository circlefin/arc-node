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

pub use arc_pq_precompile::{IPQ, PQ_ADDRESS};

use arc_execution_config::hardforks::ArcHardforkFlags;
use reth_ethereum::evm::revm::precompile::{PrecompileError, PrecompileOutput};

pub(crate) fn run_pq(
    input: reth_evm::precompiles::PrecompileInput,
    _hardfork_flags: ArcHardforkFlags,
) -> Result<PrecompileOutput, PrecompileError> {
    arc_pq_precompile::run_pq_precompile(input.gas, input.data, input.reservoir)
}

// arc-pq-precompile cannot depend on arc-precompiles (cycle), so this cross-crate equality check
// is placed here to ensure EARLY_REVERT_GAS and PRECOMPILE_EARLY_REVERT_GAS_PENALTY stay in sync.
const _: () = assert!(
    arc_pq_precompile::EARLY_REVERT_GAS == crate::helpers::PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
    "arc_pq_precompile::EARLY_REVERT_GAS diverged from PRECOMPILE_EARLY_REVERT_GAS_PENALTY"
);

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_evm::EvmFactory;
    use alloy_primitives::{address, hex, Address, TxKind};
    use alloy_sol_types::SolCall;
    use arc_evm::{ArcEvm, ArcEvmFactory};
    use arc_execution_config::chainspec::ArcChainSpec;
    use reth_evm::{eth::EthEvmContext, precompiles::PrecompilesMap, Evm};
    use revm::{
        context::{
            result::{EVMError, ExecResultAndState, ExecutionResult},
            TxEnv,
        },
        context_interface::result::HaltReason,
        database::{CacheDB, InMemoryDB},
        handler::instructions::EthInstructions,
        inspector::NoOpInspector,
        interpreter::interpreter::EthInterpreter,
    };
    use std::sync::Arc;

    const TEST_USER_ADDRESS: Address = address!("0x1234567890123456789012345678901234567890");

    type TestEvm = ArcEvm<
        EthEvmContext<CacheDB<InMemoryDB>>,
        NoOpInspector,
        EthInstructions<EthInterpreter, EthEvmContext<CacheDB<InMemoryDB>>>,
        PrecompilesMap,
    >;

    /// Helper to create an EVM with Zero6 hardfork activated
    fn create_test_evm() -> TestEvm {
        let db = CacheDB::new(InMemoryDB::default());
        let mut inner_chain_spec = reth_chainspec::ChainSpecBuilder::mainnet()
            .berlin_activated()
            .london_activated()
            .paris_activated()
            .shanghai_activated()
            .build();

        // Activate Zero6 hardfork at block 0 (PQ precompile is gated on Zero6)
        inner_chain_spec.hardforks.insert(
            arc_execution_config::hardforks::ArcHardfork::Zero6,
            reth_ethereum_forks::ForkCondition::Block(0),
        );

        let chain_spec = Arc::new(ArcChainSpec::new(inner_chain_spec));
        let factory = ArcEvmFactory::new(chain_spec.clone());
        factory.create_evm(db, reth_evm::EvmEnv::default())
    }

    /// Helper to call SLH-DSA-SHA2-128s verifier with given inputs.
    /// Param order matches the ABI: (vk, message, sig).
    fn transact_slh_dsa_verifier(
        evm: &mut TestEvm,
        vk: Vec<u8>,
        message: Vec<u8>,
        sig: Vec<u8>,
    ) -> Result<ExecResultAndState<ExecutionResult<HaltReason>>, EVMError<std::convert::Infallible>>
    {
        evm.transact_raw(TxEnv {
            caller: TEST_USER_ADDRESS,
            kind: TxKind::Call(PQ_ADDRESS),
            data: IPQ::verifySlhDsaSha2128sCall {
                vk: vk.into(),
                message: message.into(),
                sig: sig.into(),
            }
            .abi_encode()
            .into(),
            ..Default::default()
        })
    }

    /// Assert transaction succeeded and extract boolean result
    fn assert_success_and_decode(
        result: &Result<
            ExecResultAndState<ExecutionResult<HaltReason>>,
            EVMError<std::convert::Infallible>,
        >,
    ) -> bool {
        assert!(result.is_ok(), "Transaction should succeed");
        let exec_result = result.as_ref().unwrap();

        if !exec_result.result.is_success() {
            match &exec_result.result {
                ExecutionResult::Revert { output, .. } => {
                    eprintln!("Reverted with output: {}", hex::encode(output));
                }
                ExecutionResult::Halt { reason, .. } => {
                    eprintln!("Halted with reason: {:?}", reason);
                }
                _ => {}
            }
        }

        assert!(
            exec_result.result.is_success(),
            "Transaction should be successful"
        );

        IPQ::verifySlhDsaSha2128sCall::abi_decode_returns(
            exec_result
                .result
                .output()
                .expect("transaction result should have output"),
        )
        .expect("Should decode return value")
    }

    /// Assert transaction completed but failed (revert/halt)
    fn assert_failure(
        result: &Result<
            ExecResultAndState<ExecutionResult<HaltReason>>,
            EVMError<std::convert::Infallible>,
        >,
        reason: &str,
    ) {
        assert!(result.is_ok(), "Transaction should complete (not panic)");
        let exec_result = result.as_ref().unwrap();
        assert!(
            !exec_result.result.is_success(),
            "Transaction should fail: {}",
            reason
        );
    }

    #[test]
    fn test_verify_valid_slh_dsa_sha2_128s_signature() {
        use slh_dsa::signature::{Keypair, Signer};
        use slh_dsa::SigningKey;

        let mut evm = create_test_evm();

        // Generate keypair from seeds for deterministic testing
        // SLH-DSA requires 3 seeds: sk_seed, sk_prf, and pk_seed (each 16 bytes for SHA2-128s)
        let sk_seed = [1u8; 16];
        let sk_prf = [2u8; 16];
        let pk_seed = [3u8; 16];
        let signing_key =
            SigningKey::<slh_dsa::Sha2_128s>::slh_keygen_internal(&sk_seed, &sk_prf, &pk_seed);
        let verifying_key = signing_key.verifying_key();

        // Sign a message (note: SLH-DSA sign() is deterministic)
        let msg = b"Hello quantum-resistant world";
        let signature = signing_key.sign(msg);

        let result = transact_slh_dsa_verifier(
            &mut evm,
            verifying_key.to_bytes().to_vec(),
            msg.to_vec(),
            signature.to_bytes().to_vec(),
        );

        let is_valid = assert_success_and_decode(&result);
        assert!(is_valid, "Valid SLH-DSA signature should verify");
    }

    #[test]
    fn test_slh_dsa_sha2_128s_gas_cost() {
        use slh_dsa::signature::{Keypair, Signer};
        use slh_dsa::SigningKey;

        let mut evm = create_test_evm();

        let sk_seed = [1u8; 16];
        let sk_prf = [2u8; 16];
        let pk_seed = [3u8; 16];
        let signing_key =
            SigningKey::<slh_dsa::Sha2_128s>::slh_keygen_internal(&sk_seed, &sk_prf, &pk_seed);
        let verifying_key = signing_key.verifying_key();

        // Test with 32-byte message
        let msg = [0xBB; 32];
        let signature = signing_key.sign(&msg);

        let result = transact_slh_dsa_verifier(
            &mut evm,
            verifying_key.to_bytes().to_vec(),
            msg.to_vec(),
            signature.to_bytes().to_vec(),
        );

        assert!(result.is_ok(), "Transaction should succeed");
        let exec_result = result.as_ref().unwrap();

        // Our precompile cost: 230,000 (base) + 1 word * 6 (message) = 230,006
        // Plus EVM calldata cost for large signature (7856 bytes)
        let actual_gas = exec_result.result.tx_gas_used();

        // SLH-DSA has largest signatures, expect ~370-390K total gas
        assert!(
            actual_gas > 350_000 && actual_gas < 400_000,
            "Gas cost should be ~380K (precompile + large calldata): got {}",
            actual_gas
        );

        println!(
            "SLH-DSA-SHA2-128s total gas (32-byte msg): {} gas",
            actual_gas
        );
    }

    #[test]
    fn test_verify_invalid_slh_dsa_sha2_128s_signature() {
        use slh_dsa::signature::{Keypair, Signer};
        use slh_dsa::SigningKey;

        let mut evm = create_test_evm();

        // Generate keypair from seeds for deterministic testing
        let sk_seed = [1u8; 16];
        let sk_prf = [2u8; 16];
        let pk_seed = [3u8; 16];
        let signing_key =
            SigningKey::<slh_dsa::Sha2_128s>::slh_keygen_internal(&sk_seed, &sk_prf, &pk_seed);
        let verifying_key = signing_key.verifying_key();

        // Sign one message, verify with different message
        let msg1 = b"Hello quantum-resistant world";
        let msg2 = b"Hello quantum-resistant world!";
        let signature = signing_key.sign(msg1);

        let result = transact_slh_dsa_verifier(
            &mut evm,
            verifying_key.to_bytes().to_vec(),
            msg2.to_vec(),
            signature.to_bytes().to_vec(),
        );

        let is_valid = assert_success_and_decode(&result);
        assert!(!is_valid, "Invalid SLH-DSA signature should not verify");
    }

    #[test]
    fn test_slh_dsa_malformed_input_handling() {
        let mut evm = create_test_evm();

        // Test cases: (vk_len, sig_len, description)
        let test_cases = [
            (10, 7856, "verifying key too short"),
            (32, 100, "signature too short"),
            (100, 7856, "verifying key too long"),
            (32, 10000, "signature too long"),
        ];

        for (vk_len, sig_len, desc) in test_cases {
            let result = transact_slh_dsa_verifier(
                &mut evm,
                vec![0u8; vk_len],
                b"test".to_vec(),
                vec![0u8; sig_len],
            );
            assert_failure(&result, desc);
        }
    }
}
