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

//! P256 (secp256r1) precompile e2e tests for Arc Chain.
//!
//! Tests the EIP-7212 P256 signature verification precompile at address 0x100,
//! which is available starting from the Osaka hardfork.
//!
//! The precompile verifies ECDSA signatures on the secp256r1 (P-256) curve,
//! commonly used for passkey authentication.

use alloy_primitives::{address, bytes, Address, Bytes};
use arc_execution_e2e::{
    actions::{CallContract, ProduceBlocks},
    ArcSetup, ArcTestBuilder,
};
use eyre::Result;
use rstest::rstest;

/// P256 precompile address as defined in EIP-7212.
const P256_PRECOMPILE_ADDRESS: Address = address!("0000000000000000000000000000000000000100");

/// Expected output for valid signature: 32-byte big-endian 1.
const VALID_RESULT: Bytes =
    bytes!("0000000000000000000000000000000000000000000000000000000000000001");

/// Test P256 signature verification via eth_call.
///
/// Input format (160 bytes): hash (32) || r (32) || s (32) || x (32) || y (32)
///
/// Test vectors sourced from:
/// - revm: <https://github.com/bluealloy/revm/blob/main/crates/precompile/src/secp256r1.rs>
/// - p256-verifier: <https://github.com/daimo-eth/p256-verifier/tree/master/test-vectors>
#[rstest]
#[case::valid_signature(
    "p256_valid_sig",
    bytes!("4cee90eb86eaa050036147a12d49004b6b9c72bd725d39d4785011fe190f0b4da73bd4903f0ce3b639bbbf6e8e80d16931ff4bcf5993d58468e8fb19086e8cac36dbcd03009df8c59286b162af3bd7fcc0450c9aa81be5d10d312af6c66b1d604aebd3099c618202fcfe16ae7770b0c49ab5eadf74b754204a3bb6060e44eff37618b065f9832de4ca6ca971a7a1adc826d0f7c00181a5fb2ddf79ae00b4e10e"),
    VALID_RESULT,
)]
#[case::valid_signature_2(
    "p256_valid_sig_2",
    bytes!("3fec5769b5cf4e310a7d150508e82fb8e3eda1c2c94c61492d3bd8aea99e06c9e22466e928fdccef0de49e3503d2657d00494a00e764fd437bdafa05f5922b1fbbb77c6817ccf50748419477e843d5bac67e6a70e97dde5a57e0c983b777e1ad31a80482dadf89de6302b1988c82c29544c9c07bb910596158f6062517eb089a2f54c9a0f348752950094d3228d3b940258c75fe2a413cb70baa21dc2e352fc5"),
    VALID_RESULT,
)]
#[case::invalid_signature_wrong_hash(
    "p256_invalid_sig_wrong_hash",
    // First byte of hash changed from 4c to 3c
    bytes!("3cee90eb86eaa050036147a12d49004b6b9c72bd725d39d4785011fe190f0b4da73bd4903f0ce3b639bbbf6e8e80d16931ff4bcf5993d58468e8fb19086e8cac36dbcd03009df8c59286b162af3bd7fcc0450c9aa81be5d10d312af6c66b1d604aebd3099c618202fcfe16ae7770b0c49ab5eadf74b754204a3bb6060e44eff37618b065f9832de4ca6ca971a7a1adc826d0f7c00181a5fb2ddf79ae00b4e10e"),
    Bytes::new(),
)]
#[case::invalid_pubkey(
    "p256_invalid_pubkey",
    // Zeros for x and y coordinates
    bytes!("4cee90eb86eaa050036147a12d49004b6b9c72bd725d39d4785011fe190f0b4da73bd4903f0ce3b639bbbf6e8e80d16931ff4bcf5993d58468e8fb19086e8cac36dbcd03009df8c59286b162af3bd7fcc0450c9aa81be5d10d312af6c66b1d6000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
    Bytes::new(),
)]
#[case::malformed_input(
    "p256_malformed_input",
    // Only 64 bytes instead of required 160 bytes
    bytes!("4cee90eb86eaa050036147a12d49004b6b9c72bd725d39d4785011fe190f0b4da73bd4903f0ce3b639bbbf6e8e80d16931ff4bcf5993d58468e8fb19086e8cac"),
    Bytes::new(),
)]
#[tokio::test]
async fn test_p256_verification(
    #[case] label: &str,
    #[case] input: Bytes,
    #[case] expected: Bytes,
) -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(ProduceBlocks::new(1))
        .with_action(
            CallContract::new(label)
                .to(P256_PRECOMPILE_ADDRESS)
                .with_data(input)
                .expect_result(expected),
        )
        .run()
        .await
}
