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

use alloy_primitives::{address, Address, Bytes};
use alloy_sol_types::{sol, SolValue};
use revm::precompile::{PrecompileError, PrecompileHalt, PrecompileOutput};
use revm_interpreter::gas::KECCAK256WORD;
use revm_interpreter::Gas;
use slh_dsa::{signature::Verifier, Sha2_128s, Signature, VerifyingKey as SlhDsaVerifyingKey};

/// PQ precompile address — SLH-DSA-SHA2-128s signature verifier (Zero6-gated).
pub const PQ_ADDRESS: Address = address!("1800000000000000000000000000000000000004");

/// Base gas for SLH-DSA-SHA2-128s verification.
///
/// Conservative relative to the SHA-256 precompile's per-word work anchor. See
/// `crates/precompiles/benches/pq.rs` for the benchmark context comparing this
/// price against SLH-DSA-SHA2-128s verification and 64-byte SHA-256 / KECCAK256
/// work.
pub const VERIFY_BASE_GAS: u64 = 230_000;

/// Dynamic gas cost per 32-byte word of message input.
///
/// SLH-DSA-SHA2-128s hashes the message once via `H_msg` (SHA-256 + MGF1).
/// This is comparable to KECCAK256, so we use the same per-word rate.
pub const GAS_PER_MSG_WORD: u64 = KECCAK256WORD;

pub const EARLY_REVERT_GAS: u64 = 200;
const VK_LEN: usize = 32;
const SIG_LEN: usize = 7856;

sol! {
    /// Experimental PQ Signature Verifier precompile interface.
    interface IPQ {
        /// Verify an SLH-DSA-SHA2-128s signature.
        ///
        /// Since PQ signatures are still very new, we recommend not to solely
        /// rely on them for authentication, but pair them with classical
        /// signatures.
        ///
        /// Gas cost: 230,000 base + 6 per 32-byte word of message (same as KECCAK256)
        function verifySlhDsaSha2128s(bytes calldata vk, bytes calldata message, bytes calldata sig) external returns (bool isValid);
    }
}

fn revert_message_to_bytes(msg: &str) -> Bytes {
    const REVERT_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];
    let encoded = msg.abi_encode();
    let mut result = Vec::with_capacity(REVERT_SELECTOR.len().saturating_add(encoded.len()));
    result.extend_from_slice(&REVERT_SELECTOR);
    result.extend_from_slice(&encoded);
    Bytes::from(result)
}

fn early_revert(
    gas_counter: &mut Gas,
    reservoir: u64,
    msg: &str,
) -> Result<PrecompileOutput, PrecompileError> {
    if !gas_counter.record_regular_cost(EARLY_REVERT_GAS) {
        return Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir));
    }
    Ok(PrecompileOutput::revert(
        gas_counter.used(),
        revert_message_to_bytes(msg),
        reservoir,
    ))
}

/// Executes the PQ (SLH-DSA-SHA2-128s) precompile.
///
/// Early-path failures (short input, wrong selector, ABI decode error) charge a 200-gas
/// penalty and return OOG if the caller has insufficient gas — preventing free probing.
pub fn run_pq_precompile(
    gas: u64,
    data: &[u8],
    reservoir: u64,
) -> Result<PrecompileOutput, PrecompileError> {
    use alloy_sol_types::SolCall;

    let mut gas_counter = Gas::new(gas);

    if data.len() < 4 {
        return early_revert(&mut gas_counter, reservoir, "Input too short");
    }

    let selector = [data[0], data[1], data[2], data[3]];
    if selector != IPQ::verifySlhDsaSha2128sCall::SELECTOR {
        return early_revert(&mut gas_counter, reservoir, "Invalid selector");
    }

    let args = match IPQ::verifySlhDsaSha2128sCall::abi_decode_raw_validate(&data[4..]) {
        Ok(args) => args,
        Err(_) => return early_revert(&mut gas_counter, reservoir, "Execution reverted"),
    };

    if !gas_counter.record_regular_cost(VERIFY_BASE_GAS) {
        return Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir));
    }

    // GAS_PER_MSG_WORD (6) < 32, so the product cannot exceed u64::MAX
    #[allow(clippy::arithmetic_side_effects)]
    let msg_word_gas = (args.message.len() as u64).div_ceil(32) * GAS_PER_MSG_WORD;
    if !gas_counter.record_regular_cost(msg_word_gas) {
        return Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir));
    }

    if args.vk.len() != VK_LEN {
        return Ok(PrecompileOutput::revert(
            gas_counter.used(),
            revert_message_to_bytes("Invalid verifying key length"),
            reservoir,
        ));
    }
    if args.sig.len() != SIG_LEN {
        return Ok(PrecompileOutput::revert(
            gas_counter.used(),
            revert_message_to_bytes("Invalid signature length"),
            reservoir,
        ));
    }

    let verifying_key = match SlhDsaVerifyingKey::<Sha2_128s>::try_from(args.vk.as_ref()) {
        Ok(vk) => vk,
        Err(_) => {
            return Ok(PrecompileOutput::revert(
                gas_counter.used(),
                revert_message_to_bytes("Failed to parse verifying key"),
                reservoir,
            ));
        }
    };
    let signature = match Signature::<Sha2_128s>::try_from(args.sig.as_ref()) {
        Ok(sig) => sig,
        Err(_) => {
            return Ok(PrecompileOutput::revert(
                gas_counter.used(),
                revert_message_to_bytes("Failed to parse signature"),
                reservoir,
            ));
        }
    };

    let is_valid = verifying_key
        .verify(args.message.as_ref(), &signature)
        .is_ok();
    Ok(PrecompileOutput::new(
        gas_counter.used(),
        is_valid.abi_encode().into(),
        reservoir,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        run_pq_precompile, EARLY_REVERT_GAS, GAS_PER_MSG_WORD, IPQ, SIG_LEN, VERIFY_BASE_GAS,
        VK_LEN,
    };
    use alloy_sol_types::{SolCall, SolValue};
    use revm::precompile::PrecompileOutput;
    use slh_dsa::{
        signature::{Keypair, Signer},
        Sha2_128s, SigningKey,
    };

    fn make_keypair() -> SigningKey<Sha2_128s> {
        SigningKey::<Sha2_128s>::slh_keygen_internal(&[1u8; 16], &[2u8; 16], &[3u8; 16])
    }

    fn encode_call(vk: &[u8], message: &[u8], sig: &[u8]) -> Vec<u8> {
        IPQ::verifySlhDsaSha2128sCall {
            vk: vk.to_vec().into(),
            message: message.to_vec().into(),
            sig: sig.to_vec().into(),
        }
        .abi_encode()
    }

    fn decode_bool(output: &PrecompileOutput) -> bool {
        bool::abi_decode(&output.bytes).expect("output should be ABI-encoded bool")
    }

    #[test]
    fn valid_signature_returns_true() {
        let sk = make_keypair();
        let vk = sk.verifying_key();
        let msg = b"hello pq world";
        let sig = sk.sign(msg);

        let calldata = encode_call(&vk.to_bytes(), msg, &sig.to_bytes());
        let output = run_pq_precompile(u64::MAX, &calldata, 0).expect("should not fail");
        assert!(output.is_success());
        assert!(decode_bool(&output));
    }

    #[test]
    fn invalid_signature_returns_false() {
        let sk = make_keypair();
        let vk = sk.verifying_key();
        let sig = sk.sign(b"other message");

        let calldata = encode_call(&vk.to_bytes(), b"wrong message", &sig.to_bytes());
        let output = run_pq_precompile(u64::MAX, &calldata, 0).expect("should not fail");
        assert!(output.is_success());
        assert!(!decode_bool(&output));
    }

    #[test]
    fn short_input_early_revert_charges_penalty() {
        let output =
            run_pq_precompile(u64::MAX, &[0x01, 0x02], 0).expect("should be Ok(revert), not Err");
        assert!(output.is_revert());
        assert_eq!(output.gas_used, EARLY_REVERT_GAS);
    }

    #[test]
    fn oog_on_early_revert_when_gas_below_penalty() {
        let output = run_pq_precompile(EARLY_REVERT_GAS - 1, &[0x01, 0x02], 0)
            .expect("should be Ok(halt), not Err");
        assert!(output.is_halt());
    }

    #[test]
    fn wrong_selector_early_revert_charges_penalty() {
        let calldata = [0x00u8, 0x00, 0x00, 0x00];
        let output = run_pq_precompile(u64::MAX, &calldata, 0).expect("should be Ok(revert)");
        assert!(output.is_revert());
        assert_eq!(output.gas_used, EARLY_REVERT_GAS);
    }

    #[test]
    fn malformed_abi_payload_early_revert_charges_penalty() {
        let mut calldata = IPQ::verifySlhDsaSha2128sCall::SELECTOR.to_vec();
        calldata.extend_from_slice(&[0x00u8, 0x01]); // too short to ABI-decode
        let output = run_pq_precompile(u64::MAX, &calldata, 0).expect("should be Ok(revert)");
        assert!(output.is_revert());
        assert_eq!(output.gas_used, EARLY_REVERT_GAS);
    }

    #[test]
    fn oog_on_base_gas_returns_halt() {
        // Valid calldata structure but gas just below VERIFY_BASE_GAS — fails at base gas charge.
        let calldata = encode_call(&[0u8; VK_LEN], &[], &[0u8; SIG_LEN]);
        let output = run_pq_precompile(VERIFY_BASE_GAS - 1, &calldata, 0)
            .expect("should be Ok(halt), not Err");
        assert!(output.is_halt());
    }

    #[test]
    fn vk_wrong_length_reverts_after_base_gas() {
        let calldata = encode_call(&[0u8; 16], &[], &[0u8; SIG_LEN]);
        let output =
            run_pq_precompile(u64::MAX, &calldata, 0).expect("should be Ok(revert), not Err");
        assert!(output.is_revert());
        assert!(output.gas_used >= VERIFY_BASE_GAS);
    }

    #[test]
    fn sig_wrong_length_reverts_after_base_gas() {
        let calldata = encode_call(&[0u8; VK_LEN], &[], &[0u8; 100]);
        let output =
            run_pq_precompile(u64::MAX, &calldata, 0).expect("should be Ok(revert), not Err");
        assert!(output.is_revert());
        assert!(output.gas_used >= VERIFY_BASE_GAS);
    }

    #[test]
    fn gas_consumed_matches_formula_for_valid_call() {
        let sk = make_keypair();
        let vk = sk.verifying_key();
        let msg = [0u8; 64]; // 2 words
        let sig = sk.sign(&msg);

        let calldata = encode_call(&vk.to_bytes(), &msg, &sig.to_bytes());
        let output = run_pq_precompile(u64::MAX, &calldata, 0).expect("should not fail");
        // 2 words * GAS_PER_MSG_WORD
        #[allow(clippy::arithmetic_side_effects)]
        let expected = VERIFY_BASE_GAS + 2 * GAS_PER_MSG_WORD;
        assert_eq!(output.gas_used, expected);
    }
}
