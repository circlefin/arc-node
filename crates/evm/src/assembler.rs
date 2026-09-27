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

use alloc::sync::Arc;
use alloy_consensus::Block;
use alloy_evm::{block::BlockExecutorFactory, eth::EthBlockExecutionCtx};
use arc_execution_config::{chainspec::ArcChainSpec, gas_fee::encode_base_fee_to_bytes};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_ethereum::evm::EthBlockAssembler;
use reth_ethereum_primitives::{Receipt, TransactionSigned};
use reth_evm::execute::{BlockAssembler, BlockAssemblerInput, BlockExecutionError};
use revm::context::Block as RevmBlockContext;
use revm_primitives::B256;

use arc_precompiles::system_accounting::{
    compute_gas_values_storage_slot, unpack_gas_values_from_storage, SYSTEM_ACCOUNTING_ADDRESS,
};

#[derive(Debug, Clone)]
pub struct ArcBlockAssembler<ChainSpec = ArcChainSpec> {
    chain_spec: Arc<ChainSpec>,
}

impl<ChainSpec> ArcBlockAssembler<ChainSpec> {
    pub fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self { chain_spec }
    }
}

impl<F, ChainSpec> BlockAssembler<F> for ArcBlockAssembler<ChainSpec>
where
    F: for<'a> BlockExecutorFactory<
        ExecutionCtx<'a> = EthBlockExecutionCtx<'a>,
        Transaction = TransactionSigned,
        Receipt = Receipt,
    >,
    ChainSpec: EthChainSpec + EthereumHardforks,
{
    type Block = Block<TransactionSigned>;

    fn assemble_block(
        &self,
        mut input: BlockAssemblerInput<'_, '_, F, <Self::Block as reth_node_api::Block>::Header>,
    ) -> Result<Self::Block, BlockExecutionError> {
        let assembler = EthBlockAssembler::new(self.chain_spec.clone());

        // Loading the gas values from the system accounting contract and insert to the extra data.
        let block_number_result =
            input
                .evm_env
                .block_env()
                .number()
                .try_into()
                .inspect_err(|err| {
                    tracing::warn!("Failed to convert block number to u64: {}", err);
                });

        if let Ok(block_number) = block_number_result {
            let slot = compute_gas_values_storage_slot(block_number);

            // If the state changed, read the new value from bundle_state.
            let mut value =
                if let Some(account) = input.bundle_state.account(&SYSTEM_ACCOUNTING_ADDRESS) {
                    account.storage_slot(slot.into())
                } else {
                    None
                };

            // Read from state provider if the state is not changed.
            if value.is_none() {
                // A provider failure must not be confused with an unset slot: returning
                // `None` here leaves `extra_data` empty, which validate_extra_data_base_fee
                // rejects, so a transient read error would produce an invalid block instead
                // of failing the build.
                value = input
                    .state_provider
                    .storage(SYSTEM_ACCOUNTING_ADDRESS, slot)
                    .map_err(BlockExecutionError::other)?
            }

            if let Some(value) = value {
                let gas_values = unpack_gas_values_from_storage(B256::from(value));
                if gas_values.nextBaseFee != 0 {
                    input.execution_ctx.extra_data =
                        encode_base_fee_to_bytes(gas_values.nextBaseFee);
                }
            } else {
                tracing::warn!("Gas value not found for block number: {}", block_number);
            }
        }

        assembler.assemble_block(input)
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use crate::evm::ArcEvmConfig;
    use alloc::{sync::Arc, vec, vec::Vec};
    use arc_execution_config::chainspec::{ArcChainSpec, LOCAL_DEV};

    use alloy_consensus::Header;
    use alloy_evm::{block::BlockExecutionResult, EvmEnv};
    use alloy_primitives::{Address, BlockNumber, Bytes, StorageKey, StorageValue, U256};
    use reth_ethereum::storage::{
        AccountReader, BlockHashReader, BytecodeReader, HashedPostStateProvider,
        StateProofProvider, StateProvider, StateRootProvider, StorageRootProvider,
    };
    use reth_evm::execute::ProviderError;
    use reth_primitives_traits::{Account, Bytecode, SealedHeader};
    use reth_trie_common::{
        updates::TrieUpdates, AccountProof, ExecutionWitnessMode, HashedPostState, HashedStorage,
        MultiProof, MultiProofTargets, StorageMultiProof, StorageProof, TrieInput,
    };
    use revm::context::{BlockEnv, CfgEnv};
    use revm::database::BundleState;
    use revm_primitives::hardfork::SpecId;

    type ProviderResult<T> = Result<T, ProviderError>;

    #[test]
    fn block_assembler_creation() {
        let chain_spec = LOCAL_DEV.clone();
        let assembler = ArcBlockAssembler::new(chain_spec.clone());

        // Verify the inner assembler's chain_spec points to the same
        assert!(Arc::ptr_eq(&assembler.chain_spec, &chain_spec));
    }

    /// A `StateProvider` whose `storage` result is fixed by the test.
    ///
    /// `assemble_block` only reaches the provider through
    /// `StateProvider::storage`, so the remaining methods are unreachable
    /// stubs that panic rather than fake answers.
    struct StubStateProvider {
        storage: ProviderResult<Option<StorageValue>>,
    }

    impl StubStateProvider {
        fn returning(storage: ProviderResult<Option<StorageValue>>) -> Self {
            Self { storage }
        }
    }

    impl StateProvider for StubStateProvider {
        fn storage(
            &self,
            _account: Address,
            _storage_key: StorageKey,
        ) -> ProviderResult<Option<StorageValue>> {
            self.storage.clone()
        }
    }

    impl BytecodeReader for StubStateProvider {
        fn bytecode_by_hash(&self, _code_hash: &B256) -> ProviderResult<Option<Bytecode>> {
            unimplemented!("not reached by assemble_block")
        }
    }

    impl BlockHashReader for StubStateProvider {
        fn block_hash(&self, _number: BlockNumber) -> ProviderResult<Option<B256>> {
            unimplemented!("not reached by assemble_block")
        }

        fn canonical_hashes_range(
            &self,
            _start: BlockNumber,
            _end: BlockNumber,
        ) -> ProviderResult<Vec<B256>> {
            unimplemented!("not reached by assemble_block")
        }
    }

    impl AccountReader for StubStateProvider {
        fn basic_account(&self, _address: &Address) -> ProviderResult<Option<Account>> {
            unimplemented!("not reached by assemble_block")
        }
    }

    impl StateRootProvider for StubStateProvider {
        fn state_root(&self, _hashed_state: HashedPostState) -> ProviderResult<B256> {
            unimplemented!("not reached by assemble_block")
        }

        fn state_root_from_nodes(&self, _input: TrieInput) -> ProviderResult<B256> {
            unimplemented!("not reached by assemble_block")
        }

        fn state_root_with_updates(
            &self,
            _hashed_state: HashedPostState,
        ) -> ProviderResult<(B256, TrieUpdates)> {
            unimplemented!("not reached by assemble_block")
        }

        fn state_root_from_nodes_with_updates(
            &self,
            _input: TrieInput,
        ) -> ProviderResult<(B256, TrieUpdates)> {
            unimplemented!("not reached by assemble_block")
        }
    }

    impl HashedPostStateProvider for StubStateProvider {
        fn hashed_post_state(&self, _bundle_state: &BundleState) -> HashedPostState {
            unimplemented!("not reached by assemble_block")
        }
    }

    impl StorageRootProvider for StubStateProvider {
        fn storage_root(
            &self,
            _address: Address,
            _hashed_storage: HashedStorage,
        ) -> ProviderResult<B256> {
            unimplemented!("not reached by assemble_block")
        }

        fn storage_proof(
            &self,
            _address: Address,
            _slot: B256,
            _hashed_storage: HashedStorage,
        ) -> ProviderResult<StorageProof> {
            unimplemented!("not reached by assemble_block")
        }

        fn storage_multiproof(
            &self,
            _address: Address,
            _slots: &[B256],
            _hashed_storage: HashedStorage,
        ) -> ProviderResult<StorageMultiProof> {
            unimplemented!("not reached by assemble_block")
        }
    }

    impl StateProofProvider for StubStateProvider {
        fn proof(
            &self,
            _input: TrieInput,
            _address: Address,
            _slots: &[B256],
        ) -> ProviderResult<AccountProof> {
            unimplemented!("not reached by assemble_block")
        }

        fn multiproof(
            &self,
            _input: TrieInput,
            _targets: MultiProofTargets,
        ) -> ProviderResult<MultiProof> {
            unimplemented!("not reached by assemble_block")
        }

        fn witness(
            &self,
            _input: TrieInput,
            _target: HashedPostState,
            _mode: ExecutionWitnessMode,
        ) -> ProviderResult<Vec<Bytes>> {
            unimplemented!("not reached by assemble_block")
        }
    }

    /// The block number the assembler will derive its storage slot from.
    const BLOCK_NUMBER: u64 = 7;

    fn block_env() -> BlockEnv {
        BlockEnv {
            number: U256::from(BLOCK_NUMBER),
            ..Default::default()
        }
    }

    fn execution_ctx<'a>() -> EthBlockExecutionCtx<'a> {
        EthBlockExecutionCtx {
            parent_hash: B256::ZERO,
            parent_beacon_block_root: None,
            ommers: &[],
            withdrawals: None,
            extra_data: Default::default(),
            tx_count_hint: None,
            slot_number: None,
        }
    }

    fn bundle_state() -> BundleState {
        BundleState::default()
    }

    fn execution_result() -> BlockExecutionResult<Receipt> {
        BlockExecutionResult::default()
    }

    /// Builds the real `BlockAssemblerInput` that `assemble_block` consumes.
    fn assembler_input<'a, 'b>(
        state_provider: &'b StubStateProvider,
        parent: &'a SealedHeader,
        bundle_state: &'a BundleState,
        output: &'b BlockExecutionResult<Receipt>,
    ) -> BlockAssemblerInput<'a, 'b, ArcEvmConfig, Header> {
        BlockAssemblerInput::new(
            EvmEnv::new(CfgEnv::new_with_spec(SpecId::default()), block_env()),
            execution_ctx(),
            parent,
            vec![],
            output,
            bundle_state,
            state_provider,
            B256::ZERO,
        )
    }

    /// A provider read failure must fail the build, not produce a block with
    /// empty `extra_data`.
    ///
    /// Before the fix, `unwrap_or(None)` turned the error into "slot unset",
    /// so `assemble_block` returned `Ok` with an empty `extra_data`, which
    /// `validate_extra_data_base_fee` then rejects on every validator.
    #[test]
    fn assemble_block_fails_when_state_provider_errors() {
        let assembler = ArcBlockAssembler::<ArcChainSpec>::new(LOCAL_DEV.clone());
        let parent = SealedHeader::new(Header::default(), B256::ZERO);
        let bundle_state = bundle_state();
        let output = execution_result();
        let provider =
            StubStateProvider::returning(Err(ProviderError::BlockHashNotFound(B256::ZERO)));

        let err = assembler
            .assemble_block(assembler_input(&provider, &parent, &bundle_state, &output))
            .expect_err("a provider read error must not yield a block");

        // The provider error is propagated verbatim, so the caller sees the
        // real cause rather than a generic "assemble failed".
        let internal = err
            .as_internal()
            .expect("provider errors are internal, not validation errors");
        assert!(
            internal.is_other::<ProviderError>(),
            "expected the ProviderError itself to be propagated, got: {err}"
        );
    }

    /// The genuine "slot is not set" case must keep its existing behaviour:
    /// log a warning and assemble a block, leaving `extra_data` empty.
    #[test]
    fn assemble_block_warns_and_continues_when_slot_unset() {
        let assembler = ArcBlockAssembler::<ArcChainSpec>::new(LOCAL_DEV.clone());
        let parent = SealedHeader::new(Header::default(), B256::ZERO);
        let bundle_state = bundle_state();
        let output = execution_result();
        let provider = StubStateProvider::returning(Ok(None));

        let block = assembler
            .assemble_block(assembler_input(&provider, &parent, &bundle_state, &output))
            .expect("an unset slot is not a build failure");

        assert!(
            block.header.extra_data.is_empty(),
            "no base fee is known, so extra_data must stay empty"
        );
    }
}
