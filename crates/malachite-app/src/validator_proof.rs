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

//! Validator proof creation and verification (ADR-006).
//!
//! A validator proof binds a consensus public key to a libp2p peer ID,
//! proving that the validator controls both keys. This prevents identity
//! spoofing attacks where an attacker could claim to be a validator on
//! the P2P network without possessing their consensus key.

use arc_consensus_types::codec::{network::NetCodec, Codec};
use arc_signer::ArcSigningProvider;
use bytes::Bytes;
use eyre::Result;
use malachitebft_app_channel::SigningProviderExt;
use tracing::info;

/// Create a signed validator proof binding the consensus public key to the P2P peer ID.
///
/// The proof is signed using the consensus signing provider and encoded for network
/// transmission using the network codec.
///
/// # Arguments
/// * `signing_provider` - The signing provider to sign the proof with
/// * `public_key_bytes` - The consensus public key bytes
/// * `peer_id_bytes` - The libp2p peer ID bytes
/// * `address` - The consensus address (for logging)
///
/// # Returns
/// The encoded validator proof bytes suitable for network transmission.
pub async fn create_validator_proof(
    signing_provider: &ArcSigningProvider,
    public_key_bytes: Vec<u8>,
    peer_id_bytes: Vec<u8>,
    address: &str,
) -> Result<Bytes> {
    let proof = signing_provider
        .sign_validator_proof(public_key_bytes, peer_id_bytes)
        .await
        .map_err(|e| eyre::eyre!("Failed to sign validator proof: {e}"))?;

    let proof_bytes = NetCodec
        .encode(&proof)
        .map_err(|e| eyre::eyre!("Failed to encode validator proof: {e}"))?;

    info!(address = %address, "Created validator proof for network identity");

    Ok(proof_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_consensus_types::ArcContext;
    use arc_signer::local::{LocalSigningProvider, PrivateKey};
    use malachitebft_app_channel::app::types::Keypair;
    use malachitebft_core_types::ValidatorProof;
    use rand::rngs::OsRng;

    #[tokio::test]
    async fn test_validator_proof_signing_and_encoding() {
        let private_key = PrivateKey::generate(OsRng);
        let local_signer = LocalSigningProvider::new(private_key.clone());
        let signing_provider = ArcSigningProvider::Local(local_signer);

        let p2p_keypair = Keypair::ed25519_from_bytes(private_key.inner().to_bytes()).unwrap();
        let peer_id = p2p_keypair.public().to_peer_id();

        let public_key_bytes = private_key.public_key().as_bytes().to_vec();
        let peer_id_bytes = peer_id.to_bytes();

        let proof: ValidatorProof<ArcContext> = signing_provider
            .sign_validator_proof(public_key_bytes.clone(), peer_id_bytes.clone())
            .await
            .expect("signing validator proof should succeed");

        assert_eq!(
            proof.public_key, public_key_bytes,
            "proof public_key should match input"
        );
        assert_eq!(
            proof.peer_id, peer_id_bytes,
            "proof peer_id should match input"
        );

        let encoded = NetCodec
            .encode(&proof)
            .expect("encoding proof should succeed");

        let decoded: ValidatorProof<ArcContext> = NetCodec
            .decode(encoded)
            .expect("decoding proof should succeed");

        assert_eq!(
            decoded.public_key, public_key_bytes,
            "decoded public_key should match original"
        );
        assert_eq!(
            decoded.peer_id, peer_id_bytes,
            "decoded peer_id should match original"
        );
        assert_eq!(
            decoded.signature.to_bytes(),
            proof.signature.to_bytes(),
            "decoded signature should match original"
        );

        let verification_result = signing_provider
            .verify_validator_proof(&decoded)
            .await
            .expect("verification should not error");
        assert!(
            verification_result.is_valid(),
            "validator proof signature should be valid"
        );
    }

    #[tokio::test]
    async fn test_validator_proof_tampered_peer_id_fails_verification() {
        let private_key = PrivateKey::generate(OsRng);
        let local_signer = LocalSigningProvider::new(private_key.clone());
        let signing_provider = ArcSigningProvider::Local(local_signer);

        let p2p_keypair = Keypair::ed25519_from_bytes(private_key.inner().to_bytes()).unwrap();
        let peer_id = p2p_keypair.public().to_peer_id();

        let public_key_bytes = private_key.public_key().as_bytes().to_vec();
        let peer_id_bytes = peer_id.to_bytes();

        let proof: ValidatorProof<ArcContext> = signing_provider
            .sign_validator_proof(public_key_bytes.clone(), peer_id_bytes.clone())
            .await
            .expect("signing should succeed");

        let mut tampered_peer_id = peer_id_bytes.clone();
        tampered_peer_id[0] ^= 0xFF;

        let tampered_proof =
            ValidatorProof::<ArcContext>::new(proof.public_key, tampered_peer_id, proof.signature);

        let result = signing_provider
            .verify_validator_proof(&tampered_proof)
            .await
            .expect("verification should not error");
        assert!(
            result.is_invalid(),
            "tampered peer_id should fail verification"
        );
    }

    #[tokio::test]
    async fn test_validator_proof_tampered_public_key_fails_verification() {
        let private_key = PrivateKey::generate(OsRng);
        let local_signer = LocalSigningProvider::new(private_key.clone());
        let signing_provider = ArcSigningProvider::Local(local_signer);

        let p2p_keypair = Keypair::ed25519_from_bytes(private_key.inner().to_bytes()).unwrap();
        let peer_id = p2p_keypair.public().to_peer_id();

        let public_key_bytes = private_key.public_key().as_bytes().to_vec();
        let peer_id_bytes = peer_id.to_bytes();

        let proof: ValidatorProof<ArcContext> = signing_provider
            .sign_validator_proof(public_key_bytes.clone(), peer_id_bytes.clone())
            .await
            .expect("signing should succeed");

        let different_private_key = PrivateKey::generate(OsRng);
        let different_public_key_bytes = different_private_key.public_key().as_bytes().to_vec();

        let tampered_proof = ValidatorProof::<ArcContext>::new(
            different_public_key_bytes,
            proof.peer_id,
            proof.signature,
        );

        let result = signing_provider
            .verify_validator_proof(&tampered_proof)
            .await
            .expect("verification should not error");
        assert!(
            result.is_invalid(),
            "proof with different public_key should fail verification"
        );
    }
}
