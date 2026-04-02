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

use std::path::Path;

use alloy_rpc_types_engine::{Claims, JwtSecret};
use eyre::Ok;
use jsonwebtoken::{encode, get_current_timestamp, Algorithm, EncodingKey, Header};

/// Default algorithm used for JWT token signing.
const DEFAULT_ALGORITHM: Algorithm = Algorithm::HS256;

/// Contains the JWT secret and claims parameters.
pub struct Auth {
    key: EncodingKey,
}

impl Auth {
    /// Create a new `Auth` struct given the JWT secret.
    pub fn new(secret: JwtSecret) -> Self {
        Self {
            key: EncodingKey::from_secret(secret.as_bytes()),
        }
    }

    /// Create a new `Auth` struct given the path to the file containing the hex
    /// encoded jwt key.
    pub fn new_from_path(jwt_path: &Path) -> eyre::Result<Self> {
        Ok(Self::new(JwtSecret::from_file(jwt_path)?))
    }

    /// Generate a JWT token with `claims.iat` set to current time.
    pub fn generate_token(&self) -> eyre::Result<String> {
        let claims = self.generate_claims_at_timestamp();
        self.generate_token_with_claims(&claims)
    }

    /// Generate a JWT token with the given claims.
    fn generate_token_with_claims(&self, claims: &Claims) -> eyre::Result<String> {
        let header = Header::new(DEFAULT_ALGORITHM);
        Ok(encode(&header, claims, &self.key)?)
    }

    /// Generate a `Claims` struct with `iat` set to current time
    fn generate_claims_at_timestamp(&self) -> Claims {
        Claims {
            iat: get_current_timestamp(),
            exp: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let secret = JwtSecret::random();
        let auth = Auth::new(secret);
        let claims = auth.generate_claims_at_timestamp();
        let token = auth.generate_token_with_claims(&claims).unwrap();

        let res = secret.validate(token.as_str());
        assert!(res.is_ok());
    }
}
