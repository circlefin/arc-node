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

//! Consensus-layer chain spec: per-fork activation conditions (by block height) per network.
//!
//! Used to branch logic for BLS commit certificates, ExecutionPayloadV4, etc., so that from a
//! given height all validators use the new behavior.

use core::fmt;
use std::str::FromStr;

use alloy_rlp::RlpEncodable;
use eyre::Context;
use thiserror::Error;

use crate::{BlockHash, Height, B256};

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

/// Activation condition for a consensus fork (by block height).
///
/// Consensus-layer forks activate by block height only. This keeps `fork_version_at(height)` a
/// pure function of the argument — required so any two nodes resolve the same fork version for
/// the same height regardless of wall-clock state. Timestamp-based activation would require
/// block-timestamp lookups at every sign/verify site to stay consensus-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkCondition {
    /// Active when block number >= height.
    Block(Height),
}

impl ForkCondition {
    /// Returns true if this fork is active at the given block height.
    pub fn active_at(&self, height: Height) -> bool {
        match self {
            ForkCondition::Block(h) => height >= *h,
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

/// A consensus-layer fork and the condition that activates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsensusFork {
    /// Fork version used for consensus-signature domain separation.
    pub version: ForkVersion,
    /// Activation condition for this fork.
    pub condition: ForkCondition,
}

/// Consensus-layer chain spec: holds an ordered fork history per network.
///
/// The schedule must include every fork from genesis onward. When adding a CL fork, append a new
/// entry instead of replacing the active version. That keeps historical signature verification
/// stable because `fork_version_at(old_height)` continues to resolve the fork version that was
/// active when the message was signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsensusSpec {
    /// Chain ID for this spec.
    pub chain_id: ChainId,
    /// Ordered fork history. Entries must be sorted by activation height, starting with genesis.
    pub forks: &'static [ConsensusFork],
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

    /// Returns the consensus-layer fork version active at the given block height.
    pub fn fork_version_at(&self, height: Height) -> ForkVersion {
        let mut active_version = GENESIS_FORK_VERSION;

        for fork in self.forks {
            if !fork.condition.active_at(height) {
                break;
            }

            active_version = fork.version;
        }

        active_version
    }

    /// Returns the condition for the next scheduled fork after the given height.
    pub fn next_fork_condition_after(&self, height: Height) -> Option<ForkCondition> {
        for fork in self.forks {
            if !fork.condition.active_at(height) {
                return Some(fork.condition);
            }
        }

        None
    }

    // Example fork condition check
    // /// Returns true if the BLS commit certificate fork is active at the given height.
    // pub fn is_bls_fork_activated(&self, height: Height) -> bool {
    //     self.bls_commit_certificate
    //         .is_some_and(|c| c.active_at(height))
    // }
}

impl From<ChainId> for ConsensusSpec {
    fn from(chain_id: ChainId) -> Self {
        ConsensusSpec::for_chain_id(chain_id)
    }
}

/// Validates that a fork history is well-formed. Panics in `const` context if:
/// - the slice is empty,
/// - the first entry is not genesis (height 0, `GENESIS_FORK_VERSION`),
/// - activation heights are not non-decreasing, or
/// - fork versions are not strictly increasing.
///
/// Intended for `const _: () = validate_fork_history(SPEC.forks);` so malformed
/// schedules fail the build rather than waiting for a test run.
// `i` starts at 1, only increments, and the loop exits at `i == forks.len() <= isize::MAX`,
// so `i - 1` is always `>= 0` and `i + 1` cannot overflow.
#[allow(clippy::arithmetic_side_effects)]
const fn validate_fork_history(forks: &[ConsensusFork]) {
    assert!(!forks.is_empty(), "fork history must not be empty");

    let ForkCondition::Block(genesis_height) = forks[0].condition;
    assert!(
        genesis_height.as_u64() == 0,
        "first fork must activate at height 0 (genesis)"
    );
    assert!(
        forks[0].version == GENESIS_FORK_VERSION,
        "first fork must use GENESIS_FORK_VERSION"
    );

    let mut i = 1;
    while i < forks.len() {
        let ForkCondition::Block(prev_h) = forks[i - 1].condition;
        let ForkCondition::Block(curr_h) = forks[i].condition;

        assert!(
            curr_h.as_u64() >= prev_h.as_u64(),
            "fork activation heights must be non-decreasing"
        );
        assert!(
            forks[i].version > forks[i - 1].version,
            "fork versions must be strictly increasing"
        );

        i += 1;
    }
}

/// Consensus-layer genesis fork history.
pub const GENESIS_FORKS: &[ConsensusFork] = &[ConsensusFork {
    version: GENESIS_FORK_VERSION,
    condition: ForkCondition::Block(Height::new(0)),
}];

/// Default / devnet consensus spec (genesis fork only).
pub const DEVNET: ConsensusSpec = ConsensusSpec {
    chain_id: ChainId::Devnet,
    forks: GENESIS_FORKS,
};

/// Testnet consensus spec (genesis fork only; append a fork when activation is scheduled).
pub const TESTNET: ConsensusSpec = ConsensusSpec {
    chain_id: ChainId::Testnet,
    forks: GENESIS_FORKS,
};

/// Mainnet consensus spec (genesis fork only; append a fork when activation is scheduled).
pub const MAINNET: ConsensusSpec = ConsensusSpec {
    chain_id: ChainId::Mainnet,
    forks: GENESIS_FORKS,
};

/// Localdev consensus spec (genesis fork only; append a fork when activation is scheduled).
pub const LOCALDEV: ConsensusSpec = ConsensusSpec {
    chain_id: ChainId::Localdev,
    forks: GENESIS_FORKS,
};

const _: () = validate_fork_history(MAINNET.forks);
const _: () = validate_fork_history(TESTNET.forks);
const _: () = validate_fork_history(DEVNET.forks);
const _: () = validate_fork_history(LOCALDEV.forks);

/// Error returned when the chain ID is not recognized.
#[derive(Debug, Error)]
#[error("Unknown chain ID {chain_id}; expected one of MAINNET (5042), TESTNET (5042002), DEVNET (5042001), LOCALDEV (1337)")]
pub struct UnknownChainId {
    pub chain_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u64) -> Height {
        Height::new(n)
    }

    #[test]
    fn fork_condition_block() {
        let cond = ForkCondition::Block(h(100));
        assert!(!cond.active_at(h(99)));
        assert!(cond.active_at(h(100)));
        assert!(cond.active_at(h(101)));
    }

    #[test]
    fn validate_fork_history_accepts_well_formed_schedule() {
        const FORKS: &[ConsensusFork] = &[
            ConsensusFork {
                version: GENESIS_FORK_VERSION,
                condition: ForkCondition::Block(Height::new(0)),
            },
            ConsensusFork {
                version: 1,
                condition: ForkCondition::Block(Height::new(100)),
            },
            ConsensusFork {
                version: 2,
                condition: ForkCondition::Block(Height::new(100)),
            },
        ];
        const _: () = validate_fork_history(FORKS);
        validate_fork_history(FORKS);
    }

    #[test]
    #[should_panic(expected = "fork history must not be empty")]
    fn validate_fork_history_rejects_empty() {
        validate_fork_history(&[]);
    }

    #[test]
    #[should_panic(expected = "first fork must activate at height 0")]
    fn validate_fork_history_rejects_non_zero_genesis_height() {
        validate_fork_history(&[ConsensusFork {
            version: GENESIS_FORK_VERSION,
            condition: ForkCondition::Block(Height::new(1)),
        }]);
    }

    #[test]
    #[should_panic(expected = "first fork must use GENESIS_FORK_VERSION")]
    fn validate_fork_history_rejects_non_genesis_first_version() {
        validate_fork_history(&[ConsensusFork {
            version: 1,
            condition: ForkCondition::Block(Height::new(0)),
        }]);
    }

    #[test]
    #[should_panic(expected = "fork activation heights must be non-decreasing")]
    fn validate_fork_history_rejects_decreasing_heights() {
        validate_fork_history(&[
            ConsensusFork {
                version: GENESIS_FORK_VERSION,
                condition: ForkCondition::Block(Height::new(0)),
            },
            ConsensusFork {
                version: 1,
                condition: ForkCondition::Block(Height::new(100)),
            },
            ConsensusFork {
                version: 2,
                condition: ForkCondition::Block(Height::new(50)),
            },
        ]);
    }

    #[test]
    #[should_panic(expected = "fork versions must be strictly increasing")]
    fn validate_fork_history_rejects_non_increasing_versions() {
        validate_fork_history(&[
            ConsensusFork {
                version: GENESIS_FORK_VERSION,
                condition: ForkCondition::Block(Height::new(0)),
            },
            ConsensusFork {
                version: GENESIS_FORK_VERSION,
                condition: ForkCondition::Block(Height::new(100)),
            },
        ]);
    }

    #[test]
    fn fork_version_at_returns_genesis_when_only_genesis_is_scheduled() {
        let spec = ConsensusSpec {
            chain_id: ChainId::Localdev,
            forks: GENESIS_FORKS,
        };
        assert_eq!(spec.fork_version_at(h(0)), GENESIS_FORK_VERSION);
        assert_eq!(spec.fork_version_at(h(1_000_000)), GENESIS_FORK_VERSION);
    }

    #[test]
    fn fork_version_at() {
        const FORKS: &[ConsensusFork] = &[
            ConsensusFork {
                version: 0,
                condition: ForkCondition::Block(Height::new(0)),
            },
            ConsensusFork {
                version: 1,
                condition: ForkCondition::Block(Height::new(100)),
            },
            ConsensusFork {
                version: 2,
                condition: ForkCondition::Block(Height::new(200)),
            },
        ];
        let spec = ConsensusSpec {
            chain_id: ChainId::Localdev,
            forks: FORKS,
        };

        assert_eq!(spec.fork_version_at(h(0)), 0);
        assert_eq!(spec.fork_version_at(h(99)), 0);
        assert_eq!(spec.fork_version_at(h(100)), 1);
        assert_eq!(spec.fork_version_at(h(199)), 1);
        assert_eq!(spec.fork_version_at(h(200)), 2);
        assert_eq!(spec.fork_version_at(h(1_000_000)), 2);
    }

    #[test]
    fn next_fork_condition_after() {
        const FORKS: &[ConsensusFork] = &[
            ConsensusFork {
                version: 0,
                condition: ForkCondition::Block(Height::new(0)),
            },
            ConsensusFork {
                version: 1,
                condition: ForkCondition::Block(Height::new(100)),
            },
            ConsensusFork {
                version: 2,
                condition: ForkCondition::Block(Height::new(200)),
            },
        ];
        let spec = ConsensusSpec {
            chain_id: ChainId::Localdev,
            forks: FORKS,
        };
        assert_eq!(
            spec.next_fork_condition_after(h(0)),
            Some(ForkCondition::Block(h(100)))
        );
        assert_eq!(
            spec.next_fork_condition_after(h(100)),
            Some(ForkCondition::Block(h(200)))
        );
        assert_eq!(spec.next_fork_condition_after(h(200)), None);
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
    //         forks: GENESIS_FORKS,
    //         // bls_commit_certificate: Some(ForkCondition::Block(h(50))),
    //     };
    //     assert!(!spec.is_bls_fork_activated(h(49)));
    //     assert!(spec.is_bls_fork_activated(h(50)));
    //     assert!(spec.is_bls_fork_activated(h(51)));
    //
    //     let spec_no_bls = ConsensusSpec::default();
    //     assert!(!spec_no_bls.is_bls_fork_activated(h(0)));
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
