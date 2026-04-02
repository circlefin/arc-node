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
                value = input
                    .state_provider
                    .storage(SYSTEM_ACCOUNTING_ADDRESS, slot)
                    .unwrap_or(None)
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
    use super::*;
    use alloc::sync::Arc;
    use arc_execution_config::chainspec::LOCAL_DEV;

    #[test]
    fn block_assembler_creation() {
        let chain_spec = LOCAL_DEV.clone();
        let assembler = ArcBlockAssembler::new(chain_spec.clone());

        // Verify the inner assembler's chain_spec points to the same
        assert!(Arc::ptr_eq(&assembler.chain_spec, &chain_spec));
    }
}
