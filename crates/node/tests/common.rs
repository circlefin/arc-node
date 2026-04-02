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

#![allow(dead_code, clippy::unwrap_used)]

use alloy_genesis::Genesis;
use alloy_primitives::KECCAK256_EMPTY;
use alloy_sol_types::sol;
use arc_node_execution::{ArcEvmConfig, ArcEvmFactory};
use reth_chainspec::EthChainSpec;
use reth_e2e_test_utils::wallet::Wallet;
use reth_ethereum::evm::EthEvmConfig;
use reth_evm::{ConfigureEvm, EvmEnv, EvmFactory, InspectorFor};
use revm::{
    context::{BlockEnv, CfgEnv},
    database::InMemoryDB,
    inspector::NoOpInspector,
    state::{AccountInfo, Bytecode},
};
use revm_primitives::{hardfork::SpecId, keccak256};

use arc_execution_config::chainspec::LOCAL_DEV;

pub fn load_genesis() -> Genesis {
    serde_json::from_str(include_str!("../../../assets/localdev/genesis.json")).unwrap()
}

pub const WALLET_COUNT: usize = 10;
pub const WALLET_SENDER_INDEX: usize = 0;
pub const WALLET_RECEIVER_INDEX: usize = 1;
pub const WALLET_OPERATOR_INDEX: usize = 7;

pub fn insert_alloc_into_db(db: &mut InMemoryDB, genesis: &Genesis) {
    for addr in genesis.alloc.keys() {
        let data = genesis.alloc.get(addr).unwrap().clone();
        match data.code.clone() {
            Some(code) => db.insert_account_info(
                *addr,
                AccountInfo {
                    balance: data.balance,
                    nonce: data.nonce.unwrap_or_default(),
                    code_hash: keccak256(&code),
                    code: Some(Bytecode::new_raw(code)),
                    account_id: None,
                },
            ),
            None => db.insert_account_info(
                *addr,
                AccountInfo {
                    balance: data.balance,
                    nonce: data.nonce.unwrap_or_default(),
                    code_hash: KECCAK256_EMPTY,
                    code: None,
                    account_id: None,
                },
            ),
        }
        for (k, v) in data.storage_slots() {
            db.insert_account_storage(*addr, k.into(), v)
                .expect("insert storage");
        }
    }
}

pub fn setup_evm_env() -> (ArcEvmConfig, InMemoryDB, EvmEnv, Wallet) {
    let chain_spec = LOCAL_DEV.clone();
    setup_evm_env_with_chainspec(chain_spec)
}

pub fn setup_evm_env_with_chainspec(
    chain_spec: std::sync::Arc<arc_execution_config::chainspec::ArcChainSpec>,
) -> (ArcEvmConfig, InMemoryDB, EvmEnv, Wallet) {
    let genesis = chain_spec.genesis();

    // Create the in-memory database with the states in genesis file.
    let mut db = InMemoryDB::default();
    insert_alloc_into_db(&mut db, genesis);

    // Create testing wallets & config.
    let wallet = Wallet::new(WALLET_COUNT).with_chain_id(chain_spec.chain_id());
    let mut cfg_env = CfgEnv::new()
        .with_chain_id(chain_spec.chain_id())
        .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);

    cfg_env.disable_base_fee = true;
    let evm_config = ArcEvmConfig::new(EthEvmConfig::new_with_evm_factory(
        chain_spec.clone(),
        ArcEvmFactory::new(chain_spec.clone()),
    ));

    let block_env = BlockEnv::default();
    let evm_env = EvmEnv { cfg_env, block_env };
    (evm_config, db, evm_env, wallet)
}

pub fn setup_evm_with_chainspec(
    chain_spec: std::sync::Arc<arc_execution_config::chainspec::ArcChainSpec>,
) -> (
    <ArcEvmFactory as EvmFactory>::Evm<InMemoryDB, NoOpInspector>,
    Wallet,
) {
    let (evm_config, db, evm_env, wallet) = setup_evm_env_with_chainspec(chain_spec);
    let evm = evm_config.evm_with_env(db, evm_env);
    (evm, wallet)
}

pub fn setup_evm_with_chainspec_and_spec(
    chain_spec: std::sync::Arc<arc_execution_config::chainspec::ArcChainSpec>,
    spec: SpecId,
) -> (
    <ArcEvmFactory as EvmFactory>::Evm<InMemoryDB, NoOpInspector>,
    Wallet,
) {
    let (evm_config, db, mut evm_env, wallet) = setup_evm_env_with_chainspec(chain_spec);
    evm_env.cfg_env.spec = spec;
    let evm = evm_config.evm_with_env(db, evm_env);
    (evm, wallet)
}

pub fn setup_evm() -> (
    <ArcEvmFactory as EvmFactory>::Evm<InMemoryDB, NoOpInspector>,
    Wallet,
) {
    let (evm_config, db, evm_env, wallet) = setup_evm_env();
    let evm = evm_config.evm_with_env(db, evm_env);
    (evm, wallet)
}

pub fn setup_evm_with_inspector<I>(
    inspector: I,
) -> (<ArcEvmFactory as EvmFactory>::Evm<InMemoryDB, I>, Wallet)
where
    I: InspectorFor<ArcEvmConfig, InMemoryDB>,
{
    let (evm_config, db, evm_env, wallet) = setup_evm_env();
    let evm = evm_config.evm_with_env_and_inspector(db, evm_env, inspector);
    (evm, wallet)
}

sol! {
    contract NativeCoinAuthority {
        #[derive(PartialEq, Eq, Debug)]
        event NativeCoinMinted(address indexed recipient, uint256 amount);
        #[derive(PartialEq, Eq, Debug)]
        event NativeCoinBurned(address indexed from, uint256 amount);
        #[derive(PartialEq, Eq, Debug)]
        event NativeCoinTransferred(address indexed from, address indexed to, uint256 amount);
    }

    contract NativeFiatTokenV2_2 {
        function mint(address to, uint256 amount) public {}
        function burn(uint256 amount) public {}
        function blacklist(address account) public {}
        function owner() public view returns (address) {}

        #[derive(PartialEq, Eq, Debug)]
        event Mint(address indexed minter, address indexed recipient, uint256 value);
        #[derive(PartialEq, Eq, Debug)]
        event Burn(address indexed burner, uint256 value);
        #[derive(PartialEq, Eq, Debug)]
        event Transfer(address indexed from, address indexed to, uint256 value);
    }
}
