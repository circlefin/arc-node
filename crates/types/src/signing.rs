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

pub use malachitebft_core_types::{SignedExtension, SignedProposal, SignedVote};
pub use malachitebft_signing::{
    Error as SigningError, Signer, VerificationResult, Verifier, VerifierExt,
};
pub use malachitebft_signing_ed25519::{Ed25519, PrivateKey, PublicKey, Signature};

use async_trait::async_trait;
use malachitebft_core_types::{Context, SigningScheme};

/// Combined signing and verification provider.
///
/// Extends [`Signer`] + [`Verifier`] with byte-level signing used to authenticate
/// streamed proposal parts (the Keccak256 hash of init + data parts). Upstream
/// removed raw byte signing from the signing traits to enforce domain separation;
/// Arc re-exposes it here because proposal-parts hashing is an Arc-local protocol.
#[async_trait]
pub trait SigningProvider<Ctx>: Signer<Ctx> + Verifier<Ctx>
where
    Ctx: Context,
{
    async fn sign_bytes(
        &self,
        bytes: &[u8],
    ) -> Result<<Ctx::SigningScheme as SigningScheme>::Signature, SigningError>;

    async fn verify_signed_bytes(
        &self,
        bytes: &[u8],
        signature: &<Ctx::SigningScheme as SigningScheme>::Signature,
        public_key: &<Ctx::SigningScheme as SigningScheme>::PublicKey,
    ) -> Result<VerificationResult, SigningError>;
}
