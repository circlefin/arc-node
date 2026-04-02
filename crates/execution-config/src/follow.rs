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

//! RPC node configuration for the Arc network.

use eyre::{eyre, Result};
use reth_network_peers::TrustedPeer;
use std::str::FromStr;

use arc_shared::chain_ids::{DEVNET_CHAIN_ID, LOCALDEV_CHAIN_ID, TESTNET_CHAIN_ID};

/// Returns the WebSocket URL for the given chain ID.
pub fn url_for_chain_id(chain_id: u64) -> Result<String> {
    let url = match chain_id {
        // FIXME: use production URLs
        TESTNET_CHAIN_ID => "wss://testnet.circle-chain.com:8546",
        LOCALDEV_CHAIN_ID => "ws://localhost:8546",
        DEVNET_CHAIN_ID => "wss://devnet.circle-chain.com:8546",
        _ => return Err(eyre!("Unsupported chain for follow mode: {}", chain_id)),
    };
    Ok(url.to_string())
}

/// Returns the trusted peers (enode URLs) for the given chain ID.
pub fn trusted_peers_for_chain_id(chain_id: u64) -> Result<Vec<TrustedPeer>> {
    // FIXME: use production URLs
    let peer_strs: Vec<&str> = match chain_id {
        TESTNET_CHAIN_ID => vec![
            "enode://placeholder@testnet-boot1.circle-chain.com:30303",
            "enode://placeholder@testnet-boot2.circle-chain.com:30303",
        ],
        LOCALDEV_CHAIN_ID => vec![],
        DEVNET_CHAIN_ID => vec![
            "enode://placeholder@devnet-boot1.circle-chain.com:30303",
            "enode://placeholder@devnet-boot2.circle-chain.com:30303",
        ],
        _ => return Err(eyre!("Unsupported chain for follow mode: {}", chain_id)),
    };

    // Parse each peer string into a TrustedPeer
    let mut peers = Vec::new();
    for peer_str in peer_strs {
        let peer = TrustedPeer::from_str(peer_str)
            .map_err(|e| eyre!("Failed to parse trusted peer '{}': {}", peer_str, e))?;
        peers.push(peer);
    }

    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_for_chain_id_localdev() {
        let url = url_for_chain_id(LOCALDEV_CHAIN_ID).unwrap();
        assert_eq!(url, "ws://localhost:8546");
    }

    #[test]
    fn test_url_for_chain_id_devnet() {
        let url = url_for_chain_id(DEVNET_CHAIN_ID).unwrap();
        assert_eq!(url, "wss://devnet.circle-chain.com:8546");
    }

    #[test]
    fn test_url_for_chain_id_testnet() {
        let url = url_for_chain_id(TESTNET_CHAIN_ID).unwrap();
        assert_eq!(url, "wss://testnet.circle-chain.com:8546");
    }

    #[test]
    fn test_url_for_chain_id_unsupported() {
        let result = url_for_chain_id(999);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Unsupported chain for follow mode: 999"
        );
    }

    #[test]
    fn test_trusted_peers_for_chain_id_localdev() {
        let peers = trusted_peers_for_chain_id(LOCALDEV_CHAIN_ID).unwrap();
        assert_eq!(peers.len(), 0);
    }

    #[test]
    fn test_trusted_peers_for_chain_id_devnet() {
        // Currently returns an error because the peer strings contain placeholder node IDs
        let result = trusted_peers_for_chain_id(DEVNET_CHAIN_ID);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to parse trusted peer"));
    }

    #[test]
    fn test_trusted_peers_for_chain_id_testnet() {
        // Currently returns an error because the peer strings contain placeholder node IDs
        let result = trusted_peers_for_chain_id(TESTNET_CHAIN_ID);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to parse trusted peer"));
    }

    #[test]
    fn test_trusted_peers_for_chain_id_unsupported() {
        let result = trusted_peers_for_chain_id(999);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Unsupported chain for follow mode: 999"
        );
    }
}
