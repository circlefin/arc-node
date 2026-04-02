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

//! Post-quantum (SLH-DSA-SHA2-128s) precompile e2e tests for Arc Chain.
//!
//! Exercises the PQ verifier at [`PQ_ADDRESS`] via `eth_call`, matching the flow in
//! `tests/localdev/PQ.test.ts` but against the mock-engine `execution-e2e` harness.
//!
//! Test vectors are generated from deterministic seeds in
//! [`arc_precompiles::pq_test_vectors`] — the same seeds used by the
//! `generate_pq_test_vectors` binary.
//!
//! There is no case here for Zero6 disabled (PQ precompile unavailable): the harness uses
//! LOCAL_DEV, where Zero6 is active from genesis—same scope as `p256_precompile.rs`.

use alloy_primitives::{bytes, Bytes};
use alloy_sol_types::SolCall;
use arc_execution_e2e::{
    actions::{CallContract, ProduceBlocks},
    ArcSetup, ArcTestBuilder,
};
use arc_precompiles::{
    pq::{IPQ, PQ_ADDRESS},
    pq_test_vectors::{self, MSG_EMPTY, MSG_GOODBYE_WORLD, MSG_HELLO_WORLD},
};
use eyre::Result;
use rstest::rstest;

/// ABI-encoded `true` (single 32-byte word).
const RETURN_TRUE: Bytes =
    bytes!("0000000000000000000000000000000000000000000000000000000000000001");

/// ABI-encoded `false` (single 32-byte word).
const RETURN_FALSE: Bytes =
    bytes!("0000000000000000000000000000000000000000000000000000000000000000");

/// Wrong-length preimage used for malformed vk/sig cases (100 bytes).
const MALFORMED_100: [u8; 100] = [0u8; 100];

#[derive(Clone, Debug)]
enum PqExpected {
    ReturnTrue,
    ReturnFalse,
    Revert,
}

#[derive(Clone, Debug)]
struct PqVerifyVector {
    call_label: &'static str,
    msg: Bytes,
    vk: Bytes,
    sig: Bytes,
    expected: PqExpected,
}

fn build_vectors() -> Vec<(&'static str, PqVerifyVector)> {
    let tv = pq_test_vectors::cached_vectors();

    let vk = Bytes::from(tv.verifying_key.clone());
    let wrong_vk = Bytes::from(tv.wrong_vk());
    let sig_hello = Bytes::from(tv.sig_hello_world.clone());
    let sig_empty = Bytes::from(tv.sig_empty_message.clone());

    vec![
        (
            "valid_signature",
            PqVerifyVector {
                call_label: "pq_valid_sig",
                msg: Bytes::copy_from_slice(MSG_HELLO_WORLD),
                vk: vk.clone(),
                sig: sig_hello.clone(),
                expected: PqExpected::ReturnTrue,
            },
        ),
        (
            "valid_empty_message",
            PqVerifyVector {
                call_label: "pq_valid_empty_msg",
                msg: Bytes::copy_from_slice(MSG_EMPTY),
                vk: vk.clone(),
                sig: sig_empty,
                expected: PqExpected::ReturnTrue,
            },
        ),
        (
            "invalid_wrong_message",
            PqVerifyVector {
                call_label: "pq_invalid_sig",
                msg: Bytes::copy_from_slice(MSG_GOODBYE_WORLD),
                vk: vk.clone(),
                sig: sig_hello.clone(),
                expected: PqExpected::ReturnFalse,
            },
        ),
        (
            "wrong_verifying_key_value",
            PqVerifyVector {
                call_label: "pq_wrong_vk_value",
                msg: Bytes::copy_from_slice(MSG_HELLO_WORLD),
                vk: wrong_vk,
                sig: sig_hello.clone(),
                expected: PqExpected::ReturnFalse,
            },
        ),
        (
            "bad_verifying_key_len",
            PqVerifyVector {
                call_label: "pq_bad_vk_len",
                msg: Bytes::copy_from_slice(MSG_HELLO_WORLD),
                vk: Bytes::copy_from_slice(&MALFORMED_100),
                sig: sig_hello.clone(),
                expected: PqExpected::Revert,
            },
        ),
        (
            "bad_signature_len",
            PqVerifyVector {
                call_label: "pq_bad_sig_len",
                msg: Bytes::copy_from_slice(MSG_HELLO_WORLD),
                vk,
                sig: Bytes::copy_from_slice(&MALFORMED_100),
                expected: PqExpected::Revert,
            },
        ),
    ]
}

/// SLH-DSA-SHA2-128s precompile via `eth_call`. Each case carries message, vk, sig,
/// and expected outcome.
#[rstest]
#[case::valid_signature(0)]
#[case::valid_empty_message(1)]
#[case::invalid_wrong_message(2)]
#[case::wrong_verifying_key_value(3)]
#[case::bad_verifying_key_len(4)]
#[case::bad_signature_len(5)]
#[tokio::test]
async fn test_pq_precompile(#[case] index: usize) -> Result<()> {
    reth_tracing::init_test_tracing();

    let vectors = build_vectors();
    let (_, vector) = &vectors[index];

    let data: Bytes = IPQ::verifySlhDsaSha2128sCall {
        vk: vector.vk.clone(),
        msg: vector.msg.clone(),
        sig: vector.sig.clone(),
    }
    .abi_encode()
    .into();

    let call = CallContract::new(vector.call_label)
        .to(PQ_ADDRESS)
        .with_data(data);
    let call = match vector.expected {
        PqExpected::ReturnTrue => call.expect_result(RETURN_TRUE),
        PqExpected::ReturnFalse => call.expect_result(RETURN_FALSE),
        PqExpected::Revert => call.expect_revert(),
    };

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Advance one block after genesis — the usual e2e harness step so the mock node has a
        // progressed head before `eth_call`. This is not waiting on a hardfork height: default
        // `ArcSetup` uses LOCAL_DEV, where Arc forks (including Zero6 / PQ) are active at block 0
        // (`ARC_LOCALDEV_HARDFORKS` in `execution-config`).
        .with_action(ProduceBlocks::new(1))
        .with_action(call)
        .run()
        .await
}
