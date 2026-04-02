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

use crate::signing::Signature;

/// SSZ encoding of a signature.
pub struct SszSignature(pub Signature);

impl ssz::Encode for SszSignature {
    fn is_ssz_fixed_len() -> bool {
        <[u8; 64] as ssz::Encode>::is_ssz_fixed_len()
    }

    fn ssz_bytes_len(&self) -> usize {
        <[u8; 64] as ssz::Encode>::ssz_bytes_len(&self.0.to_bytes())
    }

    fn ssz_append(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.0.to_bytes());
    }
}

impl ssz::Decode for SszSignature {
    fn is_ssz_fixed_len() -> bool {
        <[u8; 64] as ssz::Decode>::is_ssz_fixed_len()
    }

    fn from_ssz_bytes(bytes: &[u8]) -> Result<Self, ssz::DecodeError> {
        let bytes = <[u8; 64]>::from_ssz_bytes(bytes)?;
        let signature = Signature::from_bytes(bytes);
        Ok(SszSignature(signature))
    }
}
