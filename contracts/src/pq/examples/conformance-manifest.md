# Conformance vectors

These are the fixtures the precompile is tested against. They come from the `slh_dsa` Rust crate, which implements FIPS 205, under the Apache 2.0 license. The generator is `crates/precompiles/src/bin/generate_pq_test_vectors.rs`, the cached values live in `crates/precompiles/src/pq_test_vectors.rs`, and the end to end assertions are in `crates/execution-e2e/tests/e2e/pq_precompile.rs`.

The verifying key is a valid SLH-DSA-SHA2-128s key. `wrong_vk` is that key with the last byte flipped, a valid length holding a wrong value. The malformed preimage is 100 bytes, used for the bad length cases.

Each case and its expected result.

1. valid_signature. Key, message `Hello, World!`, signature over that message. Returns true.
2. valid_empty_message. Key, empty message, signature over the empty message. Returns true.
3. invalid_wrong_message. Key, message `Goodbye, World!`, signature made over `Hello, World!`. Returns false.
4. wrong_verifying_key_value. `wrong_vk`, message `Hello, World!`, signature over `Hello, World!`. Returns false.
5. bad_verifying_key_len. Malformed key length. Reverts.
6. bad_signature_len. Malformed signature length. Reverts.

A valid key over its own message returns true. A correct length value that does not match returns false. A wrong length input reverts rather than returning false, so a caller cannot read a malformed input as a clean negative.
