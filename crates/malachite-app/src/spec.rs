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

//! Consensus-layer chain spec: activation conditions (height / time) per fork, per network.
//!
//! Used to branch logic for BLS commit certificates, ExecutionPayloadV4, etc., so that from a
//! given height or timestamp all validators use the new behavior.

use core::fmt;
use std::str::FromStr;

use alloy_rlp::RlpEncodable;
use eyre::Context;
use thiserror::Error;

use arc_consensus_types::{BlockHash, BlockTimestamp, Height, B256};

pub use arc_shared::chain_ids;

use arc_shared::chain_ids::{
    DEVNET_CHAIN_ID, LOCALDEV_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID,
};

/// Chain identifier for the consensus spec (mainnet, testnet, devnet, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainId {
    Mainnet,
    Testnet,
    Devnet,
    Localdev,
}

impl ChainId {
    /// Returns the numeric chain ID corresponding to this chain.
    pub const fn as_u64(self) -> u64 {
        match self {
            ChainId::Mainnet => MAINNET_CHAIN_ID,
            ChainId::Testnet => TESTNET_CHAIN_ID,
            ChainId::Devnet => DEVNET_CHAIN_ID,
            ChainId::Localdev => LOCALDEV_CHAIN_ID,
        }
    }
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_u64().fmt(f)
    }
}

impl FromStr for ChainId {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match parse_chain_id(s)? {
            MAINNET_CHAIN_ID => Ok(ChainId::Mainnet),
            TESTNET_CHAIN_ID => Ok(ChainId::Testnet),
            DEVNET_CHAIN_ID => Ok(ChainId::Devnet),
            LOCALDEV_CHAIN_ID => Ok(ChainId::Localdev),
            _ => Err(UnknownChainId {
                chain_id: s.to_string(),
            }
            .into()),
        }
    }
}

/// Parse chain ID from execution engine response (hex string e.g. "0x539" or decimal).
fn parse_chain_id(s: &str) -> eyre::Result<u64> {
    let s = s.trim();

    if let Some(hex_s) = s.strip_prefix("0x") {
        u64::from_str_radix(hex_s, 16).wrap_err("Invalid hex chain ID")
    } else {
        s.parse::<u64>().wrap_err("Invalid decimal chain ID")
    }
}

/// Consensus-layer fork version (0 = genesis, bump by 1 for each new fork).
pub type ForkVersion = u32;

/// Activation condition for a consensus fork (by block height, timestamp, or both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkCondition {
    /// Active when block number >= height.
    Block(Height),
    /// Active when block timestamp >= timestamp.
    Timestamp(BlockTimestamp),
    /// Active when both block number >= height and block timestamp >= timestamp.
    BlockAndTime {
        height: Height,
        timestamp: BlockTimestamp,
    },
}

impl ForkCondition {
    /// Returns true if this fork is active at the given block height and timestamp.
    pub fn active_at(&self, height: Height, timestamp: BlockTimestamp) -> bool {
        match self {
            ForkCondition::Block(h) => height >= *h,
            ForkCondition::Timestamp(t) => timestamp >= *t,
            ForkCondition::BlockAndTime {
                height: h,
                timestamp: t,
            } => height >= *h && timestamp >= *t,
        }
    }
}

/// A computed network identifier: `keccak256(rlp(chain_id, genesis_hash, cl_fork_version))`.
///
/// This uniquely identifies a network at a given point in time, taking into account the chain ID,
/// genesis block hash, and the active consensus-layer fork version. Peers can compare network IDs
/// to verify they are on the same network with the same fork schedule.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetworkId(B256);

impl NetworkId {
    /// Computes the network id as `keccak256(rlp(chain_id, genesis_hash, fork_version))`.
    ///
    /// The network id is height-dependent because the CL fork version can change at fork boundaries.
    pub fn new(chain_id: ChainId, genesis_hash: BlockHash, fork_version: ForkVersion) -> Self {
        use alloy_primitives::keccak256;

        /// RLP-encodable struct for computing the network id.
        #[derive(RlpEncodable)]
        struct NetworkIdInput {
            chain_id: u64,
            genesis_hash: BlockHash,
            fork_version: ForkVersion,
        }

        let input = NetworkIdInput {
            chain_id: chain_id.as_u64(),
            genesis_hash,
            fork_version,
        };

        Self(keccak256(alloy_rlp::encode(&input)))
    }
}

impl fmt::Debug for NetworkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for NetworkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use alloy_primitives::hex::ToHexExt;

        write!(f, "0x{}", &self.0.encode_hex()[..8])
    }
}

/// Genesis fork version (version 0).
pub const GENESIS_FORK_VERSION: ForkVersion = 0;

/// Consensus-layer chain spec: holds activation conditions for each fork per network.
///
/// `current_fork_version` is the active version before the next fork. When `next_fork_condition`
/// is met at (height, timestamp), the effective fork version becomes `current_fork_version + 1`.
/// When you add a new CL fork, set `current_fork_version` to the version we're on now and
/// `next_fork_condition` to the new fork's activation. Individual `is_*_fork_activated` functions
/// duplicate the condition checks for convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsensusSpec {
    /// Chain ID for this spec.
    pub chain_id: ChainId,
    /// Version we're on before the next fork (0 = genesis). When `next_fork_condition` is met we
    /// transition to `current_fork_version + 1`.
    pub current_fork_version: ForkVersion,
    /// When this condition is active, we transition to `current_fork_version + 1`. Before that we
    /// stay at `current_fork_version`. Set when scheduling the next fork.
    pub next_fork_condition: Option<ForkCondition>,
    // Example fork condition
    // /// From this block/height we use aggregated BLS in commit certificates (gossip + RPC).
    // pub bls_commit_certificate: Option<ForkCondition>,
}

impl ConsensusSpec {
    /// Returns the consensus spec for the given execution-layer chain ID.
    pub const fn for_chain_id(chain_id: ChainId) -> ConsensusSpec {
        match chain_id {
            ChainId::Mainnet => MAINNET,
            ChainId::Testnet => TESTNET,
            ChainId::Devnet => DEVNET,
            ChainId::Localdev => LOCALDEV,
        }
    }

    /// Returns the consensus-layer fork version active at the given block height and timestamp.
    /// When `next_fork_condition` is met, returns `current_fork_version + 1`; otherwise
    /// `current_fork_version`.
    pub fn fork_version_at(&self, height: Height, timestamp: BlockTimestamp) -> ForkVersion {
        match &self.next_fork_condition {
            None => self.current_fork_version,
            Some(cond) if cond.active_at(height, timestamp) => self.current_fork_version + 1,
            Some(_) => self.current_fork_version,
        }
    }

    /// Returns the condition for the next fork (for handshakes or display). When this is met we
    /// transition to `current_fork_version + 1`.
    pub fn next_fork_condition(&self) -> Option<ForkCondition> {
        self.next_fork_condition
    }

    // Example fork condition check
    // /// Returns true if the BLS commit certificate fork is active at the given height and timestamp.
    // pub fn is_bls_fork_activated(&self, height: Height, timestamp: BlockTimestamp) -> bool {
    //     self.bls_commit_certificate
    //         .is_some_and(|c| c.active_at(height, timestamp))
    // }
}

impl From<ChainId> for ConsensusSpec {
    fn from(chain_id: ChainId) -> Self {
        ConsensusSpec::for_chain_id(chain_id)
    }
}

/// Default / devnet consensus spec (genesis fork only).
pub const DEVNET: ConsensusSpec = ConsensusSpec {
    chain_id: ChainId::Devnet,
    current_fork_version: GENESIS_FORK_VERSION,
    next_fork_condition: None,
};

/// Testnet consensus spec (genesis fork only; set next_fork_condition when activation is scheduled).
pub const TESTNET: ConsensusSpec = ConsensusSpec {
    chain_id: ChainId::Testnet,
    current_fork_version: GENESIS_FORK_VERSION,
    next_fork_condition: None,
};

/// Mainnet consensus spec (genesis fork only; set next_fork_condition when activation is scheduled).
pub const MAINNET: ConsensusSpec = ConsensusSpec {
    chain_id: ChainId::Mainnet,
    current_fork_version: GENESIS_FORK_VERSION,
    next_fork_condition: None,
};

/// Localdev consensus spec (genesis fork only; set next_fork_condition when activation is scheduled).
pub const LOCALDEV: ConsensusSpec = ConsensusSpec {
    chain_id: ChainId::Localdev,
    current_fork_version: GENESIS_FORK_VERSION,
    next_fork_condition: None,
};

/// Error returned when the chain ID is not recognized.
#[derive(Debug, Error)]
#[error("Unknown chain ID {chain_id}; expected one of MAINNET (5042000), TESTNET (5042002), DEVNET (5042001), LOCALDEV (1337)")]
pub struct UnknownChainId {
    pub chain_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u64) -> Height {
        Height::new(n)
    }

    fn ts(secs: u64) -> BlockTimestamp {
        secs
    }

    #[test]
    fn fork_condition_block() {
        let cond = ForkCondition::Block(h(100));
        assert!(!cond.active_at(h(99), ts(0)));
        assert!(cond.active_at(h(100), ts(0)));
        assert!(cond.active_at(h(101), ts(0)));
    }

    #[test]
    fn fork_condition_timestamp() {
        let cond = ForkCondition::Timestamp(ts(1000));
        assert!(!cond.active_at(h(0), ts(999)));
        assert!(cond.active_at(h(0), ts(1000)));
        assert!(cond.active_at(h(0), ts(1001)));
    }

    #[test]
    fn fork_condition_block_and_time() {
        let cond = ForkCondition::BlockAndTime {
            height: h(50),
            timestamp: ts(500),
        };
        assert!(!cond.active_at(h(49), ts(500)));
        assert!(!cond.active_at(h(50), ts(499)));
        assert!(cond.active_at(h(50), ts(500)));
        assert!(cond.active_at(h(51), ts(501)));
    }

    #[test]
    fn fork_version_at() {
        // No next fork: always current_fork_version
        let spec = ConsensusSpec {
            chain_id: ChainId::Localdev,
            current_fork_version: 0,
            next_fork_condition: None,
        };
        assert_eq!(spec.fork_version_at(h(0), ts(0)), 0);

        // Next fork at block 100, current = 0: before 100 -> 0, at/after 100 -> 1
        let spec = ConsensusSpec {
            chain_id: ChainId::Localdev,
            current_fork_version: 0,
            next_fork_condition: Some(ForkCondition::Block(h(100))),
        };
        assert_eq!(spec.fork_version_at(h(0), ts(0)), 0);
        assert_eq!(spec.fork_version_at(h(99), ts(0)), 0);
        assert_eq!(spec.fork_version_at(h(100), ts(0)), 1);
        assert_eq!(spec.fork_version_at(h(200), ts(0)), 1);
    }

    #[test]
    fn next_fork_condition() {
        let spec = ConsensusSpec {
            chain_id: ChainId::Localdev,
            current_fork_version: 0,
            next_fork_condition: Some(ForkCondition::Block(h(100))),
        };
        assert_eq!(
            spec.next_fork_condition(),
            Some(ForkCondition::Block(h(100)))
        );
    }

    #[test]
    fn consensus_spec_for_chain_id_returns_correct_spec() {
        // Known chain IDs return the matching spec
        let spec = ConsensusSpec::for_chain_id(ChainId::Mainnet);
        assert_eq!(spec, MAINNET);

        let spec = ConsensusSpec::for_chain_id(ChainId::Devnet);
        assert_eq!(spec, DEVNET);

        let spec = ConsensusSpec::for_chain_id(ChainId::Testnet);
        assert_eq!(spec, TESTNET);

        let spec = ConsensusSpec::for_chain_id(ChainId::Localdev);
        assert_eq!(spec, LOCALDEV);
    }

    #[test]
    fn compute_network_id_deterministic() {
        let genesis_hash = B256::repeat_byte(0xAB);
        let id1 = NetworkId::new(ChainId::Localdev, genesis_hash, 0);
        let id2 = NetworkId::new(ChainId::Localdev, genesis_hash, 0);
        assert_eq!(id1, id2);
    }

    #[test]
    fn compute_network_id_changes_with_chain_id() {
        let genesis_hash = B256::repeat_byte(0x01);
        let id1 = NetworkId::new(ChainId::Localdev, genesis_hash, 0);
        let id2 = NetworkId::new(ChainId::Devnet, genesis_hash, 0);
        assert_ne!(id1, id2);
    }

    #[test]
    fn compute_network_id_changes_with_genesis_hash() {
        let id1 = NetworkId::new(ChainId::Localdev, B256::repeat_byte(0x01), 0);
        let id2 = NetworkId::new(ChainId::Localdev, B256::repeat_byte(0x02), 0);
        assert_ne!(id1, id2);
    }

    #[test]
    fn compute_network_id_changes_with_fork_version() {
        let genesis_hash = B256::repeat_byte(0x01);
        let id1 = NetworkId::new(ChainId::Localdev, genesis_hash, 0);
        let id2 = NetworkId::new(ChainId::Localdev, genesis_hash, 1);
        assert_ne!(id1, id2);
    }

    // #[test]
    // fn is_bls_fork_activated() {
    //     let spec = ConsensusSpec {
    //         chain_id: None,
    //         current_fork_version: 0,
    //         next_fork_condition: None,
    //         // bls_commit_certificate: Some(ForkCondition::Block(h(50))),
    //     };
    //     assert!(!spec.is_bls_fork_activated(h(49), ts(0)));
    //     assert!(spec.is_bls_fork_activated(h(50), ts(0)));
    //     assert!(spec.is_bls_fork_activated(h(51), ts(0)));
    //
    //     let spec_no_bls = ConsensusSpec::default();
    //     assert!(!spec_no_bls.is_bls_fork_activated(h(0), ts(0)));
    // }

    #[test]
    fn test_parse_chain_id() {
        assert_eq!(parse_chain_id("0x539").unwrap(), 1337);
        assert_eq!(parse_chain_id(" 0x539 ").unwrap(), 1337);
        assert_eq!(parse_chain_id("1337").unwrap(), 1337);
        assert_eq!(parse_chain_id(" 1337 ").unwrap(), 1337);

        assert!(parse_chain_id("0x").is_err());
        assert!(parse_chain_id("hello").is_err());
        assert!(parse_chain_id("0xG").is_err());
    }

    #[test]
    fn test_chain_id_from_str_unknown() {
        let err = "999".parse::<ChainId>().unwrap_err();
        assert!(err.to_string().contains("Unknown chain ID"));
    }

    #[test]
    fn test_chain_id_from_str_round_trip() {
        for chain in [
            ChainId::Mainnet,
            ChainId::Testnet,
            ChainId::Devnet,
            ChainId::Localdev,
        ] {
            let s = chain.to_string();
            let parsed: ChainId = s.parse().unwrap();
            assert_eq!(parsed, chain);
        }
    }
}
