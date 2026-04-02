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

extern crate alloc;

use alloc::vec;
use alloy_primitives::U256;
use alloy_serde::OtherFields;
use once_cell::sync::Lazy as LazyLock;
use reth_chainspec::hardfork;
use reth_ethereum_forks::{ChainHardforks, EthereumHardfork, ForkCondition, Hardfork};

// ref: [OpHardfork](https://docs.rs/alloy-op-hardforks/latest/alloy_op_hardforks/enum.OpHardfork.html)
hardfork!(
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    ArcHardfork {
        Zero3, // v0.3 hardfork, align to Ethereum Prague
        Zero4, // v0.4 hardfork, align to Ethereum Prague
        Zero5, // v0.5 hardfork, align to Ethereum Prague
        #[default]
        Zero6, // v0.6 hardfork
    }
);

// define our extra genesis info (hardfork table)
// ref: [OpGenesisInfo](https://docs.rs/op-alloy-rpc-types/0.19.0/op_alloy_rpc_types/struct.OpGenesisInfo.html)
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArcGenesisInfo {
    /// v0.3 hardfork block
    pub zero_3_block: Option<u64>,
    /// v0.4 hardfork block
    pub zero_4_block: Option<u64>,
    /// v0.5 hardfork block
    pub zero_5_block: Option<u64>,
    /// v0.6 hardfork block
    pub zero_6_block: Option<u64>,
}

impl ArcGenesisInfo {
    /// Extract the Arc-specific genesis info from a genesis file.
    pub fn extract_from(others: &OtherFields) -> Option<Self> {
        Self::try_from(others).ok()
    }

    pub fn get_hardfork_conditions(&self) -> Vec<(ArcHardfork, ForkCondition)> {
        let mut hardforks = Vec::new();
        for (fork_block, hardfork) in [
            (self.zero_3_block, ArcHardfork::Zero3),
            (self.zero_4_block, ArcHardfork::Zero4),
            (self.zero_5_block, ArcHardfork::Zero5),
            (self.zero_6_block, ArcHardfork::Zero6),
        ] {
            if let Some(fork_block) = fork_block {
                hardforks.push((hardfork, ForkCondition::Block(fork_block)));
            }
        }
        hardforks
    }
}

impl TryFrom<&OtherFields> for ArcGenesisInfo {
    type Error = serde_json::Error;

    fn try_from(others: &OtherFields) -> Result<Self, Self::Error> {
        others.deserialize_as()
    }
}

/// Feature-flag style hardfork information for EVM level.
///
/// Each hardfork can be independently enabled without implying other hardforks.
/// This allows different networks to have different hardfork activation orders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArcHardforkFlags {
    zero3: bool,
    zero4: bool,
    zero5: bool,
    zero6: bool,
}

impl ArcHardforkFlags {
    /// All Arc hardfork variants. Update this when adding new hardforks.
    const ALL_HARDFORKS: &'static [ArcHardfork] = &[
        ArcHardfork::Zero3,
        ArcHardfork::Zero4,
        ArcHardfork::Zero5,
        ArcHardfork::Zero6,
    ];

    /// Check if a specific hardfork is active.
    pub fn is_active(&self, hardfork: ArcHardfork) -> bool {
        match hardfork {
            ArcHardfork::Zero3 => self.zero3,
            ArcHardfork::Zero4 => self.zero4,
            ArcHardfork::Zero5 => self.zero5,
            ArcHardfork::Zero6 => self.zero6,
        }
    }

    /// Set a specific hardfork flag.
    fn set(&mut self, hardfork: ArcHardfork, value: bool) {
        match hardfork {
            ArcHardfork::Zero3 => self.zero3 = value,
            ArcHardfork::Zero4 => self.zero4 = value,
            ArcHardfork::Zero5 => self.zero5 = value,
            ArcHardfork::Zero6 => self.zero6 = value,
        }
    }

    /// Create flags from chain hardforks at a given block height.
    pub fn from_chain_hardforks(hardforks: &ChainHardforks, block: u64) -> Self {
        let mut flags = Self::default();
        for &hf in Self::ALL_HARDFORKS {
            if hardforks.is_fork_active_at_block(hf, block) {
                flags.set(hf, true);
            }
        }
        flags
    }

    /// Create flags with specific hardforks enabled.
    #[cfg(any(feature = "test-utils", test))]
    pub fn with(hardforks: &[ArcHardfork]) -> Self {
        let mut flags = Self::default();
        for &hf in hardforks {
            flags.set(hf, true);
        }
        flags
    }

    /// Returns an iterator over all possible hardfork flag combinations (2^n for n hardforks).
    ///
    /// This is useful for exhaustive testing to ensure code works correctly
    /// regardless of which hardforks are active, including non-sequential
    /// activation (e.g., Zero4 without Zero3).
    #[cfg(any(feature = "test-utils", test))]
    pub fn all_combinations() -> impl Iterator<Item = Self> {
        let n = Self::ALL_HARDFORKS.len();
        // generate 2^n combinations
        (0..(1 << n)).map(move |bits| {
            let mut flags = Self::default();
            for (i, &hf) in Self::ALL_HARDFORKS.iter().enumerate() {
                if bits & (1 << i) != 0 {
                    flags.set(hf, true);
                }
            }
            flags
        })
    }
}

// Reference Ethereum forks
// - https://github.com/ethereum/execution-specs/blob/forks/osaka/README.md
// - https://github.com/paradigmxyz/reth/blob/91defb2f9c9522007436ba6f41098d73e41cc34c/crates/ethereum/hardforks/src/hardforks/dev.rs#L14
pub(crate) static BASE_FORKS: LazyLock<ChainHardforks> = LazyLock::new(|| {
    ChainHardforks::new(vec![
        (EthereumHardfork::Frontier.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Homestead.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::Tangerine.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::SpuriousDragon.boxed(),
            ForkCondition::Block(0),
        ),
        (EthereumHardfork::Byzantium.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::Constantinople.boxed(),
            ForkCondition::Block(0),
        ),
        (
            EthereumHardfork::Petersburg.boxed(),
            ForkCondition::Block(0),
        ),
        (EthereumHardfork::Istanbul.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::MuirGlacier.boxed(),
            ForkCondition::Block(0),
        ),
        (EthereumHardfork::Berlin.boxed(), ForkCondition::Block(0)),
        (EthereumHardfork::London.boxed(), ForkCondition::Block(0)),
        (
            EthereumHardfork::ArrowGlacier.boxed(),
            ForkCondition::Block(0),
        ),
        (
            EthereumHardfork::GrayGlacier.boxed(),
            ForkCondition::Block(0),
        ),
        (
            EthereumHardfork::Paris.boxed(),
            ForkCondition::TTD {
                activation_block_number: 0,
                fork_block: None,
                total_difficulty: U256::ZERO,
            },
        ),
        (
            EthereumHardfork::Shanghai.boxed(),
            ForkCondition::Timestamp(0),
        ),
        (
            EthereumHardfork::Cancun.boxed(),
            ForkCondition::Timestamp(0),
        ),
        (
            EthereumHardfork::Prague.boxed(),
            ForkCondition::Timestamp(0),
        ),
    ])
});

/// Arc Local Dev network (1337) hardforks.
pub static ARC_LOCALDEV_HARDFORKS: LazyLock<ChainHardforks> = LazyLock::new(|| {
    let mut forks = BASE_FORKS.clone();
    forks.insert(ArcHardfork::Zero3.boxed(), ForkCondition::Block(0));
    forks.insert(ArcHardfork::Zero4.boxed(), ForkCondition::Block(0));
    // Zero5 : Osaka — paired per convention above
    forks.insert(EthereumHardfork::Osaka.boxed(), ForkCondition::Timestamp(0));
    forks.insert(ArcHardfork::Zero5.boxed(), ForkCondition::Block(0));
    forks.insert(ArcHardfork::Zero6.boxed(), ForkCondition::Block(0));
    forks
});

/// Arc Devnet network (5042001) hardforks.
pub static ARC_DEVNET_HARDFORKS: LazyLock<ChainHardforks> = LazyLock::new(|| {
    let mut forks = BASE_FORKS.clone();
    forks.insert(
        ArcHardfork::Zero3.boxed(),
        ForkCondition::Block(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET),
    );
    forks.insert(
        ArcHardfork::Zero4.boxed(),
        ForkCondition::Block(ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET),
    );
    // TODO: When Zero5 is activated on devnet, add Osaka at the same activation point:
    // forks.insert(EthereumHardfork::Osaka.boxed(), ForkCondition::Timestamp(<zero5_timestamp>));
    forks
});

/// Arc Testnet network (5042002) hardforks.
pub static ARC_TESTNET_HARDFORKS: LazyLock<ChainHardforks> = LazyLock::new(|| {
    let mut forks = BASE_FORKS.clone();
    forks.insert(
        ArcHardfork::Zero3.boxed(),
        ForkCondition::Block(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET),
    );
    forks.insert(
        ArcHardfork::Zero4.boxed(),
        ForkCondition::Block(ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET),
    );
    // TODO: When Zero5 is activated on testnet, add Osaka at the same activation point:
    // forks.insert(EthereumHardfork::Osaka.boxed(), ForkCondition::Timestamp(<zero5_timestamp>));
    forks
});

/// Constants
/// Zero3
pub const ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET: u64 = 7437594;
pub const ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET: u64 = 11172019;
/// Zero4
pub const ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET: u64 = 19491165;
pub const ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET: u64 = 26148086;

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_genesis::Genesis;

    #[test]
    fn test_arc_hardfork_names() {
        assert_eq!(ArcHardfork::Zero3.name(), "Zero3");
        assert_eq!(ArcHardfork::Zero4.name(), "Zero4");
        assert_eq!(ArcHardfork::Zero5.name(), "Zero5");
        assert_eq!(ArcHardfork::Zero6.name(), "Zero6");
    }

    #[test]
    fn test_arc_hardfork_flags() {
        // Test default (no hardforks active)
        let flags = ArcHardforkFlags::default();
        assert!(!flags.is_active(ArcHardfork::Zero3));
        assert!(!flags.is_active(ArcHardfork::Zero4));
        assert!(!flags.is_active(ArcHardfork::Zero5));
        assert!(!flags.is_active(ArcHardfork::Zero6));

        // Test from chain hardforks - localdev has Zero3-Zero5 and PQC active at block 0
        let flags = ArcHardforkFlags::from_chain_hardforks(&ARC_LOCALDEV_HARDFORKS, 0);
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(flags.is_active(ArcHardfork::Zero4));
        assert!(flags.is_active(ArcHardfork::Zero5));
        assert!(flags.is_active(ArcHardfork::Zero6));

        // Test from chain hardforks - devnet has Zero3 and Zero4 active after their activation blocks
        let flags = ArcHardforkFlags::from_chain_hardforks(
            &ARC_DEVNET_HARDFORKS,
            ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET,
        );
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(flags.is_active(ArcHardfork::Zero4));
        assert!(!flags.is_active(ArcHardfork::Zero5));
        assert!(!flags.is_active(ArcHardfork::Zero6));

        // Test from chain hardforks - devnet before Zero4 activation
        let flags = ArcHardforkFlags::from_chain_hardforks(
            &ARC_DEVNET_HARDFORKS,
            ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET - 1,
        );
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(!flags.is_active(ArcHardfork::Zero4));

        // Test from chain hardforks - devnet before Zero3 activation
        let flags = ArcHardforkFlags::from_chain_hardforks(
            &ARC_DEVNET_HARDFORKS,
            ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET - 1,
        );
        assert!(!flags.is_active(ArcHardfork::Zero3));
        assert!(!flags.is_active(ArcHardfork::Zero4));

        // Test with() helper
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero3]);
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(!flags.is_active(ArcHardfork::Zero4));

        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero4]);
        assert!(!flags.is_active(ArcHardfork::Zero3));
        assert!(flags.is_active(ArcHardfork::Zero4));

        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero3, ArcHardfork::Zero4]);
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(flags.is_active(ArcHardfork::Zero4));

        let flags = ArcHardforkFlags::with(&[]);
        assert!(!flags.is_active(ArcHardfork::Zero3));
        assert!(!flags.is_active(ArcHardfork::Zero4));

        // Test all_combinations() helper - should yield 16 combinations (2^4)
        let combinations: Vec<_> = ArcHardforkFlags::all_combinations().collect();
        assert_eq!(combinations.len(), 16);

        // Verify some key combinations are present
        assert!(combinations.contains(&ArcHardforkFlags::with(&[])));
        assert!(combinations.contains(&ArcHardforkFlags::with(&[ArcHardfork::Zero3])));
        assert!(combinations.contains(&ArcHardforkFlags::with(&[ArcHardfork::Zero4])));
        assert!(combinations.contains(&ArcHardforkFlags::with(&[
            ArcHardfork::Zero3,
            ArcHardfork::Zero4
        ])));
        assert!(combinations.contains(&ArcHardforkFlags::with(&[
            ArcHardfork::Zero3,
            ArcHardfork::Zero4,
            ArcHardfork::Zero5,
            ArcHardfork::Zero6,
        ])));
    }

    #[test]
    fn test_parse_arc_hardfork_from_genesis() {
        let s = r#"{ "config": { "zero3Block": 123123, "zero4Block": 223881, "zero5Block": 323496, "zero6Block": 423000 } }"#;

        let genesis = serde_json::from_str::<Genesis>(s).expect("Failed to parse genesis");
        let info = ArcGenesisInfo::extract_from(&genesis.config.extra_fields)
            .expect("Failed to extract genesis info");
        assert_eq!(info.zero_3_block, Some(123123));
        assert_eq!(info.zero_4_block, Some(223881));
        assert_eq!(info.zero_5_block, Some(323496));
        assert_eq!(info.zero_6_block, Some(423000));
    }

    // Verify ethereum hardforks are supported for all networks.
    fn assert_base_hardforks(forks: &ChainHardforks) {
        assert!(!forks.is_empty());

        // Ethereum hardforks not supported
        assert_eq!(forks.get(EthereumHardfork::Dao), None);

        // Ethereum hardforks supported
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Frontier, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Homestead, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Tangerine, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::SpuriousDragon, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Byzantium, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Constantinople, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Petersburg, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Istanbul, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Berlin, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::London, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::Paris, 0));
        // hardforks for delay the difficulty bomb
        assert!(forks.is_fork_active_at_block(EthereumHardfork::MuirGlacier, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::ArrowGlacier, 0));
        assert!(forks.is_fork_active_at_block(EthereumHardfork::GrayGlacier, 0));
        // Time based hardforks
        assert!(forks.is_fork_active_at_timestamp(EthereumHardfork::Shanghai, 0));
        assert!(forks.is_fork_active_at_timestamp(EthereumHardfork::Cancun, 0));
        assert!(forks.is_fork_active_at_timestamp(EthereumHardfork::Prague, 0));
    }

    #[test]
    fn test_arc_base_hardforks() {
        let forks = BASE_FORKS.clone();
        assert_base_hardforks(&forks);
        assert_eq!(forks.len(), 17);
    }

    #[test]
    fn test_arc_localdev_forks() {
        let forks = ARC_LOCALDEV_HARDFORKS.clone();
        assert_base_hardforks(&forks);
        assert_eq!(forks.len(), 22);

        // verify hardfork zero3 block
        assert!(!forks.is_fork_active_at_timestamp(ArcHardfork::Zero3, 0));
        assert!(forks.is_fork_active_at_block(ArcHardfork::Zero3, 0));

        // verify hardfork zero4 block
        assert!(!forks.is_fork_active_at_timestamp(ArcHardfork::Zero4, 0));
        assert!(forks.is_fork_active_at_block(ArcHardfork::Zero4, 0));

        // verify hardfork zero5 block
        assert!(!forks.is_fork_active_at_timestamp(ArcHardfork::Zero5, 0));
        assert!(forks.is_fork_active_at_block(ArcHardfork::Zero5, 0));

        // verify hardfork zero6 block
        assert!(!forks.is_fork_active_at_timestamp(ArcHardfork::Zero6, 0));
        assert!(forks.is_fork_active_at_block(ArcHardfork::Zero6, 0));
    }

    #[test]
    fn test_arc_devnet_forks() {
        let forks = ARC_DEVNET_HARDFORKS.clone();
        assert_base_hardforks(&forks);
        assert_eq!(forks.len(), 19);

        // verify hardfork zero3 block
        assert_eq!(
            forks.get(ArcHardfork::Zero3),
            Some(ForkCondition::Block(
                ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET
            ))
        );
        assert!(!forks.is_fork_active_at_block(
            ArcHardfork::Zero3,
            ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET - 1
        ));
        assert!(forks.is_fork_active_at_block(
            ArcHardfork::Zero3,
            ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET
        ));

        // verify hardfork zero4 block
        assert_eq!(
            forks.get(ArcHardfork::Zero4),
            Some(ForkCondition::Block(
                ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET
            ))
        );
        assert!(!forks.is_fork_active_at_block(
            ArcHardfork::Zero4,
            ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET - 1
        ));
        assert!(forks.is_fork_active_at_block(
            ArcHardfork::Zero4,
            ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET
        ));
    }

    #[test]
    fn test_arc_testnet_forks() {
        let forks = ARC_TESTNET_HARDFORKS.clone();
        assert_base_hardforks(&forks);
        assert_eq!(forks.len(), 19);

        // verify hardfork zero3 block
        assert_eq!(
            forks.get(ArcHardfork::Zero3),
            Some(ForkCondition::Block(
                ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET
            ))
        );
        assert!(!forks.is_fork_active_at_block(
            ArcHardfork::Zero3,
            ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET - 1
        ));
        assert!(forks.is_fork_active_at_block(
            ArcHardfork::Zero3,
            ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET
        ));

        // verify hardfork zero4 block
        assert_eq!(
            forks.get(ArcHardfork::Zero4),
            Some(ForkCondition::Block(
                ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET
            ))
        );
        assert!(!forks.is_fork_active_at_block(
            ArcHardfork::Zero4,
            ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET - 1
        ));
        assert!(forks.is_fork_active_at_block(
            ArcHardfork::Zero4,
            ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET
        ));
    }
}
