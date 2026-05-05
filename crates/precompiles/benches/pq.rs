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

use alloy_primitives::keccak256;
use criterion::{criterion_group, criterion_main, Criterion};
use sha2::{Digest, Sha256};
use slh_dsa::{
    signature::{Keypair, Signer, Verifier},
    Sha2_128s, Signature, SigningKey, VerifyingKey as SlhDsaVerifyingKey,
};
use std::hint::black_box;

const HASH_INPUT_BYTES: usize = 64;

// A single worst-case SLH-DSA-SHA2-128s verification performs ~4,000 SHA-256
// compression-block-equivalent calls with `slh-dsa`'s pk_seed midstate caching.
// The benchmark below compares the real verifier against one 64-byte SHA-256
// digest. On the short local PR calibration run, SLH-DSA measured ~87.1 us
// while ~4,000 * sha256_64_bytes measured ~901 us. The gas comparison is
// intentionally more conservative: 230,000 gas is greater than ~4,000 * 24 =
// ~96,000 pure SHA-256 work gas.

struct PqBenchmarkVector {
    verifying_key: SlhDsaVerifyingKey<Sha2_128s>,
    message: [u8; 32],
    signature: Signature<Sha2_128s>,
}

fn pq_benchmark_vector() -> PqBenchmarkVector {
    let sk_seed = [1u8; 16];
    let sk_prf = [2u8; 16];
    let pk_seed = [3u8; 16];
    let signing_key = SigningKey::<Sha2_128s>::slh_keygen_internal(&sk_seed, &sk_prf, &pk_seed);
    let message = [0xA5; 32];
    let signature = signing_key.sign(&message);

    PqBenchmarkVector {
        verifying_key: signing_key.verifying_key().clone(),
        message,
        signature,
    }
}

fn benchmark_pq(c: &mut Criterion) {
    let vector = pq_benchmark_vector();
    let hash_input = [0xA5; HASH_INPUT_BYTES];

    let mut group = c.benchmark_group("pq");

    group.bench_function("slh_dsa_sha2_128s_verify", |b| {
        b.iter(|| {
            let is_valid = black_box(&vector.verifying_key)
                .verify(
                    black_box(vector.message.as_slice()),
                    black_box(&vector.signature),
                )
                .is_ok();
            black_box(is_valid)
        });
    });

    group.bench_function("sha256_64_bytes", |b| {
        b.iter(|| {
            let digest = Sha256::digest(black_box(hash_input.as_slice()));
            black_box(digest)
        });
    });

    group.bench_function("keccak256_64_bytes", |b| {
        b.iter(|| {
            let digest = keccak256(black_box(hash_input.as_slice()));
            black_box(digest)
        });
    });

    group.finish();
}

criterion_group!(benches, benchmark_pq);
criterion_main!(benches);
