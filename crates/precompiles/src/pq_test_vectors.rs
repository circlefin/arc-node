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

//! Deterministic SLH-DSA-SHA2-128s test vectors for PQ precompile tests.
//!
//! Generates keypairs and signatures from fixed seeds so that the output is
//! identical across runs. Used by both unit tests (`crates/precompiles`) and
//! e2e tests (`crates/execution-e2e`).
//!
//! The same seeds power the `generate_pq_test_vectors` binary — if that
//! binary's output changes, this module's output changes in lockstep.

use slh_dsa::{
    signature::{Keypair as SlhDsaKeypair, Signer as SlhDsaSigner},
    Sha2_128s, SigningKey as SlhDsaSigningKey, VerifyingKey as SlhDsaVerifyingKey,
};

/// Fixed seeds matching the `generate_pq_test_vectors` binary.
const SK_SEED: [u8; 16] = [1u8; 16];
const SK_PRF: [u8; 16] = [2u8; 16];
const PK_SEED: [u8; 16] = [3u8; 16];

/// Messages used across test vectors.
pub const MSG_HELLO_WORLD: &[u8] = b"Hello, World!";
pub const MSG_EMPTY: &[u8] = b"";
pub const MSG_GOODBYE_WORLD: &[u8] = b"Goodbye, World!";

/// Pre-computed test vectors for SLH-DSA-SHA2-128s.
pub struct PqTestVectors {
    pub verifying_key: Vec<u8>,
    pub sig_hello_world: Vec<u8>,
    pub sig_empty_message: Vec<u8>,
}

impl PqTestVectors {
    /// Generate all test vectors from deterministic seeds.
    pub fn generate() -> Self {
        let signing_key =
            SlhDsaSigningKey::<Sha2_128s>::slh_keygen_internal(&SK_SEED, &SK_PRF, &PK_SEED);
        let vk: SlhDsaVerifyingKey<Sha2_128s> = signing_key.verifying_key().clone();

        let sig_hello = signing_key.sign(MSG_HELLO_WORLD);
        let sig_empty = signing_key.sign(MSG_EMPTY);

        Self {
            verifying_key: vk.to_bytes().to_vec(),
            sig_hello_world: sig_hello.to_bytes().to_vec(),
            sig_empty_message: sig_empty.to_bytes().to_vec(),
        }
    }

    /// Verifying key with last byte flipped — valid 32-byte encoding, wrong key.
    pub fn wrong_vk(&self) -> Vec<u8> {
        let mut vk = self.verifying_key.clone();
        let last = vk.last_mut().expect("vk is non-empty");
        *last ^= 0x01;
        vk
    }
}

/// Cache vectors per-process so repeated calls (e.g. parameterised rstest) don't
/// re-derive ~8 KB signatures each time.
pub fn cached_vectors() -> &'static PqTestVectors {
    use std::sync::LazyLock;
    static VECTORS: LazyLock<PqTestVectors> = LazyLock::new(PqTestVectors::generate);
    &VECTORS
}
