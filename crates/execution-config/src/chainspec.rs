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

use alloy_eips::eip1559::DEFAULT_ELASTICITY_MULTIPLIER;
use alloy_eips::eip7840::BlobParams;
use alloy_evm::eth::spec::EthExecutorSpec;
use alloy_genesis::Genesis;
#[cfg(any(feature = "test-utils", test))]
use alloy_primitives::{address, b256};
use alloy_primitives::{Address, U256};
use eyre::Result;
use once_cell::sync::Lazy as LazyLock;
use reth_chainspec::{
    BaseFeeParams, Chain, ChainSpec, DepositContract, EthChainSpec, EthereumHardfork,
    EthereumHardforks, ForkCondition, ForkFilter, ForkId, Hardfork, Hardforks, Head,
};
use reth_cli::chainspec::{parse_genesis, ChainSpecParser};
use reth_ethereum_primitives::EthPrimitives;
use reth_network_peers::NodeRecord;
use reth_primitives_traits::NodePrimitives;
use revm_primitives::B256;
use std::sync::Arc;

#[cfg(any(feature = "test-utils", test))]
use crate::hardforks::ArcHardfork;
#[cfg(any(feature = "test-utils", test))]
use crate::native_coin_control::compute_is_blocklisted_storage_slot;
use crate::{
    gas_fee::decode_base_fee_from_bytes,
    hardforks::{
        ArcGenesisInfo, ArcHardforkFlags, ARC_DEVNET_HARDFORKS, ARC_LOCALDEV_HARDFORKS,
        ARC_MAINNET_HARDFORKS, ARC_TESTNET_HARDFORKS,
    },
};

use crate::chain_ids::*;

const ARC_SUPPORTED: &[&str] = &["arc-mainnet", "arc-testnet", "arc-localdev", "arc-devnet"];
const ARC_BASE_FEE_MAX_CHANGE_DENOMINATOR: u128 = 50; // 1/50 = 2%

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ArcChainSpecParser;

impl ChainSpecParser for ArcChainSpecParser {
    type ChainSpec = ArcChainSpec;

    const SUPPORTED_CHAINS: &'static [&'static str] = ARC_SUPPORTED;

    fn parse(s: &str) -> Result<Arc<Self::ChainSpec>> {
        match s {
            "arc-localdev" => Ok(LOCAL_DEV.clone()),
            "arc-devnet" => Ok(DEVNET.clone()),
            "arc-testnet" => Ok(TESTNET.clone()),
            "arc-mainnet" => Ok(MAINNET.clone()),
            _ => {
                let genesis = parse_genesis(s)?;
                Ok(Arc::new(ArcChainSpec::from(genesis)))
            }
        }
    }
}

/// Block gas limit configuration
///
/// Use [`BlockGasLimitConfig::new`] to construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockGasLimitConfig {
    min: u64,
    max: u64,
    default: u64,
}

impl BlockGasLimitConfig {
    /// Creates a new `BlockGasLimitConfig`.
    ///
    /// # Panics
    /// Panics if `min > default` or `default > max`.
    pub fn new(min: u64, max: u64, default: u64) -> Self {
        assert!(
            min <= default && default <= max,
            "invalid block gas limit config: min ({min}) <= default ({default}) <= max ({max})"
        );
        Self { min, max, default }
    }

    pub fn min(&self) -> u64 {
        self.min
    }

    pub fn max(&self) -> u64 {
        self.max
    }

    pub fn default(&self) -> u64 {
        self.default
    }
}

/// Provides block gas limit configuration at a given block height.
///
pub trait BlockGasLimitProvider {
    /// Returns the block gas limit config for the given block height.
    fn block_gas_limit_config(&self, block_height: u64) -> BlockGasLimitConfig;
}

impl<T: BlockGasLimitProvider> BlockGasLimitProvider for Arc<T> {
    fn block_gas_limit_config(&self, block_height: u64) -> BlockGasLimitConfig {
        (**self).block_gas_limit_config(block_height)
    }
}

impl<T: BlockGasLimitProvider + ?Sized> BlockGasLimitProvider for &T {
    fn block_gas_limit_config(&self, block_height: u64) -> BlockGasLimitConfig {
        (**self).block_gas_limit_config(block_height)
    }
}

/// A bounded parameter with a minimum, default, and maximum value.
///
/// Used by [`BaseFeeConfig`] to validate on-chain values sourced from ProtocolConfig and
/// substitute the default when the on-chain value is out of the `[min, max]` range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedParam<T> {
    min: T,
    default: T,
    max: T,
}

impl BoundedParam<u64> {
    /// Validates that min <= default <= max.
    pub const fn new(min: u64, default: u64, max: u64) -> Self {
        assert!(
            min <= default && default <= max,
            "invalid BoundedParam: must satisfy min <= default <= max"
        );
        Self { min, default, max }
    }
}

impl<T: PartialOrd + Copy> BoundedParam<T> {
    /// Returns `on_chain` if it is within `[min, max]`; otherwise returns `default`.
    pub fn resolve(&self, on_chain: T) -> T {
        if on_chain >= self.min && on_chain <= self.max {
            on_chain
        } else {
            self.default
        }
    }
}

/// Resolved base fee calculation parameters (after bounds-checking).
///
/// All values are in basis points unless noted (e.g. `k_rate = 200` means 2%).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseFeeCalcParams {
    /// Exponential smoothing factor [0, 100]. 0 = no smoothing, 100 = raw gas used.
    pub alpha: u64,
    /// Max base fee change rate per block in basis points (200 = 2%).
    pub k_rate: u64,
    /// Target gas utilisation in basis points (5000 = 50%).
    pub inverse_elasticity_multiplier: u64,
}

/// Complete base fee configuration for a network (ADR-0004).
///
/// Each calculation parameter field holds its own `[min, default, max]` bounds via
/// [`BoundedParam`]: if the on-chain value falls outside the range for that field,
/// `default` is used instead.
///
/// `absolute_min_base_fee` and `absolute_max_base_fee` clamp the *output* after
/// both the computation and the ProtocolConfig's own `minBaseFee`/`maxBaseFee` clamp.
///
/// Use [`BaseFeeConfig::new`] to construct; direct struct literal construction is only
/// available inside this module (fields are private outside `chainspec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseFeeConfig {
    pub alpha: BoundedParam<u64>,
    pub k_rate: BoundedParam<u64>,
    pub inverse_elasticity_multiplier: BoundedParam<u64>,
    /// Absolute floor on the computed base fee output.
    pub absolute_min_base_fee: u64,
    /// Absolute ceiling on the computed base fee output.
    pub absolute_max_base_fee: u64,
}

impl BaseFeeConfig {
    /// Validates that min / max are appropriately ordered.
    pub const fn new(
        alpha: BoundedParam<u64>,
        k_rate: BoundedParam<u64>,
        inverse_elasticity_multiplier: BoundedParam<u64>,
        absolute_min_base_fee: u64,
        absolute_max_base_fee: u64,
    ) -> Self {
        assert!(
            absolute_min_base_fee <= absolute_max_base_fee,
            "invalid BaseFeeConfig: absolute_min_base_fee must be <= absolute_max_base_fee"
        );
        Self {
            alpha,
            k_rate,
            inverse_elasticity_multiplier,
            absolute_min_base_fee,
            absolute_max_base_fee,
        }
    }

    /// Resolves `BaseFeeCalcParams` from an optional on-chain `FeeParams`.
    ///
    /// If `fee_params` is `None`, returns the defaults for each field.
    /// Otherwise validates each field independently and substitutes the default for any
    /// field that is out of the `[min, max]` range.
    pub fn resolve_calc_params(
        &self,
        fee_params: Option<&crate::protocol_config::IProtocolConfig::FeeParams>,
    ) -> BaseFeeCalcParams {
        match fee_params {
            None => BaseFeeCalcParams {
                alpha: self.alpha.default,
                k_rate: self.k_rate.default,
                inverse_elasticity_multiplier: self.inverse_elasticity_multiplier.default,
            },
            Some(fp) => BaseFeeCalcParams {
                alpha: self.alpha.resolve(fp.alpha),
                k_rate: self.k_rate.resolve(fp.kRate),
                inverse_elasticity_multiplier: self
                    .inverse_elasticity_multiplier
                    .resolve(fp.inverseElasticityMultiplier),
            },
        }
    }

    /// Clamps `base_fee` to `[absolute_min_base_fee, absolute_max_base_fee]`.
    pub fn clamp_absolute(&self, base_fee: u64) -> u64 {
        base_fee.clamp(self.absolute_min_base_fee, self.absolute_max_base_fee)
    }
}

/// Provides base fee configuration at a given block height.
pub trait BaseFeeConfigProvider {
    fn base_fee_config(&self, block_height: u64) -> BaseFeeConfig;
}

impl<T: BaseFeeConfigProvider> BaseFeeConfigProvider for Arc<T> {
    fn base_fee_config(&self, block_height: u64) -> BaseFeeConfig {
        (**self).base_fee_config(block_height)
    }
}

impl<T: BaseFeeConfigProvider + ?Sized> BaseFeeConfigProvider for &T {
    fn base_fee_config(&self, block_height: u64) -> BaseFeeConfig {
        (**self).base_fee_config(block_height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArcChainSpec {
    pub inner: ChainSpec,
}

impl ArcChainSpec {
    pub fn new(inner: ChainSpec) -> Self {
        Self { inner }
    }

    /// Get the hardfork flags for a given (block height, timestamp).
    ///
    /// Returns feature flags indicating which Arc hardforks are active at the given
    /// head. Both inputs are required because Arc hardfork schedules are
    /// network-specific and may use either block heights or timestamps.
    pub fn get_hardfork_flags(&self, height: u64, timestamp: u64) -> ArcHardforkFlags {
        ArcHardforkFlags::from_chain_hardforks(&self.inner.hardforks, height, timestamp)
    }
}

impl BlockGasLimitProvider for ArcChainSpec {
    fn block_gas_limit_config(&self, _block_height: u64) -> BlockGasLimitConfig {
        let (min, max) = match self.chain().id() {
            MAINNET_CHAIN_ID => (10_000_000, 200_000_000),
            TESTNET_CHAIN_ID => (10_000_000, 200_000_000),
            _ => (1_000_000, 1_000_000_000),
        };
        BlockGasLimitConfig::new(min, max, 30_000_000)
    }
}

const BASE_FEE_CONFIG_MAINNET: BaseFeeConfig = BaseFeeConfig::new(
    BoundedParam::new(1, 20, 100),
    BoundedParam::new(1, 200, 1_000),
    BoundedParam::new(1, 5000, 9_000),
    1,
    20_000_000_000_000, // 20,000 gwei
);

const BASE_FEE_CONFIG_TESTNET: BaseFeeConfig = BaseFeeConfig::new(
    BoundedParam::new(1, 20, 100),
    BoundedParam::new(1, 200, 1_000),
    BoundedParam::new(1, 5000, 9_000),
    1,
    20_000_000_000_000, // 20,000 gwei
);

const BASE_FEE_CONFIG_DEFAULT: BaseFeeConfig = BaseFeeConfig::new(
    BoundedParam::new(1, 20, 100),
    BoundedParam::new(1, 200, 10_000),
    BoundedParam::new(1, 5000, 10_000),
    1,
    u64::MAX - 1,
);

impl BaseFeeConfigProvider for ArcChainSpec {
    // While the same config is used for all blockheights, it is available to ease future hardfork transitions
    fn base_fee_config(&self, _block_height: u64) -> BaseFeeConfig {
        match self.chain().id() {
            MAINNET_CHAIN_ID => BASE_FEE_CONFIG_MAINNET,
            TESTNET_CHAIN_ID => BASE_FEE_CONFIG_TESTNET,
            _ => BASE_FEE_CONFIG_DEFAULT,
        }
    }
}

/// ERC-7201 namespaced storage slots for ProtocolConfig (proxy at 0x3600..0001).
/// Base: keccak256(abi.encode(uint256(keccak256("arc.storage.ProtocolConfig")) - 1)) & ~0xff
#[cfg(any(feature = "test-utils", test))]
const PROTOCOL_CONFIG_BLOCK_GAS_LIMIT_SLOT: B256 =
    b256!("668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385203");
/// ERC-1967 implementation slot on the proxy.
#[cfg(any(feature = "test-utils", test))]
const PROXY_IMPLEMENTATION_SLOT: B256 =
    b256!("360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc");

/// Creates a custom localdev chain spec for testing with specific hardfork activations.
///
/// This starts with base Ethereum forks and adds only the specified Arc hardforks.
/// Each entry pairs a hardfork with the [`ForkCondition`] that activates it — use
/// `ForkCondition::Block(n)` for block-gated forks and `ForkCondition::Timestamp(t)`
/// for timestamp-gated ones.
///
/// # Example
/// ```ignore
/// use arc_execution_config::chainspec::localdev_with_hardforks;
/// use arc_execution_config::hardforks::ArcHardfork;
/// use reth_chainspec::ForkCondition;
///
/// // Create a chain spec with Zero3 and Zero4 active at genesis
/// let spec = localdev_with_hardforks(&[
///     (ArcHardfork::Zero3, ForkCondition::Block(0)),
///     (ArcHardfork::Zero4, ForkCondition::Block(0)),
/// ]);
///
/// // Test Zero7 activating at a future timestamp
/// let spec = localdev_with_hardforks(&[
///     (ArcHardfork::Zero3, ForkCondition::Block(0)),
///     (ArcHardfork::Zero7, ForkCondition::Timestamp(1_800_000_000)),
/// ]);
/// ```
#[cfg(any(feature = "test-utils", test))]
pub fn localdev_with_hardforks(hardforks: &[(ArcHardfork, ForkCondition)]) -> Arc<ArcChainSpec> {
    use crate::hardforks::BASE_FORKS;

    let genesis: Genesis =
        serde_json::from_str(include_str!("../../../assets/localdev/genesis.json"))
            .expect("Can't deserialize localdev genesis json");

    let mut inner = ChainSpec::from_genesis(genesis);
    inner.hardforks = BASE_FORKS.clone();

    for &(hardfork, condition) in hardforks {
        // Match to access the constant value for .boxed() — needed for the 'static lifetime.
        match hardfork {
            ArcHardfork::Zero3 => inner
                .hardforks
                .insert(ArcHardfork::Zero3.boxed(), condition),
            ArcHardfork::Zero4 => inner
                .hardforks
                .insert(ArcHardfork::Zero4.boxed(), condition),
            ArcHardfork::Zero5 => inner
                .hardforks
                .insert(ArcHardfork::Zero5.boxed(), condition),
            ArcHardfork::Zero6 => inner
                .hardforks
                .insert(ArcHardfork::Zero6.boxed(), condition),
            ArcHardfork::Zero7 => inner
                .hardforks
                .insert(ArcHardfork::Zero7.boxed(), condition),
        };
    }

    Arc::new(ArcChainSpec::new(inner))
}

/// Creates a localdev chain spec with an address pre-blocklisted in NativeCoinControl.
#[cfg(any(feature = "test-utils", test))]
pub fn localdev_with_storage_override(blocklisted_address: Option<Address>) -> Arc<ArcChainSpec> {
    let mut genesis: Genesis =
        serde_json::from_str(include_str!("../../../assets/localdev/genesis.json"))
            .expect("Can't deserialize localdev genesis json");

    if let Some(blocklisted_address) = blocklisted_address {
        const BLOCKLISTED_STATUS: B256 =
            b256!("0000000000000000000000000000000000000000000000000000000000000001");

        let native_coin_control_address = address!("0x1800000000000000000000000000000000000001");
        let native_coin_control_account = genesis
            .alloc
            .get_mut(&native_coin_control_address)
            .expect("LOCAL_DEV genesis missing NativeCoinControl account");
        let native_coin_control_storage = native_coin_control_account
            .storage
            .get_or_insert_with(Default::default);
        native_coin_control_storage.insert(
            compute_is_blocklisted_storage_slot(blocklisted_address),
            BLOCKLISTED_STATUS,
        );
    }

    let mut inner = ChainSpec::from_genesis(genesis);
    inner.hardforks = ARC_LOCALDEV_HARDFORKS.clone();
    Arc::new(ArcChainSpec::new(inner))
}

/// Creates a localdev chain spec with addresses pre-denylisted in the Denylist contract.
#[cfg(any(feature = "test-utils", test))]
pub fn localdev_with_denylisted_addresses(
    denylisted_addresses: impl IntoIterator<Item = Address>,
) -> Arc<ArcChainSpec> {
    use crate::addresses_denylist::{
        compute_denylist_storage_slot, DEFAULT_DENYLIST_ADDRESS, DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
    };

    const DENYLISTED_STATUS: B256 =
        b256!("0000000000000000000000000000000000000000000000000000000000000001");

    let mut genesis: Genesis =
        serde_json::from_str(include_str!("../../../assets/localdev/genesis.json"))
            .expect("Can't deserialize localdev genesis json");

    let denylist_account = genesis
        .alloc
        .get_mut(&DEFAULT_DENYLIST_ADDRESS)
        .expect("LOCAL_DEV genesis missing Denylist account");
    let storage = denylist_account
        .storage
        .get_or_insert_with(Default::default);
    for addr in denylisted_addresses {
        storage.insert(
            compute_denylist_storage_slot(addr, DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
            DENYLISTED_STATUS,
        );
    }

    let mut inner = ChainSpec::from_genesis(genesis);
    inner.hardforks = ARC_LOCALDEV_HARDFORKS.clone();
    Arc::new(ArcChainSpec::new(inner))
}

/// Creates a localdev chain spec with a custom blockGasLimit in ProtocolConfig storage.
#[cfg(any(feature = "test-utils", test))]
pub fn localdev_with_block_gas_limit(block_gas_limit: u64) -> Arc<ArcChainSpec> {
    localdev_with_protocol_config_overrides(&[(
        PROTOCOL_CONFIG_BLOCK_GAS_LIMIT_SLOT,
        U256::from(block_gas_limit).into(),
    )])
}

/// Creates a localdev chain spec where ProtocolConfig reverts on any call.
/// Achieved by zeroing the ERC-1967 implementation slot on the proxy.
#[cfg(any(feature = "test-utils", test))]
pub fn localdev_with_protocol_config_reverts() -> Arc<ArcChainSpec> {
    localdev_with_protocol_config_overrides(&[(PROXY_IMPLEMENTATION_SLOT, B256::ZERO)])
}

/// Creates a localdev chain spec with arbitrary storage overrides on the
/// ProtocolConfig proxy account.
#[cfg(any(feature = "test-utils", test))]
fn localdev_with_protocol_config_overrides(overrides: &[(B256, B256)]) -> Arc<ArcChainSpec> {
    let mut genesis: Genesis =
        serde_json::from_str(include_str!("../../../assets/localdev/genesis.json"))
            .expect("Can't deserialize localdev genesis json");

    let protocol_config_account = genesis
        .alloc
        .get_mut(&crate::protocol_config::PROTOCOL_CONFIG_ADDRESS)
        .expect("LOCAL_DEV genesis missing ProtocolConfig account");

    let storage = protocol_config_account
        .storage
        .get_or_insert_with(Default::default);

    for &(slot, value) in overrides {
        storage.insert(slot, value);
    }

    let mut inner = ChainSpec::from_genesis(genesis);
    inner.hardforks = ARC_LOCALDEV_HARDFORKS.clone();
    Arc::new(ArcChainSpec::new(inner))
}

// localdev chain spec.
pub static LOCAL_DEV: LazyLock<Arc<ArcChainSpec>> = LazyLock::new(|| {
    let genesis: Genesis =
        serde_json::from_str(include_str!("../../../assets/localdev/genesis.json"))
            .expect("Can't deserialize localdev genesis json");
    let mut inner = ChainSpec::from_genesis(genesis);
    inner.hardforks = ARC_LOCALDEV_HARDFORKS.clone();
    ArcChainSpec::new(inner).into()
});

pub static DEVNET: LazyLock<Arc<ArcChainSpec>> = LazyLock::new(|| {
    let genesis: Genesis =
        serde_json::from_str(include_str!("../../../assets/devnet/genesis.json"))
            .expect("Can't deserialize Devnet genesis json");
    let mut inner = ChainSpec::from_genesis(genesis);
    inner.hardforks = ARC_DEVNET_HARDFORKS.clone();
    ArcChainSpec::new(inner).into()
});

pub static TESTNET: LazyLock<Arc<ArcChainSpec>> = LazyLock::new(|| {
    let genesis: Genesis =
        serde_json::from_str(include_str!("../../../assets/testnet/genesis.json"))
            .expect("Can't deserialize Testnet genesis json");
    let mut inner = ChainSpec::from_genesis(genesis);
    inner.hardforks = ARC_TESTNET_HARDFORKS.clone();
    ArcChainSpec::new(inner).into()
});

pub static MAINNET: LazyLock<Arc<ArcChainSpec>> = LazyLock::new(|| {
    let genesis: Genesis =
        serde_json::from_str(include_str!("../../../assets/mainnet/genesis.json"))
            .expect("Can't deserialize Mainnet genesis json");
    let mut inner = ChainSpec::from_genesis(genesis);
    inner.hardforks = ARC_MAINNET_HARDFORKS.clone();
    ArcChainSpec::new(inner).into()
});

/// Returns the bundled chainspec for a known Arc chain ID, or `None` if the
/// chain ID is unknown.
pub fn bundled_chainspec_for_chain_id(chain_id: u64) -> Option<Arc<ArcChainSpec>> {
    match chain_id {
        LOCALDEV_CHAIN_ID => Some(LOCAL_DEV.clone()),
        MAINNET_CHAIN_ID => Some(MAINNET.clone()),
        DEVNET_CHAIN_ID => Some(DEVNET.clone()),
        TESTNET_CHAIN_ID => Some(TESTNET.clone()),
        _ => None,
    }
}

impl From<ChainSpec> for ArcChainSpec {
    fn from(inner: ChainSpec) -> Self {
        Self::new(inner)
    }
}

impl From<Genesis> for ArcChainSpec {
    fn from(genesis: Genesis) -> Self {
        let mut inner = ChainSpec::from_genesis(genesis);

        // For mainnet, devnet, and testnet, we don't read the fork configuration from
        // genesis. Patch the hardfork table from the predefined value instead.
        //
        // Localdev is intentionally NOT hardcoded here so that genesis.json controls
        // hardfork activation — the nightly-upgrade test patches genesis.json with jq
        // and relies on the node reading those values.  The named network "arc-localdev"
        // (LOCAL_DEV static) still uses ARC_LOCALDEV_HARDFORKS directly.
        match inner.chain().id() {
            MAINNET_CHAIN_ID => {
                inner.hardforks = ARC_MAINNET_HARDFORKS.clone();
            }
            DEVNET_CHAIN_ID => {
                inner.hardforks = ARC_DEVNET_HARDFORKS.clone();
            }
            TESTNET_CHAIN_ID => {
                inner.hardforks = ARC_TESTNET_HARDFORKS.clone();
            }
            _ => {
                if let Some(extra) =
                    ArcGenesisInfo::extract_from(&inner.genesis().config.extra_fields)
                {
                    for (hardfork, condition) in extra.get_hardfork_conditions() {
                        inner.hardforks.insert(hardfork, condition);
                    }
                }
            }
        }
        Self::new(inner)
    }
}

impl EthChainSpec for ArcChainSpec {
    type Header = <EthPrimitives as NodePrimitives>::BlockHeader;

    fn chain(&self) -> Chain {
        self.inner.chain()
    }

    // Do not use this function, use `calc_next_block_base_fee` directly instead.
    fn base_fee_params_at_timestamp(&self, _timestamp: u64) -> BaseFeeParams {
        BaseFeeParams::new(
            ARC_BASE_FEE_MAX_CHANGE_DENOMINATOR,
            DEFAULT_ELASTICITY_MULTIPLIER as u128,
        )
    }

    fn blob_params_at_timestamp(&self, timestamp: u64) -> Option<BlobParams> {
        self.inner.blob_params_at_timestamp(timestamp)
    }

    fn deposit_contract(&self) -> Option<&DepositContract> {
        None
    }

    fn genesis_hash(&self) -> B256 {
        self.inner.genesis_hash()
    }

    fn prune_delete_limit(&self) -> usize {
        self.inner.prune_delete_limit()
    }

    fn display_hardforks(&self) -> Box<dyn core::fmt::Display> {
        Box::new(self.inner.display_hardforks())
    }

    fn genesis_header(&self) -> &Self::Header {
        self.inner.genesis_header()
    }

    fn genesis(&self) -> &Genesis {
        self.inner.genesis()
    }

    fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        self.inner.bootnodes()
    }

    fn final_paris_total_difficulty(&self) -> Option<U256> {
        self.inner.final_paris_total_difficulty()
    }

    fn chain_id(&self) -> u64 {
        self.chain().id()
    }

    fn is_optimism(&self) -> bool {
        false
    }

    fn is_ethereum(&self) -> bool {
        false
    }

    fn next_block_base_fee(&self, parent: &Self::Header, _target_timestamp: u64) -> Option<u64> {
        let child_number = parent.number.saturating_add(1);
        let base_fee_config = self.base_fee_config(child_number);
        if let Some(base_fee) = decode_base_fee_from_bytes(&parent.extra_data) {
            Some(base_fee)
        } else {
            // Fallback that should never be hit once Zero5 is activated: use field defaults
            // from BaseFeeConfig since no ProtocolConfig data is available.
            let calc = base_fee_config.resolve_calc_params(None);
            let raw = crate::gas_fee::arc_calc_next_block_base_fee(
                parent.gas_used,
                parent.gas_limit,
                parent.base_fee_per_gas.unwrap_or_default(),
                calc.k_rate,
                calc.inverse_elasticity_multiplier,
            );
            Some(base_fee_config.clamp_absolute(raw))
        }
    }
}

impl EthereumHardforks for ArcChainSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        self.inner.ethereum_fork_activation(fork)
    }

    fn is_ethereum_fork_active_at_timestamp(&self, fork: EthereumHardfork, timestamp: u64) -> bool {
        self.ethereum_fork_activation(fork)
            .active_at_timestamp(timestamp)
    }

    fn is_ethereum_fork_active_at_block(&self, fork: EthereumHardfork, block_number: u64) -> bool {
        self.ethereum_fork_activation(fork)
            .active_at_block(block_number)
    }

    fn is_homestead_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::Homestead, block_number)
    }

    fn is_tangerine_whistle_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::Tangerine, block_number)
    }

    fn is_spurious_dragon_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::SpuriousDragon, block_number)
    }

    fn is_byzantium_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::Byzantium, block_number)
    }

    fn is_constantinople_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::Constantinople, block_number)
    }

    fn is_petersburg_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::Petersburg, block_number)
    }

    fn is_istanbul_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::Istanbul, block_number)
    }

    fn is_berlin_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::Berlin, block_number)
    }

    fn is_london_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::London, block_number)
    }

    fn is_paris_active_at_block(&self, block_number: u64) -> bool {
        self.is_ethereum_fork_active_at_block(EthereumHardfork::Paris, block_number)
    }

    fn is_shanghai_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Shanghai, timestamp)
    }

    fn is_cancun_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Cancun, timestamp)
    }

    fn is_prague_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Prague, timestamp)
    }

    fn is_osaka_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Osaka, timestamp)
    }

    fn is_amsterdam_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Amsterdam, timestamp)
    }

    fn is_bpo1_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Bpo1, timestamp)
    }

    fn is_bpo2_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Bpo2, timestamp)
    }

    fn is_bpo3_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Bpo3, timestamp)
    }

    fn is_bpo4_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Bpo4, timestamp)
    }

    fn is_bpo5_active_at_timestamp(&self, timestamp: u64) -> bool {
        self.is_ethereum_fork_active_at_timestamp(EthereumHardfork::Bpo5, timestamp)
    }
}

impl Hardforks for ArcChainSpec {
    fn fork<H: Hardfork>(&self, fork: H) -> ForkCondition {
        self.inner.fork(fork)
    }

    fn forks_iter(&self) -> impl Iterator<Item = (&dyn Hardfork, ForkCondition)> {
        self.inner.forks_iter()
    }

    fn fork_id(&self, head: &Head) -> ForkId {
        self.inner.fork_id(head)
    }

    fn latest_fork_id(&self) -> ForkId {
        self.inner.latest_fork_id()
    }

    fn fork_filter(&self, head: Head) -> ForkFilter {
        self.inner.fork_filter(head)
    }

    fn is_fork_active_at_timestamp<H: Hardfork>(&self, fork: H, timestamp: u64) -> bool {
        self.fork(fork).active_at_timestamp(timestamp)
    }

    fn is_fork_active_at_block<H: Hardfork>(&self, fork: H, block_number: u64) -> bool {
        self.fork(fork).active_at_block(block_number)
    }
}

impl EthExecutorSpec for ArcChainSpec {
    fn deposit_contract_address(&self) -> Option<Address> {
        None
    }
}

// Test Arc LocalDev chain spec parsing
#[cfg(test)]
mod tests {
    use super::*;

    use crate::chain_ids::{
        DEVNET_CHAIN_ID, LOCALDEV_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID,
    };
    use crate::hardforks::{
        ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_DEVNET,
        ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET,
        ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET, ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET,
        ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET, ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET,
        ARC_ZERO5_HARDFORK_BLOCK_ACTIVATION_DEVNET,
        ARC_ZERO5_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET,
        ARC_ZERO6_HARDFORK_BLOCK_ACTIVATION_DEVNET,
        ARC_ZERO6_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET, BASE_FORKS,
    };

    fn assert_arc_chainspec_evm_hardforks(spec: &ArcChainSpec) {
        // ---- Block-gated forks (chronological) ----
        // No helper function for Frontier
        assert!(spec.is_homestead_active_at_block(0));
        assert!(spec.is_tangerine_whistle_active_at_block(0));
        assert!(spec.is_spurious_dragon_active_at_block(0));
        assert!(spec.is_byzantium_active_at_block(0));
        assert!(spec.is_constantinople_active_at_block(0));
        assert!(spec.is_petersburg_active_at_block(0));
        assert!(spec.is_istanbul_active_at_block(0));

        assert!(spec.is_ethereum_fork_active_at_block(EthereumHardfork::MuirGlacier, 0));
        assert!(spec.is_berlin_active_at_block(0));
        assert!(spec.is_london_active_at_block(0));
        assert!(spec.is_ethereum_fork_active_at_block(EthereumHardfork::ArrowGlacier, 0));
        assert!(spec.is_ethereum_fork_active_at_block(EthereumHardfork::GrayGlacier, 0));
        assert!(spec.is_paris_active_at_block(0));

        // ---- Timestamp-gated forks (chronological) ----
        assert!(spec.is_shanghai_active_at_timestamp(0));
        assert!(spec.is_cancun_active_at_timestamp(0));
        assert!(spec.is_prague_active_at_timestamp(0));

        // Sanity
        assert!(!spec.is_ethereum());
        assert!(!spec.is_optimism());

        // Forks beyond osaka
        assert!(!spec.is_amsterdam_active_at_timestamp(0));
        assert!(!spec.is_bpo1_active_at_timestamp(0));
        assert!(!spec.is_bpo2_active_at_timestamp(0));
        assert!(!spec.is_bpo3_active_at_timestamp(0));
        assert!(!spec.is_bpo4_active_at_timestamp(0));
        assert!(!spec.is_bpo5_active_at_timestamp(0));

        // Verify each fork is supported
        let supported_hardforks = [
            EthereumHardfork::Frontier,
            EthereumHardfork::Homestead,
            EthereumHardfork::Tangerine,
            EthereumHardfork::SpuriousDragon,
            EthereumHardfork::Byzantium,
            EthereumHardfork::Constantinople,
            EthereumHardfork::Petersburg,
            EthereumHardfork::Istanbul,
            EthereumHardfork::MuirGlacier,
            EthereumHardfork::Berlin,
            EthereumHardfork::London,
            EthereumHardfork::ArrowGlacier,
            EthereumHardfork::GrayGlacier,
            EthereumHardfork::Paris,
            EthereumHardfork::Shanghai,
            EthereumHardfork::Cancun,
            EthereumHardfork::Prague,
        ];

        for fork in supported_hardforks {
            let cond = spec.ethereum_fork_activation(fork);
            if cond.active_at_block(0) {
                assert!(
                    cond.active_at_block(0),
                    "Fork {:?} not block-active at 0",
                    fork
                );
            } else if cond.active_at_timestamp(0) {
                assert!(
                    cond.active_at_timestamp(0),
                    "Fork {:?} not ts-active at 0",
                    fork
                );
            } else {
                panic!(
                    "Fork {:?} has neither block nor timestamp active at 0 (cond: {:?})",
                    fork, cond
                );
            }
        }

        // Empty deposit contract
        assert!(spec.deposit_contract().is_none());
        assert!(spec.deposit_contract_address().is_none());

        // BaseFeeParams
        let base_fee_params = spec.base_fee_params_at_timestamp(0);
        assert_eq!(
            base_fee_params.max_change_denominator,
            ARC_BASE_FEE_MAX_CHANGE_DENOMINATOR
        );
        assert_eq!(
            base_fee_params.elasticity_multiplier,
            DEFAULT_ELASTICITY_MULTIPLIER as u128
        );

        // Bootnodes
        assert_eq!(spec.bootnodes(), spec.inner.bootnodes());

        // Blob params
        assert_eq!(
            spec.blob_params_at_timestamp(0),
            spec.inner.blob_params_at_timestamp(0)
        );

        // Genesis
        assert_eq!(spec.genesis_hash(), spec.inner.genesis_hash());
        assert_eq!(spec.genesis(), spec.inner.genesis());

        // Misc
        assert!(spec.final_paris_total_difficulty().is_none());
    }

    #[test]
    fn test_load_genesis_localdev() {
        let spec = ArcChainSpecParser::parse("../../assets/localdev/genesis.json")
            .expect("Failed to parse arc-localdev");
        assert_eq!(spec.chain().id(), LOCALDEV_CHAIN_ID);
        assert_arc_chainspec_evm_hardforks(&spec);
        assert_eq!(spec.forks_iter().count(), 23);
        assert!(spec.is_osaka_active_at_timestamp(0));

        // verify zero3 hardfork block
        assert!(!spec.is_fork_active_at_timestamp(ArcHardfork::Zero3, 1762732800 - 1));
        assert!(
            spec.is_fork_active_at_block(ArcHardfork::Zero3, 0),
            "Zero3 should be active at block 0 in hardfork.rs, and load by chainspec"
        );
        // verify zero4 hardfork block
        assert!(
            spec.is_fork_active_at_block(ArcHardfork::Zero4, 0),
            "Zero4 should be active at block 0 in hardfork.rs, and load by chainspec"
        );
        // verify zero5 hardfork block
        assert!(
            spec.is_fork_active_at_block(ArcHardfork::Zero5, 0),
            "Zero5 should be active at block 0 in hardfork.rs, and load by chainspec"
        );
        // verify zero6 hardfork block
        assert!(
            spec.is_fork_active_at_block(ArcHardfork::Zero6, 0),
            "Zero6 should be active at block 0 in hardfork.rs, and load by chainspec"
        );
        // Zero7 activates by timestamp (Arc convention from Zero7 onward).
        assert!(
            spec.is_fork_active_at_timestamp(ArcHardfork::Zero7, 0),
            "Zero7 should be active at timestamp 0 in hardfork.rs, and load by chainspec"
        );
        let flags = spec.get_hardfork_flags(0, 0);
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(flags.is_active(ArcHardfork::Zero4));
        assert!(flags.is_active(ArcHardfork::Zero5));
        assert!(flags.is_active(ArcHardfork::Zero6));
        assert!(flags.is_active(ArcHardfork::Zero7));
    }

    #[test]
    fn test_arc_localdev_chainspec() {
        let spec = ArcChainSpecParser::parse("arc-localdev").expect("Failed to parse arc-localdev");
        assert_eq!(spec.chain().id(), LOCALDEV_CHAIN_ID);
        assert_arc_chainspec_evm_hardforks(&spec);
        assert!(spec.is_osaka_active_at_timestamp(0));
        assert_eq!(spec.forks_iter().count(), 23);

        // verify zero3 hardfork block
        assert!(!spec.is_fork_active_at_timestamp(ArcHardfork::Zero3, 1762732800));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero3, 0));
        // verify zero4 hardfork block
        assert!(!spec.is_fork_active_at_timestamp(ArcHardfork::Zero4, 1762732800));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero4, 0));
        // verify zero5 hardfork block
        assert!(!spec.is_fork_active_at_timestamp(ArcHardfork::Zero5, 1762732800));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero5, 0));
        // verify zero6 hardfork block
        assert!(!spec.is_fork_active_at_timestamp(ArcHardfork::Zero6, 1762732800));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero6, 0));
        // Zero7 activates by timestamp (Arc convention from Zero7 onward).
        assert!(spec.is_fork_active_at_timestamp(ArcHardfork::Zero7, 0));
        assert!(!spec.is_fork_active_at_block(ArcHardfork::Zero7, 0));
        let flags = spec.get_hardfork_flags(0, 0);
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(flags.is_active(ArcHardfork::Zero4));
        assert!(flags.is_active(ArcHardfork::Zero5));
        assert!(flags.is_active(ArcHardfork::Zero6));
        assert!(flags.is_active(ArcHardfork::Zero7));
        assert_eq!(
            spec.display_hardforks().to_string(),
            r#"Pre-merge hard forks (block based):
- Frontier                         @0
- Homestead                        @0
- Tangerine                        @0
- SpuriousDragon                   @0
- Byzantium                        @0
- Constantinople                   @0
- Petersburg                       @0
- Istanbul                         @0
- MuirGlacier                      @0
- Berlin                           @0
- London                           @0
- ArrowGlacier                     @0
- GrayGlacier                      @0
- Zero3                            @0
- Zero4                            @0
- Zero5                            @0
- Zero6                            @0
Merge hard forks:
- Paris                            @0 (network is known to be merged)
Post-merge hard forks (timestamp based):
- Shanghai                         @0          blob: (target: 6, max: 9, fraction: 5007716)
- Cancun                           @0          blob: (target: 6, max: 9, fraction: 5007716)
- Prague                           @0          blob: (target: 6, max: 9, fraction: 5007716)
- Osaka                            @0          blob: (target: 6, max: 9, fraction: 5007716)
- Zero7                            @0          blob: (target: 6, max: 9, fraction: 5007716)"#
        );
    }

    #[test]
    fn test_arc_mainnet_chainspec() {
        let spec = ArcChainSpecParser::parse("arc-mainnet").expect("Failed to parse arc-mainnet");
        assert_eq!(spec.chain().id(), MAINNET_CHAIN_ID);

        // Pin the genesis hash to catch any unintended drift in
        // assets/mainnet/genesis.json. Update only when a deliberate respin
        // happens (e.g. revised admin set, additional prefund, hardfork shift).
        assert_eq!(
            spec.genesis_hash().to_string(),
            "0x09944e07412986bb417fd0006c89ffb71ee523d68ce2017ec2dabc944c42edad",
            "the genesis hash of assets/mainnet/genesis.json changed unexpectedly"
        );

        assert_arc_chainspec_evm_hardforks(&spec);
        assert!(spec.is_osaka_active_at_timestamp(0));
        assert_eq!(spec.forks_iter().count(), 22);

        // Mainnet launches at Zero6: Zero3..Zero6 active at block 0; Zero7 is not scheduled on mainnet.
        let flags = spec.get_hardfork_flags(0, 0);
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(flags.is_active(ArcHardfork::Zero4));
        assert!(flags.is_active(ArcHardfork::Zero5));
        assert!(flags.is_active(ArcHardfork::Zero6));
        assert!(!flags.is_active(ArcHardfork::Zero7));

        assert_eq!(
            spec.display_hardforks().to_string(),
            r#"Pre-merge hard forks (block based):
- Frontier                         @0
- Homestead                        @0
- Tangerine                        @0
- SpuriousDragon                   @0
- Byzantium                        @0
- Constantinople                   @0
- Petersburg                       @0
- Istanbul                         @0
- MuirGlacier                      @0
- Berlin                           @0
- London                           @0
- ArrowGlacier                     @0
- GrayGlacier                      @0
- Zero3                            @0
- Zero4                            @0
- Zero5                            @0
- Zero6                            @0
Merge hard forks:
- Paris                            @0 (network is known to be merged)
Post-merge hard forks (timestamp based):
- Shanghai                         @0          blob: (target: 6, max: 9, fraction: 5007716)
- Cancun                           @0          blob: (target: 6, max: 9, fraction: 5007716)
- Prague                           @0          blob: (target: 6, max: 9, fraction: 5007716)
- Osaka                            @0          blob: (target: 6, max: 9, fraction: 5007716)"#
        );
    }

    #[test]
    fn test_bundled_chainspec_for_chain_id() {
        // Round-trip: looking up a chain ID must return the matching spec —
        // guards against a regression like the helper returning Some(LOCAL_DEV)
        // for DEVNET_CHAIN_ID.
        assert_eq!(
            bundled_chainspec_for_chain_id(LOCALDEV_CHAIN_ID)
                .expect("localdev bundled")
                .chain()
                .id(),
            LOCALDEV_CHAIN_ID
        );
        assert_eq!(
            bundled_chainspec_for_chain_id(DEVNET_CHAIN_ID)
                .expect("devnet bundled")
                .chain()
                .id(),
            DEVNET_CHAIN_ID
        );
        assert_eq!(
            bundled_chainspec_for_chain_id(TESTNET_CHAIN_ID)
                .expect("testnet bundled")
                .chain()
                .id(),
            TESTNET_CHAIN_ID
        );
        assert!(bundled_chainspec_for_chain_id(999_999).is_none());

        assert_eq!(
            bundled_chainspec_for_chain_id(MAINNET_CHAIN_ID)
                .expect("mainnet bundled")
                .chain()
                .id(),
            MAINNET_CHAIN_ID
        );
    }

    /// Expected activations are pinned here.
    #[test]
    fn test_mainnet_chainspec_paths_agree() {
        use alloy_genesis::Genesis;

        let from_parser = ArcChainSpecParser::parse("arc-mainnet").expect("named parser path");
        let from_helper = bundled_chainspec_for_chain_id(MAINNET_CHAIN_ID).expect("helper path");
        let from_genesis = ArcChainSpec::from(
            serde_json::from_str::<Genesis>(&format!(
                r#"{{ "config": {{ "chainId": {} }}, "alloc": {{}} }}"#,
                MAINNET_CHAIN_ID
            ))
            .expect("synthetic mainnet genesis parses"),
        );

        assert!(
            Arc::ptr_eq(&from_parser, &from_helper),
            "parser and helper must return the same MAINNET Arc"
        );

        let expected_active: &[(ArcHardfork, bool)] = &[
            (ArcHardfork::Zero3, true),
            (ArcHardfork::Zero4, true),
            (ArcHardfork::Zero5, true),
            (ArcHardfork::Zero6, true),
            (ArcHardfork::Zero7, false), // deferred — not active at launch
        ];
        let paths: [(&str, &ArcChainSpec); 3] = [
            ("parser", &from_parser),
            ("helper", &from_helper),
            ("From<Genesis>", &from_genesis),
        ];
        for (label, spec) in paths {
            for &(fork, want) in expected_active {
                assert_eq!(
                    spec.get_hardfork_flags(0, 0).is_active(fork),
                    want,
                    "{label}: {fork:?} active={want}"
                );
            }
            assert!(
                spec.is_osaka_active_at_timestamp(0),
                "{label}: Osaka must be active at timestamp 0"
            );
        }
    }

    #[test]
    fn test_arc_devnet_chainspec() {
        let spec = ArcChainSpecParser::parse("arc-devnet").expect("Failed to parse arc-devnet");
        assert_eq!(spec.chain().id(), DEVNET_CHAIN_ID);

        // Verify the genesis hash for devnet. The hash may changed when we reset the devnet.
        // Otherwise, the genesis hash should be the same.
        assert_eq!(
            spec.genesis_hash().to_string(),
            "0x41c417868fee948f58602b01a84ce0ddb5ffe2184f7e9ab43b9c8d7e5eb47067",
            "the genesis hash of assets/devnet/genesis.json changed unexpectedly"
        );
        assert_eq!(spec.forks_iter().count(), 23);
        assert_arc_chainspec_evm_hardforks(&spec);
        assert!(!spec.is_osaka_active_at_timestamp(0));
        assert!(spec.is_osaka_active_at_timestamp(ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_DEVNET));

        let flags_before =
            spec.get_hardfork_flags(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET - 1, 0);
        assert!(!flags_before.is_active(ArcHardfork::Zero3));
        assert!(!flags_before.is_active(ArcHardfork::Zero4));

        let flags_at = spec.get_hardfork_flags(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET, 0);
        assert!(flags_at.is_active(ArcHardfork::Zero3));
        assert!(!flags_at.is_active(ArcHardfork::Zero4));

        let flags_before_zero4 =
            spec.get_hardfork_flags(ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET - 1, 0);
        assert!(flags_before_zero4.is_active(ArcHardfork::Zero3));
        assert!(!flags_before_zero4.is_active(ArcHardfork::Zero4));

        let flags_at_zero4 = spec.get_hardfork_flags(ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET, 0);
        assert!(flags_at_zero4.is_active(ArcHardfork::Zero3));
        assert!(flags_at_zero4.is_active(ArcHardfork::Zero4));
        assert!(!flags_at_zero4.is_active(ArcHardfork::Zero5));

        let flags_before_zero5 =
            spec.get_hardfork_flags(ARC_ZERO5_HARDFORK_BLOCK_ACTIVATION_DEVNET - 1, 0);
        assert!(flags_before_zero5.is_active(ArcHardfork::Zero3));
        assert!(flags_before_zero5.is_active(ArcHardfork::Zero4));
        assert!(!flags_before_zero5.is_active(ArcHardfork::Zero5));

        let flags_at_zero5 = spec.get_hardfork_flags(ARC_ZERO5_HARDFORK_BLOCK_ACTIVATION_DEVNET, 0);
        assert!(flags_at_zero5.is_active(ArcHardfork::Zero3));
        assert!(flags_at_zero5.is_active(ArcHardfork::Zero4));
        assert!(flags_at_zero5.is_active(ArcHardfork::Zero5));
        assert!(!flags_at_zero5.is_active(ArcHardfork::Zero6));

        let flags_before_zero6 =
            spec.get_hardfork_flags(ARC_ZERO6_HARDFORK_BLOCK_ACTIVATION_DEVNET - 1, 0);
        assert!(flags_before_zero6.is_active(ArcHardfork::Zero3));
        assert!(flags_before_zero6.is_active(ArcHardfork::Zero4));
        assert!(flags_before_zero6.is_active(ArcHardfork::Zero5));
        assert!(!flags_before_zero6.is_active(ArcHardfork::Zero6));

        let flags_at_zero6 = spec.get_hardfork_flags(ARC_ZERO6_HARDFORK_BLOCK_ACTIVATION_DEVNET, 0);
        assert!(flags_at_zero6.is_active(ArcHardfork::Zero3));
        assert!(flags_at_zero6.is_active(ArcHardfork::Zero4));
        assert!(flags_at_zero6.is_active(ArcHardfork::Zero5));
        assert!(flags_at_zero6.is_active(ArcHardfork::Zero6));

        assert_eq!(
            spec.display_hardforks().to_string(),
            r#"Pre-merge hard forks (block based):
- Frontier                         @0
- Homestead                        @0
- Tangerine                        @0
- SpuriousDragon                   @0
- Byzantium                        @0
- Constantinople                   @0
- Petersburg                       @0
- Istanbul                         @0
- MuirGlacier                      @0
- Berlin                           @0
- London                           @0
- ArrowGlacier                     @0
- GrayGlacier                      @0
- Zero3                            @7437594
- Zero4                            @19491165
- Zero5                            @32371192
- Zero6                            @40033853
Merge hard forks:
- Paris                            @0 (network is known to be merged)
Post-merge hard forks (timestamp based):
- Shanghai                         @0          blob: (target: 6, max: 9, fraction: 5007716)
- Cancun                           @0          blob: (target: 6, max: 9, fraction: 5007716)
- Prague                           @0          blob: (target: 6, max: 9, fraction: 5007716)
- Osaka                            @1775483400          blob: (target: 6, max: 9, fraction: 5007716)
- Zero7                            @1780495200          blob: (target: 6, max: 9, fraction: 5007716)"#
        );
        assert_eq!(
            spec.fork(ArcHardfork::Zero3),
            ForkCondition::Block(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET)
        );
        assert_eq!(
            spec.fork(ArcHardfork::Zero4),
            ForkCondition::Block(ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_DEVNET)
        );
        assert_eq!(
            spec.fork(ArcHardfork::Zero5),
            ForkCondition::Block(ARC_ZERO5_HARDFORK_BLOCK_ACTIVATION_DEVNET)
        );
        assert_eq!(
            spec.fork(ArcHardfork::Zero6),
            ForkCondition::Block(ARC_ZERO6_HARDFORK_BLOCK_ACTIVATION_DEVNET)
        );
    }

    #[test]
    fn test_arc_testnet_chainspec() {
        let spec = ArcChainSpecParser::parse("arc-testnet").expect("Failed to parse arc-testnet");
        assert_eq!(spec.chain().id(), TESTNET_CHAIN_ID);

        // Verify the genesis hash for testnet. The genesis hash should be the same.
        assert_eq!(
            spec.genesis_hash().to_string(),
            "0xe20e653af4441e8c6088e172b129d56420139824400477287b46e7101ae2bb1f",
            "the genesis hash of assets/testnet/genesis.json changed unexpectedly"
        );
        assert_arc_chainspec_evm_hardforks(&spec);
        assert!(!spec.is_osaka_active_at_timestamp(0));
        assert!(spec.is_osaka_active_at_timestamp(ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET));
        assert_eq!(spec.forks_iter().count(), 23);

        // Zero3
        let flags_before_zero3 =
            spec.get_hardfork_flags(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET - 1, 0);
        assert!(!flags_before_zero3.is_active(ArcHardfork::Zero3));
        assert!(!flags_before_zero3.is_active(ArcHardfork::Zero4));

        let flags_at_zero3 =
            spec.get_hardfork_flags(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET, 0);
        assert!(flags_at_zero3.is_active(ArcHardfork::Zero3));
        assert!(!flags_at_zero3.is_active(ArcHardfork::Zero4));
        assert_eq!(
            spec.fork(ArcHardfork::Zero3),
            ForkCondition::Block(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET)
        );

        // Zero4
        let flags_before_zero4 =
            spec.get_hardfork_flags(ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET - 1, 0);
        assert!(!flags_before_zero4.is_active(ArcHardfork::Zero4));

        let flags_at_zero4 =
            spec.get_hardfork_flags(ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET, 0);
        assert!(flags_at_zero4.is_active(ArcHardfork::Zero4));
        assert_eq!(
            spec.fork(ArcHardfork::Zero4),
            ForkCondition::Block(ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET)
        );

        // Zero5 — activates by timestamp on testnet. Use a block past Zero4's activation
        // so Zero4 still reads as active in the snapshot.
        let post_zero4_block = ARC_ZERO4_HARDFORK_BLOCK_ACTIVATION_TESTNET + 1;
        let flags_before_zero5 = spec.get_hardfork_flags(
            post_zero4_block,
            ARC_ZERO5_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET - 1,
        );
        assert!(flags_before_zero5.is_active(ArcHardfork::Zero4));
        assert!(!flags_before_zero5.is_active(ArcHardfork::Zero5));

        let flags_at_zero5 = spec.get_hardfork_flags(
            post_zero4_block,
            ARC_ZERO5_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET,
        );
        assert!(flags_at_zero5.is_active(ArcHardfork::Zero4));
        assert!(flags_at_zero5.is_active(ArcHardfork::Zero5));
        assert_eq!(
            spec.fork(ArcHardfork::Zero5),
            ForkCondition::Timestamp(ARC_ZERO5_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET)
        );

        // Zero6 — activates by timestamp on testnet (same timestamp as Zero5 by current schedule).
        let flags_before_zero6 = spec.get_hardfork_flags(
            post_zero4_block,
            ARC_ZERO6_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET - 1,
        );
        assert!(!flags_before_zero6.is_active(ArcHardfork::Zero6));

        let flags_at_zero6 = spec.get_hardfork_flags(
            post_zero4_block,
            ARC_ZERO6_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET,
        );
        assert!(flags_at_zero6.is_active(ArcHardfork::Zero5));
        assert!(flags_at_zero6.is_active(ArcHardfork::Zero6));
        assert_eq!(
            spec.fork(ArcHardfork::Zero6),
            ForkCondition::Timestamp(ARC_ZERO6_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET)
        );

        assert_eq!(
            spec.fork(EthereumHardfork::Osaka),
            ForkCondition::Timestamp(ARC_OSAKA_HARDFORK_TIMESTAMP_ACTIVATION_TESTNET)
        );

        assert_eq!(
            spec.display_hardforks().to_string(),
            r#"Pre-merge hard forks (block based):
- Frontier                         @0
- Homestead                        @0
- Tangerine                        @0
- SpuriousDragon                   @0
- Byzantium                        @0
- Constantinople                   @0
- Petersburg                       @0
- Istanbul                         @0
- MuirGlacier                      @0
- Berlin                           @0
- London                           @0
- ArrowGlacier                     @0
- GrayGlacier                      @0
- Zero3                            @11172019
- Zero4                            @26148086
Merge hard forks:
- Paris                            @0 (network is known to be merged)
Post-merge hard forks (timestamp based):
- Shanghai                         @0          blob: (target: 6, max: 9, fraction: 5007716)
- Cancun                           @0          blob: (target: 6, max: 9, fraction: 5007716)
- Prague                           @0          blob: (target: 6, max: 9, fraction: 5007716)
- Osaka                            @1779890400          blob: (target: 6, max: 9, fraction: 5007716)
- Zero5                            @1779894517          blob: (target: 6, max: 9, fraction: 5007716)
- Zero6                            @1779894517          blob: (target: 6, max: 9, fraction: 5007716)
- Zero7                            @1781791200          blob: (target: 6, max: 9, fraction: 5007716)"#
        );
    }

    #[test]
    fn test_gas_limit_config_localdev() {
        let spec = ArcChainSpecParser::parse("arc-localdev").expect("Failed to parse arc-localdev");
        let config = spec.block_gas_limit_config(0);
        assert_eq!(
            config,
            BlockGasLimitConfig::new(1_000_000, 1_000_000_000, 30_000_000)
        );
    }

    #[test]
    fn test_gas_limit_config_testnet() {
        let spec = ArcChainSpecParser::parse("arc-testnet").expect("Failed to parse arc-testnet");
        let config = spec.block_gas_limit_config(0);
        assert_eq!(
            config,
            BlockGasLimitConfig::new(10_000_000, 200_000_000, 30_000_000)
        );
    }

    #[test]
    fn test_gas_limit_config_devnet() {
        let spec = ArcChainSpecParser::parse("arc-devnet").expect("Failed to parse arc-devnet");
        let config = spec.block_gas_limit_config(0);
        assert_eq!(
            config,
            BlockGasLimitConfig::new(1_000_000, 1_000_000_000, 30_000_000)
        );
    }

    #[test]
    fn test_gas_limit_config_mainnet() {
        // Mainnet has no parseable chainspec name (genesis.json is gitignored), so build
        // a synthetic spec by cloning localdev and overriding the chain id.
        let mut spec =
            (*ArcChainSpecParser::parse("arc-localdev").expect("localdev parses")).clone();
        spec.inner.chain = Chain::from_id(MAINNET_CHAIN_ID);
        let config = spec.block_gas_limit_config(0);
        assert_eq!(
            config,
            BlockGasLimitConfig::new(10_000_000, 200_000_000, 30_000_000)
        );
    }

    #[test]
    fn test_base_fee_config_mainnet() {
        let mut spec =
            (*ArcChainSpecParser::parse("arc-localdev").expect("localdev parses")).clone();
        spec.inner.chain = Chain::from_id(MAINNET_CHAIN_ID);
        let cfg = spec.base_fee_config(0);

        assert_eq!(cfg.absolute_min_base_fee, 1);
        assert_eq!(cfg.absolute_max_base_fee, 20_000_000_000_000); // 20,000 gwei
        assert_eq!(cfg.alpha, BoundedParam::new(1, 20, 100));
        assert_eq!(cfg.k_rate, BoundedParam::new(1, 200, 1_000));
        assert_eq!(
            cfg.inverse_elasticity_multiplier,
            BoundedParam::new(1, 5000, 9_000)
        );
    }

    /// Exercises the named-chain arms of the match (the
    /// alternative path to the `arc-mainnet` / `arc-devnet` / `arc-testnet`
    /// parser, used when someone passes `--chain <path>`).
    #[test]
    fn test_from_genesis_named_chain_ids_apply_predefined_hardforks() {
        use alloy_genesis::Genesis;
        fn parse_with_chain_id(chain_id: u64) -> ArcChainSpec {
            let s = format!(
                r#"{{ "config": {{ "chainId": {} }}, "alloc": {{}} }}"#,
                chain_id
            );
            let genesis: Genesis = serde_json::from_str(&s).expect("parse genesis");
            ArcChainSpec::from(genesis)
        }

        // Mainnet
        let spec = parse_with_chain_id(MAINNET_CHAIN_ID);
        let flags = spec.get_hardfork_flags(0, 0);
        assert!(flags.is_active(ArcHardfork::Zero3));
        assert!(flags.is_active(ArcHardfork::Zero4));
        assert!(flags.is_active(ArcHardfork::Zero5));
        assert!(flags.is_active(ArcHardfork::Zero6));
        assert!(!flags.is_active(ArcHardfork::Zero7));
        assert!(spec.is_osaka_active_at_timestamp(0));

        // Devnet
        let spec = parse_with_chain_id(DEVNET_CHAIN_ID);
        assert!(spec
            .get_hardfork_flags(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_DEVNET, 0)
            .is_active(ArcHardfork::Zero3));
        assert!(!spec.get_hardfork_flags(0, 0).is_active(ArcHardfork::Zero3));

        // Testnet
        let spec = parse_with_chain_id(TESTNET_CHAIN_ID);
        assert!(spec
            .get_hardfork_flags(ARC_ZERO3_HARDFORK_BLOCK_ACTIVATION_TESTNET, 0)
            .is_active(ArcHardfork::Zero3));
        assert!(!spec.get_hardfork_flags(0, 0).is_active(ArcHardfork::Zero3));
    }

    /// Simulates the nightly-upgrade scenario: genesis.json with a future osakaTime.
    /// Verifies that From<Genesis> correctly reads osakaTime and activates Osaka
    /// only at the specified timestamp.
    #[test]
    fn test_from_genesis_with_future_osaka_time() {
        use alloy_genesis::Genesis;

        let s = r#"{
            "config": {
                "chainId": 1337,
                "zero3Block": 0, "zero4Block": 0, "zero5Block": 100,
                "osakaTime": 9999
            },
            "alloc": {}
        }"#;
        let genesis: Genesis = serde_json::from_str(s).expect("Failed to parse genesis");
        let spec = ArcChainSpec::from(genesis);

        // Osaka NOT active before timestamp 9999
        assert!(!spec.is_osaka_active_at_timestamp(0));
        assert!(!spec.is_osaka_active_at_timestamp(9998));
        // Osaka active at timestamp 9999
        assert!(spec.is_osaka_active_at_timestamp(9999));
        assert!(spec.is_osaka_active_at_timestamp(10000));

        // zero5 block-based activation also works
        assert!(!spec.is_fork_active_at_block(ArcHardfork::Zero5, 99));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero5, 100));
    }

    #[test]
    #[should_panic(expected = "invalid block gas limit config")]
    fn test_gas_limit_config_default_below_min_panics() {
        BlockGasLimitConfig::new(10, 100, 5);
    }

    #[test]
    #[should_panic(expected = "invalid block gas limit config")]
    fn test_gas_limit_config_default_above_max_panics() {
        BlockGasLimitConfig::new(10, 100, 200);
    }

    // --- BaseFeeConfig / BaseFeeCalcParams unit tests ---

    fn make_config() -> BaseFeeConfig {
        BaseFeeConfig::new(
            BoundedParam::new(1, 20, 100),
            BoundedParam::new(1, 200, 10_000),
            BoundedParam::new(1, 5000, 10_000),
            1,
            u64::MAX - 1,
        )
    }

    fn make_fee_params(
        alpha: u64,
        k_rate: u64,
        inverse_elasticity_multiplier: u64,
    ) -> crate::protocol_config::IProtocolConfig::FeeParams {
        crate::protocol_config::IProtocolConfig::FeeParams {
            alpha,
            kRate: k_rate,
            inverseElasticityMultiplier: inverse_elasticity_multiplier,
            minBaseFee: alloy_primitives::U256::from(1u64),
            maxBaseFee: alloy_primitives::U256::from(u64::MAX),
            blockGasLimit: alloy_primitives::U256::from(30_000_000u64),
        }
    }

    #[test]
    fn test_resolve_calc_params_none_returns_default() {
        let config = make_config();
        let calc = config.resolve_calc_params(None);
        assert_eq!(calc.alpha, config.alpha.default);
        assert_eq!(calc.k_rate, config.k_rate.default);
        assert_eq!(
            calc.inverse_elasticity_multiplier,
            config.inverse_elasticity_multiplier.default
        );
    }

    #[test]
    fn test_resolve_calc_params_in_range_passes_through() {
        let config = make_config();
        // All values within bounds but different from defaults
        let fp = make_fee_params(50, 500, 3000);
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(calc.alpha, 50);
        assert_eq!(calc.k_rate, 500);
        assert_eq!(calc.inverse_elasticity_multiplier, 3000);
    }

    #[test]
    fn test_resolve_calc_params_at_min_boundary_passes_through() {
        let config = make_config();
        let fp = make_fee_params(
            config.alpha.min,
            config.k_rate.min,
            config.inverse_elasticity_multiplier.min,
        );
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(calc.alpha, config.alpha.min);
        assert_eq!(calc.k_rate, config.k_rate.min);
        assert_eq!(
            calc.inverse_elasticity_multiplier,
            config.inverse_elasticity_multiplier.min
        );
    }

    #[test]
    fn test_resolve_calc_params_at_max_boundary_passes_through() {
        let config = make_config();
        let fp = make_fee_params(
            config.alpha.max,
            config.k_rate.max,
            config.inverse_elasticity_multiplier.max,
        );
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(calc.alpha, config.alpha.max);
        assert_eq!(calc.k_rate, config.k_rate.max);
        assert_eq!(
            calc.inverse_elasticity_multiplier,
            config.inverse_elasticity_multiplier.max
        );
    }

    #[test]
    fn test_resolve_calc_params_alpha_above_max_uses_default() {
        let config = make_config();
        let fp = make_fee_params(
            config.alpha.max + 1,
            config.k_rate.default + 1,
            config.inverse_elasticity_multiplier.default + 1,
        );
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(calc.alpha, config.alpha.default);
        // Unchanged
        assert_eq!(calc.k_rate, config.k_rate.default + 1);
        assert_eq!(
            calc.inverse_elasticity_multiplier,
            config.inverse_elasticity_multiplier.default + 1
        );
    }

    #[test]
    fn test_resolve_calc_params_k_rate_above_max_uses_default() {
        let config = make_config();
        let fp = make_fee_params(
            config.alpha.default + 1,
            config.k_rate.max + 1,
            config.inverse_elasticity_multiplier.default + 1,
        );
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(calc.k_rate, config.k_rate.default);
        // Unchanged
        assert_eq!(calc.alpha, config.alpha.default + 1);
        assert_eq!(
            calc.inverse_elasticity_multiplier,
            config.inverse_elasticity_multiplier.default + 1
        )
    }

    #[test]
    fn test_resolve_calc_params_elasticity_above_max_uses_default() {
        let config = make_config();
        let fp = make_fee_params(
            config.alpha.default + 1,
            config.k_rate.default + 1,
            config.inverse_elasticity_multiplier.max + 1,
        );
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(
            calc.inverse_elasticity_multiplier,
            config.inverse_elasticity_multiplier.default
        );
        assert_eq!(calc.k_rate, config.k_rate.default + 1);
        assert_eq!(calc.alpha, config.alpha.default + 1);
    }

    #[test]
    fn test_resolve_calc_params_alpha_below_min_uses_default() {
        let config = make_config();
        let fp = make_fee_params(
            config.alpha.min - 1,
            config.k_rate.default + 1,
            config.inverse_elasticity_multiplier.default + 1,
        );
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(
            calc.alpha,
            config.alpha.default // default
        );
        // Unchanged
        assert_eq!(calc.k_rate, config.k_rate.default + 1);
        assert_eq!(
            calc.inverse_elasticity_multiplier,
            config.inverse_elasticity_multiplier.default + 1
        );
    }

    #[test]
    fn test_resolve_calc_params_k_rate_below_min_uses_default() {
        let config = make_config();
        let fp = make_fee_params(
            config.alpha.default,
            config.k_rate.min - 1,
            config.inverse_elasticity_multiplier.default,
        );
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(
            calc.k_rate,
            config.k_rate.default // default
        );
    }

    #[test]
    fn test_resolve_calc_params_elasticity_below_min_uses_default() {
        let config = make_config();
        let fp = make_fee_params(
            config.alpha.default,
            config.k_rate.default,
            config.inverse_elasticity_multiplier.min - 1,
        );
        let calc = config.resolve_calc_params(Some(&fp));
        assert_eq!(
            calc.inverse_elasticity_multiplier,
            config.inverse_elasticity_multiplier.default // default
        );
    }

    #[test]
    fn test_clamp_absolute() {
        let config = BaseFeeConfig::new(
            BoundedParam::new(0, 20, 100),
            BoundedParam::new(0, 200, 10_000),
            BoundedParam::new(1, 5000, 10_000),
            100,
            1000,
        );
        assert_eq!(config.clamp_absolute(0), 100);
        assert_eq!(config.clamp_absolute(99), 100);
        assert_eq!(config.clamp_absolute(500), 500);
        assert_eq!(config.clamp_absolute(1000), 1000);
        assert_eq!(config.clamp_absolute(1001), 1000);
    }

    #[test]
    #[should_panic(expected = "invalid BaseFeeConfig")]
    fn test_base_fee_config_inverted_absolute_bounds_panics() {
        BaseFeeConfig::new(
            BoundedParam::new(0, 20, 100),
            BoundedParam::new(0, 200, 10_000),
            BoundedParam::new(1, 5000, 10_000),
            1000, // min > max — should panic
            100,
        );
    }

    #[test]
    #[should_panic(expected = "invalid BoundedParam")]
    fn test_bounded_param_inverted_bounds_panics() {
        BoundedParam::new(100u64, 20, 50);
    }

    static MOCK_ARC_HARDFORKS: LazyLock<[(Box<dyn Hardfork>, ForkCondition); 6]> =
        LazyLock::new(|| {
            [
                (ArcHardfork::Zero3.boxed(), ForkCondition::Block(0)),
                (ArcHardfork::Zero4.boxed(), ForkCondition::Block(10)),
                (
                    EthereumHardfork::Osaka.boxed(),
                    ForkCondition::Timestamp(1779244750),
                ),
                (
                    ArcHardfork::Zero5.boxed(),
                    ForkCondition::Timestamp(1779244760),
                ),
                (
                    ArcHardfork::Zero6.boxed(),
                    ForkCondition::Timestamp(1779244770),
                ),
                (
                    ArcHardfork::Zero7.boxed(),
                    ForkCondition::Timestamp(1779244780),
                ),
            ]
        });

    #[test]
    fn test_arc_hardfork_ids() {
        use reth_chainspec::Hardforks;

        let genesis: Genesis =
            serde_json::from_str(include_str!("../../../assets/devnet/genesis.json"))
                .expect("Can't deserialize Devnet genesis json");

        let mut prev_head = Head {
            number: 0,
            timestamp: 0,
            ..Default::default()
        };
        let make_spec = |i: usize| -> ArcChainSpec {
            let mut inner = ChainSpec::from_genesis(genesis.clone());
            inner.hardforks = BASE_FORKS.clone();
            for (hardfork, cond) in MOCK_ARC_HARDFORKS[0..i + 1].iter() {
                inner.hardforks.insert(hardfork, *cond);
            }
            ArcChainSpec::new(inner)
        };
        let mut prev_spec = make_spec(0);
        let mut prev_hardfork = MOCK_ARC_HARDFORKS[0].0.clone();

        for i in 1..MOCK_ARC_HARDFORKS.len() {
            let hardfork = MOCK_ARC_HARDFORKS[i].0.clone();
            let spec = make_spec(i);
            // simulate the next head according to the current fork condition
            let head = match MOCK_ARC_HARDFORKS[i].1 {
                ForkCondition::Block(block) => Head {
                    timestamp: prev_head.timestamp,
                    number: block,
                    ..Default::default()
                },
                ForkCondition::TTD {
                    fork_block: Some(block),
                    ..
                } => Head {
                    number: block,
                    ..Default::default()
                },
                ForkCondition::Timestamp(timestamp) => Head {
                    number: prev_head.number,
                    timestamp,
                    ..Default::default()
                },
                _ => panic!("unexpected fork condition"),
            };
            let msg = format!(
                "[iter={i}, {hardfork:?}, prev_head=({},{}), head=({},{})]",
                prev_head.number, prev_head.timestamp, head.number, head.timestamp
            );
            println!("{}", msg);

            let prev_filter = prev_spec.fork_filter(prev_head);
            let mut filter = spec.fork_filter(prev_head);

            // make sure when we add the next hardfork, it could still valid for previos version.
            let prev_fork_id = prev_filter.current();
            let fork_id = filter.current();

            assert_eq!(
                prev_fork_id.hash, fork_id.hash,
                "[{msg}] fork hash should be the same when add a new hardfork",
            );
            assert_eq!(
                filter.validate(prev_fork_id),
                Ok(()),
                "[{msg}] fork id for the prev verions should validate by new version"
            );
            assert_eq!(
                prev_filter.validate(fork_id),
                Ok(()),
                "[{msg}] fork id for the new verions should validate by previous version"
            );

            // fork_id() use a different compute path, verify the value is the same as filter
            assert_eq!(
                prev_spec.fork_id(&prev_head),
                prev_fork_id,
                "[{msg}] computed fork id should be the same as it from previous filter"
            );
            assert_eq!(
                spec.fork_id(&prev_head),
                fork_id,
                "[{msg}] spec.fork_id(&prev_head) mismatched"
            );
            let next_fork_id = spec.fork_id(&head);

            // Verify the fork ID by hardfork
            assert_eq!(
                prev_spec.inner.hardfork_fork_id(prev_hardfork.clone()),
                Some(prev_fork_id),
                "[{msg}] computed fork id by hardfork mismatched for previous spec"
            );
            assert_eq!(
                spec.inner.hardfork_fork_id(prev_hardfork.clone()),
                Some(fork_id)
            );
            assert_eq!(
                spec.inner.hardfork_fork_id(hardfork.clone()),
                Some(next_fork_id)
            );

            // Set the new head.
            let transition: Option<reth_chainspec::ForkTransition> = filter.set_head(head);
            assert!(
                transition.is_some(),
                "[{msg}] transition should be happened on next head"
            );

            // Verify the fork ID is the same as filter on next head
            assert_eq!(
                spec.fork_id(&head),
                filter.current(),
                "[{msg}] spec.fork_id(&head) mismatched on next head"
            );
            assert_eq!(
                next_fork_id,
                filter.current(),
                "[{msg}] computed fork ID mismatched on next head"
            );

            (prev_head, prev_spec, prev_hardfork) = (head, spec, hardfork);
        }
    }
}
