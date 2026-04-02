// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0

//! Generate test vectors for PQ signature verification tests
//!
//! This binary generates valid test vectors for all supported PQ signature schemes
//! and outputs them in JSON format for use in TypeScript tests.

use serde::Serialize;
use slh_dsa::{
    signature::{Keypair as SlhDsaKeypair, Signer as SlhDsaSigner},
    Sha2_128s, SigningKey as SlhDsaSigningKey,
};

#[derive(Serialize)]
struct TestVector {
    scheme: String,
    verifying_key: String,
    message: String,
    signature: String,
    is_valid: bool,
}

#[derive(Serialize)]
struct TestVectors {
    slh_dsa_sha2_128s: Vec<TestVector>,
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    format!(
        "0x{}",
        bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    )
}

fn generate_slh_dsa_sha2_vectors() -> Vec<TestVector> {
    let mut vectors = Vec::new();

    // Generate a valid keypair using internal keygen with deterministic seeds
    let sk_seed = [1u8; 16];
    let sk_prf = [2u8; 16];
    let pk_seed = [3u8; 16];
    let signing_key =
        SlhDsaSigningKey::<Sha2_128s>::slh_keygen_internal(&sk_seed, &sk_prf, &pk_seed);
    let verifying_key = signing_key.verifying_key();
    let public_key = verifying_key.to_bytes();

    // Test 1: Valid signature
    let msg1 = b"Hello, World!";
    let sig1 = signing_key.sign(msg1);
    vectors.push(TestVector {
        scheme: "SLH-DSA-SHA2-128s".to_string(),
        verifying_key: bytes_to_hex(&public_key),
        message: bytes_to_hex(msg1),
        signature: bytes_to_hex(&sig1.to_bytes()),
        is_valid: true,
    });

    // Test 2: Valid signature with empty message
    let msg2 = b"";
    let sig2 = signing_key.sign(msg2);
    vectors.push(TestVector {
        scheme: "SLH-DSA-SHA2-128s".to_string(),
        verifying_key: bytes_to_hex(&public_key),
        message: bytes_to_hex(msg2),
        signature: bytes_to_hex(&sig2.to_bytes()),
        is_valid: true,
    });

    // Test 3: Invalid signature (wrong message)
    let msg3 = b"Hello, World!";
    let msg3_wrong = b"Goodbye, World!";
    let sig3 = signing_key.sign(msg3);
    vectors.push(TestVector {
        scheme: "SLH-DSA-SHA2-128s".to_string(),
        verifying_key: bytes_to_hex(&public_key),
        message: bytes_to_hex(msg3_wrong),
        signature: bytes_to_hex(&sig3.to_bytes()),
        is_valid: false,
    });

    vectors
}

fn main() {
    let vectors = TestVectors {
        slh_dsa_sha2_128s: generate_slh_dsa_sha2_vectors(),
    };

    let json = serde_json::to_string_pretty(&vectors).expect("Failed to serialize test vectors");
    println!("{}", json);
}
