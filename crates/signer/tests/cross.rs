// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

#![cfg(all(test, feature = "integration-remote-signer"))]

use arc_remote_signer::SigningProvider;
use arc_remote_signer::{RemoteSigningConfig, RemoteSigningError, RemoteSigningProvider};
use arc_signer::local::LocalSigningProvider;
use malachitebft_signing_ed25519::PrivateKey;

async fn remote_provider() -> Result<RemoteSigningProvider, RemoteSigningError> {
    let config = RemoteSigningConfig::default();
    RemoteSigningProvider::new(config).await
}

fn local_provider() -> LocalSigningProvider {
    let mut rng = rand::thread_rng();
    let private_key = PrivateKey::generate(&mut rng);
    LocalSigningProvider::new(private_key)
}

#[tokio::test]
async fn local_verify_remote_signature() {
    let local = local_provider();
    let remote = remote_provider().await.expect("Failed to create provider");
    let public_key = remote.public_key().await.expect("Failed to get public key");

    let message = b"test message for validation";

    // Sign the message
    let signature = remote.sign_bytes(message).await.unwrap();

    // Validate using the local provider
    let result = local
        .verify_signed_bytes(message, &signature, &public_key)
        .await
        .expect("Verification failed");

    assert!(
        result.is_valid(),
        "Local provider failed to verify remote signature"
    );
}

#[tokio::test]
async fn remote_verify_local_signature() {
    let local = local_provider();
    let remote = remote_provider().await.expect("Failed to create provider");
    let public_key = local.public_key();

    let message = b"another test message for validation";

    // Sign the message
    let signature = local.sign_bytes(message).await.unwrap();

    // Validate using the remote provider
    let result = remote
        .verify_signed_bytes(message, &signature, &public_key)
        .await
        .expect("Verification failed");

    assert!(
        result.is_valid(),
        "Remote provider failed to verify local signature"
    );
}
