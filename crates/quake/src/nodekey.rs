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

//! Nodekey management for Reth P2P identity

use alloy_primitives::hex;
use color_eyre::eyre::{eyre, Result};
use indexmap::IndexMap;
use reth_network_peers::{pk2id, NodeRecord, PeerId};
use secp256k1::{SecretKey, SECP256K1};
use std::fs;
use std::net::IpAddr;
use std::path::Path;
use tracing::debug;

use crate::node::{IpAddress, NodeName};

/// Default P2P port for Reth
const RETH_P2P_PORT: u16 = 30303;

/// Ethereum Node URL to identify and connect to Ethereum nodes in the P2P network.
/// Format: enode://<node_id>@<ip_address>:<tcp_port>?discport=<udp_port>
///   where <node_id> is the public key of the node.
pub(crate) type Enode = String;

/// Nodekey data for a node, containing the private key and peer identity.
///
/// IP addresses are intentionally not stored here because a node may have
/// different IPs on different subnets. The correct IP for an enode URL
/// depends on which subnet the connecting peer shares with this node.
pub(crate) struct NodekeyData {
    /// secp256k1 secret key
    pub secret_key: SecretKey,
    /// Public key identity used in enode URLs
    pub peer_id: PeerId,
}

impl NodekeyData {
    /// Build an enode URL for the given IP address.
    ///
    /// The IP must be an address that the connecting peer can reach
    /// (i.e., on a subnet shared between the two nodes).
    pub fn enode_for_ip(&self, ip: &IpAddress) -> Result<Enode> {
        let address: IpAddr = ip
            .parse()
            .map_err(|e| eyre!("Invalid IP address '{ip}': {e}"))?;
        let record = NodeRecord {
            address,
            tcp_port: RETH_P2P_PORT,
            udp_port: RETH_P2P_PORT,
            id: self.peer_id,
        };
        Ok(record.to_string())
    }
}

/// Load or generate nodekeys for all nodes.
///
/// When `force` is false and a nodekey file already exists on disk, the existing key is loaded
/// so that the in-memory state matches what Reth will use at startup. When `force` is true or
/// no file exists, a fresh key is generated.
pub(crate) fn load_or_generate_nodekeys(
    node_names: &[NodeName],
    testnet_dir: &Path,
    force: bool,
) -> Result<IndexMap<NodeName, NodekeyData>> {
    let mut nodekeys = IndexMap::new();

    for name in node_names {
        let nodekey_path = testnet_dir.join(name).join("reth").join("nodekey");
        let (secret_key, peer_id) = if !force && nodekey_path.exists() {
            let sk = load_nodekey(&nodekey_path)?;
            let pid = pk2id(&sk.public_key(SECP256K1));
            debug!(node=%name, "Loaded existing nodekey from disk");
            (sk, pid)
        } else {
            let pair = generate_nodekey();
            debug!(node=%name, "Generated new nodekey");
            pair
        };

        debug!(node=%name, peer_id=%peer_id, "Nodekey ready");
        nodekeys.insert(
            name.clone(),
            NodekeyData {
                secret_key,
                peer_id,
            },
        );
    }

    Ok(nodekeys)
}

/// Generate a secp256k1 private key (nodekey) and derive the PeerId (public key).
///
/// Returns (secret_key, peer_id) where:
/// - secret_key: secp256k1 SecretKey to write to the nodekey file
/// - peer_id: PeerId (B512) representing the public key for enode URLs
fn generate_nodekey() -> (SecretKey, PeerId) {
    let secret_key = SecretKey::new(&mut rand::thread_rng());
    let peer_id = pk2id(&secret_key.public_key(SECP256K1));
    (secret_key, peer_id)
}

/// Load a nodekey from an existing file on disk.
///
/// Expects the file to contain a 64-character hex string (no 0x prefix) encoding
/// a 32-byte secp256k1 private key — the same format Reth writes and reads.
fn load_nodekey(path: &Path) -> Result<SecretKey> {
    let hex_key =
        fs::read_to_string(path).map_err(|e| eyre!("Failed to read nodekey from {path:?}: {e}"))?;
    let bytes = hex::decode(hex_key.trim())
        .map_err(|e| eyre!("Invalid hex in nodekey file {path:?}: {e}"))?;
    SecretKey::from_slice(&bytes)
        .map_err(|e| eyre!("Invalid secp256k1 key in nodekey file {path:?}: {e}"))
}

/// Write nodekey files to each node's Reth data directory.
///
/// The nodekey file contains the hex-encoded 32-byte private key that Reth uses for P2P identity.
/// Reth expects the key as a 64-character hex string (no 0x prefix).
pub(crate) fn write_nodekey_files(
    testnet_dir: &Path,
    nodekeys: &IndexMap<NodeName, NodekeyData>,
    force: bool,
) -> Result<()> {
    for (name, data) in nodekeys {
        let nodekey_path = testnet_dir.join(name).join("reth").join("nodekey");

        if !force && nodekey_path.exists() {
            debug!("⏭️ Skipping writing nodekey for {name}");
            continue;
        }

        // Create parent directory if it doesn't exist
        if let Some(parent) = nodekey_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write hex-encoded private key (Reth expects 64 hex chars, no 0x prefix)
        let hex_key = hex::encode(data.secret_key.secret_bytes());
        fs::write(&nodekey_path, hex_key)?;
        debug!(node=%name, path=%nodekey_path.display(), "Generated nodekey");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_node_names(count: usize) -> Vec<NodeName> {
        (0..count).map(|i| format!("node-{i}")).collect()
    }

    #[test]
    fn load_or_generate_creates_fresh_keys_when_no_files_exist() {
        let dir = tempdir().unwrap();
        let names = test_node_names(2);

        let keys = load_or_generate_nodekeys(&names, dir.path(), false).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains_key("node-0"));
        assert!(keys.contains_key("node-1"));
    }

    #[test]
    fn load_or_generate_loads_existing_keys_when_not_forced() {
        let dir = tempdir().unwrap();
        let names = test_node_names(1);

        // First run: generate + write
        let first = load_or_generate_nodekeys(&names, dir.path(), false).unwrap();
        write_nodekey_files(dir.path(), &first, false).unwrap();

        // Second run without force: must load the same key
        let second = load_or_generate_nodekeys(&names, dir.path(), false).unwrap();

        assert_eq!(
            first["node-0"].secret_key.secret_bytes(),
            second["node-0"].secret_key.secret_bytes(),
        );
        assert_eq!(first["node-0"].peer_id, second["node-0"].peer_id);
    }

    #[test]
    fn load_or_generate_regenerates_keys_when_forced() {
        let dir = tempdir().unwrap();
        let names = test_node_names(1);

        let first = load_or_generate_nodekeys(&names, dir.path(), false).unwrap();
        write_nodekey_files(dir.path(), &first, false).unwrap();

        // force=true must ignore existing files and generate fresh keys
        let second = load_or_generate_nodekeys(&names, dir.path(), true).unwrap();

        // Probabilistically different (2^-256 collision chance)
        assert_ne!(
            first["node-0"].secret_key.secret_bytes(),
            second["node-0"].secret_key.secret_bytes(),
        );
    }

    #[test]
    fn write_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let names = test_node_names(1);

        let original = load_or_generate_nodekeys(&names, dir.path(), false).unwrap();
        write_nodekey_files(dir.path(), &original, true).unwrap();

        let path = dir.path().join("node-0").join("reth").join("nodekey");
        let loaded = load_nodekey(&path).unwrap();

        assert_eq!(
            original["node-0"].secret_key.secret_bytes(),
            loaded.secret_bytes(),
        );
    }

    #[test]
    fn enode_for_ip_produces_valid_enode_url() {
        let dir = tempdir().unwrap();
        let names = test_node_names(1);
        let keys = load_or_generate_nodekeys(&names, dir.path(), false).unwrap();
        let data = &keys["node-0"];

        let enode = data.enode_for_ip(&"172.19.2.0".to_string()).unwrap();
        assert!(enode.starts_with("enode://"));
        assert!(enode.contains("172.19.2.0"));
        assert!(enode.contains("30303"));
    }

    #[test]
    fn load_nodekey_rejects_invalid_hex() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad_nodekey");
        fs::write(&path, "not-valid-hex!").unwrap();

        assert!(load_nodekey(&path).is_err());
    }

    #[test]
    fn load_nodekey_rejects_wrong_length() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("short_nodekey");
        fs::write(&path, "abcd").unwrap();

        assert!(load_nodekey(&path).is_err());
    }
}
