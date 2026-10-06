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
use alloy_sol_types::{abi, sol, SolCall, SolType, SolValue};
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

/// The `(bytes, bytes, bytes)` parameter tuple of [`IPQ::verifySlhDsaSha2128sCall`].
type CallParams<'a> = <IPQ::verifySlhDsaSha2128sCall as SolCall>::Parameters<'a>;

/// Token form of [`CallParams`]: three `PackedSeqToken`s that borrow from the input.
type CallTokens<'a> = <IPQ::verifySlhDsaSha2128sCall as SolCall>::Token<'a>;

/// Gas for hashing `msg_len` message bytes, one KECCAK256-rate charge per 32-byte word.
///
/// `GAS_PER_MSG_WORD` (6) < 32, so `div_ceil(32) * GAS_PER_MSG_WORD` cannot exceed `u64::MAX`.
#[allow(clippy::arithmetic_side_effects)]
fn msg_word_gas(msg_len: usize) -> u64 {
    (msg_len as u64).div_ceil(32) * GAS_PER_MSG_WORD
}

/// Executes the PQ (SLH-DSA-SHA2-128s) precompile.
///
/// Early-path failures (short input, wrong selector, ABI decode error) charge a 200-gas
/// penalty and return OOG if the caller has insufficient gas — preventing free probing.
///
/// Base and message gas are reserved before the byte arguments are copied out of the
/// input, so the work a call performs is bounded by the gas it pays. Decoding is split to
/// allow this: `abi_decode_raw_validate` is tokenize, then `type_check`, then `detokenize`,
/// and only `detokenize` copies. The first two steps run before the charges; the copy runs
/// after them.
pub fn run_pq_precompile(
    gas: u64,
    data: &[u8],
    reservoir: u64,
) -> Result<PrecompileOutput, PrecompileError> {
    let mut gas_counter = Gas::new(gas);

    if data.len() < 4 {
        return early_revert(&mut gas_counter, reservoir, "Input too short");
    }

    let selector = [data[0], data[1], data[2], data[3]];
    if selector != IPQ::verifySlhDsaSha2128sCall::SELECTOR {
        return early_revert(&mut gas_counter, reservoir, "Invalid selector");
    }

    // Tokenize and type-check without copying: reads the offset and length words, bounds-checks
    // them against the body, and yields borrowed slices. O(1) regardless of input size.
    let tokens = match abi::decode_sequence::<CallTokens<'_>>(&data[4..])
        .and_then(|tokens| CallParams::<'_>::type_check(&tokens).map(|()| tokens))
    {
        Ok(tokens) => tokens,
        Err(_) => return early_revert(&mut gas_counter, reservoir, "Execution reverted"),
    };

    if !gas_counter.record_regular_cost(VERIFY_BASE_GAS) {
        return Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir));
    }

    if !gas_counter.record_regular_cost(msg_word_gas(tokens.1.as_slice().len())) {
        return Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir));
    }

    // Paid for: copy the byte arguments out of the input.
    let args = IPQ::verifySlhDsaSha2128sCall::new(CallParams::<'_>::detokenize(tokens));

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
        early_revert, revert_message_to_bytes, run_pq_precompile, EARLY_REVERT_GAS,
        GAS_PER_MSG_WORD, IPQ, SIG_LEN, VERIFY_BASE_GAS, VK_LEN,
    };
    use alloy_primitives::U256;
    use alloy_sol_types::{SolCall, SolValue};
    use proptest::prelude::*;
    use revm::precompile::{PrecompileError, PrecompileHalt, PrecompileOutput};
    use revm_interpreter::Gas;
    use slh_dsa::{
        signature::{Keypair, Signer, Verifier},
        Sha2_128s, Signature, SigningKey, VerifyingKey as SlhDsaVerifyingKey,
    };
    use std::sync::atomic::Ordering;
    use std::sync::LazyLock;

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

    // --- Gas is reserved before the byte arguments are copied ---

    /// Calldata whose three dynamic offsets alias one `shared_len`-byte array. The ABI decoder
    /// accepts it; `vk` and `sig` then fail their length checks, but only after base and
    /// message gas have been charged.
    fn aliased_calldata(shared_len: usize) -> Vec<u8> {
        let mut calldata = IPQ::verifySlhDsaSha2128sCall::SELECTOR.to_vec();
        for _ in 0..3 {
            calldata.extend_from_slice(&U256::from(0x60).to_be_bytes::<32>());
        }
        calldata.extend_from_slice(&U256::from(shared_len).to_be_bytes::<32>());
        let padded_len = shared_len.div_ceil(32).saturating_mul(32);
        calldata.resize(calldata.len().saturating_add(padded_len), 0);
        calldata
    }

    #[test]
    fn zero_gas_aliased_input_halts_out_of_gas() {
        let calldata = aliased_calldata(1 << 20);
        let output = run_pq_precompile(0, &calldata, 0).expect("Ok(halt)");
        assert!(output.is_halt());
        assert_eq!(output.halt_reason(), Some(&PrecompileHalt::OutOfGas));
    }

    #[test]
    fn underfunded_aliased_input_below_base_gas_halts_out_of_gas() {
        let calldata = aliased_calldata(1 << 20);
        let output =
            run_pq_precompile(VERIFY_BASE_GAS.saturating_sub(1), &calldata, 0).expect("Ok(halt)");
        assert!(output.is_halt());
        assert_eq!(output.halt_reason(), Some(&PrecompileHalt::OutOfGas));
    }

    /// An underfunded call must not copy the byte arguments. 64 zero-gas calls with a 1 MiB
    /// aliased body would copy 3 MiB each (192 MiB total) if the arguments were copied before
    /// gas is reserved; reserving first allocates nothing for the arguments. Other tests run
    /// concurrently, so the bound is loose (16 MiB) but still an order of magnitude below the
    /// copying figure.
    #[test]
    fn underfunded_aliased_input_is_not_copied() {
        const ITERATIONS: usize = 64;
        const BODY_LEN: usize = 1 << 20;
        const MAX_ALLOCATED_BYTES: usize = 16 << 20;

        let calldata = aliased_calldata(BODY_LEN);
        let before = alloc_counter::ALLOCATED_BYTES.load(Ordering::Relaxed);
        for _ in 0..ITERATIONS {
            let output = run_pq_precompile(0, &calldata, 0).expect("Ok(halt)");
            assert!(output.is_halt());
        }
        let allocated = alloc_counter::ALLOCATED_BYTES
            .load(Ordering::Relaxed)
            .saturating_sub(before);
        assert!(
            allocated < MAX_ALLOCATED_BYTES,
            "underfunded calls allocated {allocated} bytes; the byte arguments are being copied \
             before gas is reserved"
        );
    }

    /// Global allocator that counts requested bytes. Only compiled into this crate's test
    /// binary; every operation is delegated to `System` unchanged.
    mod alloc_counter {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::sync::atomic::{AtomicUsize, Ordering};

        pub(super) static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

        struct CountingAllocator;

        // SAFETY: every call is forwarded to `System` with the caller's arguments unchanged;
        // the counter is a relaxed atomic and never touches the allocation itself.
        unsafe impl GlobalAlloc for CountingAllocator {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
                // SAFETY: same contract as this function's caller.
                unsafe { System.alloc(layout) }
            }

            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                // SAFETY: same contract as this function's caller.
                unsafe { System.dealloc(ptr, layout) }
            }

            unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
                ALLOCATED_BYTES.fetch_add(new_size, Ordering::Relaxed);
                // SAFETY: same contract as this function's caller.
                unsafe { System.realloc(ptr, layout, new_size) }
            }
        }

        #[global_allocator]
        static GLOBAL: CountingAllocator = CountingAllocator;
    }

    /// Reference implementation that decodes the full input before charging gas. The
    /// differential test checks that `run_pq_precompile` returns the same output for every
    /// `(gas, input, reservoir)`, so reserving gas first changes no observable result.
    #[allow(clippy::arithmetic_side_effects)]
    fn legacy_run_pq_precompile(
        gas: u64,
        data: &[u8],
        reservoir: u64,
    ) -> Result<PrecompileOutput, PrecompileError> {
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

    /// A valid signature over a fixed message, signed once (SLH-DSA signing is slow).
    static VALID_CALLDATA: LazyLock<Vec<u8>> = LazyLock::new(|| {
        let sk = make_keypair();
        let msg = b"differential vector";
        let sig = sk.sign(msg);
        encode_call(&sk.verifying_key().to_bytes(), msg, &sig.to_bytes())
    });

    /// Canonical encodings with the exact `vk`/`sig` sizes over-represented so the length
    /// checks and the verify path are both reached.
    fn arb_canonical_body() -> impl Strategy<Value = Vec<u8>> {
        let vk_len = prop_oneof![3 => Just(VK_LEN), 1 => 0usize..=64];
        let msg_len = prop_oneof![1 => Just(0usize), 3 => 0usize..=4096];
        let sig_len = prop_oneof![3 => Just(SIG_LEN), 1 => 0usize..=8192];
        (vk_len, msg_len, sig_len).prop_map(|(vk, msg, sig)| {
            encode_call(&vec![0x11; vk], &vec![0x22; msg], &vec![0x33; sig])
        })
    }

    /// Every input shape the decoder and gas checks branch on: canonical, a real valid
    /// signature, aliased offsets, truncated bodies, corrupted offset words, random bodies
    /// behind the right selector, and fully random bytes.
    fn arb_input() -> impl Strategy<Value = Vec<u8>> {
        let selector = IPQ::verifySlhDsaSha2128sCall::SELECTOR;
        prop_oneof![
            3 => arb_canonical_body(),
            1 => Just(VALID_CALLDATA.clone()),
            2 => (0usize..=4096).prop_map(aliased_calldata),
            2 => (arb_canonical_body(), 0usize..=256).prop_map(|(mut body, cut)| {
                body.truncate(body.len().saturating_sub(cut));
                body
            }),
            2 => (arb_canonical_body(), 0usize..3, any::<u64>()).prop_map(
                |(mut body, word, value)| {
                    let start = 4usize.saturating_add(word.saturating_mul(32));
                    if let Some(slot) = body.get_mut(start..start.saturating_add(32)) {
                        slot.copy_from_slice(&U256::from(value).to_be_bytes::<32>());
                    }
                    body
                }
            ),
            1 => prop::collection::vec(any::<u8>(), 0..=512).prop_map(move |mut body| {
                let mut calldata = selector.to_vec();
                calldata.append(&mut body);
                calldata
            }),
            1 => prop::collection::vec(any::<u8>(), 0..=512),
        ]
    }

    /// Gas values clustered around every threshold the precompile checks.
    fn arb_gas() -> impl Strategy<Value = u64> {
        prop_oneof![
            1 => Just(0u64),
            2 => 0u64..=1_000,
            3 => VERIFY_BASE_GAS.saturating_sub(64)..=VERIFY_BASE_GAS.saturating_add(8_192),
            1 => any::<u64>(),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Reserve-first must be observably identical to the legacy decode-then-charge order
        /// for every input, gas, and reservoir: same status, bytes, gas used, and reservoir.
        #[test]
        fn reserve_first_matches_legacy_ordering(
            input in arb_input(),
            gas in arb_gas(),
            reservoir in any::<u64>(),
        ) {
            let current = run_pq_precompile(gas, &input, reservoir)
                .expect("pq precompile never returns Err");
            let legacy = legacy_run_pq_precompile(gas, &input, reservoir)
                .expect("pq precompile never returns Err");
            prop_assert_eq!(current, legacy);
        }
    }
}
